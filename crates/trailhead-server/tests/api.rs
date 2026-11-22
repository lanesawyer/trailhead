mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use trailhead_client::creds::Cred;
use trailhead_server::{AppState, BATCH_HEADER, open_local, router};

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
        let cfg = common::local_config(dir.path());
        let (store, _) = open_local(&cfg).await.unwrap();
        let human = std::fs::read_to_string(cfg.creds.file(Cred::User)).unwrap().trim().to_string();
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
    assert!(health.body.get("store_error").is_none(), "{}", health.body);
    assert!(health.body["version"].is_string());
    // The pid and paths name the local user, so they need a token.
    for k in ["pid", "exe", "exe_modified_ms", "db"] {
        assert!(health.body.get(k).is_none(), "{k} leaked: {}", health.body);
    }
    let health = api.as_human("GET", "/v1/health", None).await;
    assert_eq!(health.body["pid"], std::process::id());
    let health = api.call(Some("th_nope"), "GET", "/v1/health", None, &[]).await;
    assert_eq!(health.status, StatusCode::OK);
    assert!(health.body.get("pid").is_none());
    let r = api.call(None, "GET", "/v1/ready", None, &[]).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.body["error"]["code"], "unauthorized");
    let r = api.call(Some("th_nope"), "GET", "/v1/ready", None, &[]).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn users_lists_the_workspaces_people() {
    let api = Api::new().await;
    let r = api.as_human("GET", "/v1/users", None).await;
    assert_eq!(r.status, StatusCode::OK);
    let users = r.body.as_array().unwrap();
    assert_eq!(users.len(), 1);
    assert!(users[0]["name"].is_string() && users[0]["id"].is_string());
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
    // An agent creates its own sub-agent, never needing a human's credentials.
    let r = api.call(Some(&secret), "POST", "/v1/agents", Some(json!({"name": "child"})), &[]).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert_eq!(r.body["agent"]["name"], "claude/child");

    let me = api.call(Some(&secret), "GET", "/v1/me", None, &[]).await;
    assert_eq!(me.body["agent_id"], me.body["agent"]["id"]);
    assert!(me.body["agent_id"].is_string());
    assert!(api.as_human("GET", "/v1/me", None).await.body["agent_id"].is_null());

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
async fn composed_writes_are_all_or_nothing() {
    let api = Api::new().await;
    api.seed().await;
    let r = api
        .as_human("POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": "orphan", "blocked_by": ["AUTH-9"]})))
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.body);
    assert_eq!(api.as_human("GET", "/v1/tasks", None).await.body, json!([]));

    let r = api.as_human("POST", "/v1/agents", Some(json!({"name": "greedy", "rate_limit_per_minute": 0}))).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);
    assert_eq!(api.as_human("GET", "/v1/agents", None).await.body, json!([]));

    let r = api.as_human("PATCH", "/v1/projects/AUTH", Some(json!({"require_review": true}))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let r = api.as_human("POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": "work"}))).await;
    assert_eq!(r.body["key"], "AUTH-1");
    let agent = api.as_human("POST", "/v1/agents", Some(json!({"name": "worker"}))).await;
    let secret = agent.body["secret"].as_str().unwrap().to_string();
    api.call(Some(&secret), "POST", "/v1/tasks/AUTH-1/claim", None, &[]).await;
    let finish = |status: &str| Some(json!({"status": status, "handoff": format!("sent to {status}")}));
    let r = api.call(Some(&secret), "PATCH", "/v1/tasks/AUTH-1", finish("done"), &[]).await;
    assert_eq!(r.body["error"]["code"], "review_required", "{}", r.body);
    assert_eq!(api.as_human("GET", "/v1/tasks/AUTH-1/comments", None).await.body, json!([]));
    let r = api.call(Some(&secret), "PATCH", "/v1/tasks/AUTH-1", finish("in_review"), &[]).await;
    assert_eq!(r.body["status"], "in_review", "{}", r.body);
    let comments = api.as_human("GET", "/v1/tasks/AUTH-1/comments", None).await.body;
    assert_eq!((&comments[0]["kind"], &comments[0]["body"]), (&json!("handoff"), &json!("sent to in_review")));
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
    assert_eq!(r.body["due_on"], "2026-11-01");
    let r = api.as_human("PATCH", "/v1/tasks/AUTH-1", Some(json!({"due_at": null}))).await;
    assert!(r.body["due_at"].is_null());
    assert_eq!(r.body["due_on"], "2026-11-01");
    let r = api.as_human("PATCH", "/v1/tasks/AUTH-1", Some(json!({"due_on": null}))).await;
    assert!(r.body["due_on"].is_null());
    let day_only = json!({"due_on": "2026-11-01", "due_tz": "America/Los_Angeles"});
    let r = api.as_human("PATCH", "/v1/tasks/AUTH-1", Some(day_only)).await;
    assert_eq!((&r.body["due_on"], &r.body["due_at"]), (&json!("2026-11-01"), &Value::Null));
    let r = api.as_human("GET", "/v1/tasks?due_before=2026-11-02T06:59:00Z&fields=key", None).await;
    assert_eq!(r.body, json!([]));
    let r = api.as_human("GET", "/v1/tasks?due_before=2026-11-02T08:00:00Z&fields=key", None).await;
    assert_eq!(r.body, json!([{"key": "AUTH-1"}]));
    let r = api.as_human("PATCH", "/v1/tasks/AUTH-1", Some(json!({"status": "bogus"}))).await;
    assert!(r.status.is_client_error());
}

#[tokio::test]
async fn scheduled_days_filter_todays_plan() {
    let api = Api::new().await;
    api.seed().await;
    let body =
        json!({"project": "AUTH", "title": "plan", "scheduled_on": "2026-10-17", "due_at": "2026-11-01T12:00:00Z"});
    let r = api.as_human("POST", "/v1/tasks", Some(body)).await;
    assert_eq!(r.body["scheduled_on"], "2026-10-17");
    assert_eq!(r.body["due_at"], "2026-11-01T12:00:00Z");
    api.as_human("POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": "later", "scheduled_on": "2026-10-18"})))
        .await;
    let r = api.as_human("GET", "/v1/tasks?scheduled_through=2026-10-17&fields=key", None).await;
    assert_eq!(r.body, json!([{"key": "AUTH-1"}]));
    let r = api.as_human("GET", "/v1/tasks?scheduled_through=tomorrow", None).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = api.as_human("PATCH", "/v1/tasks/AUTH-1", Some(json!({"scheduled_on": "10/18/2026"}))).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let at = json!({"scheduled_at": "2026-10-17T13:00:00-07:00", "scheduled_tz": "America/Los_Angeles"});
    let r = api.as_human("PATCH", "/v1/tasks/AUTH-1", Some(at)).await;
    assert_eq!(r.body["scheduled_on"], "2026-10-17");
    assert_eq!(r.body["scheduled_at"], "2026-10-17T20:00:00Z");
    assert_eq!(r.body["scheduled_tz"], "America/Los_Angeles");
    let r = api.as_human("GET", "/v1/tasks?sort=scheduled&fields=key", None).await;
    assert_eq!(r.body, json!([{"key": "AUTH-1"}, {"key": "AUTH-2"}]));
    let r = api.as_human("PATCH", "/v1/tasks/AUTH-1", Some(json!({"scheduled_on": null}))).await;
    assert!(r.body["scheduled_on"].is_null() && r.body["scheduled_at"].is_null());
    assert_eq!(r.body["due_at"], "2026-11-01T12:00:00Z");
}

#[tokio::test]
async fn reopening_keeps_the_same_cli_token() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = common::local_config(dir.path());
    drop(open_local(&cfg).await.unwrap());
    let first = std::fs::read_to_string(cfg.creds.file(Cred::User)).unwrap();
    drop(open_local(&cfg).await.unwrap());
    assert_eq!(first, std::fs::read_to_string(cfg.creds.file(Cred::User)).unwrap());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(cfg.creds.file(Cred::User)).unwrap().permissions().mode() & 0o777, 0o600);
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

#[tokio::test]
async fn rate_limited_tokens_get_429_with_retry_after() {
    let api = Api::new().await;
    let r = api.as_human("POST", "/v1/agents", Some(json!({"name": "bot", "rate_limit_per_minute": 2}))).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert_eq!(r.body["token"]["rate_limit_per_minute"], 2);
    let bot = r.body["secret"].as_str().unwrap().to_string();
    for _ in 0..2 {
        assert_eq!(api.call(Some(&bot), "GET", "/v1/me", None, &[]).await.status, StatusCode::OK);
    }
    let r = api.call(Some(&bot), "GET", "/v1/me", None, &[]).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.body["error"]["code"], "rate_limited");
    let retry: u64 = r.headers[header::RETRY_AFTER].to_str().unwrap().parse().unwrap();
    assert!((1..=30).contains(&retry), "{retry}");
    let mcp = api.call(Some(&bot), "POST", "/mcp", Some(json!({})), &[]).await;
    assert_eq!(mcp.status, StatusCode::TOO_MANY_REQUESTS);
    // Other tokens have their own budget.
    assert_eq!(api.as_human("GET", "/v1/me", None).await.status, StatusCode::OK);
}

#[tokio::test]
async fn scoped_tokens_only_see_their_projects() {
    let api = Api::new().await;
    api.seed().await;
    api.as_human("POST", "/v1/projects", Some(json!({"key": "WEB", "name": "Web"}))).await;
    let web = api.as_human("POST", "/v1/tasks", Some(json!({"project": "WEB", "title": "w"}))).await;
    assert_eq!(web.status, StatusCode::CREATED);
    api.as_human("POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": "a"}))).await;
    api.as_human("POST", "/v1/agents", Some(json!({"name": "bot"}))).await;

    let r = api.as_human("POST", "/v1/tokens", Some(json!({"name": "ci", "agent": "bot", "projects": ["AUTH"]}))).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert_eq!(r.body["token"]["principal"]["kind"], "agent");
    let bot = r.body["secret"].as_str().unwrap().to_string();

    let ready = api.call(Some(&bot), "GET", "/v1/ready", None, &[]).await;
    let keys: Vec<_> = ready.body.as_array().unwrap().iter().map(|t| t["key"].as_str().unwrap()).collect();
    assert_eq!(keys, vec!["AUTH-1"]);
    let r = api.call(Some(&bot), "GET", "/v1/tasks/WEB-1", None, &[]).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = api.call(Some(&bot), "POST", "/v1/tasks", Some(json!({"project": "WEB", "title": "x"})), &[]).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.body["error"]["code"], "forbidden");

    let tokens = api.as_human("GET", "/v1/tokens", None).await;
    let ci = tokens.body.as_array().unwrap().iter().find(|t| t["name"] == "ci").unwrap();
    assert_eq!(ci["projects"].as_array().unwrap().len(), 1);
    assert!(ci["rate_limit_per_minute"].is_null());

    let r = api.as_human("POST", "/v1/tokens", Some(json!({"name": "x", "agent": "nobody"}))).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // Rescope in place: the same secret now reaches WEB, and an omitted field stays put.
    let path = format!("/v1/tokens/{}", ci["id"].as_str().unwrap());
    let r = api.as_human("PATCH", &path, Some(json!({"rate_limit_per_minute": 60}))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["projects"].as_array().unwrap().len(), 1);
    let r = api.call(Some(&bot), "PATCH", &path, Some(json!({"projects": null})), &[]).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "the agent can't widen its own token");
    let r = api.as_human("PATCH", &path, Some(json!({"projects": null}))).await;
    assert!(r.body["projects"].is_null() && r.body["rate_limit_per_minute"] == 60, "{}", r.body);
    let r = api.call(Some(&bot), "GET", "/v1/tasks/WEB-1", None, &[]).await;
    assert_eq!(r.status, StatusCode::OK);
}

#[tokio::test]
async fn idempotency_keys_belong_to_the_token() {
    let api = Api::new().await;
    api.seed().await;
    let first = api.as_human("POST", "/v1/agents", Some(json!({"name": "bot"}))).await.body["secret"]
        .as_str()
        .unwrap()
        .to_string();
    let second = api
        .as_human("POST", "/v1/tokens", Some(json!({"name": "bot scoped", "agent": "bot", "projects": ["AUTH"]})))
        .await
        .body["secret"]
        .as_str()
        .unwrap()
        .to_string();
    let body = Some(json!({"project": "AUTH", "title": "same key"}));
    let a = api.call(Some(&first), "POST", "/v1/tasks", body.clone(), &[("idempotency-key", "k1")]).await;
    let b = api.call(Some(&second), "POST", "/v1/tasks", body, &[("idempotency-key", "k1")]).await;
    assert_eq!(a.status, StatusCode::CREATED);
    assert_eq!(b.status, StatusCode::CREATED);
    assert!(b.headers.get("idempotent-replayed").is_none());
    assert_ne!(a.body["key"], b.body["key"]);
}

#[tokio::test]
async fn task_list_sorts_and_pages_and_event_list_filters_kinds() {
    let api = Api::new().await;
    api.seed().await;
    for title in ["a", "b", "c"] {
        let r = api.as_human("POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": title}))).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    }
    let r = api.as_human("GET", "/v1/tasks?sort=-created_at&limit=2&offset=1&fields=key", None).await;
    assert_eq!(r.body, json!([{"key": "AUTH-2"}, {"key": "AUTH-1"}]));
    let r = api.as_human("GET", "/v1/tasks?sort=sideways", None).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);

    let r = api.as_human("GET", "/v1/events?kinds=project.,nope&limit=1", None).await;
    let kinds: Vec<&str> = r.body.as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["project.created"]);
}

#[tokio::test]
async fn task_list_can_include_claims_with_holder_names() {
    let api = Api::new().await;
    api.seed().await;
    for title in ["agent's", "mine", "free"] {
        api.as_human("POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": title}))).await;
    }
    let agent = api.as_human("POST", "/v1/agents", Some(json!({"name": "claude"}))).await;
    let secret = agent.body["secret"].as_str().unwrap().to_string();
    let r = api.call(Some(&secret), "POST", "/v1/tasks/AUTH-1/claim", None, &[]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    api.as_human("POST", "/v1/tasks/AUTH-2/claim", None).await;

    let r = api.as_human("GET", "/v1/tasks?sort=created_at&fields=key&include=claim", None).await;
    let items = r.body.as_array().unwrap();
    assert_eq!(items[0]["claim"]["holder"], "claude");
    assert!(items[0]["claim"]["expires_at"].is_string());
    assert_eq!(items[1]["claim"]["holder"], "lane");
    assert!(items[1]["claim"]["expires_at"].is_null());
    assert!(items[2]["claim"].is_null());
    assert_eq!(items[2]["key"], "AUTH-3");

    let plain = api.as_human("GET", "/v1/tasks", None).await;
    assert!(plain.body[0].get("claim").is_none());
    let r = api.as_human("GET", "/v1/tasks?include=everything", None).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn search_hits_carry_where_they_matched() {
    let api = Api::new().await;
    api.seed().await;
    api.as_human("POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": "Rotate keys"}))).await;
    let r = api.as_human("GET", "/v1/search?q=rotate&project=AUTH&status=open&fields=key", None).await;
    assert_eq!(
        r.body,
        json!([{"key": "AUTH-1", "match": {"field": "title", "snippet": "Rotate keys", "comment_id": null}}])
    );
    let r = api.as_human("GET", "/v1/search?q=rotate&status=done", None).await;
    assert_eq!(r.body, json!([]));
}

#[tokio::test]
async fn task_list_can_include_open_questions_and_last_handoff() {
    let api = Api::new().await;
    api.seed().await;
    for title in ["asks", "quiet"] {
        api.as_human("POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": title}))).await;
    }
    let post = async |kind: &str, body: &str| {
        let r = api.as_human("POST", "/v1/tasks/AUTH-1/comments", Some(json!({"kind": kind, "body": body}))).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
        r.body["id"].as_str().unwrap().to_string()
    };
    post("handoff", "first pass").await;
    post("handoff", "second pass").await;
    let answered = post("question", "which region?").await;
    post("question", "which plan?").await;
    let r = api.as_human("POST", &format!("/v1/comments/{answered}/resolve"), Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);

    let r = api
        .as_human(
            "GET",
            "/v1/tasks?status=open,needs_input&sort=created_at&fields=key&include=open_questions,last_handoff",
            None,
        )
        .await;
    let items = r.body.as_array().unwrap();
    let questions: Vec<_> = items[0]["open_questions"].as_array().unwrap().iter().map(|c| c["body"].clone()).collect();
    assert_eq!(questions, [json!("which plan?")]);
    assert_eq!(items[0]["last_handoff"]["body"], "second pass");
    assert_eq!(items[1]["open_questions"], json!([]));
    assert!(items[1]["last_handoff"].is_null());
    assert!(items[0].get("claim").is_none());
}

#[tokio::test]
async fn task_list_can_include_every_blocker() {
    let api = Api::new().await;
    api.seed().await;
    for title in ["blocker", "other blocker"] {
        api.as_human("POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": title}))).await;
    }
    let r = api
        .as_human(
            "POST",
            "/v1/tasks",
            Some(json!({"project": "AUTH", "title": "blocked", "blocked_by": ["AUTH-2", "AUTH-1"]})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    api.as_human("POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": "free"}))).await;
    let r = api.as_human("GET", "/v1/tasks?sort=created_at&fields=id,key&include=blocked_by", None).await;
    let items = r.body.as_array().unwrap();
    assert_eq!(items[2]["blocked_by"], json!([items[0]["id"], items[1]["id"]]));
    assert_eq!(items[0]["blocked_by"], json!([]));
    assert_eq!(items[2]["blocked_by_hidden"], json!(0));
    assert_eq!(items[3]["blocked_by"], json!([]));
    let r = api.as_human("GET", "/v1/tasks?fields=key", None).await;
    assert!(r.body[0].get("blocked_by").is_none());
}

#[tokio::test]
async fn review_comments_carry_a_typed_verdict() {
    let api = Api::new().await;
    api.seed().await;
    api.as_human("POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": "rotate keys"}))).await;
    let post = async |body: serde_json::Value| api.as_human("POST", "/v1/tasks/AUTH-1/comments", Some(body)).await;

    let r = post(json!({"kind": "review", "body": "tests miss expiry", "verdict": "request_changes", "model": "opus"}))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert_eq!(r.body["verdict"], "request_changes");
    assert_eq!(r.body["model"], "opus");

    let r = post(json!({"kind": "review", "body": "no verdict"})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);
    let r = post(json!({"kind": "note", "body": "sneaky", "verdict": "approve"})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);

    let r = api.as_human("GET", "/v1/tasks/AUTH-1/comments", None).await;
    let comments = r.body.as_array().unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0]["kind"], "review");
}

#[tokio::test]
async fn grade_is_set_cleared_and_filtered_over_http() {
    let api = Api::new().await;
    api.seed().await;
    let r = api.as_human("POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": "graded", "grade": 3}))).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert_eq!(r.body["grade"], 3);
    api.as_human("POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": "plain"}))).await;
    let r = api.as_human("POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": "bad", "grade": 9}))).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);

    let r = api.as_human("PATCH", "/v1/tasks/AUTH-2", Some(json!({"grade": 2}))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["grade"], 2);
    // Omitting grade leaves it alone; null clears it.
    let r = api.as_human("PATCH", "/v1/tasks/AUTH-2", Some(json!({"title": "renamed"}))).await;
    assert_eq!(r.body["grade"], 2);
    let r = api.as_human("PATCH", "/v1/tasks/AUTH-2", Some(json!({"grade": null}))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert!(r.body["grade"].is_null(), "{}", r.body);
    let r = api.as_human("PATCH", "/v1/tasks/AUTH-2", Some(json!({"grade": 7}))).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);

    let keys = |r: &Res| -> Vec<String> {
        r.body.as_array().unwrap().iter().map(|t| t["key"].as_str().unwrap().to_string()).collect()
    };
    for path in ["/v1/tasks", "/v1/ready"] {
        let r = api.as_human("GET", &format!("{path}?grade=3"), None).await;
        assert_eq!(keys(&r), ["AUTH-1"], "{path}");
        let r = api.as_human("GET", &format!("{path}?grade=graded"), None).await;
        assert_eq!(keys(&r), ["AUTH-1"], "{path}");
        let r = api.as_human("GET", &format!("{path}?grade=ungraded"), None).await;
        assert_eq!(keys(&r), ["AUTH-2"], "{path}");
        let r = api.as_human("GET", &format!("{path}?grade=nope"), None).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{path}: {}", r.body);
        let r = api.as_human("GET", &format!("{path}?grade=5"), None).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{path}: {}", r.body);
    }
}

#[tokio::test]
async fn an_agent_cant_set_a_first_grade_or_lower_one() {
    let api = Api::new().await;
    api.seed().await;
    api.as_human("POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": "ungraded"}))).await;
    api.as_human("POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": "graded", "grade": 2}))).await;
    let agent = api.as_human("POST", "/v1/agents", Some(json!({"name": "claude"}))).await;
    let secret = agent.body["secret"].as_str().unwrap().to_string();
    let patch = async |task: &str, body: Value| {
        api.call(Some(&secret), "PATCH", &format!("/v1/tasks/{task}"), Some(body), &[]).await
    };

    let r = patch("AUTH-1", json!({"grade": 2})).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.body);
    assert_eq!(r.body["error"]["code"], "forbidden");
    let r = api
        .call(Some(&secret), "POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": "x", "grade": 2})), &[])
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.body);

    assert_eq!(patch("AUTH-2", json!({"grade": 1})).await.status, StatusCode::FORBIDDEN);
    assert_eq!(patch("AUTH-2", json!({"grade": null})).await.status, StatusCode::FORBIDDEN);
    let r = patch("AUTH-2", json!({"grade": 4})).await;
    assert_eq!(r.status, StatusCode::OK, "an agent may raise a grade: {}", r.body);
    assert_eq!(r.body["grade"], 4);
    let r = api.as_human("GET", "/v1/tasks/AUTH-1", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["task"]["key"], "AUTH-1", "{}", r.body);
    assert!(r.body["task"].as_object().is_some_and(|t| t.get("grade").is_some_and(Value::is_null)), "{}", r.body);
}

#[tokio::test]
async fn creating_a_task_lists_possible_duplicates_among_open_tasks() {
    let api = Api::new().await;
    api.seed().await;
    let create =
        |title: &'static str| api.as_human("POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": title})));
    let first = create("Rotate signing keys").await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    assert!(first.body.get("possible_duplicates").is_none(), "{}", first.body);

    let dup = create("Rotate the signing keys").await;
    assert_eq!(dup.status, StatusCode::CREATED, "{}", dup.body);
    assert_eq!(dup.body["possible_duplicates"], json!(["AUTH-1"]));

    let unrelated = create("Write onboarding docs").await;
    assert_eq!(unrelated.status, StatusCode::CREATED, "{}", unrelated.body);
    assert!(unrelated.body.get("possible_duplicates").is_none(), "{}", unrelated.body);

    // A finished task is no longer a candidate.
    let done1 = api.as_human("PATCH", "/v1/tasks/AUTH-1", Some(json!({"status": "done"}))).await;
    assert_eq!(done1.status, StatusCode::OK, "{}", done1.body);
    let done2 = api.as_human("PATCH", "/v1/tasks/AUTH-2", Some(json!({"status": "done"}))).await;
    assert_eq!(done2.status, StatusCode::OK, "{}", done2.body);
    let again = create("Rotate signing keys").await;
    assert_eq!(again.status, StatusCode::CREATED, "{}", again.body);
    assert!(again.body.get("possible_duplicates").is_none(), "{}", again.body);
}

#[tokio::test]
async fn task_list_can_include_the_last_review() {
    let api = Api::new().await;
    api.seed().await;
    for title in ["reviewed", "unreviewed"] {
        api.as_human("POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": title}))).await;
    }
    for (verdict, body) in [("approve", "fine"), ("request_changes", "rebase")] {
        let r = api
            .as_human(
                "POST",
                "/v1/tasks/AUTH-1/comments",
                Some(json!({"kind": "review", "verdict": verdict, "body": body})),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    }
    let r = api.as_human("GET", "/v1/tasks?sort=created_at&fields=key&include=last_review", None).await;
    let items = r.body.as_array().unwrap();
    assert_eq!(items[0]["last_review"]["body"], "rebase");
    assert_eq!(items[0]["last_review"]["verdict"], "request_changes");
    assert!(items[1]["last_review"].is_null());
}

#[tokio::test]
async fn health_reports_a_store_another_connection_has_locked() {
    let api = Api::new().await;
    let db = libsql::Builder::new_local(api._dir.path().join("t.db")).build().await.unwrap();
    let other = db.connect().unwrap();
    other.execute("BEGIN IMMEDIATE", ()).await.unwrap();

    let started = std::time::Instant::now();
    let health = api.call(None, "GET", "/v1/health", None, &[]).await;
    assert!(started.elapsed() < std::time::Duration::from_millis(1500), "{:?}", started.elapsed());
    assert_eq!(health.status, StatusCode::OK);
    assert_eq!(health.body["ok"], false);
    assert!(health.body["store_error"].is_string(), "{}", health.body);

    other.execute("ROLLBACK", ()).await.unwrap();
    let health = api.call(None, "GET", "/v1/health", None, &[]).await;
    assert_eq!(health.body["ok"], true);
}

#[tokio::test]
async fn shell_build_says_whether_a_builder_runs_and_what_it_built() {
    let api = Api::new().await;
    let r = api.as_human("GET", "/v1/shell/build", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["configured"], false);
    assert!(r.body["current"].is_null() && r.body["error"].is_null(), "{}", r.body);
    assert_eq!(api.call(None, "GET", "/v1/shell/build", None, &[]).await.status, StatusCode::UNAUTHORIZED);

    let dir = tempfile::tempdir().unwrap();
    let cfg = common::local_config(dir.path());
    let (store, _) = open_local(&cfg).await.unwrap();
    let state = AppState::new(store);
    let layout = trailhead_builder::Layout::new(dir.path());
    let at = jiff::Timestamp::UNIX_EPOCH;
    let built = |rev: &str| Some(trailhead_builder::Built { rev: rev.into(), at });
    let saved =
        trailhead_builder::State { current: built("b"), previous: built("a"), building: Some("c".into()), error: None };
    layout.save(&saved).unwrap();
    layout.mark_bad("x").unwrap();
    state.shell.set(trailhead_builder::Builds::new(layout, trailhead_builder::System::new())).unwrap();
    let human = std::fs::read_to_string(cfg.creds.file(Cred::User)).unwrap().trim().to_string();
    let api = Api { app: router(Arc::new(state)), human, _dir: dir };
    let r = api.as_human("GET", "/v1/shell/build", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.body,
        json!({"configured": true, "current": "b", "previous": "a", "building": "c", "error": null, "bad": ["x"],
               "changes": [], "changes_total": 0})
    );
    for rev in ["x", "c", "HEAD"] {
        let r = api.as_human("GET", &format!("/v1/shell/source/{rev}"), None).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{rev}: only a build that is kept has its source handed out");
    }
    assert_eq!(api.call(None, "GET", "/v1/shell/source/b", None, &[]).await.status, StatusCode::UNAUTHORIZED);
    for since in ["0123456789abcdef", "HEAD", "--upload-pack=x", "%20"] {
        let r = api.as_human("GET", &format!("/v1/shell/build?since={since}"), None).await;
        assert_eq!(r.status, StatusCode::OK, "{since}");
        assert_eq!((r.body["current"].as_str(), &r.body["changes"]), (Some("b"), &json!([])), "{since}");
    }
}

#[tokio::test]
async fn asking_from_the_window_is_a_persons_and_makes_a_graded_task_or_a_spec_task() {
    let api = Api::new().await;
    let r = api.as_human("GET", "/v1/shell/ask", None).await;
    assert_eq!((r.status, &r.body), (StatusCode::OK, &json!({"configured": false, "repo": null, "targets": []})));
    let r = api.as_human("POST", "/v1/shell/ask", Some(json!({"text": "more", "project": "TICK"}))).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "no apps repo: {}", r.body);

    let dir = tempfile::tempdir().unwrap();
    let cfg = common::local_config(dir.path());
    let (store, _) = open_local(&cfg).await.unwrap();
    let state = AppState::new(store);
    let builds =
        trailhead_builder::Builds::new(trailhead_builder::Layout::new(dir.path()), trailhead_builder::System::new());
    state.shell.set(builds.from_origin("git@github.com:lane/trailhead-apps.git")).unwrap();
    let human = std::fs::read_to_string(cfg.creds.file(Cred::User)).unwrap().trim().to_string();
    let api = Api { app: router(Arc::new(state)), human, _dir: dir };
    let factory = json!({"factory": {"repo": "lane/trailhead-apps", "path": "apps/ticklist"}});
    let r = api
        .as_human("POST", "/v1/projects", Some(json!({"key": "TICK", "name": "Ticklist", "metadata": factory})))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);

    let r = api.as_human("GET", "/v1/shell/ask", None).await;
    assert_eq!(
        r.body,
        json!({"configured": true, "repo": "lane/trailhead-apps",
               "targets": [{"project": "TICK", "name": "Ticklist", "path": "apps/ticklist"}]})
    );

    let agent = api.as_human("POST", "/v1/agents", Some(json!({"name": "claude"}))).await;
    let secret = agent.body["secret"].as_str().unwrap().to_string();
    let r =
        api.call(Some(&secret), "POST", "/v1/shell/ask", Some(json!({"text": "more", "project": "TICK"})), &[]).await;
    assert_eq!((r.status, &r.body["error"]["code"]), (StatusCode::FORBIDDEN, &json!("forbidden")), "{}", r.body);

    for bad in [
        json!({"text": "more"}),
        json!({"text": "more", "project": "TICK", "new_app": {"name": "Notes"}}),
        json!({"text": " ", "project": "TICK"}),
        json!({"project": "TICK"}),
        json!({"text": "more", "new_app": {}}),
    ] {
        let r = api.as_human("POST", "/v1/shell/ask", Some(bad.clone())).await;
        assert_eq!((r.status, &r.body["error"]["code"]), (StatusCode::BAD_REQUEST, &json!("invalid")), "{bad}");
    }

    let r = api.as_human("POST", "/v1/shell/ask", Some(json!({"text": "A Completed view", "project": "TICK"}))).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert_eq!(
        r.body,
        json!({"project": {"key": "TICK", "name": "Ticklist", "created": false},
               "task": {"key": "TICK-1", "title": "A Completed view"}, "planned": false, "notes": []})
    );
    let task = api.as_human("GET", "/v1/tasks/TICK-1", None).await;
    assert_eq!((&task.body["task"]["grade"], &task.body["task"]["priority"]), (&json!(2), &json!(2)), "{}", task.body);

    let r =
        api.as_human("POST", "/v1/shell/ask", Some(json!({"text": "notes", "new_app": {"name": "Trail Notes"}}))).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert_eq!(r.body["project"], json!({"key": "TN", "name": "Trail Notes", "created": true}));
    assert_eq!((&r.body["task"]["key"], &r.body["planned"]), (&json!("TN-1"), &json!(true)));
    let spec = api.as_human("GET", "/v1/tasks/TN-1", None).await;
    assert_eq!(spec.body["task"]["grade"], json!(null), "{}", spec.body);
    assert_eq!(spec.body["task"]["metadata"]["plan"]["spec"]["ready"], json!(false), "{}", spec.body);
    let plan = json!({"text": "Sync with CalDAV", "project": "TICK", "plan": true, "model": "opus"});
    let r = api.as_human("POST", "/v1/shell/ask", Some(plan)).await;
    assert_eq!((r.status, &r.body["planned"]), (StatusCode::CREATED, &json!(true)), "{}", r.body);
    let project = api.as_human("GET", "/v1/projects/TN", None).await;
    assert_eq!(project.body["metadata"]["factory"], json!({"repo": "lane/trailhead-apps", "path": "apps/trail-notes"}));
}

#[tokio::test]
async fn approving_a_plan_over_http_grades_its_tasks() {
    let api = Api::new().await;
    api.as_human("POST", "/v1/projects", Some(json!({"key": "TICK", "name": "Ticklist"}))).await;
    let spec = json!({"project": "TICK", "title": "Plan it", "metadata": {"plan": {"spec": {}}}});
    assert_eq!(api.as_human("POST", "/v1/tasks", Some(spec)).await.status, StatusCode::CREATED);
    for title in ["a", "b"] {
        let child = json!({"project": "TICK", "title": title, "parent": "TICK-1"});
        assert_eq!(api.as_human("POST", "/v1/tasks", Some(child)).await.status, StatusCode::CREATED);
    }
    let partial = api.as_human("POST", "/v1/tasks/TICK-1/plan/approve", Some(json!({"grades": {"TICK-2": 2}}))).await;
    assert_eq!((partial.status, &partial.body["error"]["code"]), (StatusCode::BAD_REQUEST, &json!("invalid")));
    let bad = api.as_human("POST", "/v1/tasks/TICK-1/plan/approve", Some(json!({"grades": {"TICK-2": 9}}))).await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.body);
    let grades = json!({"grades": {"TICK-2": 1, "TICK-3": 3}});
    let r = api.as_human("POST", "/v1/tasks/TICK-1/plan/approve", Some(grades)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert!(r.body["metadata"]["plan"]["spec"]["approved"].is_i64(), "{}", r.body);
    let b = api.as_human("GET", "/v1/tasks/TICK-3", None).await;
    assert_eq!(b.body["task"]["grade"], json!(3));
}

/// A server whose user config is `text`, in a directory of its own, read as what the server started with.
async fn with_config(text: &str) -> (Api, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = common::local_config(dir.path());
    let (store, _) = open_local(&cfg).await.unwrap();
    let path = dir.path().join("config").join("config.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    let state = AppState::new(store);
    state.config.set(trailhead_server::user_config::ConfigFile::open(path.clone())).ok().unwrap();
    let human = std::fs::read_to_string(cfg.creds.file(Cred::User)).unwrap().trim().to_string();
    (Api { app: router(Arc::new(state)), human, _dir: dir }, path)
}

/// A directory with a `.git` in it, as the builder wants `[shell] repo` to be.
fn checkout(dir: &std::path::Path, name: &str) -> String {
    let repo = dir.join(name);
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    repo.display().to_string()
}

#[tokio::test]
async fn the_config_is_a_persons_and_shows_typed_settings_never_the_file() {
    let api = Api::new().await;
    assert_eq!(api.as_human("GET", "/v1/config", None).await.status, StatusCode::NOT_FOUND, "no user config");

    let (api, path) = with_config("url = \"http://127.0.0.1:7241\"\ntoken = \"th_fake_secret_value\"\n").await;
    let agent = api.as_human("POST", "/v1/agents", Some(json!({"name": "claude"}))).await;
    let secret = agent.body["secret"].as_str().unwrap().to_string();
    for (method, body) in [("GET", None), ("PATCH", Some(json!({"version": "x", "shell": {"jobs": 2}})))] {
        let r = api.call(Some(&secret), method, "/v1/config", body, &[]).await;
        assert_eq!((r.status, &r.body["error"]["code"]), (StatusCode::FORBIDDEN, &json!("forbidden")), "{method}");
    }
    let r = api.as_human("GET", "/v1/config", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let text = r.body.to_string();
    assert!(!text.contains("th_fake_secret_value") && !text.contains("7241"), "only typed settings: {text}");
    assert!(r.body["problem"].as_str().unwrap().contains("unknown field `token`"), "{}", r.body);
    assert_eq!(r.body["path"], path.display().to_string());
    assert_eq!(
        r.body["shell"],
        json!({"repo": null, "package": null, "profile": "release", "jobs": 3, "poll_secs": 60,
               "defaults": ["repo", "package", "profile", "jobs", "poll_secs"]})
    );
    assert_eq!((&r.body["restart_needed"], &r.body["waiting"]), (&json!(false), &json!([])));
}

#[tokio::test]
async fn a_change_keeps_the_rest_of_the_file_and_its_backup_and_waits_for_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let repo = checkout(dir.path(), "apps");
    let before = format!(
        "# this machine\nurl = \"http://127.0.0.1:7241\" # the usual port\n\n[shell]\n# the apps\nrepo = {repo:?} # mine\njobs = 2 # leave room\n"
    );
    let (api, path) = with_config(&before).await;
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o640)).unwrap();
    let read = api.as_human("GET", "/v1/config", None).await;
    assert_eq!(read.body["problem"], json!(null), "{}", read.body);
    assert_eq!(read.body["shell"]["defaults"], json!(["package", "profile", "poll_secs"]));
    let version = read.body["version"].clone();

    let change = json!({"version": version, "shell": {"jobs": 4, "poll_secs": 30}});
    let r = api.as_human("PATCH", "/v1/config", Some(change)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after, before.replace("jobs = 2 # leave room\n", "jobs = 4 # leave room\npoll_secs = 30\n"));
    let backup = path.with_file_name("config.toml.bak");
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), before);
    for kept in [&path, &backup] {
        let mode = std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(kept).unwrap().permissions());
        assert_eq!(mode & 0o777, 0o640, "{}", kept.display());
    }
    assert_eq!((&r.body["shell"]["jobs"], &r.body["restart_needed"]), (&json!(4), &json!(true)));
    assert_eq!(r.body["waiting"], json!(["shell.jobs", "shell.poll_secs"]));
    assert_eq!(r.body["running"]["shell"]["jobs"], 2, "what the server started with");
    assert!(r.body["running"].get("factory").is_none());
    assert_ne!(r.body["version"], version);

    let back = json!({"version": r.body["version"], "shell": {"jobs": 2, "poll_secs": null}});
    let r = api.as_human("PATCH", "/v1/config", Some(back)).await;
    assert_eq!((r.status, &r.body["restart_needed"]), (StatusCode::OK, &json!(false)), "{}", r.body);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "back as it was, byte for byte");
}

#[tokio::test]
async fn a_change_the_server_would_refuse_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let repo = checkout(dir.path(), "apps");
    let before = format!("[shell]\nrepo = {repo:?}\n");
    let (api, path) = with_config(&before).await;
    let version = api.as_human("GET", "/v1/config", None).await.body["version"].clone();
    let missing = dir.path().join("no-such-repo").display().to_string();
    let plain = dir.path().display().to_string();
    let mut bad = vec![
        (json!({"shell": {"jobs": 0}}), "[shell] jobs must be at least 1".to_string()),
        (json!({"shell": {"repo": missing}}), format!("[shell] repo {missing} isn't a directory")),
        (json!({"shell": {"repo": plain}}), format!("[shell] repo {plain} isn't a git checkout")),
    ];
    if cfg!(feature = "factory") {
        bad.push((json!({"factory": {"repos": [missing]}}), "no-such-repo/.trailhead.toml".into()));
        bad.push((json!({"factory": {"pause_at": {"five_hour": 101}}}), "it's a percent, 0 to 100".into()));
        bad.push((json!({"factory": {"pause_at": {"five_hours": 80}}}), "unknown window `five_hours`".into()));
    }
    for (change, words) in bad {
        let mut change = change;
        change["version"] = version.clone();
        let r = api.as_human("PATCH", "/v1/config", Some(change.clone())).await;
        assert_eq!((r.status, &r.body["error"]["code"]), (StatusCode::BAD_REQUEST, &json!("invalid")), "{change}");
        let message = r.body["error"]["message"].as_str().unwrap();
        assert!(message.contains(&words), "{change}: {message}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "{change}");
        assert!(!path.with_file_name("config.toml.bak").exists(), "{change}");
    }
}

#[tokio::test]
async fn a_file_changed_on_disk_since_it_was_read_is_not_overwritten() {
    let (api, path) = with_config("[shell]\njobs = 2\n").await;
    let version = api.as_human("GET", "/v1/config", None).await.body["version"].clone();
    std::fs::write(&path, "[shell]\njobs = 5 # by hand\n").unwrap();
    let r = api.as_human("PATCH", "/v1/config", Some(json!({"version": version, "shell": {"jobs": 3}}))).await;
    assert_eq!((r.status, &r.body["error"]["code"]), (StatusCode::PRECONDITION_FAILED, &json!("version_conflict")));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "[shell]\njobs = 5 # by hand\n");
    let r = api.as_human("GET", "/v1/config", None).await;
    assert_eq!((&r.body["shell"]["jobs"], &r.body["waiting"]), (&json!(5), &json!(["shell.jobs"])));
}

#[tokio::test]
async fn a_config_that_is_a_link_stays_one() {
    let (api, path) = with_config("").await;
    let target = path.with_file_name("dotfiles.toml");
    std::fs::write(&target, "url = \"http://127.0.0.1:7241\"\n").unwrap();
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    let version = api.as_human("GET", "/v1/config", None).await.body["version"].clone();
    let r = api.as_human("PATCH", "/v1/config", Some(json!({"version": version, "shell": {"jobs": 2}}))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert!(std::fs::symlink_metadata(&path).unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "url = \"http://127.0.0.1:7241\"\n\n[shell]\njobs = 2\n");
}

#[cfg(feature = "factory")]
#[tokio::test]
async fn the_factory_can_be_turned_off_and_its_repos_listed_while_it_is() {
    let (api, path) = with_config("[factory] # the machine's factory\nmax_sandboxes = 2\n").await;
    let read = api.as_human("GET", "/v1/config", None).await;
    assert_eq!(read.body["factory"]["defaults"], json!(["repos", "disabled", "pause_at"]), "{}", read.body);
    let off = json!({"version": read.body["version"], "factory": {"disabled": true, "pause_at": {"five_hour": 80}}});
    let r = api.as_human("PATCH", "/v1/config", Some(off)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let missing = "/no/such/repo";
    let repos = json!({"version": r.body["version"], "factory": {"repos": [missing]}});
    let r = api.as_human("PATCH", "/v1/config", Some(repos)).await;
    assert_eq!(r.status, StatusCode::OK, "a disabled factory reads no repo, at start or here: {}", r.body);
    assert_eq!(r.body["waiting"], json!(["factory.repos", "factory.disabled", "factory.pause_at"]));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[factory] # the machine's factory\nmax_sandboxes = 2\ndisabled = true\npause_at = { five_hour = 80 }\nrepos = [\"/no/such/repo\"]\n"
    );
    let on = json!({"version": r.body["version"], "factory": {"disabled": false}});
    let r = api.as_human("PATCH", "/v1/config", Some(on)).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "turning it on reads the repo: {}", r.body);
}

#[cfg(not(feature = "factory"))]
#[tokio::test]
async fn without_the_factory_its_table_is_neither_shown_nor_changed() {
    let (api, path) = with_config("[factory]\nrepos = []\n").await;
    let read = api.as_human("GET", "/v1/config", None).await;
    assert_eq!((&read.body["factory"], &read.body["factory_built"]), (&json!(null), &json!(false)));
    let change = json!({"version": read.body["version"], "factory": {"disabled": true}});
    let r = api.as_human("PATCH", "/v1/config", Some(change)).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "[factory]\nrepos = []\n");
}

#[tokio::test]
async fn notifications_are_raised_listed_read_and_resolved_over_http() {
    let api = Api::new().await;
    api.seed().await;
    api.as_human("POST", "/v1/tasks", Some(json!({"project": "AUTH", "title": "Pick a colour"}))).await;
    let agent = api.as_human("POST", "/v1/agents", Some(json!({"name": "worker"}))).await;
    let agent = agent.body["secret"].as_str().unwrap().to_string();

    // An agent's question tells the person, by the engine's rule.
    let q = json!({"kind": "question", "body": "Red or blue?"});
    let q = api.call(Some(&agent), "POST", "/v1/tasks/AUTH-1/comments", Some(q), &[]).await;
    assert_eq!(q.status, StatusCode::CREATED, "{}", q.body);

    // An app raises its own through the window, as the person.
    let raise = json!({
        "source": "ticklist", "kind": "ticklist.streak", "title": "A streak", "body": "Seven days.",
        "subject": "streak", "actions": [{"label": "See it", "url": "https://example.com/streak"}],
    });
    let mine = api.as_human("POST", "/v1/notifications", Some(raise)).await;
    assert_eq!(mine.status, StatusCode::CREATED, "{}", mine.body);
    let id = mine.body["id"].as_str().unwrap().to_string();
    let bad = json!({"source": "ticklist", "kind": "k", "title": "t", "actions": [{"label": "x", "url": "file:///x"}]});
    let bad = api.as_human("POST", "/v1/notifications", Some(bad)).await;
    assert_eq!((bad.status, bad.body["error"]["code"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid")));

    let list = api.as_human("GET", "/v1/notifications", None).await;
    assert_eq!(list.body["unread"], 2, "{}", list.body);
    let got = list.body["notifications"].as_array().unwrap();
    assert_eq!((got[0]["title"].as_str(), got[0]["actions"][0]["label"].as_str()), (Some("A streak"), Some("See it")));
    assert_eq!((got[1]["kind"].as_str(), got[1]["source"].as_str()), (Some("task.question"), Some("server")));
    assert_eq!((got[1]["task_key"].as_str(), got[1]["body"].as_str()), (Some("AUTH-1"), Some("Red or blue?")));
    let page = api.as_human("GET", &format!("/v1/notifications?before={id}&limit=5"), None).await;
    assert_eq!(page.body["notifications"].as_array().unwrap().len(), 1);

    // Only the person says what they have seen.
    let r = api.call(Some(&agent), "POST", &format!("/v1/notifications/{id}/read"), None, &[]).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.body);
    let r = api.as_human("POST", &format!("/v1/notifications/{id}/read"), None).await;
    assert!(r.body["read_at"].is_string(), "{}", r.body);
    let list = api.as_human("GET", "/v1/notifications?unread=true", None).await;
    assert_eq!((list.body["unread"].as_i64(), list.body["notifications"].as_array().unwrap().len()), (Some(1), 1));
    let r = api.as_human("POST", &format!("/v1/notifications/{id}/unread"), None).await;
    assert!(r.body["read_at"].is_null(), "{}", r.body);

    // The raiser resolves by subject; the engine resolves its own when the task is finished.
    let about = json!({"source": "ticklist", "subject": "streak"});
    let r = api.as_human("POST", "/v1/notifications/resolve", Some(about)).await;
    assert_eq!(r.body["count"], 1, "{}", r.body);
    let r = api.as_human("POST", &format!("/v1/notifications/{id}/resolve"), None).await;
    assert!(r.body["resolved_at"].is_string(), "{}", r.body);
    api.as_human("PATCH", "/v1/tasks/AUTH-1", Some(json!({"status": "cancelled"}))).await;
    assert_eq!(api.as_human("GET", "/v1/notifications", None).await.body["unread"], 0);

    let again =
        api.as_human("POST", "/v1/notifications", Some(json!({"source": "s", "kind": "k", "title": "t"}))).await;
    assert_eq!(again.status, StatusCode::CREATED);
    assert_eq!(api.as_human("POST", "/v1/notifications/read-all", None).await.body["count"], 1);
    let missing = api.as_human("POST", &format!("/v1/notifications/{}/read", uuid::Uuid::now_v7()), None).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_event_stream_carries_a_notifications_life() {
    let api = Api::new().await;
    api.seed().await;
    let res = api
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/events/stream")
                .header(header::AUTHORIZATION, format!("Bearer {}", api.human))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let reader = tokio::spawn(read_sse(res.into_body(), |s| s.contains("event: notification.read")));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let n =
        api.as_human("POST", "/v1/notifications", Some(json!({"source": "s", "kind": "k", "title": "Hello"}))).await;
    api.as_human("POST", &format!("/v1/notifications/{}/read", n.body["id"].as_str().unwrap()), None).await;
    let got = reader.await.unwrap();
    assert!(got.contains("event: notification.raised"), "{got}");
    assert!(got.contains("\"entity\":\"notification\"") && got.contains("\"title\":\"Hello\""), "{got}");
}
