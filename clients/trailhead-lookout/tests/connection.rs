//! Lookout's net thread and `Connection` state against an in-process server and control plane over a local iroh
//! connection (no relay, no discovery service).

use std::sync::Arc;
use std::time::Duration;

use bevy::prelude::*;
use iroh::address_lookup::MemoryLookup;
use iroh::endpoint::presets;
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey};
use tokio::sync::broadcast;
use trailhead_core::{Ctx, NewProject, Principal, Store};
use trailhead_factory::logs::{LogLevel, LogLine};
use trailhead_factory::{
    ControlState, FactoryEvent, FactorySnapshot, FactoryState, SandboxState, SandboxStatus, Sequenced,
};
use trailhead_lookout::connection::{Api, Connection, ConnectionPlugin, ConnectionStatus, LocalEndpoint, SendNet};
use trailhead_lookout::factory::{Campsite, FactoryLink, FactoryStatus};
use trailhead_lookout::logs::LogState;
use trailhead_lookout::net::{NetCommand, NetConfig, Network, Reason};
use trailhead_lookout::sidebar;
use trailhead_wire::ui::{LogFeed, RepoFeed, StatusFeed, StatusFrame, UI_ALPN, UiAllowlist, UiProtocol, Unsupported};
use trailhead_wire::{read_frame, write_frame};

/// One repo, "o/r", with an idle sandbox, whose later events the test sends.
struct TestFeed(broadcast::Sender<Sequenced>, Arc<std::sync::Mutex<Option<FactoryState>>>);

fn sandbox(name: &str, state: SandboxState) -> SandboxStatus {
    SandboxStatus { name: name.into(), target: None, state, task: None, role: None, warm: false, own: false }
}

impl StatusFeed for TestFeed {
    fn subscribe(&self) -> Vec<RepoFeed> {
        // Like the real feed, which has no repos until the control plane runs.
        if self.state().is_some_and(|s| s != FactoryState::Running) {
            return Vec::new();
        }
        let snapshot = FactorySnapshot {
            control: ControlState::Up,
            last_error: None,
            sandboxes: vec![sandbox("camp-1", SandboxState::Idle)],
            runs: vec![],
            holding: None,
            targets: Vec::new(),
            human_grades: None,
            queue: Vec::new(),
            usage: Vec::new(),
            seq: 1,
        };
        vec![RepoFeed { repo: "o/r".into(), snapshot, events: self.0.subscribe() }]
    }

    fn state(&self) -> Option<FactoryState> {
        self.1.lock().unwrap().clone()
    }
}

/// Two lines kept, and whatever the test sends after.
struct TestLogs(broadcast::Sender<LogLine>);

fn log_line(message: &str) -> LogLine {
    LogLine {
        ts_ms: 1,
        level: LogLevel::Info,
        target: "t".into(),
        repo: None,
        task: Some("TRAIL-1".into()),
        message: message.into(),
    }
}

impl LogFeed for TestLogs {
    fn subscribe(&self) -> (Vec<LogLine>, broadcast::Receiver<LogLine>) {
        (vec![log_line("one"), log_line("two")], self.0.subscribe())
    }
}

async fn control_plane(key: SecretKey, lookup: &Arc<MemoryLookup>, rig: &Rig) -> (Router, EndpointAddr) {
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(key)
        .relay_mode(RelayMode::Disabled)
        .address_lookup(lookup.clone())
        .bind()
        .await
        .unwrap();
    let addr = endpoint.addr();
    lookup.add_endpoint_info(addr.clone());
    let mut ui = UiProtocol::new(rig.server, rig.allowed.clone())
        .with_status(Arc::new(TestFeed(rig.events.clone(), rig.control.clone())));
    if let Some(logs) = &rig.logs {
        ui = ui.with_logs(Arc::new(TestLogs(logs.clone())));
    }
    let router = Router::builder(endpoint).accept(UI_ALPN, ui).spawn();
    (router, addr)
}

struct Rig {
    dir: tempfile::TempDir,
    server: std::net::SocketAddr,
    allowed: UiAllowlist,
    token: String,
    agent_token: String,
    /// The server's state and the user's context, to revoke the token while Lookout is signed in.
    state: Arc<trailhead_server::AppState>,
    human: Ctx,
    token_id: uuid::Uuid,
    events: broadcast::Sender<Sequenced>,
    /// The log the control plane serves; `None` for a control plane that doesn't know the request.
    logs: Option<broadcast::Sender<LogLine>>,
    /// The control plane state the status frame carries.
    control: Arc<std::sync::Mutex<Option<FactoryState>>>,
}

impl Rig {
    async fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("trailhead.db");
        let store = Store::open_local(db.to_str().unwrap()).await.unwrap();
        let (ws, user) = store.bootstrap_local("Lane").await.unwrap();
        let human = Ctx::user(ws.id, user.id);
        let project = NewProject { key: "TRAIL".into(), name: "Trailhead".into(), ..Default::default() };
        store.create_project(&human, project).await.unwrap();
        let (token_row, token) = store.create_token(&human, Principal::User(user.id), "ui").await.unwrap();
        let agent = store.create_agent(&human, "bot").await.unwrap();
        let (_, agent_token) = store.create_token(&human, Principal::Agent(agent.id), "bot").await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = listener.local_addr().unwrap();
        let state = Arc::new(trailhead_server::AppState::new(store));
        let app = trailhead_server::router(state.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let allowed = UiAllowlist::new(dir.path().join("ui-allowed"));
        Rig {
            dir,
            server,
            allowed,
            token,
            agent_token,
            state,
            human,
            token_id: token_row.id,
            events: broadcast::channel(16).0,
            logs: Some(broadcast::channel(16).0),
            control: Arc::new(std::sync::Mutex::new(Some(FactoryState::Running))),
        }
    }

    fn config(&self, lookup: &Arc<MemoryLookup>) -> NetConfig {
        NetConfig {
            config_dir: self.dir.path().join("lookout"),
            trailhead_config_dir: self.dir.path().join("config"),
            trailhead_data_dir: self.dir.path().join("data"),
            keyring: false,
            network: Network::Direct(lookup.clone()),
            status_retry: Duration::from_millis(100),
        }
    }
}

fn app(config: NetConfig, target: Option<String>) -> App {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, bevy::state::app::StatesPlugin))
        .add_plugins(ConnectionPlugin::new(config, target));
    app
}

fn state(app: &App) -> Connection {
    *app.world().resource::<State<Connection>>().get()
}

fn reason(app: &App) -> Option<Reason> {
    app.world().resource::<ConnectionStatus>().reason.as_ref().map(|r| r.cause().clone())
}

/// Runs the app until `done` holds, or fails with where it got stuck.
async fn until(app: &mut App, what: &str, done: impl Fn(&App) -> bool) {
    for _ in 0..400 {
        app.update();
        if done(app) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("never reached: {what}; stuck at {:?} ({:?})", state(app), reason(app));
}

fn send(app: &mut App, command: NetCommand) {
    app.world_mut().write_message(SendNet(command));
}

#[tokio::test(flavor = "multi_thread")]
async fn connects_signs_in_and_goes_live() {
    let rig = Rig::new().await;
    let lookup = Arc::new(MemoryLookup::new());
    let (_router, addr) = control_plane(SecretKey::generate(), &lookup, &rig).await;
    let mut app = app(rig.config(&lookup), Some(addr.id.to_string()));

    // Its own id shows up so a person can allow it, and until they do the refusal says so.
    until(&mut app, "endpoint id", |a| a.world().resource::<LocalEndpoint>().0.is_some()).await;
    let me: iroh::EndpointId = app.world().resource::<LocalEndpoint>().0.clone().unwrap().parse().unwrap();
    until(&mut app, "refused", |a| reason(a) == Some(Reason::NotAllowed)).await;
    assert_eq!(state(&app), Connection::Offline);

    rig.allowed.add(me).unwrap();
    until(&mut app, "needs a login", |a| reason(a) == Some(Reason::NeedsLogin)).await;
    send(&mut app, NetCommand::Login("nope".into()));
    until(&mut app, "token rejected", |a| reason(a) == Some(Reason::TokenRejected)).await;
    // An agent's token isn't a way in: Lookout acts only as the user.
    send(&mut app, NetCommand::Login(rig.agent_token.clone()));
    until(&mut app, "agent token refused", |a| reason(a) == Some(Reason::AgentToken)).await;
    assert!(app.world().get_resource::<Api>().is_none());

    send(&mut app, NetCommand::Login(rig.token.clone()));
    until(&mut app, "live", |a| state(a) == Connection::Live).await;
    assert_eq!(reason(&app), None);
    let api = &app.world().resource::<Api>().0;
    assert_eq!((api.token.as_str(), api.user.as_str()), (rig.token.as_str(), "Lane"));
    let me = reqwest::Client::new().get(format!("{}/v1/me", api.base)).bearer_auth(&api.token).send().await.unwrap();
    assert!(me.status().is_success());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_token_revoked_while_live_is_noticed() {
    let rig = Rig::new().await;
    let lookup = Arc::new(MemoryLookup::new());
    let (_router, addr) = control_plane(SecretKey::generate(), &lookup, &rig).await;
    let mut app = app(rig.config(&lookup), Some(addr.id.to_string()));
    allowed_and_live(&mut app, &rig).await;

    rig.state.store.revoke_token(&rig.human, rig.token_id).await.unwrap();
    until(&mut app, "token rejected", |a| reason(a) == Some(Reason::TokenRejected)).await;
    assert_eq!(state(&app), Connection::Offline);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_saved_login_and_target_are_used_on_the_next_start() {
    let rig = Rig::new().await;
    let lookup = Arc::new(MemoryLookup::new());
    let (_router, addr) = control_plane(SecretKey::generate(), &lookup, &rig).await;
    let config = rig.config(&lookup);
    let mut first = app(config.clone(), Some(addr.id.to_string()));
    until(&mut first, "endpoint id", |a| a.world().resource::<LocalEndpoint>().0.is_some()).await;
    let id = first.world().resource::<LocalEndpoint>().0.clone().unwrap();
    rig.allowed.add(id.parse().unwrap()).unwrap();
    send(&mut first, NetCommand::Login(rig.token.clone()));
    until(&mut first, "live", |a| state(a) == Connection::Live).await;
    drop(first);

    // Same key, same target, and the token the CLI would have saved: no arguments, no login.
    let mut second = app(config, None);
    until(&mut second, "live again", |a| state(a) == Connection::Live).await;
    assert_eq!(second.world().resource::<LocalEndpoint>().0.as_deref(), Some(id.as_str()));
}

#[tokio::test(flavor = "multi_thread")]
async fn goes_offline_when_the_control_plane_stops_and_comes_back() {
    let rig = Rig::new().await;
    let lookup = Arc::new(MemoryLookup::new());
    let key = SecretKey::generate();
    let (router, addr) = control_plane(key.clone(), &lookup, &rig).await;
    let mut app = app(rig.config(&lookup), Some(addr.id.to_string()));
    until(&mut app, "endpoint id", |a| a.world().resource::<LocalEndpoint>().0.is_some()).await;
    rig.allowed.add(app.world().resource::<LocalEndpoint>().0.clone().unwrap().parse().unwrap()).unwrap();
    send(&mut app, NetCommand::Login(rig.token.clone()));
    until(&mut app, "live", |a| state(a) == Connection::Live).await;

    router.shutdown().await.unwrap();
    until(&mut app, "offline", |a| state(a) == Connection::Offline).await;
    assert!(matches!(reason(&app), Some(Reason::Unreachable(_))), "{:?}", reason(&app));

    let (_router, _) = control_plane(key, &lookup, &rig).await;
    until(&mut app, "live again", |a| state(a) == Connection::Live).await;
    assert_eq!(reason(&app), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bad_endpoint_id_is_explained() {
    let rig = Rig::new().await;
    let lookup = Arc::new(MemoryLookup::new());
    let mut app = app(rig.config(&lookup), None);
    until(&mut app, "no target", |a| reason(a) == Some(Reason::NoTarget)).await;
    send(&mut app, NetCommand::Connect("not an id".into()));
    until(&mut app, "bad target", |a| matches!(reason(a), Some(Reason::BadTarget(_)))).await;
    assert_eq!(state(&app), Connection::Offline);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bad_connect_replaces_the_old_target_and_its_error_stays() {
    let rig = Rig::new().await;
    let lookup = Arc::new(MemoryLookup::new());
    let (_router, addr) = control_plane(SecretKey::generate(), &lookup, &rig).await;
    let mut app = app(rig.config(&lookup), Some(addr.id.to_string()));
    until(&mut app, "endpoint id", |a| a.world().resource::<LocalEndpoint>().0.is_some()).await;
    rig.allowed.add(app.world().resource::<LocalEndpoint>().0.clone().unwrap().parse().unwrap()).unwrap();
    send(&mut app, NetCommand::Login(rig.token.clone()));
    until(&mut app, "live", |a| state(a) == Connection::Live).await;

    send(&mut app, NetCommand::Connect("not an id".into()));
    until(&mut app, "bad target", |a| matches!(reason(a), Some(Reason::BadTarget(_)))).await;
    // Past a probe (every 2s), the old control plane hasn't taken the link back.
    for _ in 0..100 {
        app.update();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(state(&app), Connection::Offline);
    assert!(matches!(reason(&app), Some(Reason::BadTarget(_))), "{:?}", reason(&app));
}

fn campsites(app: &mut App) -> Vec<(String, SandboxState)> {
    let world = app.world_mut();
    let mut all: Vec<_> =
        world.query::<&Campsite>().iter(world).map(|c| (c.status.name.clone(), c.status.state)).collect();
    all.sort_by(|a, b| a.0.cmp(&b.0));
    all
}

fn link(app: &App) -> FactoryLink {
    app.world().resource::<FactoryStatus>().link.clone()
}

/// The words on the header's connection chip for the app's current links.
fn chip_words(app: &App) -> &'static str {
    let w = app.world();
    sidebar::connection_chip(state(app), w.resource::<ConnectionStatus>(), &w.resource::<FactoryStatus>().link).1
}

async fn allowed_and_live(app: &mut App, rig: &Rig) {
    until(app, "endpoint id", |a| a.world().resource::<LocalEndpoint>().0.is_some()).await;
    rig.allowed.add(app.world().resource::<LocalEndpoint>().0.clone().unwrap().parse().unwrap()).unwrap();
    send(app, NetCommand::Login(rig.token.clone()));
    until(app, "live", |a| state(a) == Connection::Live).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_factory_status_arrives_over_the_ui_protocol() {
    let rig = Rig::new().await;
    let lookup = Arc::new(MemoryLookup::new());
    let (_router, addr) = control_plane(SecretKey::generate(), &lookup, &rig).await;
    let mut app = app(rig.config(&lookup), Some(addr.id.to_string()));
    allowed_and_live(&mut app, &rig).await;

    // The snapshot has the idle campsite; the stream adds a busy one.
    until(&mut app, "snapshot", |a| link(a) == FactoryLink::Running).await;
    assert_eq!(campsites(&mut app), [("camp-1".to_string(), SandboxState::Idle)]);
    let busy = FactoryEvent::SandboxChanged { sandbox: sandbox("camp-2", SandboxState::Busy) };
    rig.events.send(Sequenced { seq: 2, event: busy }).unwrap();
    until(&mut app, "event", |a| a.world().resource::<FactoryStatus>().repos["o/r"].seq == 2).await;
    assert_eq!(campsites(&mut app).len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_log_is_followed_only_while_asked_for() {
    let rig = Rig::new().await;
    let lookup = Arc::new(MemoryLookup::new());
    let (_router, addr) = control_plane(SecretKey::generate(), &lookup, &rig).await;
    let mut app = app(rig.config(&lookup), Some(addr.id.to_string()));
    allowed_and_live(&mut app, &rig).await;
    let messages =
        |a: &App| a.world().resource::<LogState>().entries().map(|e| e.line.message.clone()).collect::<Vec<_>>();

    // Nothing is followed until the Logs page asks.
    for _ in 0..10 {
        app.update();
    }
    assert!(messages(&app).is_empty());

    app.world_mut().resource_mut::<LogState>().set_streaming(true);
    send(&mut app, NetCommand::Logs(true));
    until(&mut app, "the backlog", |a| messages(a) == ["one", "two"]).await;
    rig.logs.as_ref().unwrap().send(log_line("three")).unwrap();
    until(&mut app, "a new line", |a| messages(a) == ["one", "two", "three"]).await;

    // Closed, nobody is following: a line sent now never arrives.
    app.world_mut().resource_mut::<LogState>().set_streaming(false);
    send(&mut app, NetCommand::Logs(false));
    for _ in 0..20 {
        app.update();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // No receiver left on the control plane's side: it stopped serving the stream.
    assert!(rig.logs.as_ref().unwrap().send(log_line("four")).is_err());
    for _ in 0..20 {
        app.update();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!messages(&app).contains(&"four".to_string()));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_factory_without_logs_is_too_old_to_send_them() {
    let mut rig = Rig::new().await;
    rig.logs = None;
    let lookup = Arc::new(MemoryLookup::new());
    let (_router, addr) = control_plane(SecretKey::generate(), &lookup, &rig).await;
    let mut app = app(rig.config(&lookup), Some(addr.id.to_string()));
    allowed_and_live(&mut app, &rig).await;
    app.world_mut().resource_mut::<LogState>().set_streaming(true);
    send(&mut app, NetCommand::Logs(true));
    until(&mut app, "the note", |a| a.world().resource::<LogState>().note().is_some()).await;
    assert_eq!(app.world().resource::<LogState>().note(), Some("this factory is too old to send logs"));
}

#[tokio::test(flavor = "multi_thread")]
async fn when_the_server_goes_away_the_last_state_stays_and_it_is_unreachable() {
    let rig = Rig::new().await;
    let lookup = Arc::new(MemoryLookup::new());
    let key = SecretKey::generate();
    let (router, addr) = control_plane(key.clone(), &lookup, &rig).await;
    let mut app = app(rig.config(&lookup), Some(addr.id.to_string()));
    allowed_and_live(&mut app, &rig).await;
    until(&mut app, "snapshot", |a| link(a) == FactoryLink::Running).await;
    let tunnel = app.world().resource::<Api>().0.base.clone();

    router.shutdown().await.unwrap();
    until(&mut app, "unreachable", |a| link(a) == FactoryLink::Unreachable).await;
    // The last known state is kept, and there is no way around the tunnel: the board goes offline.
    assert_eq!(app.world().resource::<FactoryStatus>().label(), "unreachable");
    assert_eq!(campsites(&mut app), [("camp-1".to_string(), SandboxState::Idle)]);
    until(&mut app, "offline", |a| state(a) == Connection::Offline).await;

    // The control plane comes back: the tunnel and a fresh snapshot.
    let (_router, _) = control_plane(key, &lookup, &rig).await;
    until(&mut app, "back on the tunnel", |a| {
        link(a) == FactoryLink::Running && state(a) == Connection::Live && a.world().resource::<Api>().0.base == tunnel
    })
    .await;
    assert_eq!(chip_words(&app), "Live");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_control_plane_that_is_not_running_is_shown_over_the_board() {
    let rig = Rig::new().await;
    let lookup = Arc::new(MemoryLookup::new());
    *rig.control.lock().unwrap() = Some(FactoryState::Starting { reason: Some("sprites down".into()) });
    let (_router, addr) = control_plane(SecretKey::generate(), &lookup, &rig).await;
    let mut app = app(rig.config(&lookup), Some(addr.id.to_string()));
    allowed_and_live(&mut app, &rig).await;
    until(&mut app, "control", |a| matches!(link(a), FactoryLink::Control(_))).await;
    assert_eq!(app.world().resource::<FactoryStatus>().label(), "factory starting: sprites down");
    assert_eq!(chip_words(&app), "Factory starting");
    // The server ends the stream with no repos to feed; the link stays at the control state across retries.
    for _ in 0..30 {
        app.update();
        assert!(matches!(link(&app), FactoryLink::Control(_)), "{:?}", link(&app));
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    *rig.control.lock().unwrap() = Some(FactoryState::Running);
    until(&mut app, "running", |a| link(a) == FactoryLink::Running).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_lookout_is_offline_not_redirected() {
    let rig = Rig::new().await;
    let lookup = Arc::new(MemoryLookup::new());
    let (_router, addr) = control_plane(SecretKey::generate(), &lookup, &rig).await;
    let mut app = app(rig.config(&lookup), Some(addr.id.to_string()));
    until(&mut app, "refused", |a| reason(a) == Some(Reason::NotAllowed)).await;
    assert_eq!(state(&app), Connection::Offline);
}

/// A control plane from before the status request: it forwards the API, and answers anything else with the error.
#[derive(Debug, Clone)]
struct OldFactory {
    server: std::net::SocketAddr,
    /// Closes the connection on a status request instead of answering it, as a link that keeps dropping would.
    drop_status: bool,
    /// When each status request arrived.
    asked: Arc<std::sync::Mutex<Vec<std::time::Instant>>>,
}

impl iroh::protocol::ProtocolHandler for OldFactory {
    async fn accept(&self, conn: iroh::endpoint::Connection) -> Result<(), iroh::protocol::AcceptError> {
        while let Ok((mut send, mut recv)) = conn.accept_bi().await {
            let (server, drop_status, asked) = (self.server, self.drop_status, self.asked.clone());
            let conn = conn.clone();
            tokio::spawn(async move {
                let request: serde_json::Value = read_frame(&mut recv).await.unwrap().unwrap();
                if request["op"] == "api" {
                    let tcp = tokio::net::TcpStream::connect(server).await.unwrap();
                    let (mut tcp_read, mut tcp_write) = tcp.into_split();
                    let up = tokio::io::copy(&mut recv, &mut tcp_write);
                    let down = tokio::io::copy(&mut tcp_read, &mut send);
                    let _ = tokio::join!(up, down);
                    let _ = send.finish();
                } else if request["op"] == "status" && drop_status {
                    asked.lock().unwrap().push(std::time::Instant::now());
                    conn.close(9u32.into(), b"gone");
                } else {
                    asked.lock().unwrap().push(std::time::Instant::now());
                    let unsupported = Unsupported {
                        got: request,
                        known: vec!["api".into()],
                        version: "0.1.0".into(),
                        build: "old".into(),
                    };
                    write_frame(&mut send, &StatusFrame::Unsupported(unsupported)).await.unwrap();
                    let _ = send.finish();
                    let _ = send.stopped().await;
                }
            });
        }
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_factory_older_than_lookout_says_so() {
    let rig = Rig::new().await;
    let lookup = Arc::new(MemoryLookup::new());
    let key = SecretKey::generate();
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(key)
        .relay_mode(RelayMode::Disabled)
        .address_lookup(lookup.clone())
        .bind()
        .await
        .unwrap();
    let addr = endpoint.addr();
    lookup.add_endpoint_info(addr.clone());
    let _router = Router::builder(endpoint)
        .accept(UI_ALPN, OldFactory { server: rig.server, drop_status: false, asked: Arc::default() })
        .spawn();
    let mut app = app(rig.config(&lookup), Some(addr.id.to_string()));

    allowed_and_live(&mut app, &rig).await;
    until(&mut app, "factory too old", |a| matches!(link(a), FactoryLink::TooOld(_))).await;
    let label = app.world().resource::<FactoryStatus>().label();
    assert!(
        label.contains("older than this Lookout") && label.contains("reinstall trailhead and restart its server"),
        "{label}"
    );
    assert!(label.contains("status") && label.contains("old"), "the request and the build are named: {label}");
    // A later retry doesn't turn it into "factory stopped".
    for _ in 0..20 {
        app.update();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(matches!(link(&app), FactoryLink::TooOld(_)));
}

/// Starts a factory that refuses or drops every status request, and an app dialing it. Returns the app, the times of
/// the status requests, and the router that keeps the factory up.
async fn app_on_old_factory(
    rig: &Rig,
    drop_status: bool,
) -> (App, Arc<std::sync::Mutex<Vec<std::time::Instant>>>, Router) {
    let lookup = Arc::new(MemoryLookup::new());
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::generate())
        .relay_mode(RelayMode::Disabled)
        .address_lookup(lookup.clone())
        .bind()
        .await
        .unwrap();
    let addr = endpoint.addr();
    lookup.add_endpoint_info(addr.clone());
    let asked = Arc::<std::sync::Mutex<Vec<std::time::Instant>>>::default();
    let factory = OldFactory { server: rig.server, drop_status, asked: asked.clone() };
    let router = Router::builder(endpoint).accept(UI_ALPN, factory).spawn();
    (app(rig.config(&lookup), Some(addr.id.to_string())), asked, router)
}

/// The gaps between the status requests, once `count` have arrived.
async fn gaps_after(
    app: &mut App,
    asked: &Arc<std::sync::Mutex<Vec<std::time::Instant>>>,
    count: usize,
) -> Vec<Duration> {
    for _ in 0..400 {
        app.update();
        if asked.lock().unwrap().len() >= count {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let times = asked.lock().unwrap().clone();
    assert!(times.len() >= count, "only {} status requests", times.len());
    times.windows(2).map(|w| w[1] - w[0]).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_status_request_is_retried_with_growing_gaps() {
    let rig = Rig::new().await;
    let (mut app, asked, _router) = app_on_old_factory(&rig, false).await;
    allowed_and_live(&mut app, &rig).await;
    // The retry is 100 ms here, so the gaps are about 100, 200, 400 and 800 ms.
    let gaps = gaps_after(&mut app, &asked, 5).await;
    for pair in gaps.windows(2) {
        assert!(pair[1] > pair[0].mul_f32(1.5), "the gaps grow: {gaps:?}");
    }
    let label = app.world().resource::<FactoryStatus>().label();
    assert!(label.contains("trying again in"), "the next try is shown: {label}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_link_reconnects_quickly() {
    let rig = Rig::new().await;
    let (mut app, asked, _router) = app_on_old_factory(&rig, true).await;
    allowed_and_live(&mut app, &rig).await;
    let gaps = gaps_after(&mut app, &asked, 5).await;
    // Refusals would have reached 800 ms by now.
    assert!(gaps.iter().all(|g| *g < Duration::from_millis(500)), "the retry stays quick: {gaps:?}");
    assert!(matches!(link(&app), FactoryLink::Unreachable));
}

/// A control plane whose allowlist doesn't have this Lookout: it closes every connection, and notes when.
#[derive(Debug, Clone)]
struct StrictFactory {
    dialed: Arc<std::sync::Mutex<Vec<std::time::Instant>>>,
}

impl iroh::protocol::ProtocolHandler for StrictFactory {
    async fn accept(&self, conn: iroh::endpoint::Connection) -> Result<(), iroh::protocol::AcceptError> {
        self.dialed.lock().unwrap().push(std::time::Instant::now());
        conn.close(trailhead_wire::ui::CLOSE_NOT_ALLOWED.into(), b"not allowed");
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lookout_that_is_not_allowed_is_dialed_with_growing_gaps() {
    let rig = Rig::new().await;
    let lookup = Arc::new(MemoryLookup::new());
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::generate())
        .relay_mode(RelayMode::Disabled)
        .address_lookup(lookup.clone())
        .bind()
        .await
        .unwrap();
    let addr = endpoint.addr();
    lookup.add_endpoint_info(addr.clone());
    let dialed = Arc::<std::sync::Mutex<Vec<std::time::Instant>>>::default();
    let _router = Router::builder(endpoint).accept(UI_ALPN, StrictFactory { dialed: dialed.clone() }).spawn();
    let mut app = app(rig.config(&lookup), Some(addr.id.to_string()));

    until(&mut app, "refused", |a| reason(a) == Some(Reason::NotAllowed)).await;
    let from = dialed.lock().unwrap().len();
    // Probing every 100 ms for 2 s would dial about 60 times (the probe dials twice, and the status stream once).
    for _ in 0..40 {
        app.update();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let dials = dialed.lock().unwrap().len() - from;
    assert!(dials < 30, "the refusal is retried with growing gaps, but it was dialed {dials} times in 2 s");
    let shown = app.world().resource::<ConnectionStatus>().reason.clone().unwrap().to_string();
    assert!(shown.contains("trying again in"), "the next try is shown: {shown}");
}

/// A server built without the factory has no status feed: the op is reset, and Lookout shows what it shows for a
/// control plane that refuses the status request.
#[tokio::test(flavor = "multi_thread")]
async fn a_server_without_the_factory_is_shown_like_one_with_no_status_feed() {
    let rig = Rig::new().await;
    let lookup = Arc::new(MemoryLookup::new());
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::generate())
        .relay_mode(RelayMode::Disabled)
        .address_lookup(lookup.clone())
        .bind()
        .await
        .unwrap();
    let addr = endpoint.addr();
    lookup.add_endpoint_info(addr.clone());
    let ui = UiProtocol::new(rig.server, rig.allowed.clone());
    let _router = Router::builder(endpoint).accept(UI_ALPN, ui).spawn();
    let mut app = app(rig.config(&lookup), Some(addr.id.to_string()));
    allowed_and_live(&mut app, &rig).await;
    until(&mut app, "no status feed", |a| matches!(link(a), FactoryLink::Refused(_))).await;
    for _ in 0..20 {
        app.update();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(matches!(link(&app), FactoryLink::Refused(_)), "{:?}", link(&app));
}
