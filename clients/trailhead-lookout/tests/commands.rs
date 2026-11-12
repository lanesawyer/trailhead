//! Commands: the outbox's gating and retry in a headless app, and `execute` against an in-process server, including
//! a retry of a write the server already applied.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use reqwest::Method;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use trailhead_core::{Ctx, NewProject, NewTask, Principal, Store};
use trailhead_lookout::commands::*;
use trailhead_lookout::connection::{Connection, SendNet};
use trailhead_lookout::net::NetCommand;

// ---- the Bevy side ----

fn app() -> App {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, StatesPlugin)).init_state::<Connection>().add_plugins(CommandsPlugin);
    app.world_mut().resource_mut::<Outbox>().backoff = Duration::ZERO;
    app
}

fn set(app: &mut App, state: Connection) {
    app.world_mut().resource_mut::<NextState<Connection>>().set(state);
    app.update();
}

fn submit(app: &mut App, source: Entity) {
    let request = Request::new(Method::PATCH, "/v1/tasks/TRAIL-1", Some(json!({"title": "x"})), Some(3));
    app.world_mut().write_message(Submit { source, request });
}

/// Runs a frame and returns what the net thread was asked to do, and what controls were told.
fn step(app: &mut App) -> (Vec<(CommandId, u32, Request)>, Vec<CommandDone>) {
    app.update();
    let world = app.world_mut();
    let sent = world
        .resource_mut::<Messages<SendNet>>()
        .drain()
        .filter_map(|SendNet(c)| match c {
            NetCommand::Request { id, attempt, request } => Some((id, attempt, request)),
            _ => None,
        })
        .collect();
    let done = world.resource_mut::<Messages<CommandDone>>().drain().collect();
    (sent, done)
}

fn reply(app: &mut App, id: CommandId, attempt: u32, result: Result<Outcome, String>) {
    app.world_mut().write_message(Reply { id, attempt, result });
}

#[test]
fn nothing_is_sent_until_live_and_a_submit_outside_live_is_refused() {
    let mut app = app();
    let control = app.world_mut().spawn_empty().id();
    set(&mut app, Connection::Syncing);
    submit(&mut app, control);
    let (sent, done) = step(&mut app);
    assert!(sent.is_empty());
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].source, control);
    assert!(matches!(done[0].outcome, Outcome::Failed(_)));
    assert!(app.world().resource::<Outbox>().is_empty());
}

#[test]
fn the_answer_goes_back_to_the_control_that_sent_it() {
    let mut app = app();
    let (a, b) = (app.world_mut().spawn_empty().id(), app.world_mut().spawn_empty().id());
    set(&mut app, Connection::Live);
    submit(&mut app, a);
    submit(&mut app, b);
    let (sent, _) = step(&mut app);
    assert_eq!(sent.len(), 2);
    let request = &sent[0].2;
    assert_eq!((request.if_match, request.method.clone()), (Some(3), Method::PATCH));
    assert_ne!(sent[0].2.key, sent[1].2.key);

    reply(&mut app, sent[1].0, 1, Ok(Outcome::Conflict { current_version: Some(4), message: "stale".into() }));
    reply(&mut app, sent[0].0, 1, Ok(Outcome::Ok { version: Some(4) }));
    let (_, done) = step(&mut app);
    let for_a = done.iter().find(|d| d.source == a).unwrap();
    let for_b = done.iter().find(|d| d.source == b).unwrap();
    assert_eq!(for_a.outcome, Outcome::Ok { version: Some(4) });
    assert!(matches!(for_b.outcome, Outcome::Conflict { current_version: Some(4), .. }));
    assert!(app.world().resource::<Outbox>().is_empty());
}

#[test]
fn a_command_in_flight_when_the_link_drops_is_sent_again_with_the_same_key() {
    let mut app = app();
    let control = app.world_mut().spawn_empty().id();
    set(&mut app, Connection::Live);
    submit(&mut app, control);
    let (sent, _) = step(&mut app);
    let (id, key) = (sent[0].0, sent[0].2.key.clone());

    set(&mut app, Connection::Syncing);
    let (resent, done) = step(&mut app);
    assert!(resent.is_empty() && done.is_empty(), "held while not live");
    assert_eq!(app.world().resource::<Outbox>().len(), 1);

    set(&mut app, Connection::Live);
    let (resent, _) = step(&mut app);
    assert_eq!(resent.len(), 1);
    assert_eq!((resent[0].0, resent[0].1, resent[0].2.key.as_str()), (id, 2, key.as_str()));

    // The first attempt's answer, arriving late, settles it; the second's is then ignored.
    reply(&mut app, id, 1, Ok(Outcome::Ok { version: Some(4) }));
    reply(&mut app, id, 2, Ok(Outcome::Ok { version: Some(4) }));
    let (_, done) = step(&mut app);
    assert_eq!(done.len(), 1);
}

#[test]
fn a_stale_no_answer_does_not_resend_a_later_attempt() {
    let mut app = app();
    let control = app.world_mut().spawn_empty().id();
    set(&mut app, Connection::Live);
    submit(&mut app, control);
    let (sent, _) = step(&mut app);
    let id = sent[0].0;
    set(&mut app, Connection::Syncing);
    set(&mut app, Connection::Live);
    let (resent, _) = step(&mut app);
    assert_eq!(resent[0].1, 2);
    reply(&mut app, id, 1, Err("connection reset".into()));
    let (again, done) = step(&mut app);
    assert!(again.is_empty() && done.is_empty());
}

#[test]
fn a_rate_limited_command_is_sent_again_with_its_key_until_attempts_run_out() {
    let mut app = app();
    let control = app.world_mut().spawn_empty().id();
    set(&mut app, Connection::Live);
    submit(&mut app, control);
    let limited = Outcome::RateLimited { retry_after: Some(Duration::ZERO), message: "slow".into() };
    let mut keys = Vec::new();
    let mut finished = None;
    for _ in 0..20 {
        let (sent, done) = step(&mut app);
        if let Some(d) = done.into_iter().next() {
            finished = Some(d);
            break;
        }
        if let Some((id, n, request)) = sent.first() {
            keys.push(request.key.clone());
            reply(&mut app, *id, *n, Ok(limited.clone()));
        }
    }
    assert_eq!(keys.len() as u32, MAX_ATTEMPTS);
    assert!(keys.iter().all(|k| *k == keys[0]));
    let done = finished.expect("handed back, not dropped");
    assert_eq!(done.source, control);
    assert_eq!(done.outcome, limited);
    assert!(app.world().resource::<Outbox>().is_empty());
}

#[test]
fn a_rate_limited_command_waits_for_retry_after() {
    let mut app = app();
    let control = app.world_mut().spawn_empty().id();
    set(&mut app, Connection::Live);
    submit(&mut app, control);
    let (sent, _) = step(&mut app);
    let limited = Outcome::RateLimited { retry_after: Some(Duration::from_secs(600)), message: "slow".into() };
    reply(&mut app, sent[0].0, 1, Ok(limited));
    let (sent, done) = step(&mut app);
    assert!(sent.is_empty() && done.is_empty(), "held until Retry-After");
    assert_eq!(app.world().resource::<Outbox>().len(), 1);
}

#[test]
fn a_command_that_never_gets_an_answer_is_reported_failed() {
    let mut app = app();
    let control = app.world_mut().spawn_empty().id();
    set(&mut app, Connection::Live);
    submit(&mut app, control);
    let mut attempt = 0;
    let mut finished = None;
    for _ in 0..20 {
        let (sent, done) = step(&mut app);
        if let Some(d) = done.into_iter().next() {
            finished = Some(d);
            break;
        }
        if let Some((id, n, _)) = sent.first() {
            attempt = *n;
            reply(&mut app, *id, *n, Err("timed out".into()));
        }
    }
    let done = finished.expect("reported, not dropped");
    assert_eq!(attempt, MAX_ATTEMPTS);
    assert_eq!(done.source, control);
    assert!(matches!(done.outcome, Outcome::Failed(_)));
    assert!(app.world().resource::<Outbox>().is_empty());
}

#[test]
fn resends_after_reconnects_and_not_connected_refusals_are_not_failed_attempts() {
    let mut app = app();
    let control = app.world_mut().spawn_empty().id();
    set(&mut app, Connection::Live);
    submit(&mut app, control);
    let mut sent = step(&mut app).0;
    // The link flaps more often than MAX_ATTEMPTS while the request is out.
    for _ in 0..MAX_ATTEMPTS + 2 {
        set(&mut app, Connection::Syncing);
        set(&mut app, Connection::Live);
        sent = step(&mut app).0;
        assert_eq!(sent.len(), 1);
    }
    // The net thread isn't live yet either.
    for _ in 0..MAX_ATTEMPTS + 2 {
        let (id, n, _) = sent[0].clone();
        reply(&mut app, id, n, Err(NOT_CONNECTED.into()));
        let (again, done) = step(&mut app);
        assert!(done.is_empty(), "not a failure");
        sent = again;
        assert_eq!(sent.len(), 1);
    }
    // Real failures still count from zero.
    let mut finished = None;
    for _ in 0..20 {
        let (id, n, _) = sent[0].clone();
        reply(&mut app, id, n, Err("timed out".into()));
        let (again, done) = step(&mut app);
        if let Some(d) = done.into_iter().next() {
            finished = Some(d);
            break;
        }
        sent = again;
    }
    assert!(matches!(finished.expect("reported").outcome, Outcome::Failed(_)));
}

// ---- the request ----

struct Rig {
    _dir: tempfile::TempDir,
    server: SocketAddr,
    token: String,
}

impl Rig {
    async fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("trailhead.db");
        let store = Store::open_local(db.to_str().unwrap()).await.unwrap();
        let (ws, user) = store.bootstrap_local("Lane").await.unwrap();
        let ctx = Ctx::user(ws.id, user.id);
        let project = NewProject { key: "TRAIL".into(), name: "Trailhead".into(), ..Default::default() };
        store.create_project(&ctx, project).await.unwrap();
        let new = NewTask { project: "TRAIL".into(), title: "one".into(), ..Default::default() };
        store.create_task(&ctx, new).await.unwrap();
        let (_, token) = store.create_token(&ctx, Principal::User(user.id), "ui").await.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = listener.local_addr().unwrap();
        let app = trailhead_server::router(Arc::new(trailhead_server::AppState::new(store)));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Rig { _dir: dir, server, token }
    }

    async fn run(&self, token: &str, request: &Request) -> Result<Outcome, String> {
        let http = reqwest::Client::new();
        execute(&http, &format!("http://{}", self.server), token, request).await
    }

    async fn title(&self) -> Value {
        let http = reqwest::Client::new();
        let url = format!("http://{}/v1/tasks/TRAIL-1", self.server);
        http.get(url).bearer_auth(&self.token).send().await.unwrap().json::<Value>().await.unwrap()["task"].clone()
    }
}

fn retitle(title: &str, if_match: Option<i64>) -> Request {
    Request::new(Method::PATCH, "/v1/tasks/TRAIL-1", Some(json!({ "title": title })), if_match)
}

#[tokio::test]
async fn a_write_answers_ok_with_the_new_version() {
    let rig = Rig::new().await;
    let before = rig.title().await["version"].as_i64().unwrap();
    let out = rig.run(&rig.token, &retitle("two", Some(before))).await.unwrap();
    assert_eq!(out, Outcome::Ok { version: Some(before + 1) });
    assert_eq!(rig.title().await["title"], "two");
}

#[tokio::test]
async fn a_stale_version_is_a_conflict_with_the_current_one() {
    let rig = Rig::new().await;
    let before = rig.title().await["version"].as_i64().unwrap();
    rig.run(&rig.token, &retitle("two", Some(before))).await.unwrap();
    let out = rig.run(&rig.token, &retitle("three", Some(before))).await.unwrap();
    assert!(matches!(out, Outcome::Conflict { current_version: Some(v), .. } if v == before + 1), "{out:?}");
    assert_eq!(rig.title().await["title"], "two");
}

#[tokio::test]
async fn a_retry_the_server_already_applied_gets_the_same_answer_and_applies_nothing_twice() {
    let rig = Rig::new().await;
    let before = rig.title().await["version"].as_i64().unwrap();
    let request = retitle("two", Some(before));
    // The first answer was lost on the way back; the retry carries the same key.
    let first = rig.run(&rig.token, &request).await.unwrap();
    let retry = rig.run(&rig.token, &request).await.unwrap();
    assert_eq!(first, Outcome::Ok { version: Some(before + 1) });
    assert_eq!(retry, first);
    assert_eq!(rig.title().await["version"].as_i64(), Some(before + 1));
    // Without the key, the same stale If-Match would have been a conflict.
    let mut fresh = request.clone();
    fresh.key = "another".into();
    assert!(matches!(rig.run(&rig.token, &fresh).await.unwrap(), Outcome::Conflict { .. }));
}

#[tokio::test]
async fn a_connection_that_cannot_be_made_is_worth_retrying() {
    let rig = Rig::new().await;
    let http = reqwest::Client::new();
    let gone = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();
    let out = execute(&http, &format!("http://{gone}"), &rig.token, &retitle("x", None)).await;
    assert!(out.is_err());
}

#[tokio::test]
async fn a_bad_token_is_a_failure_not_a_retry() {
    let rig = Rig::new().await;
    let out = rig.run("not-a-token", &retitle("x", None)).await.unwrap();
    assert!(matches!(out, Outcome::Failed(_)), "{out:?}");
}
