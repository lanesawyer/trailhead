//! `trailhead/ui/0`: who gets in, and that what is forwarded to the server (the event stream included) works.

use std::sync::Arc;
use std::time::Duration;

use iroh::address_lookup::MemoryLookup;
use iroh::endpoint::{ReadError, ReadExactError, VarInt, presets};
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey};
use tokio::sync::broadcast;
use trailhead_core::{Ctx, NewProject, NewTask, Principal, Store};
use trailhead_factory::logs::{LogLevel, LogLine};
use trailhead_factory::{ControlState, FactoryEvent, FactorySnapshot, Sequenced, TaskKey};
use trailhead_wire::api::ApiTunnel;
use trailhead_wire::ui::{
    LogFeed, LogFrame, LogStream, RepoFeed, SERVER_UNREACHABLE, StatusFeed, StatusFrame, StatusStream, UI_ALPN,
    UiAllowlist, UiProtocol, dial_ui, open_ui_tunnel,
};
use trailhead_wire::{API_ALPN, Peers, read_frame, write_frame};

/// One repo whose events the test sends.
struct TestFeed(broadcast::Sender<Sequenced>);

impl StatusFeed for TestFeed {
    fn subscribe(&self) -> Vec<RepoFeed> {
        let snapshot = FactorySnapshot {
            control: ControlState::Up,
            last_error: None,
            sandboxes: vec![],
            runs: vec![],
            holding: None,
            targets: Vec::new(),
            human_grades: None,
            queue: Vec::new(),
            usage: Vec::new(),
            seq: 7,
        };
        vec![RepoFeed { repo: "o/r".into(), snapshot, events: self.0.subscribe() }]
    }
}

/// A log with two lines kept, and the lines the test sends after.
struct TestLogs(broadcast::Sender<LogLine>);

fn log_line(message: &str) -> LogLine {
    LogLine { ts_ms: 1, level: LogLevel::Info, target: "t".into(), repo: None, task: None, message: message.into() }
}

impl LogFeed for TestLogs {
    fn subscribe(&self) -> (Vec<LogLine>, broadcast::Receiver<LogLine>) {
        (vec![log_line("one"), log_line("two")], self.0.subscribe())
    }
}

async fn endpoint(key: SecretKey, lookup: Arc<MemoryLookup>, alpns: Vec<Vec<u8>>) -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .secret_key(key)
        .relay_mode(RelayMode::Disabled)
        .address_lookup(lookup)
        .alpns(alpns)
        .bind()
        .await
        .unwrap()
}

struct Rig {
    _dir: tempfile::TempDir,
    store: Store,
    human: Ctx,
    token: String,
    control: EndpointAddr,
    allowed: UiAllowlist,
    peers: Peers,
    events: broadcast::Sender<Sequenced>,
    logs: broadcast::Sender<LogLine>,
    _router: Router,
}

impl Rig {
    async fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("trailhead.db");
        let db = db.to_str().unwrap();
        let server_store = Store::open_local(db).await.unwrap();
        let (ws, user) = server_store.bootstrap_local("Lane").await.unwrap();
        let human = Ctx::user(ws.id, user.id);
        let project = NewProject { key: "TRAIL".into(), name: "Trailhead".into(), ..Default::default() };
        server_store.create_project(&human, project).await.unwrap();
        let (_, token) = server_store.create_token(&human, Principal::User(user.id), "ui").await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = listener.local_addr().unwrap();
        let app = trailhead_server::router(Arc::new(trailhead_server::AppState::new(server_store)));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let store = Store::open_local(db).await.unwrap();

        let allowed = UiAllowlist::new(dir.path().join("ui-allowed"));
        let peers = Peers::default();
        let events = broadcast::channel(16).0;
        let logs = broadcast::channel(16).0;
        let control_ep = endpoint(SecretKey::generate(), Arc::new(MemoryLookup::new()), vec![]).await;
        let control = control_ep.addr();
        let router = Router::builder(control_ep)
            .accept(
                UI_ALPN,
                UiProtocol::new(server, allowed.clone())
                    .with_status(Arc::new(TestFeed(events.clone())))
                    .with_logs(Arc::new(TestLogs(logs.clone()))),
            )
            .accept(API_ALPN, ApiTunnel::new(server, peers.clone()))
            .spawn();
        Rig { _dir: dir, store, human, token, control, allowed, peers, events, logs, _router: router }
    }

    /// An endpoint that knows how to find the control plane.
    async fn client(&self, key: SecretKey) -> Endpoint {
        let lookup = Arc::new(MemoryLookup::new());
        lookup.add_endpoint_info(self.control.clone());
        endpoint(key, lookup, vec![]).await
    }
}

#[tokio::test]
async fn an_allowed_endpoint_reaches_the_api_with_its_token() {
    let r = Rig::new().await;
    let key = SecretKey::generate();
    r.allowed.add(key.public()).unwrap();
    let api = open_ui_tunnel(r.client(key).await, r.control.id).await.unwrap();
    let http = reqwest::Client::new();

    let me = http.get(format!("http://{api}/v1/me")).bearer_auth(&r.token).send().await.unwrap();
    assert!(me.status().is_success(), "{}", me.status());
    // The allowlist decides who connects; the server still decides what the token may do.
    let anon = http.get(format!("http://{api}/v1/me")).send().await.unwrap();
    assert_eq!(anon.status(), 401);
    let bad = http.get(format!("http://{api}/v1/me")).bearer_auth("nope").send().await.unwrap();
    assert_eq!(bad.status(), 401);
}

#[tokio::test]
async fn an_endpoint_not_on_the_list_is_refused() {
    let r = Rig::new().await;
    r.allowed.add(SecretKey::generate().public()).unwrap();
    let api = open_ui_tunnel(r.client(SecretKey::generate()).await, r.control.id).await.unwrap();
    let sent = reqwest::Client::new().get(format!("http://{api}/v1/me")).bearer_auth(&r.token).send();
    let result = tokio::time::timeout(Duration::from_secs(10), sent).await.unwrap();
    assert!(result.is_err(), "no HTTP response for a stranger: {result:?}");
}

#[tokio::test]
async fn allowing_and_removing_take_effect_without_a_restart() {
    let r = Rig::new().await;
    let key = SecretKey::generate();
    let client = r.client(key.clone()).await;
    let denied = async {
        let api = open_ui_tunnel(client.clone(), r.control.id).await.unwrap();
        let get = reqwest::Client::new().get(format!("http://{api}/v1/me")).bearer_auth(&r.token).send();
        tokio::time::timeout(Duration::from_secs(10), get).await.unwrap().is_err()
    };
    assert!(denied.await);
    r.allowed.add(key.public()).unwrap();
    let api = open_ui_tunnel(client.clone(), r.control.id).await.unwrap();
    let get = reqwest::Client::new().get(format!("http://{api}/v1/me")).bearer_auth(&r.token).send();
    assert!(tokio::time::timeout(Duration::from_secs(10), get).await.unwrap().unwrap().status().is_success());
}

#[tokio::test]
async fn a_daemons_endpoint_cannot_use_it() {
    let r = Rig::new().await;
    // A daemon the control plane started is let through the API tunnel, but that doesn't make it a UI.
    let daemon_key = SecretKey::generate();
    r.peers.lock().unwrap().insert(daemon_key.public());
    let daemon = r.client(daemon_key).await;
    let api = open_ui_tunnel(daemon, r.control.id).await.unwrap();
    let sent = reqwest::Client::new().get(format!("http://{api}/v1/me")).bearer_auth(&r.token).send();
    let result = tokio::time::timeout(Duration::from_secs(10), sent).await.unwrap();
    assert!(result.is_err(), "a daemon got an answer: {result:?}");
}

#[tokio::test]
async fn the_event_stream_survives_being_forwarded() {
    let r = Rig::new().await;
    let key = SecretKey::generate();
    r.allowed.add(key.public()).unwrap();
    let api = open_ui_tunnel(r.client(key).await, r.control.id).await.unwrap();
    let mut stream = reqwest::Client::new()
        .get(format!("http://{api}/v1/events/stream"))
        .bearer_auth(&r.token)
        .send()
        .await
        .unwrap();
    assert!(stream.status().is_success(), "{}", stream.status());
    assert!(stream.headers()["content-type"].to_str().unwrap().starts_with("text/event-stream"));

    // Events written after the stream opened arrive on it, one after another, without it ending.
    for title in ["first through the tunnel", "second through the tunnel"] {
        let new = NewTask { project: "TRAIL".into(), title: title.into(), ..Default::default() };
        r.store.create_task(&r.human, new).await.unwrap();
        let mut seen = String::new();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !seen.contains(title) {
                let chunk = stream.chunk().await.unwrap().expect("the stream ended");
                seen.push_str(&String::from_utf8_lossy(&chunk));
            }
        })
        .await
        .unwrap_or_else(|_| panic!("no event for {title:?}; got {seen:?}"));
    }
}

async fn next(stream: &mut StatusStream) -> Option<StatusFrame> {
    tokio::time::timeout(Duration::from_secs(10), stream.next()).await.unwrap().unwrap()
}

#[tokio::test]
async fn status_is_a_snapshot_then_the_events_after_it() {
    let r = Rig::new().await;
    let key = SecretKey::generate();
    r.allowed.add(key.public()).unwrap();
    let client = r.client(key).await;
    let mut stream = StatusStream::open(dial_ui(&client, r.control.id).await.unwrap()).await.unwrap();

    let Some(StatusFrame::Snapshot { repos, .. }) = next(&mut stream).await else {
        panic!("the first frame is the snapshot");
    };
    assert_eq!((repos[0].repo.as_str(), repos[0].snapshot.seq), ("o/r", 7));

    let event = Sequenced { seq: 8, event: FactoryEvent::RunEnded { task: TaskKey("TRAIL-1".into()) } };
    r.events.send(event.clone()).unwrap();
    assert_eq!(next(&mut stream).await, Some(StatusFrame::Event { repo: "o/r".into(), event }));
}

#[tokio::test]
async fn status_ends_when_a_subscriber_lags_so_the_ui_resyncs() {
    let r = Rig::new().await;
    let key = SecretKey::generate();
    r.allowed.add(key.public()).unwrap();
    let client = r.client(key).await;
    let mut stream = StatusStream::open(dial_ui(&client, r.control.id).await.unwrap()).await.unwrap();
    assert!(matches!(next(&mut stream).await, Some(StatusFrame::Snapshot { .. })));

    // The channel holds 16 and the runtime is single-threaded, so the forwarder cannot run during
    // this loop: its first receive finds it has fallen behind.
    for seq in 8..8 + 17 {
        let event = Sequenced { seq, event: FactoryEvent::RunEnded { task: TaskKey("TRAIL-1".into()) } };
        r.events.send(event).unwrap();
    }
    loop {
        match next(&mut stream).await {
            Some(StatusFrame::Event { .. }) => {}
            None => break,
            other => panic!("expected the stream to end after a lag, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn logs_are_the_kept_lines_then_each_new_one() {
    let r = Rig::new().await;
    let key = SecretKey::generate();
    r.allowed.add(key.public()).unwrap();
    let client = r.client(key).await;
    let mut stream = LogStream::open(dial_ui(&client, r.control.id).await.unwrap()).await.unwrap();
    let mut next = async || tokio::time::timeout(Duration::from_secs(10), stream.next()).await.unwrap().unwrap();

    assert_eq!(next().await, Some(LogFrame::Backlog { lines: vec![log_line("one"), log_line("two")] }));
    // The receiver is made when the stream opens, so a send after the backlog is not missed.
    r.logs.send(log_line("three")).unwrap();
    assert_eq!(next().await, Some(LogFrame::Line(log_line("three"))));
}

#[tokio::test]
async fn a_logs_reader_that_falls_behind_is_told_how_many_lines_it_missed() {
    let r = Rig::new().await;
    let key = SecretKey::generate();
    r.allowed.add(key.public()).unwrap();
    let client = r.client(key).await;
    let mut stream = LogStream::open(dial_ui(&client, r.control.id).await.unwrap()).await.unwrap();
    let mut next = async || tokio::time::timeout(Duration::from_secs(10), stream.next()).await.unwrap().unwrap();
    assert!(matches!(next().await, Some(LogFrame::Backlog { .. })));

    // The channel holds 16 and the runtime is single-threaded, so the server cannot receive during this
    // loop: 17 sends push the oldest out.
    for n in 0..17 {
        r.logs.send(log_line(&format!("burst {n}"))).unwrap();
    }
    assert_eq!(next().await, Some(LogFrame::Skipped { count: 1 }));
    assert_eq!(next().await, Some(LogFrame::Line(log_line("burst 1"))));
}

#[tokio::test]
async fn status_is_refused_to_an_endpoint_that_is_not_allowed() {
    let r = Rig::new().await;
    r.allowed.add(SecretKey::generate().public()).unwrap();
    let client = r.client(SecretKey::generate()).await;
    let opened = async {
        let mut stream = StatusStream::open(dial_ui(&client, r.control.id).await?).await?;
        stream.next().await
    };
    let result = tokio::time::timeout(Duration::from_secs(10), opened).await.unwrap();
    assert!(!matches!(result, Ok(Some(_))), "a stranger got status: {result:?}");
}

#[tokio::test]
async fn an_unknown_request_is_answered_with_what_the_factory_knows() {
    let r = Rig::new().await;
    let key = SecretKey::generate();
    r.allowed.add(key.public()).unwrap();
    let client = r.client(key).await;
    let conn = dial_ui(&client, r.control.id).await.unwrap();
    // Twice: a retrying UI gets the answer every time, though the factory logs it once.
    for _ in 0..2 {
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        write_frame(&mut send, &serde_json::json!({"op": "from_the_future", "x": 1})).await.unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(10), read_frame::<StatusFrame>(&mut recv)).await;
        let Some(StatusFrame::Unsupported(u)) = reply.unwrap().unwrap() else {
            panic!("an unknown request should be answered with the error");
        };
        assert_eq!(u.got["op"], "from_the_future");
        assert_eq!(u.known, ["api", "status", "logs"]);
        assert_eq!(u.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(u.build, "unknown");
        // And then the stream ends.
        assert!(read_frame::<StatusFrame>(&mut recv).await.unwrap().is_none());
    }
}

#[tokio::test]
async fn an_api_request_resets_with_its_own_code_when_the_server_is_down() {
    let dir = tempfile::tempdir().unwrap();
    // An address nothing listens on: bind, note it, drop.
    let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();
    let allowed = UiAllowlist::new(dir.path().join("ui-allowed"));
    let control_ep = endpoint(SecretKey::generate(), Arc::new(MemoryLookup::new()), vec![]).await;
    let control = control_ep.addr();
    let _router = Router::builder(control_ep).accept(UI_ALPN, UiProtocol::new(server, allowed.clone())).spawn();
    let key = SecretKey::generate();
    allowed.add(key.public()).unwrap();
    let lookup = Arc::new(MemoryLookup::new());
    lookup.add_endpoint_info(control.clone());
    let client = endpoint(key, lookup, vec![]).await;
    let conn = dial_ui(&client, control.id).await.unwrap();
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    write_frame(&mut send, &serde_json::json!({"op": "api"})).await.unwrap();
    send.finish().unwrap();
    let mut buf = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(10), recv.read_exact(&mut buf)).await.unwrap();
    match read {
        Err(ReadExactError::ReadError(ReadError::Reset(code))) => {
            assert_eq!(code, VarInt::from(SERVER_UNREACHABLE));
        }
        other => panic!("expected a reset with SERVER_UNREACHABLE, got {other:?}"),
    }
}

#[tokio::test]
async fn disallowing_drops_a_connection_that_is_already_open() {
    let r = Rig::new().await;
    let key = SecretKey::generate();
    r.allowed.add(key.public()).unwrap();
    let api = open_ui_tunnel(r.client(key.clone()).await, r.control.id).await.unwrap();
    let mut stream = reqwest::Client::new()
        .get(format!("http://{api}/v1/events/stream"))
        .bearer_auth(&r.token)
        .send()
        .await
        .unwrap();
    assert!(stream.status().is_success(), "{}", stream.status());

    r.allowed.remove(&key.public()).unwrap();
    // The open stream is cut, and a new request on the same tunnel gets nothing.
    let ended =
        tokio::time::timeout(Duration::from_secs(10), async { while let Ok(Some(_)) = stream.chunk().await {} }).await;
    assert!(ended.is_ok(), "the event stream outlived the disallow");
    let get = reqwest::Client::new().get(format!("http://{api}/v1/me")).bearer_auth(&r.token).send();
    let result = tokio::time::timeout(Duration::from_secs(10), get).await.unwrap();
    assert!(result.is_err(), "a disallowed endpoint still got an answer: {result:?}");
}
