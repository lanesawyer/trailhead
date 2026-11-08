#![cfg(feature = "factory")]

mod common;

use std::sync::Arc;

use trailhead_client::creds::Cred;
use trailhead_client::paths::Paths;
use trailhead_core::Principal;
use trailhead_server::{AppState, open_local, router, with_factory_routes};

async fn get(base: &str, token: &str) -> u16 {
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    client.get(format!("{base}/v1/factory/status")).bearer_auth(token).send().await.unwrap().status().as_u16()
}

#[tokio::test]
async fn only_the_users_token_reads_the_factory_status() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = common::local_config(dir.path());
    let (store, ctx) = open_local(&cfg).await.unwrap();
    let human = std::fs::read_to_string(cfg.creds.file(Cred::User)).unwrap().trim().to_string();
    let agent = store.create_agent(&ctx, "factory").await.unwrap();
    let (_, agent) = store.create_token(&ctx, Principal::Agent(agent.id), "factory").await.unwrap();

    let store = Arc::new(store);
    let paths = Paths { data_dir: dir.path().join("data"), config_dir: dir.path().join("config") };
    let started = trailhead_factory_server::start(paths, store.clone(), "127.0.0.1:1".parse().unwrap()).unwrap();
    let state = Arc::new(AppState::new(store));
    let app = with_factory_routes(router(state.clone()), state, trailhead_factory_server::api(&started.feed));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    assert_eq!(get(&base, &human).await, 200);
    assert_eq!(get(&base, &agent).await, 403);
    assert_eq!(get(&base, "nonsense").await, 401);
}
