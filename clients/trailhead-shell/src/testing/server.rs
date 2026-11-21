//! For tests of the connection and of an app's work over it: an in-process server behind a local iroh endpoint (no
//! relay, no discovery service). Behind the `test-server` feature, since it brings the task engine and the server.

use std::sync::Arc;
use std::time::Duration;

use bevy::prelude::*;
use iroh::address_lookup::MemoryLookup;
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointAddr, SecretKey};
use trailhead_core::{Ctx, NewProject, Principal, Store};
use trailhead_wire::ui::UiAllowlist;

use super::{send, state, until};
use crate::connection::{Connection, LocalEndpoint};
use crate::net::{NetCommand, NetConfig, Network};

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
