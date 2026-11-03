//! Lookout's net thread and `Connection` state against an in-process server and control plane over a local iroh
//! connection (no relay, no discovery service).

use std::sync::Arc;
use std::time::Duration;

use bevy::prelude::*;
use iroh::address_lookup::MemoryLookup;
use iroh::endpoint::presets;
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey};
use trailhead_core::{Ctx, NewProject, Principal, Store};
use trailhead_daemon::ui::{UI_ALPN, UiAllowlist, UiProtocol};
use trailhead_lookout::connection::{Api, Connection, ConnectionPlugin, ConnectionStatus, LocalEndpoint, SendNet};
use trailhead_lookout::net::{NetCommand, NetConfig, Network, Reason};

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
    let router = Router::builder(endpoint).accept(UI_ALPN, UiProtocol::new(rig.server, rig.allowed.clone())).spawn();
    (router, addr)
}

struct Rig {
    dir: tempfile::TempDir,
    server: std::net::SocketAddr,
    allowed: UiAllowlist,
    token: String,
    agent_token: String,
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
        let (_, token) = store.create_token(&human, Principal::User(user.id), "ui").await.unwrap();
        let agent = store.create_agent(&human, "bot").await.unwrap();
        let (_, agent_token) = store.create_token(&human, Principal::Agent(agent.id), "bot").await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = listener.local_addr().unwrap();
        let app = trailhead_server::router(Arc::new(trailhead_server::AppState::new(store)));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let allowed = UiAllowlist::new(dir.path().join("ui-allowed"));
        Rig { dir, server, allowed, token, agent_token }
    }

    fn config(&self, lookup: &Arc<MemoryLookup>) -> NetConfig {
        NetConfig {
            config_dir: self.dir.path().join("lookout"),
            trailhead_config_dir: self.dir.path().join("config"),
            trailhead_data_dir: self.dir.path().join("data"),
            keyring: false,
            network: Network::Direct(lookup.clone()),
        }
    }
}

fn app(config: NetConfig, target: Option<String>) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins).add_plugins(ConnectionPlugin::new(config, target));
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
    until(&mut app, "agent token rejected", |a| reason(a) == Some(Reason::TokenRejected)).await;

    send(&mut app, NetCommand::Login(rig.token.clone()));
    until(&mut app, "live", |a| state(a) == Connection::Live).await;
    assert_eq!(reason(&app), None);
    let api = &app.world().resource::<Api>().0;
    assert_eq!((api.token.as_str(), api.user.as_str()), (rig.token.as_str(), "Lane"));
    let me = reqwest::Client::new().get(format!("{}/v1/me", api.base)).bearer_auth(&api.token).send().await.unwrap();
    assert!(me.status().is_success());
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
