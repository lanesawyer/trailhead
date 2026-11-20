//! The net thread and `Connection` state, with no app jobs, against an in-process server and control plane over a
//! local iroh connection (no relay, no discovery service). What Lookout runs over it of its own is in Lookout's
//! `tests/factory_link.rs`.

use std::sync::Arc;
use std::time::Duration;

use bevy::prelude::*;
use iroh::address_lookup::MemoryLookup;
use iroh::protocol::Router;
use iroh::{EndpointAddr, SecretKey};
use trailhead_shell::connection::{Api, Connection, ConnectionPlugin, Dialed, LocalEndpoint};
use trailhead_shell::net::{NetCommand, NetConfig, Reason};
use trailhead_shell::testing::{Server as Rig, allowed_and_live, endpoint, reason, send, state, until};
use trailhead_wire::ui::{UI_ALPN, UiProtocol};

/// A control plane that forwards the API and serves nothing else.
async fn control_plane(key: SecretKey, lookup: &Arc<MemoryLookup>, rig: &Rig) -> (Router, EndpointAddr) {
    let (endpoint, addr) = endpoint(key, lookup).await;
    let router = Router::builder(endpoint).accept(UI_ALPN, UiProtocol::new(rig.server, rig.allowed.clone())).spawn();
    (router, addr)
}

fn app(config: NetConfig, target: Option<String>) -> App {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, bevy::state::app::StatesPlugin))
        .add_plugins(ConnectionPlugin::new(config, target));
    app
}

fn dialed(app: &App) -> Option<String> {
    app.world().resource::<Dialed>().0.clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn connects_signs_in_and_goes_live() {
    let rig = Rig::new().await;
    let lookup = Arc::new(MemoryLookup::new());
    let (_router, addr) = control_plane(SecretKey::generate(), &lookup, &rig).await;
    // The start target is padded, so `Dialed` equals the bare id only once the net thread has parsed and reported it.
    let mut app = app(rig.config(&lookup), Some(format!("  {}\n", addr.id)));
    assert_ne!(dialed(&app), Some(addr.id.to_string()));

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
    assert_eq!(dialed(&app), Some(addr.id.to_string()));
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

/// While Live the probe asks `/v1/health` every time but `/v1/me` only on every 15th, so a token check isn't one
/// more request per probe.
#[tokio::test(flavor = "multi_thread")]
async fn the_live_probe_checks_the_token_only_every_few_probes() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let mut rig = Rig::new().await;
    let health = Arc::new(AtomicUsize::new(0));
    let me = Arc::new(AtomicUsize::new(0));
    let counted = {
        let (health, me) = (health.clone(), me.clone());
        trailhead_server::router(rig.state.clone()).layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let counter = match request.uri().path() {
                    "/v1/health" => Some(health.clone()),
                    "/v1/me" => Some(me.clone()),
                    _ => None,
                };
                async move {
                    if let Some(counter) = counter {
                        counter.fetch_add(1, Ordering::SeqCst);
                    }
                    next.run(request).await
                }
            },
        ))
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    rig.server = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, counted).await.unwrap() });

    let lookup = Arc::new(MemoryLookup::new());
    let (_router, addr) = control_plane(SecretKey::generate(), &lookup, &rig).await;
    let mut app = app(rig.config(&lookup), Some(addr.id.to_string()));
    allowed_and_live(&mut app, &rig).await;

    let (health_from, me_from) = (health.load(Ordering::SeqCst), me.load(Ordering::SeqCst));
    // Probing every 100 ms, 5 s is about 50 probes while Live.
    for _ in 0..100 {
        app.update();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let probes = health.load(Ordering::SeqCst) - health_from;
    let checks = me.load(Ordering::SeqCst) - me_from;
    assert!(probes >= 30, "the probe ran only {probes} times in 5 s");
    assert!(checks >= 1, "the Live probe never checked the token in {probes} probes");
    assert!(checks <= probes / 15 + 1, "{checks} token checks in {probes} probes, expected about one in 15");
    assert_eq!(state(&app), Connection::Live);
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
    // Padded, so `Dialed` only equals the bare id once the net thread has parsed and reported it.
    let mut app = app(rig.config(&lookup), Some(format!("  {}\n", addr.id)));
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
    assert_eq!(dialed(&app), Some(addr.id.to_string()));
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
    assert_eq!(dialed(&app), None);
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
    assert_eq!(dialed(&app), None);
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
