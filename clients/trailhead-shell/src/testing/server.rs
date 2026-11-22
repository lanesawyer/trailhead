//! For tests of the connection and of an app's work over it: an in-process server behind a local iroh endpoint (no
//! relay, no discovery service). Behind the `test-server` feature, since it brings the task engine and the server.
//!
//! An app's own test goes live with [`live`], which needs nothing of the network named in the test: declare the app's
//! collections on the server ([`Server::declare`]), go live, write as someone else would ([`Server::client`]), and
//! wait for the record mirror with [`records_caught_up`].

use std::sync::Arc;
use std::time::Duration;

use bevy::prelude::*;
use iroh::address_lookup::MemoryLookup;
use iroh::endpoint::presets;
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, SecretKey};
use trailhead_core::{Ctx, NewProject, Principal, Store};
use trailhead_wire::ui::{UI_ALPN, UiAllowlist, UiProtocol};

use super::{send, state, until};
use crate::connection::{Connection, LocalEndpoint};
use crate::net::{NetCommand, NetConfig, Network};
use crate::records::RecordMirror;
use crate::{AppId, ShellApp, ShellPlugin};

/// The server, and what a test needs to sign in to it and to allow the app's endpoint.
pub struct Server {
    pub dir: tempfile::TempDir,
    pub server: std::net::SocketAddr,
    pub allowed: UiAllowlist,
    pub token: String,
    pub agent_token: String,
    /// The server's state and the user's context, to revoke the token while the app is signed in.
    pub state: Arc<trailhead_server::AppState>,
    pub human: Ctx,
    pub token_id: uuid::Uuid,
}

impl Server {
    pub async fn new() -> Server {
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
        Server { dir, server, allowed, token, agent_token, state, human, token_id: token_row.id }
    }

    /// The API as the server's person, for a test to write what someone or something else would.
    pub fn client(&self) -> trailhead_client::http::Client {
        trailhead_client::http::Client::new(format!("http://{}", self.server), self.token.clone())
    }

    /// Registers an app's `collections.toml` with `PUT /v1/apps/{app}/declaration`, as the builder would from an apps
    /// repo.
    pub async fn declare(&self, app: AppId, collections_toml: &str) {
        let declared = self.client().declare_app(app.0, collections_toml).await;
        declared.unwrap_or_else(|e| panic!("declaring `{}`: {}", app.0, e.chain()));
    }

    /// Registers what `app` keeps, from the `collections.toml` its [`ShellApp::collections`] was given.
    pub async fn declare_app(&self, app: &impl ShellApp) {
        let toml = app.collections().toml().expect("the app's collections are `Collections::declared`");
        self.declare(app.manifest().id, toml).await;
    }

    pub fn config(&self, lookup: &Arc<MemoryLookup>) -> NetConfig {
        NetConfig {
            config_dir: self.dir.path().join("shell"),
            trailhead_config_dir: self.dir.path().join("config"),
            trailhead_data_dir: self.dir.path().join("data"),
            keyring: false,
            network: Network::Direct(lookup.clone()),
            status_retry: Duration::from_millis(100),
        }
    }
}

/// A control plane's endpoint with `key`, found through `lookup`.
pub async fn endpoint(key: SecretKey, lookup: &Arc<MemoryLookup>) -> (Endpoint, EndpointAddr) {
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(key)
        .relay_mode(iroh::RelayMode::Disabled)
        .address_lookup(lookup.clone())
        .bind()
        .await
        .unwrap();
    let addr = endpoint.addr();
    lookup.add_endpoint_info(addr.clone());
    (endpoint, addr)
}

/// Lets the app's endpoint in, signs in with the server's user token, and runs the app until it is live.
pub async fn allowed_and_live(app: &mut App, server: &Server) {
    until(app, "endpoint id", |a| a.world().resource::<LocalEndpoint>().0.is_some()).await;
    server.allowed.add(app.world().resource::<LocalEndpoint>().0.clone().unwrap().parse().unwrap()).unwrap();
    send(app, NetCommand::Login(server.token.clone()));
    until(app, "live", |a| state(a) == Connection::Live).await;
}

/// A connected shell, live on a [`Server`] through a control plane of its own. Holds the control plane, which goes
/// with it.
pub struct LiveShell {
    pub app: App,
    _control_plane: Router,
}

impl std::ops::Deref for LiveShell {
    type Target = App;
    fn deref(&self) -> &App {
        &self.app
    }
}

impl std::ops::DerefMut for LiveShell {
    fn deref_mut(&mut self) -> &mut App {
        &mut self.app
    }
}

/// `shell`, headless and connected to `server`: allowed in, signed in with its user token, and run until live. The
/// records of what its apps keep load from then on; [`records_caught_up`] waits for them.
pub async fn live(server: &Server, shell: ShellPlugin) -> LiveShell {
    let lookup = Arc::new(MemoryLookup::new());
    let (endpoint, addr) = endpoint(SecretKey::generate(), &lookup).await;
    let protocol = UiProtocol::new(server.server, server.allowed.clone());
    let control_plane = Router::builder(endpoint).accept(UI_ALPN, protocol).spawn();
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, bevy::state::app::StatesPlugin))
        .add_plugins(shell.connecting(server.config(&lookup), Some(addr.id.to_string())));
    allowed_and_live(&mut app, server).await;
    LiveShell { app, _control_plane: control_plane }
}

/// Runs the app until the record mirror holds everything `id` keeps as the server has it now: each kept collection
/// loaded, and the app's newest event applied.
pub async fn records_caught_up(app: &mut App, server: &Server, id: AppId) {
    let newest = server.state.store.latest_app_event_id(&server.human, id.0).await.unwrap_or(0);
    until(app, &format!("the records of `{}` caught up to event {newest}", id.0), |a| {
        let mirror = a.world().resource::<RecordMirror>();
        mirror.kept(id).iter().all(|c| mirror.loaded(id, c)) && mirror.last_event(id) >= newest
    })
    .await;
}
