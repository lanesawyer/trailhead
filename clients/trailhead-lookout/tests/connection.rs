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
use trailhead_daemon::net::{read_frame, write_frame};
use trailhead_daemon::ui::{RepoFeed, StatusFeed, StatusFrame, UI_ALPN, UiAllowlist, UiProtocol, Unsupported};
use trailhead_factory::{ControlState, FactoryEvent, FactorySnapshot, SandboxState, SandboxStatus, Sequenced};
use trailhead_lookout::connection::{Api, Connection, ConnectionPlugin, ConnectionStatus, LocalEndpoint, SendNet};
use trailhead_lookout::factory::{Campsite, FactoryLink, FactoryStatus};
use trailhead_lookout::net::{NetCommand, NetConfig, Network, Reason};

/// One repo, "o/r", with an idle sandbox, whose later events the test sends.
struct TestFeed(broadcast::Sender<Sequenced>);

fn sandbox(name: &str, state: SandboxState) -> SandboxStatus {
    SandboxStatus { name: name.into(), target: None, state, task: None, role: None, warm: false, own: false }
}

impl StatusFeed for TestFeed {
    fn subscribe(&self) -> Vec<RepoFeed> {
        let snapshot = FactorySnapshot {
            control: ControlState::Up,
            last_error: None,
            sandboxes: vec![sandbox("camp-1", SandboxState::Idle)],
            runs: vec![],
            seq: 1,
        };
        vec![RepoFeed { repo: "o/r".into(), snapshot, events: self.0.subscribe() }]
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
    let router = Router::builder(endpoint)
        .accept(
            UI_ALPN,
            UiProtocol::new(rig.server, rig.allowed.clone()).with_status(Arc::new(TestFeed(rig.events.clone()))),
        )
        .spawn();
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
        }
    }

    fn config(&self, lookup: &Arc<MemoryLookup>) -> NetConfig {
        NetConfig {
            config_dir: self.dir.path().join("lookout"),
            trailhead_config_dir: self.dir.path().join("config"),
            trailhead_data_dir: self.dir.path().join("data"),
            keyring: false,
            network: Network::Direct(lookup.clone()),
            server_url: None,
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
    app.world().resource::<ConnectionStatus>().reason.clone()
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
async fn when_the_factory_stops_the_last_state_stays_and_the_board_uses_the_server_url() {
    let rig = Rig::new().await;
    let lookup = Arc::new(MemoryLookup::new());
    let key = SecretKey::generate();
    let (router, addr) = control_plane(key.clone(), &lookup, &rig).await;
    let mut config = rig.config(&lookup);
    config.server_url = Some(format!("http://{}", rig.server));
    let mut app = app(config, Some(addr.id.to_string()));
    allowed_and_live(&mut app, &rig).await;
    until(&mut app, "snapshot", |a| link(a) == FactoryLink::Running).await;
    let tunnel = app.world().resource::<Api>().0.base.clone();

    router.shutdown().await.unwrap();
    until(&mut app, "factory stopped", |a| link(a) == FactoryLink::Stopped).await;
    // Trailhead's last known state is kept, marked stopped, and the board is reached without the control plane.
    assert_eq!(app.world().resource::<FactoryStatus>().label(), "factory stopped");
    assert_eq!(campsites(&mut app), [("camp-1".to_string(), SandboxState::Idle)]);
    let direct = format!("http://{}", rig.server);
    until(&mut app, "live on the server url", |a| {
        state(a) == Connection::Live && a.world().resource::<Api>().0.base == direct
    })
    .await;
    let api = &app.world().resource::<Api>().0;
    let me = reqwest::Client::new().get(format!("{}/v1/me", api.base)).bearer_auth(&api.token).send().await.unwrap();
    assert!(me.status().is_success());

    // The control plane comes back: the tunnel and a fresh snapshot replace the fallback.
    let (_router, _) = control_plane(key, &lookup, &rig).await;
    until(&mut app, "back on the tunnel", |a| {
        link(a) == FactoryLink::Running && state(a) == Connection::Live && a.world().resource::<Api>().0.base == tunnel
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_lookout_does_not_fall_back_to_the_server_url() {
    let rig = Rig::new().await;
    let lookup = Arc::new(MemoryLookup::new());
    let (_router, addr) = control_plane(SecretKey::generate(), &lookup, &rig).await;
    let mut config = rig.config(&lookup);
    config.server_url = Some(format!("http://{}", rig.server));
    let mut app = app(config, Some(addr.id.to_string()));
    until(&mut app, "refused", |a| reason(a) == Some(Reason::NotAllowed)).await;
    assert_eq!(state(&app), Connection::Offline);
}

#[test]
fn the_cli_server_url_comes_from_the_environment_then_the_config_then_the_default() {
    use trailhead_lookout::net::cli_server_url;
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(cli_server_url(dir.path(), None), "http://127.0.0.1:7241");
    std::fs::write(dir.path().join("config.toml"), "url = \"http://10.0.0.5:9000/\"\n").unwrap();
    assert_eq!(cli_server_url(dir.path(), None), "http://10.0.0.5:9000");
    assert_eq!(cli_server_url(dir.path(), Some("http://127.0.0.1:1".into())), "http://127.0.0.1:1");
}

/// A control plane from before the status request: it forwards the API, and answers anything else with the error.
#[derive(Debug, Clone)]
struct OldFactory(std::net::SocketAddr);

impl iroh::protocol::ProtocolHandler for OldFactory {
    async fn accept(&self, conn: iroh::endpoint::Connection) -> Result<(), iroh::protocol::AcceptError> {
        while let Ok((mut send, mut recv)) = conn.accept_bi().await {
            let server = self.0;
            tokio::spawn(async move {
                let request: serde_json::Value = read_frame(&mut recv).await.unwrap().unwrap();
                if request["op"] == "api" {
                    let tcp = tokio::net::TcpStream::connect(server).await.unwrap();
                    let (mut tcp_read, mut tcp_write) = tcp.into_split();
                    let up = tokio::io::copy(&mut recv, &mut tcp_write);
                    let down = tokio::io::copy(&mut tcp_read, &mut send);
                    let _ = tokio::join!(up, down);
                    let _ = send.finish();
                } else {
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
    let _router = Router::builder(endpoint).accept(UI_ALPN, OldFactory(rig.server)).spawn();
    let mut app = app(rig.config(&lookup), Some(addr.id.to_string()));

    allowed_and_live(&mut app, &rig).await;
    until(&mut app, "factory too old", |a| matches!(link(a), FactoryLink::TooOld(_))).await;
    let label = app.world().resource::<FactoryStatus>().label();
    assert!(label.contains("older than this Lookout") && label.contains("reinstall trailhead-factory"), "{label}");
    assert!(label.contains("status") && label.contains("old"), "the request and the build are named: {label}");
    // A later retry doesn't turn it into "factory stopped".
    for _ in 0..20 {
        app.update();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(matches!(link(&app), FactoryLink::TooOld(_)));
}
