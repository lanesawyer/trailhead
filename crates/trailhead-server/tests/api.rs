use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use trailhead_server::{AppState, BATCH_HEADER, LocalConfig, open_local, router};

struct Api {
    app: Router,
    human: String,
    _dir: tempfile::TempDir,
}

struct Res {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Value,
}

impl Api {
    async fn new() -> Api {
        let dir = tempfile::tempdir().unwrap();
        let cfg = LocalConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            db: dir.path().join("t.db"),
            credentials: dir.path().join("credentials"),
            user_name: "lane".into(),
            pid_file: None,
        };
        let (store, _) = open_local(&cfg).await.unwrap();
        let human = std::fs::read_to_string(&cfg.credentials).unwrap().trim().to_string();
        Api { app: router(Arc::new(AppState::new(store))), human, _dir: dir }
    }

    async fn call(
        &self,
        token: Option<&str>,
        method: &str,
        path: &str,
        body: Option<Value>,
        extra: &[(&str, &str)],
    ) -> Res {
        let mut req = Request::builder().method(method).uri(path);
        if let Some(t) = token {
            req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        for (k, v) in extra {
            req = req.header(*k, *v);
        }
        let req = match body {
            Some(b) => req.header(header::CONTENT_TYPE, "application/json").body(Body::from(b.to_string())),
            None => req.body(Body::empty()),
        }
        .unwrap();
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let body = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
        Res { status, headers, body }
    }

    async fn as_human(&self, method: &str, path: &str, body: Option<Value>) -> Res {
        self.call(Some(&self.human.clone()), method, path, body, &[]).await
    }

    async fn seed(&self) {
        let r = self.as_human("POST", "/v1/projects", Some(json!({"key": "AUTH", "name": "Auth"}))).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    }
}

#[tokio::test]
async fn health_is_open_and_everything_else_needs_a_token() {
    let api = Api::new().await;
    let health = api.call(None, "GET", "/v1/health", None, &[]).await;
    assert_eq!(health.status, StatusCode::OK);
    assert_eq!(health.body["ok"], true);
    assert_eq!(health.body["pid"], std::process::id());
    let r = api.call(None, "GET", "/v1/ready", None, &[]).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.body["error"]["code"], "unauthorized");
    let r = api.call(Some("th_nope"), "GET", "/v1/ready", None, &[]).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn agent_lifecycle_over_http() {
    let api = Api::new().await;
    api.seed().await;
    let a = api.as_human("POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": "first", "priority": 2}))).await;
    assert_eq!(a.status, StatusCode::CREATED);
    let b = api
        .as_human("POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": "second", "blocked_by": ["AUTH-1"]})))
        .await;
    assert_eq!(b.status, StatusCode::CREATED);

    let agent = api.as_human("POST", "/v1/agents", Some(json!({"name": "claude"}))).await;
    let secret = agent.body["secret"].as_str().unwrap().to_string();
    let r = api.call(Some(&secret), "POST", "/v1/agents", Some(json!({"name": "child"})), &[]).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    let ready = api.call(Some(&secret), "GET", "/v1/ready?fields=key,title", None, &[]).await;
    assert_eq!(ready.body, json!([{"key": "AUTH-1", "title": "first"}]));

    let r = api.call(Some(&secret), "POST", "/v1/tasks/AUTH-2/claim", None, &[]).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.body["error"]["code"], "blocked_by");
    assert_eq!(r.body["error"]["blocked_by"], json!(["AUTH-1"]));

    let r = api.call(Some(&secret), "POST", "/v1/tasks/auth-1/claim", Some(json!({"ttl": 600})), &[]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["task"]["status"], "in_progress");

    let r = api.call(Some(&secret), "PATCH", "/v1/tasks/AUTH-1", Some(json!({"status": "done"})), &[]).await;
    assert_eq!(r.status, StatusCode::OK);
    let ready = api.call(Some(&secret), "GET", "/v1/ready?fields=key", None, &[]).await;
    assert_eq!(ready.body, json!([{"key": "AUTH-2"}]));

    let tokens = api.as_human("GET", "/v1/tokens", None).await;
    let agent_token = tokens.body.as_array().unwrap().iter().find(|t| t["principal"]["kind"] == "agent").unwrap();
    let r = api.as_human("DELETE", &format!("/v1/tokens/{}", agent_token["id"].as_str().unwrap()), None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(api.call(Some(&secret), "GET", "/v1/me", None, &[]).await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn etags_guard_against_stale_writes() {
    let api = Api::new().await;
    api.seed().await;
    let r = api.as_human("POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": "v1"}))).await;
    assert_eq!(r.headers[header::ETAG], "\"1\"");
    let token = api.human.clone();
    let patch = Some(json!({"title": "v2"}));
    let r = api.call(Some(&token), "PATCH", "/v1/tasks/AUTH-1", patch.clone(), &[("if-match", "\"1\"")]).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers[header::ETAG], "\"2\"");
    let r = api.call(Some(&token), "PATCH", "/v1/tasks/AUTH-1", patch, &[("if-match", "\"1\"")]).await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(r.body["error"]["current_version"], 2);
}

#[tokio::test]
async fn batch_header_lets_a_request_be_undone() {
    let api = Api::new().await;
    api.seed().await;
    let r = api.as_human("POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": "a"}))).await;
    let r2 = api
        .as_human("POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": "b", "blocked_by": ["AUTH-1"]})))
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let batch = r2.headers[BATCH_HEADER].to_str().unwrap().to_string();
    let undo = api.as_human("POST", "/v1/undo", Some(json!({"batch_id": batch}))).await;
    assert_eq!(undo.status, StatusCode::OK, "{}", undo.body);
    assert_eq!(undo.body["reverted"].as_array().unwrap().len(), 2);
    assert_eq!(api.as_human("GET", "/v1/tasks/AUTH-2", None).await.status, StatusCode::NOT_FOUND);
    let ready = api.as_human("GET", "/v1/ready?fields=key", None).await;
    assert_eq!(ready.body, json!([{"key": "AUTH-1"}]));
}

#[tokio::test]
async fn nullable_fields_clear_on_null() {
    let api = Api::new().await;
    api.seed().await;
    api.as_human(
        "POST",
        "/v1/tasks",
        Some(json!({"project": "AUTH", "title": "due", "due_at": "2026-11-01T12:00:00Z"})),
    )
    .await;
    let r = api.as_human("PATCH", "/v1/tasks/AUTH-1", Some(json!({"title": "still due"}))).await;
    assert_eq!(r.body["due_at"], "2026-11-01T12:00:00Z");
    let r = api.as_human("PATCH", "/v1/tasks/AUTH-1", Some(json!({"due_at": null}))).await;
    assert!(r.body["due_at"].is_null());
    let r = api.as_human("PATCH", "/v1/tasks/AUTH-1", Some(json!({"status": "bogus"}))).await;
    assert!(r.status.is_client_error());
}

#[tokio::test]
async fn reopening_keeps_the_same_cli_token() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = LocalConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        db: dir.path().join("t.db"),
        credentials: dir.path().join("credentials"),
        user_name: "lane".into(),
        pid_file: None,
    };
    drop(open_local(&cfg).await.unwrap());
    let first = std::fs::read_to_string(&cfg.credentials).unwrap();
    drop(open_local(&cfg).await.unwrap());
    assert_eq!(first, std::fs::read_to_string(&cfg.credentials).unwrap());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&cfg.credentials).unwrap().permissions().mode() & 0o777, 0o600);
    }
}

/// Reads SSE text from a streaming body until `done` says stop, or 5s pass.
async fn read_sse(body: Body, done: impl Fn(&str) -> bool) -> String {
    let mut body = body;
    let mut out = String::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while !done(&out) {
        let frame = tokio::time::timeout_at(deadline, body.frame()).await.expect("timed out waiting for events");
        if let Some(Ok(f)) = frame
            && let Some(data) = f.data_ref()
        {
            out.push_str(&String::from_utf8_lossy(data));
        }
    }
    out
}

#[tokio::test]
async fn event_stream_delivers_new_writes_and_resumes() {
    let api = Api::new().await;
    api.seed().await;
    let stream_req = |extra: &[(&str, &str)], query: &str| {
        let mut req = Request::builder()
            .uri(format!("/v1/events/stream{query}"))
            .header(header::AUTHORIZATION, format!("Bearer {}", api.human));
        for (k, v) in extra {
            req = req.header(*k, *v);
        }
        req.body(Body::empty()).unwrap()
    };

    // A fresh stream starts at "now": the project.created from seeding isn't replayed.
    let res = api.app.clone().oneshot(stream_req(&[], "")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()[header::CONTENT_TYPE], "text/event-stream");
    let reader = tokio::spawn(read_sse(res.into_body(), |s| s.contains("event: task.created")));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    api.as_human("POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": "live"}))).await;
    let got = reader.await.unwrap();
    assert!(!got.contains("project.created"), "{got}");
    assert!(got.contains("\"title\":\"live\""), "{got}");

    // Resuming replays from the cursor, filtered by kind.
    let res = api.app.clone().oneshot(stream_req(&[("last-event-id", "0")], "?kinds=project.")).await.unwrap();
    let got = read_sse(res.into_body(), |s| s.contains("event: project.created")).await;
    assert!(got.contains("id: 1\n"), "{got}");
    assert!(!got.contains("task.created"), "{got}");

    let res = api.app.clone().oneshot(stream_req(&[], "?task=NOPE-1")).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn idempotency_key_replays_the_first_response() {
    let api = Api::new().await;
    api.seed().await;
    let human = api.human.clone();
    let body = json!({"project": "AUTH", "title": "once"});
    let key = [("idempotency-key", "k-1")];
    let first = api.call(Some(&human), "POST", "/v1/tasks", Some(body.clone()), &key).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    assert!(first.headers.get("idempotent-replayed").is_none());
    let again = api.call(Some(&human), "POST", "/v1/tasks", Some(body.clone()), &key).await;
    assert_eq!(again.status, StatusCode::CREATED);
    assert_eq!(again.headers["idempotent-replayed"], "true");
    assert_eq!(again.body, first.body);
    assert_eq!(again.headers[header::ETAG], first.headers[header::ETAG]);
    assert_eq!(again.headers[BATCH_HEADER], first.headers[BATCH_HEADER]);

    let tasks = api.as_human("GET", "/v1/tasks?project=AUTH", None).await;
    assert_eq!(tasks.body.as_array().unwrap().len(), 1, "{}", tasks.body);

    let other = api.call(Some(&human), "POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": "x"})), &key).await;
    assert_eq!(other.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(other.body["error"]["code"], "idempotency_key_reused");
    let other_path = api.call(Some(&human), "POST", "/v1/projects", Some(body), &key).await;
    assert_eq!(other_path.body["error"]["code"], "idempotency_key_reused");
}

#[tokio::test]
async fn failed_requests_are_not_cached_under_their_key() {
    let api = Api::new().await;
    let human = api.human.clone();
    let body = json!({"project": "AUTH", "title": "early"});
    let key = [("idempotency-key", "retry-me")];
    let r = api.call(Some(&human), "POST", "/v1/tasks", Some(body.clone()), &key).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.body);
    api.seed().await;
    let r = api.call(Some(&human), "POST", "/v1/tasks", Some(body), &key).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert!(r.headers.get("idempotent-replayed").is_none());
    assert_eq!(r.body["key"], "AUTH-1");
}

#[tokio::test]
async fn idempotency_keys_are_scoped_per_actor() {
    let api = Api::new().await;
    api.seed().await;
    let mut secrets = Vec::new();
    for name in ["a", "b"] {
        let r = api.as_human("POST", "/v1/agents", Some(json!({"name": name}))).await;
        secrets.push(r.body["secret"].as_str().unwrap().to_string());
    }
    let key = [("idempotency-key", "shared")];
    let a =
        api.call(Some(&secrets[0]), "POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": "a"})), &key).await;
    let b =
        api.call(Some(&secrets[1]), "POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": "b"})), &key).await;
    assert_eq!(a.status, StatusCode::CREATED, "{}", a.body);
    assert_eq!(b.status, StatusCode::CREATED, "{}", b.body);
    assert!(b.headers.get("idempotent-replayed").is_none());
    assert_eq!(a.body["key"], "AUTH-1");
    assert_eq!(b.body["key"], "AUTH-2");
}

#[tokio::test]
async fn bad_idempotency_keys_and_reads_are_handled() {
    let api = Api::new().await;
    api.seed().await;
    let human = api.human.clone();
    let long = "k".repeat(256);
    let r = api
        .call(
            Some(&human),
            "POST",
            "/v1/tasks",
            Some(json!({"project": "AUTH", "title": "t"})),
            &[("idempotency-key", &long)],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = api.call(None, "POST", "/v1/tasks", Some(json!({})), &[("idempotency-key", "k")]).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    for _ in 0..2 {
        let r = api.call(Some(&human), "GET", "/v1/ready", None, &[("idempotency-key", "read")]).await;
        assert_eq!(r.status, StatusCode::OK);
        assert!(r.headers.get("idempotent-replayed").is_none());
    }
}
