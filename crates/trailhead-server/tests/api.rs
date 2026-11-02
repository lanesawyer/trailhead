mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use trailhead_server::creds::Cred;
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
    assert!(r.body["task"]["grade"].is_null(), "{}", r.body);
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
