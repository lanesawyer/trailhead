//! `SpritesCompute` against an in-process stand-in for the Sprites API, shaped after the OpenAPI spec and the
//! responses seen in the M0 spikes.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde_json::{Value, json};
use trailhead_compute_sprites::SpritesCompute;
use trailhead_factory::{CheckpointId, Compute, Egress, Error, Labels, Program, SandboxFile, SandboxId, SandboxSpec};

const TOKEN: &str = "test-token";
const PAGE: usize = 2;

#[derive(Default)]
struct Sprite {
    labels: Vec<String>,
    policy: Value,
    checkpoints: Vec<String>,
    restores: Vec<String>,
    /// Restores whose progress stream is cut off before it completes, as when the container restarts mid-stream.
    cut_restores: usize,
    files: BTreeMap<String, (Vec<u8>, String)>,
    services: BTreeMap<String, Value>,
    started: Vec<String>,
}

#[derive(Default)]
struct Api {
    sprites: BTreeMap<String, Sprite>,
    creates: usize,
}

type Shared = Arc<Mutex<Api>>;

fn authed(headers: &HeaderMap) -> bool {
    headers.get("authorization").and_then(|h| h.to_str().ok()) == Some(&format!("Bearer {TOKEN}"))
}

fn sprite_json(name: &str, s: &Sprite) -> Value {
    json!({ "id": format!("sprite-{name}"), "name": name, "status": "cold", "labels": s.labels })
}

fn ndjson(events: &[Value]) -> Response {
    let body: String = events.iter().map(|e| format!("{e}\n")).collect();
    ([("content-type", "application/x-ndjson")], body).into_response()
}

async fn list(State(api): State<Shared>, headers: HeaderMap, Query(q): Query<BTreeMap<String, String>>) -> Response {
    if !authed(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let api = api.lock().unwrap();
    let prefix = q.get("prefix").cloned().unwrap_or_default();
    let names: Vec<&String> = api.sprites.keys().filter(|n| n.starts_with(&prefix)).collect();
    let start: usize = q.get("continuation_token").map(|t| t.parse().unwrap()).unwrap_or(0);
    let page: Vec<Value> = names.iter().skip(start).take(PAGE).map(|n| sprite_json(n, &api.sprites[*n])).collect();
    let more = start + PAGE < names.len();
    Json(json!({
        "sprites": page, "has_more": more, "running": 0, "warm": 0, "cold": page.len(), "name": "org",
        "next_continuation_token": if more { (start + PAGE).to_string() } else { String::new() },
    }))
    .into_response()
}

async fn create(State(api): State<Shared>, Json(body): Json<Value>) -> Response {
    let mut api = api.lock().unwrap();
    let name = body["name"].as_str().unwrap().to_string();
    if api.sprites.contains_key(&name) {
        return (StatusCode::BAD_REQUEST, "sprite already exists").into_response();
    }
    let labels = body["labels"].as_array().map(|a| a.iter().map(|l| l.as_str().unwrap().to_string()).collect());
    let sprite = Sprite { labels: labels.unwrap_or_default(), ..Default::default() };
    let out = sprite_json(&name, &sprite);
    api.creates += 1;
    api.sprites.insert(name, sprite);
    (StatusCode::CREATED, Json(out)).into_response()
}

async fn show(State(api): State<Shared>, Path(name): Path<String>) -> Response {
    match api.lock().unwrap().sprites.get(&name) {
        Some(s) => Json(sprite_json(&name, s)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn update(State(api): State<Shared>, Path(name): Path<String>, Json(body): Json<Value>) -> Response {
    let mut api = api.lock().unwrap();
    let Some(s) = api.sprites.get_mut(&name) else { return StatusCode::NOT_FOUND.into_response() };
    if body["clear_labels"] == json!(true) {
        s.labels.clear();
    } else if let Some(labels) = body["labels"].as_array().filter(|a| !a.is_empty()) {
        s.labels = labels.iter().map(|l| l.as_str().unwrap().to_string()).collect();
    }
    Json(sprite_json(&name, s)).into_response()
}

async fn destroy(State(api): State<Shared>, Path(name): Path<String>) -> StatusCode {
    match api.lock().unwrap().sprites.remove(&name) {
        Some(_) => StatusCode::NO_CONTENT,
        None => StatusCode::NOT_FOUND,
    }
}

async fn policy(State(api): State<Shared>, Path(name): Path<String>, Json(body): Json<Value>) -> Response {
    let mut api = api.lock().unwrap();
    let Some(s) = api.sprites.get_mut(&name) else { return StatusCode::NOT_FOUND.into_response() };
    s.policy = body;
    Json(json!({ "status": "updated" })).into_response()
}

async fn checkpoint(State(api): State<Shared>, Path(name): Path<String>, Json(body): Json<Value>) -> Response {
    let mut api = api.lock().unwrap();
    let Some(s) = api.sprites.get_mut(&name) else { return StatusCode::NOT_FOUND.into_response() };
    s.checkpoints.push(body["comment"].as_str().unwrap_or_default().to_string());
    // v1 is the initial checkpoint every new Sprite has.
    let id = format!("v{}", s.checkpoints.len() + 1);
    ndjson(&[
        json!({ "type": "info", "data": "Creating checkpoint...", "time": "2026-10-27T01:47:55Z" }),
        json!({ "type": "info", "data": format!("  ID: {id}"), "time": "2026-10-27T01:47:55Z" }),
        json!({ "type": "complete", "data": format!("Checkpoint {id} created successfully"), "time": "2026-10-27T01:47:55Z" }),
    ])
}

async fn restore(State(api): State<Shared>, Path((name, cp)): Path<(String, String)>) -> Response {
    let mut api = api.lock().unwrap();
    let Some(s) = api.sprites.get_mut(&name) else { return StatusCode::NOT_FOUND.into_response() };
    let known = cp
        .strip_prefix('v')
        .and_then(|n| n.parse::<usize>().ok())
        .is_some_and(|n| n >= 1 && n <= s.checkpoints.len() + 1);
    let start =
        json!({ "type": "info", "data": format!("Restoring from checkpoint {cp}..."), "time": "2026-10-27T01:48:02Z" });
    if !known {
        let err =
            format!("Failed to restore checkpoint: failed to find checkpoint \"{cp}\": SBD checkpoint not found: {cp}");
        return ndjson(&[start, json!({ "type": "error", "error": err, "time": "2026-10-27T01:48:02Z" })]);
    }
    s.restores.push(cp.clone());
    if s.cut_restores > 0 {
        s.cut_restores -= 1;
        let chunks: Vec<std::result::Result<String, std::io::Error>> =
            vec![Ok(format!("{start}\n")), Err(std::io::Error::other("container restarted"))];
        let body = axum::body::Body::from_stream(futures_util::stream::iter(chunks));
        return ([("content-type", "application/x-ndjson")], body).into_response();
    }
    ndjson(&[
        start,
        json!({ "type": "complete", "data": format!("Restore from {cp} complete"), "time": "2026-10-27T01:48:04Z" }),
    ])
}

async fn write_file(
    State(api): State<Shared>,
    Path(name): Path<String>,
    Query(q): Query<BTreeMap<String, String>>,
    body: Bytes,
) -> Response {
    let mut api = api.lock().unwrap();
    let Some(s) = api.sprites.get_mut(&name) else { return StatusCode::NOT_FOUND.into_response() };
    s.files.insert(q["path"].clone(), (body.to_vec(), q["mode"].clone()));
    Json(json!({ "path": q["path"], "size": body.len() })).into_response()
}

async fn rename_file(State(api): State<Shared>, Path(name): Path<String>, Json(body): Json<Value>) -> Response {
    let mut api = api.lock().unwrap();
    let Some(s) = api.sprites.get_mut(&name) else { return StatusCode::NOT_FOUND.into_response() };
    let Some(file) = s.files.remove(body["source"].as_str().unwrap()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    s.files.insert(body["dest"].as_str().unwrap().to_string(), file);
    Json(json!({ "source": body["source"], "dest": body["dest"] })).into_response()
}

async fn put_service(
    State(api): State<Shared>,
    Path((name, svc)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Response {
    let mut api = api.lock().unwrap();
    let Some(s) = api.sprites.get_mut(&name) else { return StatusCode::NOT_FOUND.into_response() };
    s.services.insert(svc.clone(), body.clone());
    Json(json!({ "name": svc, "cmd": body["cmd"] })).into_response()
}

/// A service whose command is `false` exits straight away, like a daemon that can't start.
async fn start_service(State(api): State<Shared>, Path((name, svc)): Path<(String, String)>) -> Response {
    let mut api = api.lock().unwrap();
    let Some(s) = api.sprites.get_mut(&name) else { return StatusCode::NOT_FOUND.into_response() };
    let Some(def) = s.services.get(&svc).cloned() else { return StatusCode::NOT_FOUND.into_response() };
    s.started.push(svc);
    let started = json!({ "type": "started", "timestamp": 1 });
    if def["cmd"] == "false" {
        ndjson(&[
            started,
            json!({ "type": "stderr", "data": "no secrets file\n", "timestamp": 2 }),
            json!({ "type": "exit", "exit_code": 1, "timestamp": 3 }),
        ])
    } else {
        ndjson(&[
            started,
            json!({ "type": "stdout", "data": "listening\n", "timestamp": 2 }),
            json!({ "type": "complete", "timestamp": 3 }),
        ])
    }
}

async fn exec(
    State(api): State<Shared>,
    Path(name): Path<String>,
    axum::extract::RawQuery(q): axum::extract::RawQuery,
) -> Response {
    let mut api = api.lock().unwrap();
    let Some(s) = api.sprites.get_mut(&name) else { return StatusCode::NOT_FOUND.into_response() };
    let args: Vec<String> = url_args(&q.unwrap_or_default());
    s.started.push(format!("exec {}", args.join(" ")));
    "".into_response()
}

/// The repeated `cmd` query parameters, decoded.
fn url_args(q: &str) -> Vec<String> {
    q.split('&')
        .filter_map(|kv| kv.strip_prefix("cmd="))
        .map(|v| {
            let v = v.replace('+', " ");
            let mut out = Vec::new();
            let b = v.as_bytes();
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'%' && i + 2 < b.len() {
                    out.push(u8::from_str_radix(&v[i + 1..i + 3], 16).unwrap());
                    i += 3;
                } else {
                    out.push(b[i]);
                    i += 1;
                }
            }
            String::from_utf8(out).unwrap()
        })
        .collect()
}

async fn serve() -> (Shared, String) {
    let api: Shared = Arc::default();
    let app = Router::new()
        .route("/v1/sprites", get(list).post(create))
        .route("/v1/sprites/{name}", get(show).put(update).delete(destroy))
        .route("/v1/sprites/{name}/policy/network", post(policy))
        .route("/v1/sprites/{name}/checkpoint", post(checkpoint))
        .route("/v1/sprites/{name}/checkpoints/{cp}/restore", post(restore))
        .route("/v1/sprites/{name}/fs/write", put(write_file))
        .route("/v1/sprites/{name}/exec", post(exec))
        .route("/v1/sprites/{name}/fs/rename", post(rename_file))
        .route("/v1/sprites/{name}/services/{svc}", put(put_service))
        .route("/v1/sprites/{name}/services/{svc}/start", post(start_service))
        .with_state(api.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (api, url)
}

fn labels(pairs: &[(&str, &str)]) -> Labels {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn spec(name: &str, l: &[(&str, &str)], domains: &[&str]) -> SandboxSpec {
    SandboxSpec {
        name: name.into(),
        labels: labels(l),
        egress: Egress { domains: domains.iter().map(|d| d.to_string()).collect() },
    }
}

#[tokio::test]
async fn create_sets_labels_and_locks_egress_and_is_idempotent() {
    let (api, url) = serve().await;
    let compute = SpritesCompute::with_url(&url, TOKEN).unwrap();
    let s = spec("trail-1-a1", &[("factory", "trailhead"), ("role", "worker")], &["*.iroh.link"]);

    let sandbox = compute.create(&s).await.unwrap();
    assert_eq!(sandbox.id, SandboxId("trail-1-a1".into()));
    assert_eq!(sandbox.labels, s.labels);
    let again = compute.create(&s).await.unwrap();
    assert_eq!(again, sandbox);

    let api = api.lock().unwrap();
    assert_eq!(api.creates, 1);
    let sprite = &api.sprites["trail-1-a1"];
    assert_eq!(sprite.labels, ["factory=trailhead", "role=worker"]);
    assert_eq!(
        sprite.policy["rules"],
        json!([
            { "domain": "*", "action": "deny" },
            { "domain": "api.sprites.dev", "action": "allow" },
            { "domain": "*.iroh.link", "action": "allow" },
        ])
    );
}

#[tokio::test]
async fn list_pages_and_filters_by_label() {
    let (_api, url) = serve().await;
    let compute = SpritesCompute::with_url(&url, TOKEN).unwrap();
    for (name, role) in [("a", "worker"), ("b", "reviewer"), ("c", "worker"), ("d", "worker"), ("e", "worker")] {
        compute.create(&spec(name, &[("factory", "trailhead"), ("role", role)], &[])).await.unwrap();
    }
    let workers = compute.list(&labels(&[("role", "worker")])).await.unwrap();
    let names: Vec<_> = workers.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["a", "c", "d", "e"]);
    assert_eq!(compute.list(&Labels::new()).await.unwrap().len(), 5);

    let narrowed = SpritesCompute::with_url(&url, TOKEN).unwrap().with_prefix("d");
    assert_eq!(narrowed.list(&Labels::new()).await.unwrap().len(), 1);
}

#[tokio::test]
async fn kill_switch_replaces_labels_and_cuts_egress() {
    let (api, url) = serve().await;
    let compute = SpritesCompute::with_url(&url, TOKEN).unwrap();
    let id =
        compute.create(&spec("k", &[("factory", "trailhead"), ("role", "worker")], &["*.iroh.link"])).await.unwrap().id;

    compute.set_labels(&id, &labels(&[("factory", "trailhead")])).await.unwrap();
    compute.set_egress(&id, &Egress::none()).await.unwrap();
    {
        let api = api.lock().unwrap();
        assert_eq!(api.sprites["k"].labels, ["factory=trailhead"]);
        assert_eq!(api.sprites["k"].policy["rules"], json!([{ "domain": "*", "action": "deny" }]));
    }
    assert!(compute.list(&labels(&[("role", "worker")])).await.unwrap().is_empty());

    compute.set_labels(&id, &Labels::new()).await.unwrap();
    assert!(api.lock().unwrap().sprites["k"].labels.is_empty());
}

#[tokio::test]
async fn checkpoint_and_restore() {
    let (api, url) = serve().await;
    let compute = SpritesCompute::with_url(&url, TOKEN).unwrap();
    let id = compute.create(&spec("c", &[], &[])).await.unwrap().id;

    let cp = compute.checkpoint(&id, "before attempt 1").await.unwrap();
    assert_eq!(cp, CheckpointId("v2".into()));
    compute.restore(&id, &cp).await.unwrap();
    assert_eq!(api.lock().unwrap().sprites["c"].checkpoints, ["before attempt 1"]);
    assert_eq!(api.lock().unwrap().sprites["c"].restores, ["v2"]);

    let missing = compute.restore(&id, &CheckpointId("v9".into())).await;
    assert!(matches!(missing, Err(Error::NotFound(_))), "{missing:?}");
}

#[tokio::test]
async fn destroy_is_idempotent_and_errors_map() {
    let (_api, url) = serve().await;
    let compute = SpritesCompute::with_url(&url, TOKEN).unwrap();
    let id = compute.create(&spec("d", &[], &[])).await.unwrap().id;
    compute.destroy(&id).await.unwrap();
    compute.destroy(&id).await.unwrap();
    assert!(matches!(compute.checkpoint(&id, "x").await, Err(Error::NotFound(_))));

    let wrong = SpritesCompute::with_url(&url, "nope").unwrap();
    assert!(matches!(wrong.list(&Labels::new()).await, Err(Error::Forbidden(_))));
}

#[tokio::test]
async fn start_uploads_files_and_starts_a_service() {
    let (api, url) = serve().await;
    let compute = SpritesCompute::with_url(&url, TOKEN).unwrap();
    let id = compute.create(&spec("s", &[], &[])).await.unwrap().id;
    let program = Program {
        name: "trailhead-daemon".into(),
        files: vec![
            SandboxFile { path: "/home/sprite/.trailhead/daemon".into(), contents: b"\x7fELF".to_vec(), mode: 0o755 },
            SandboxFile { path: "/home/sprite/.trailhead/secrets.json".into(), contents: b"{}".to_vec(), mode: 0o600 },
        ],
        cmd: "/home/sprite/.trailhead/daemon".into(),
        args: vec!["--secrets".into(), "/home/sprite/.trailhead/secrets.json".into()],
    };
    compute.start(&id, &program).await.unwrap();
    {
        let api = api.lock().unwrap();
        let s = &api.sprites["s"];
        assert_eq!(s.files["/home/sprite/.trailhead/daemon"], (b"\x7fELF".to_vec(), "0755".to_string()));
        assert_eq!(s.files["/home/sprite/.trailhead/secrets.json"].1, "0600");
        assert_eq!(s.files.len(), 2, "no temp files left behind: {:?}", s.files.keys());
        assert_eq!(
            s.services["trailhead-daemon"]["args"],
            json!(["--secrets", "/home/sprite/.trailhead/secrets.json"])
        );
        assert_eq!(s.started, ["trailhead-daemon"]);
    }

    let broken = Program { name: "broken".into(), files: vec![], cmd: "false".into(), args: vec![] };
    let err = compute.start(&id, &broken).await.unwrap_err().to_string();
    assert!(err.contains("exited") && err.contains("no secrets file"), "{err}");
}

#[tokio::test]
async fn wake_takes_a_short_hold() {
    let (api, url) = serve().await;
    let compute = SpritesCompute::with_url(&url, TOKEN).unwrap();
    let id = compute.create(&spec("w", &[], &[])).await.unwrap().id;
    compute.wake(&id).await.unwrap();
    let started = api.lock().unwrap().sprites["w"].started.clone();
    assert_eq!(started.len(), 1);
    assert!(
        started[0].contains("--unix-socket /.sprite/api.sock") && started[0].contains(r#"{"expire":"2m"}"#),
        "{started:?}"
    );
    assert!(started[0].ends_with("http://sprite/v1/tasks/trailhead-wake"), "{started:?}");
}

#[tokio::test]
async fn an_interrupted_restore_is_retried() {
    let (api, url) = serve().await;
    let compute = SpritesCompute::with_url(&url, TOKEN).unwrap();
    let id = compute.create(&spec("r", &[], &[])).await.unwrap().id;
    let cp = compute.checkpoint(&id, "warm").await.unwrap();
    api.lock().unwrap().sprites.get_mut("r").unwrap().cut_restores = 1;

    compute.restore(&id, &cp).await.unwrap();
    assert_eq!(api.lock().unwrap().sprites["r"].restores, ["v2", "v2"], "cut off once, then restored again");

    let unknown = compute.restore(&id, &CheckpointId("v9".into())).await;
    assert!(matches!(unknown, Err(Error::NotFound(_))), "an error event isn't retried: {unknown:?}");
}
