mod common;

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use trailhead_client::creds::Cred;
use trailhead_core::{Ctx, Store};
use trailhead_server::{AppState, declarations, open_local, router};

const SHAPE: &str = r#"
[collections.snoozes]
fields = { item = "string", until = "timestamp", note = "string?" }
"#;

/// `SHAPE` and a collection filled from a feed, in the JSON form, which carries the feed's fields.
fn with_feed() -> Value {
    json!({"collections": {
        "snoozes": {"fields": {"item": "string", "until": "timestamp", "note": "string?"}},
        "pull_requests": {"from": "github.pull_requests", "fields": {"title": "string"}},
    }})
}

struct Api {
    app: Router,
    human: String,
    store: Arc<Store>,
    ctx: Ctx,
    _dir: tempfile::TempDir,
}

struct Res {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Value,
}

impl Api {
    async fn new() -> Api {
        Api::with(|_, _| {}).await
    }

    /// A server whose state `setup` may change first, given the data directory.
    async fn with(setup: impl FnOnce(&mut AppState, &Path)) -> Api {
        let dir = tempfile::tempdir().unwrap();
        let cfg = common::local_config(dir.path());
        let (store, ctx) = open_local(&cfg).await.unwrap();
        let human = std::fs::read_to_string(cfg.creds.file(Cred::User)).unwrap().trim().to_string();
        let mut state = AppState::new(store);
        setup(&mut state, dir.path());
        let store = state.store.clone();
        Api { app: router(Arc::new(state)), human, store, ctx, _dir: dir }
    }

    async fn send(&self, token: &str, req: axum::http::request::Builder, body: Body) -> Res {
        let req = req.header(header::AUTHORIZATION, format!("Bearer {token}")).body(body).unwrap();
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let body = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
        Res { status, headers, body }
    }

    async fn call(&self, token: &str, method: &str, path: &str, body: Option<Value>, extra: &[(&str, &str)]) -> Res {
        let mut req = Request::builder().method(method).uri(path);
        for (k, v) in extra {
            req = req.header(*k, *v);
        }
        match body {
            Some(b) => {
                let req = req.header(header::CONTENT_TYPE, "application/json");
                self.send(token, req, Body::from(b.to_string())).await
            }
            None => self.send(token, req, Body::empty()).await,
        }
    }

    async fn human(&self, method: &str, path: &str, body: Option<Value>) -> Res {
        self.call(&self.human.clone(), method, path, body, &[]).await
    }

    async fn declare_toml(&self, token: &str, app: &str, text: &str) -> Res {
        let req = Request::builder()
            .method("PUT")
            .uri(format!("/v1/apps/{app}/declaration"))
            .header(header::CONTENT_TYPE, "application/toml");
        self.send(token, req, Body::from(text.to_string())).await
    }

    /// Declares `cairn` with a fed collection, and `notes`.
    async fn seed(&self) {
        let r = self.human("PUT", "/v1/apps/cairn/declaration", Some(with_feed())).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        let r = self.declare_toml(&self.human, "notes", "[collections.notes]\nfields = { text = \"string\" }").await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    }

    /// An agent whose token reaches `apps`, and its id.
    async fn agent(&self, name: &str, apps: &[&str]) -> (String, String) {
        let r = self.human("POST", "/v1/agents", Some(json!({"name": name, "apps": apps}))).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
        (r.body["secret"].as_str().unwrap().into(), r.body["agent"]["id"].as_str().unwrap().into())
    }

    async fn snooze(&self, token: &str, item: &str) -> Value {
        let body = json!({"data": {"item": item, "until": "2026-11-23T09:00:00Z"}});
        let r = self.call(token, "POST", "/v1/apps/cairn/records/snoozes", Some(body), &[]).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
        r.body
    }
}

fn error(r: &Res, status: StatusCode, code: &str, says: &str) {
    assert_eq!(r.status, status, "{}", r.body);
    assert_eq!(r.body["error"]["code"], code, "{}", r.body);
    let msg = r.body["error"]["message"].as_str().unwrap();
    assert!(msg.contains(says), "{msg:?} doesn't say {says:?}");
}

#[tokio::test]
async fn an_app_is_declared_and_its_collections_read_back() {
    let api = Api::new().await;
    let r = api.declare_toml(&api.human, "cairn", SHAPE).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["id"], "cairn");
    assert_eq!(r.body["collections"]["snoozes"]["fields"]["note"], "string?");

    let r = api.human("PUT", "/v1/apps/cairn/declaration", Some(with_feed())).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let r = api.human("GET", "/v1/apps/cairn", None).await;
    assert_eq!(
        r.body["collections"]["pull_requests"],
        json!({"fields": {"title": "string"}, "from": "github.pull_requests"})
    );
    let r = api.human("GET", "/v1/apps", None).await;
    assert_eq!(r.body.as_array().unwrap().iter().map(|a| &a["id"]).collect::<Vec<_>>(), [&json!("cairn")]);

    error(
        &api.declare_toml(&api.human, "cairn", "[collections.snoozes]\nfields = { item = \"time\" }").await,
        StatusCode::BAD_REQUEST,
        "invalid",
        "field `item`: `time` isn't a field type",
    );
    error(
        &api.declare_toml(&api.human, "cairn", "[collections.pull_requests]\nfrom = \"github.pull_requests\"").await,
        StatusCode::BAD_REQUEST,
        "invalid",
        "no built apps repo to read the feed's fields from",
    );
    error(&api.declare_toml(&api.human, "Cairn", SHAPE).await, StatusCode::BAD_REQUEST, "invalid", "isn't an app's id");
    let (bot, _) = api.agent("bot", &["cairn"]).await;
    error(&api.declare_toml(&bot, "cairn", SHAPE).await, StatusCode::FORBIDDEN, "forbidden", "only a person");

    // Once a collection holds records, its shape changes only by addition.
    api.snooze(&api.human, "a").await;
    let narrower = "[collections.snoozes]\nfields = { item = \"string\", until = \"timestamp\" }";
    error(
        &api.declare_toml(&api.human, "cairn", narrower).await,
        StatusCode::BAD_REQUEST,
        "invalid",
        "`snoozes.note` is gone",
    );
    let mut wider_json = with_feed();
    wider_json["collections"]["later"] = json!({"fields": {"at": "day"}});
    assert_eq!(api.human("PUT", "/v1/apps/cairn/declaration", Some(wider_json)).await.status, StatusCode::OK);
    assert_eq!(api.human("GET", "/v1/apps/nope", None).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn records_are_created_read_changed_and_removed_with_their_versions() {
    let api = Api::new().await;
    api.seed().await;
    let created = api.snooze(&api.human, "a").await;
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!((created["version"].as_i64(), created["key"].is_null()), (Some(1), true));
    let at = format!("/v1/apps/cairn/records/snoozes/{id}");

    let r = api.human("GET", &at, None).await;
    assert_eq!((r.status, r.headers[header::ETAG].to_str().unwrap()), (StatusCode::OK, "\"1\""));
    assert_eq!(r.body, created);
    error(
        &api.human("GET", &format!("/v1/apps/cairn/records/notes/{id}"), None).await,
        StatusCode::NOT_FOUND,
        "not_found",
        "in notes",
    );
    error(&api.human("GET", "/v1/apps/cairn/records/snoozes/nope", None).await, StatusCode::BAD_REQUEST, "invalid", "");

    let post = |data: Value| api.human("POST", "/v1/apps/cairn/records/snoozes", Some(json!({"data": data})));
    error(&post(json!({"item": "x"})).await, StatusCode::BAD_REQUEST, "invalid", "field `until` is missing");
    error(
        &post(json!({"item": "x", "until": "2026-11-23T09:00:00Z", "what": 1})).await,
        StatusCode::BAD_REQUEST,
        "invalid",
        "unknown field `what`",
    );
    error(
        &post(json!({"item": 1, "until": "2026-11-23T09:00:00Z"})).await,
        StatusCode::BAD_REQUEST,
        "invalid",
        "field `item` should be a string, not 1",
    );
    error(
        &api.human("POST", "/v1/apps/cairn/records/nope", Some(json!({"data": {}}))).await,
        StatusCode::NOT_FOUND,
        "not_found",
        "collection nope",
    );

    let patch = async |version: &str, body: Value| {
        api.call(&api.human, "PATCH", &at, Some(body), &[("if-match", version)]).await
    };
    let r = patch("\"7\"", json!({"note": "later"})).await;
    error(&r, StatusCode::PRECONDITION_FAILED, "version_conflict", "");
    assert_eq!(r.body["error"]["current_version"], 1);
    let r = patch("\"1\"", json!({"note": "later"})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(
        (&r.body["outcome"], &r.body["record"]["version"], &r.body["record"]["data"]["note"]),
        (&json!("changed"), &json!(2), &json!("later"))
    );
    assert_eq!(r.headers[header::ETAG], "\"2\"");
    let r = patch("\"2\"", json!({"note": "later"})).await;
    assert_eq!((&r.body["outcome"], &r.body["record"]["version"]), (&json!("unchanged"), &json!(2)));
    let r = patch("*", json!({"note": null})).await;
    assert!(r.body["record"]["data"].get("note").is_none(), "{}", r.body);
    error(&patch("\"3\"", json!({"item": null})).await, StatusCode::BAD_REQUEST, "invalid", "field `item` is missing");

    let r = api.human("PUT", &at, Some(json!({"item": "b", "until": "2026-11-24T09:00:00Z"}))).await;
    assert_eq!(
        (&r.body["outcome"], &r.body["record"]["data"]["item"], &r.body["record"]["version"]),
        (&json!("changed"), &json!("b"), &json!(4))
    );

    error(
        &api.call(&api.human, "DELETE", &at, None, &[("if-match", "\"1\"")]).await,
        StatusCode::PRECONDITION_FAILED,
        "version_conflict",
        "",
    );
    let r = api.call(&api.human, "DELETE", &at, None, &[("if-match", "\"4\"")]).await;
    assert_eq!((r.status, &r.body["data"]["item"]), (StatusCode::OK, &json!("b")));
    assert_eq!(api.human("GET", &at, None).await.status, StatusCode::NOT_FOUND);

    let keyed = api
        .human(
            "POST",
            "/v1/apps/cairn/records/snoozes",
            Some(json!({"key": "k", "data": {"item": "k", "until": "2026-11-23T09:00:00Z"}})),
        )
        .await;
    assert_eq!((keyed.status, &keyed.body["key"]), (StatusCode::CREATED, &json!("k")));
    let again = api
        .human(
            "POST",
            "/v1/apps/cairn/records/snoozes",
            Some(json!({"key": "k", "data": {"item": "k", "until": "2026-11-23T09:00:00Z"}})),
        )
        .await;
    error(&again, StatusCode::CONFLICT, "already_exists", "key \"k\"");
}

#[tokio::test]
async fn records_page_in_the_order_they_were_made() {
    let api = Api::new().await;
    api.seed().await;
    let mut ids = Vec::new();
    for i in 0..5 {
        ids.push(api.snooze(&api.human, &i.to_string()).await["id"].as_str().unwrap().to_string());
    }
    let page = async |q: String| api.human("GET", &format!("/v1/apps/cairn/records/snoozes{q}"), None).await;
    let r = page("?limit=2".into()).await;
    let got: Vec<_> = r.body.as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap().to_string()).collect();
    assert_eq!(got, ids[..2]);
    assert_eq!(r.headers["trailhead-event-cursor"], "5");
    let r = page(format!("?after={}&limit=10", ids[1])).await;
    assert_eq!(r.body.as_array().unwrap().len(), 3);
    assert_eq!(page("?limit=nope".into()).await.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn writing_by_key_twice_leaves_one_record_and_one_event() {
    let api = Api::new().await;
    api.seed().await;
    // A key with `/` and `#` is one percent-encoded segment.
    let at = "/v1/apps/cairn/keys/snoozes/lanesawyer%2Ftrailhead%23123";
    let data = json!({"item": "review", "until": "2026-11-23T09:00:00Z"});
    let r = api.human("PUT", at, Some(data.clone())).await;
    assert_eq!((r.status, &r.body["outcome"]), (StatusCode::CREATED, &json!("created")), "{}", r.body);
    assert_eq!(r.body["record"]["key"], "lanesawyer/trailhead#123");
    let r = api.human("PUT", at, Some(data.clone())).await;
    assert_eq!(
        (r.status, &r.body["outcome"], &r.body["record"]["version"]),
        (StatusCode::OK, &json!("unchanged"), &json!(1))
    );
    let events = api.human("GET", "/v1/apps/cairn/events", None).await;
    assert_eq!(events.body.as_array().unwrap().len(), 1, "an unchanged write records nothing");

    let r = api.human("PUT", at, Some(json!({"item": "review", "until": "2026-11-24T09:00:00Z"}))).await;
    assert_eq!((&r.body["outcome"], &r.body["record"]["version"]), (&json!("changed"), &json!(2)));
    let r = api.human("GET", at, None).await;
    assert_eq!((r.status, &r.body["version"]), (StatusCode::OK, &json!(2)));
    let all = api.human("GET", "/v1/apps/cairn/records/snoozes", None).await;
    assert_eq!(all.body.as_array().unwrap().len(), 1);
    error(
        &api.human("PUT", "/v1/apps/cairn/keys/snoozes/k", Some(json!({"item": 1}))).await,
        StatusCode::BAD_REQUEST,
        "invalid",
        "field `item`",
    );
    assert_eq!(api.human("GET", "/v1/apps/cairn/keys/snoozes/nope", None).await.status, StatusCode::NOT_FOUND);

    // An idempotency key replays a create instead of making a second record.
    let body = json!({"data": {"item": "once", "until": "2026-11-23T09:00:00Z"}});
    let path = "/v1/apps/cairn/records/snoozes";
    let first = api.call(&api.human, "POST", path, Some(body.clone()), &[("idempotency-key", "k1")]).await;
    let again = api.call(&api.human, "POST", path, Some(body), &[("idempotency-key", "k1")]).await;
    assert_eq!((again.headers["idempotent-replayed"].to_str().unwrap(), &again.body), ("true", &first.body));
    assert_eq!(api.human("GET", path, None).await.body.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn a_batch_is_all_or_nothing_and_undone_as_one() {
    let api = Api::new().await;
    api.seed().await;
    let a = api.snooze(&api.human, "a").await;
    let good = json!({"op": "create", "collection": "snoozes", "data": {"item": "b", "until": "2026-11-23T09:00:00Z"}});
    let bad = json!({"op": "create", "collection": "snoozes", "data": {"item": "c"}});
    let r = api.human("POST", "/v1/apps/cairn/batch", Some(json!({"writes": [good, bad]}))).await;
    error(&r, StatusCode::BAD_REQUEST, "invalid", "field `until` is missing");
    assert_eq!(api.human("GET", "/v1/apps/cairn/records/snoozes", None).await.body.as_array().unwrap().len(), 1);
    error(
        &api.human("POST", "/v1/apps/cairn/batch", Some(json!({"writes": []}))).await,
        StatusCode::BAD_REQUEST,
        "invalid",
        "1 to 1000 writes",
    );
    error(
        &api.human("POST", "/v1/apps/cairn/batch", Some(json!({"writes": [{"op": "sideways"}]}))).await,
        StatusCode::BAD_REQUEST,
        "invalid",
        "sideways",
    );

    let writes = json!({"writes": [
        good,
        {"op": "put", "collection": "snoozes", "key": "k", "data": {"item": "k", "until": "2026-11-23T09:00:00Z"}},
        {"op": "patch", "id": a["id"], "data": {"note": "n"}, "version": 1},
        {"op": "delete", "id": a["id"]},
    ]});
    let r = api.human("POST", "/v1/apps/cairn/batch", Some(writes)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let outcomes: Vec<_> =
        r.body["results"].as_array().unwrap().iter().map(|w| w["outcome"].as_str().unwrap()).collect();
    assert_eq!(outcomes, ["created", "created", "changed", "deleted"]);
    let batch = r.body["batch_id"].clone();
    assert_eq!(r.headers["trailhead-batch"].to_str().unwrap(), batch.as_str().unwrap());
    assert_eq!(api.human("GET", "/v1/apps/cairn/records/snoozes", None).await.body.as_array().unwrap().len(), 2);

    let r = api.human("POST", "/v1/apps/cairn/undo", Some(json!({"batch_id": batch}))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["reverted"].as_array().unwrap().len(), 3);
    let left = api.human("GET", "/v1/apps/cairn/records/snoozes", None).await;
    assert_eq!(left.body.as_array().unwrap().iter().map(|r| &r["id"]).collect::<Vec<_>>(), [&a["id"]]);
}

#[tokio::test]
async fn an_event_is_undone_unless_changed_since() {
    let api = Api::new().await;
    api.seed().await;
    let a = api.snooze(&api.human, "a").await;
    let at = format!("/v1/apps/cairn/records/snoozes/{}", a["id"].as_str().unwrap());
    api.human("PATCH", &at, Some(json!({"note": "one"}))).await;
    api.human("PATCH", &at, Some(json!({"note": "two"}))).await;
    error(
        &api.human("POST", "/v1/apps/cairn/undo", Some(json!({"event_id": 2}))).await,
        StatusCode::CONFLICT,
        "undo_conflict",
        "",
    );
    let r = api.human("POST", "/v1/apps/cairn/undo", Some(json!({"event_id": 3}))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(api.human("GET", &at, None).await.body["data"]["note"], "one");
    error(
        &api.human("POST", "/v1/apps/cairn/undo", Some(json!({}))).await,
        StatusCode::BAD_REQUEST,
        "invalid",
        "exactly one",
    );
    error(
        &api.human("POST", "/v1/apps/cairn/undo", Some(json!({"event_id": 99}))).await,
        StatusCode::NOT_FOUND,
        "not_found",
        "event 99",
    );
    let events = api.human("GET", "/v1/apps/cairn/events?after=3&collection=snoozes", None).await;
    let kinds: Vec<_> = events.body.as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["record.reverted"]);
}

#[tokio::test]
async fn a_fed_collection_is_written_by_nothing_here() {
    let api = Api::new().await;
    api.seed().await;
    let data = json!({"title": "x"});
    let forbidden = |r: Res| error(&r, StatusCode::FORBIDDEN, "forbidden", "filled from feed github.pull_requests");
    forbidden(api.human("POST", "/v1/apps/cairn/records/pull_requests", Some(json!({"data": data}))).await);
    forbidden(api.human("PUT", "/v1/apps/cairn/keys/pull_requests/1", Some(data.clone())).await);
    let batch = json!({"writes": [{"op": "put", "collection": "pull_requests", "key": "1", "data": data}]});
    forbidden(api.human("POST", "/v1/apps/cairn/batch", Some(batch)).await);
    let r = api.human("GET", "/v1/apps/cairn/records/pull_requests", None).await;
    assert_eq!((r.status, r.body.as_array().unwrap().len()), (StatusCode::OK, 0));
}

#[tokio::test]
async fn a_token_reaches_only_the_apps_it_names() {
    let api = Api::new().await;
    api.seed().await;
    let record = api.snooze(&api.human, "a").await;
    let (none, _) = api.agent("none", &[]).await;
    let id = record["id"].as_str().unwrap();
    for (method, path, body) in [
        ("GET", "/v1/apps".to_string(), None),
        ("GET", "/v1/apps/cairn".into(), None),
        ("GET", "/v1/apps/cairn/records/snoozes".into(), None),
        ("POST", "/v1/apps/cairn/records/snoozes".into(), Some(json!({"data": {}}))),
        ("GET", format!("/v1/apps/cairn/records/snoozes/{id}"), None),
        ("PATCH", format!("/v1/apps/cairn/records/snoozes/{id}"), Some(json!({}))),
        ("PUT", format!("/v1/apps/cairn/records/snoozes/{id}"), Some(json!({}))),
        ("DELETE", format!("/v1/apps/cairn/records/snoozes/{id}"), None),
        ("GET", "/v1/apps/cairn/keys/snoozes/k".into(), None),
        ("PUT", "/v1/apps/cairn/keys/snoozes/k".into(), Some(json!({}))),
        ("POST", "/v1/apps/cairn/batch".into(), Some(json!({"writes": [{"op": "delete", "id": id}]}))),
        ("GET", "/v1/apps/cairn/events".into(), None),
        ("GET", "/v1/apps/cairn/events/stream".into(), None),
        ("POST", "/v1/apps/cairn/undo".into(), Some(json!({"event_id": 1}))),
        ("PUT", "/v1/apps/cairn/declaration".into(), Some(with_feed())),
    ] {
        let r = api.call(&none, method, &path, body, &[]).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{method} {path}: {}", r.body);
    }

    let (bot, _) = api.agent("bot", &["cairn"]).await;
    let r = api.call(&bot, "GET", "/v1/apps", None, &[]).await;
    assert_eq!(r.body.as_array().unwrap().iter().map(|a| &a["id"]).collect::<Vec<_>>(), [&json!("cairn")]);
    let r = api.call(&bot, "GET", "/v1/apps/notes/records/notes", None, &[]).await;
    error(&r, StatusCode::FORBIDDEN, "forbidden", "can't reach app notes");
    api.snooze(&bot, "mine").await;
    let r = api.human("GET", "/v1/apps", None).await;
    assert_eq!(r.body.as_array().unwrap().len(), 2, "a person reaches every app");

    // A token's apps are set and changed like its projects, never wider than the issuer's.
    let r = api.human("GET", "/v1/tokens", None).await;
    let bots: Vec<_> = r.body.as_array().unwrap().iter().filter(|t| t["name"] == "bot").collect();
    assert_eq!(bots[0]["apps"], json!(["cairn"]));
    let token = bots[0]["id"].as_str().unwrap().to_string();
    let r = api.human("PATCH", &format!("/v1/tokens/{token}"), Some(json!({"apps": ["notes"]}))).await;
    assert_eq!((r.status, &r.body["apps"]), (StatusCode::OK, &json!(["notes"])), "{}", r.body);
    assert_eq!(api.call(&bot, "GET", "/v1/apps/cairn", None, &[]).await.status, StatusCode::FORBIDDEN);
    let r = api.human("PATCH", &format!("/v1/tokens/{token}"), Some(json!({"apps": null}))).await;
    assert!(r.body["apps"].is_null(), "{}", r.body);
    assert_eq!(api.call(&bot, "GET", "/v1/apps", None, &[]).await.status, StatusCode::FORBIDDEN);
    let r = api.human("POST", "/v1/tokens", Some(json!({"name": "t", "agent": "bot", "apps": ["notes"]}))).await;
    assert_eq!((r.status, &r.body["token"]["apps"]), (StatusCode::CREATED, &json!(["notes"])), "{}", r.body);
    let r = api.human("POST", "/v1/tokens", Some(json!({"name": "t", "agent": "bot", "apps": ["Not An App"]}))).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);
}

#[tokio::test]
async fn reverting_an_agent_reverts_its_app_writes_per_app() {
    let api = Api::new().await;
    api.seed().await;
    let (bot, bot_id) = api.agent("bot", &["cairn", "notes"]).await;
    let since = (jiff::Timestamp::now() - jiff::SignedDuration::from_mins(1)).to_string();
    let mine = api.snooze(&bot, "mine").await;
    let r = api.call(&bot, "POST", "/v1/apps/notes/records/notes", Some(json!({"data": {"text": "hi"}})), &[]).await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = api.human("POST", "/v1/revert", Some(json!({"actor_id": bot_id, "since": since}))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert!(r.body["reverted"].is_array(), "the task report's shape stays: {}", r.body);
    assert_eq!(r.body["apps"]["cairn"]["reverted"][0]["record_id"], mine["id"]);
    assert_eq!(r.body["apps"]["notes"]["reverted"].as_array().unwrap().len(), 1);
    assert!(r.body.get("apps_failed").is_none());
    let at = format!("/v1/apps/cairn/records/snoozes/{}", mine["id"].as_str().unwrap());
    assert_eq!(api.human("GET", &at, None).await.status, StatusCode::NOT_FOUND);

    // Nothing to revert in apps leaves `apps` out.
    let (_, other_id) = api.agent("other", &[]).await;
    let r = api.human("POST", "/v1/revert", Some(json!({"actor_id": other_id, "since": since}))).await;
    assert!(r.body.get("apps").is_none(), "{}", r.body);
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

fn ids(sse: &str) -> Vec<i64> {
    sse.lines().filter_map(|l| l.strip_prefix("id: ")).map(|i| i.parse().unwrap()).collect()
}

#[tokio::test]
async fn a_snapshot_then_the_stream_misses_and_repeats_nothing() {
    let api = Api::new().await;
    api.seed().await;
    api.snooze(&api.human, "a").await;
    api.snooze(&api.human, "b").await;
    let stream = |query: &str, extra: &[(&str, &str)]| {
        let mut req = Request::builder()
            .uri(format!("/v1/apps/cairn/events/stream{query}"))
            .header(header::AUTHORIZATION, format!("Bearer {}", api.human));
        for (k, v) in extra {
            req = req.header(*k, *v);
        }
        api.app.clone().oneshot(req.body(Body::empty()).unwrap())
    };

    let page = api.human("GET", "/v1/apps/cairn/records/snoozes", None).await;
    assert_eq!(page.body.as_array().unwrap().len(), 2);
    let cursor = page.headers["trailhead-event-cursor"].to_str().unwrap().to_string();
    assert_eq!(cursor, "2");
    // A write after the snapshot and before the stream opens is delivered, once.
    api.snooze(&api.human, "c").await;
    let res = stream(&format!("?after={cursor}"), &[]).await.unwrap();
    assert_eq!(
        (res.status(), res.headers()[header::CONTENT_TYPE].to_str().unwrap()),
        (StatusCode::OK, "text/event-stream")
    );
    let reader = tokio::spawn(read_sse(res.into_body(), |s| s.contains("\"item\":\"d\"")));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    api.snooze(&api.human, "d").await;
    let got = reader.await.unwrap();
    assert_eq!(ids(&got), [3, 4], "{got}");
    assert!(got.contains("event: record.created"), "{got}");

    // Last-Event-ID wins over `after`, and filters hold.
    let res = stream("?after=3&collection=snoozes", &[("last-event-id", "0")]).await.unwrap();
    let got = read_sse(res.into_body(), |s| ids(s).len() == 4).await;
    assert_eq!(ids(&got), [1, 2, 3, 4]);
    // A fresh stream starts at the newest event.
    let res = stream("", &[]).await.unwrap();
    let reader = tokio::spawn(read_sse(res.into_body(), |s| s.contains("\"item\":\"e\"")));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    api.snooze(&api.human, "e").await;
    assert_eq!(ids(&reader.await.unwrap()), [5]);
    let res = api.app.clone().oneshot(
        Request::builder()
            .uri("/v1/apps/nope/events/stream")
            .header(header::AUTHORIZATION, format!("Bearer {}", api.human))
            .body(Body::empty())
            .unwrap(),
    );
    assert_eq!(res.await.unwrap().status(), StatusCode::NOT_FOUND);
}

/// An apps repo's checkout: a plain app, an app with a fed collection, a broken file, a directory that isn't an
/// app's id, an app whose feed has no connector, and an app with no data.
fn checkout(dir: &Path) {
    let write = |path: &str, text: &str| {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write("apps/notes/collections.toml", "[collections.notes]\nfields = { text = \"string\" }\n");
    write(
        "apps/cairn/collections.toml",
        "[collections.snoozes]\nfields = { item = \"string\" }\n\n[collections.pull_requests]\nfrom = \"github.pull_requests\"\n",
    );
    write(
        "connectors/github/connector.toml",
        "id = \"github\"\nname = \"GitHub\"\nevery = \"5m\"\nhosts = [\"api.github.com\"]\n\n[feeds.pull_requests]\nfields = { title = \"string\", url = \"url\", draft = \"boolean?\" }\n",
    );
    write("apps/broken/collections.toml", "[collections.x]\nfields = { y = \"nope\" }\n");
    write("apps/Not_An_App/collections.toml", "[collections.x]\nfields = { y = \"string\" }\n");
    write("apps/jira-view/collections.toml", "[collections.tickets]\nfrom = \"jira.tickets\"\n");
    write("apps/plain/src/main.rs", "fn main() {}\n");
}

#[tokio::test]
async fn a_checkouts_declarations_are_registered_each_on_its_own() {
    let api = Api::new().await;
    let dir = tempfile::tempdir().unwrap();
    checkout(dir.path());
    let done = declarations::register_checkout(&api.store, &api.ctx, dir.path()).await;
    assert_eq!(done.registered, ["cairn", "notes"]);
    let refused: std::collections::BTreeMap<_, _> = done.refused.into_iter().collect();
    assert_eq!(refused.keys().map(String::as_str).collect::<Vec<_>>(), ["Not_An_App", "broken", "jira-view"]);
    assert!(
        refused["broken"]
            .starts_with("apps/broken/collections.toml: collection `x`: field `y`: `nope` isn't a field type"),
        "{}",
        refused["broken"]
    );
    assert!(refused["Not_An_App"].contains("isn't an app's id"), "{}", refused["Not_An_App"]);
    assert!(refused["jira-view"].contains("there is no connectors/jira/connector.toml"), "{}", refused["jira-view"]);

    let r = api.human("GET", "/v1/apps/cairn", None).await;
    assert_eq!(
        r.body["collections"]["pull_requests"],
        json!({"fields": {"title": "string", "url": "url", "draft": "boolean?"}, "from": "github.pull_requests"})
    );

    // A change the store refuses is that app's refusal, and the declaration it had stays.
    api.human("POST", "/v1/apps/notes/records/notes", Some(json!({"data": {"text": "kept"}}))).await;
    std::fs::write(
        dir.path().join("apps/notes/collections.toml"),
        "[collections.notes]\nfields = { body = \"string\" }\n",
    )
    .unwrap();
    let done = declarations::register_checkout(&api.store, &api.ctx, dir.path()).await;
    assert_eq!(done.registered, ["cairn"]);
    assert!(
        done.refused.iter().any(|(app, why)| app == "notes" && why.contains("it holds records")),
        "{:?}",
        done.refused
    );
    assert_eq!(
        api.human("GET", "/v1/apps/notes", None).await.body["collections"]["notes"]["fields"],
        json!({"text": "string"})
    );
}

#[tokio::test]
async fn the_builders_last_built_checkout_is_what_is_registered() {
    let mut layout = None;
    let api = Api::with(|state, data| {
        let l = trailhead_builder::Layout::new(data);
        state.shell.set(trailhead_builder::Builds::new(l.clone(), trailhead_builder::System::new())).unwrap();
        layout = Some(l);
    })
    .await;
    let layout = layout.unwrap();
    checkout(&layout.src());
    let builds = trailhead_builder::Builds::new(layout.clone(), trailhead_builder::System::new());
    assert_eq!(declarations::register_built(&api.store, &api.ctx, &builds).await, None, "nothing is built yet");
    let built = |rev: &str| Some(trailhead_builder::Built { rev: rev.into(), at: jiff::Timestamp::UNIX_EPOCH });
    layout.save(&trailhead_builder::State { current: built("a"), ..Default::default() }).unwrap();
    let done = declarations::register_built(&api.store, &api.ctx, &builds).await.unwrap();
    assert_eq!(done.registered, ["cairn", "notes"]);

    // The declaration route fills a fed collection's fields from the same checkout.
    let r = api.declare_toml(&api.human, "feeds", "[collections.prs]\nfrom = \"github.pull_requests\"\n").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["collections"]["prs"]["fields"]["title"], "string");
    layout
        .save(&trailhead_builder::State { current: built("a"), building: Some("b".into()), ..Default::default() })
        .unwrap();
    assert_eq!(declarations::register_built(&api.store, &api.ctx, &builds).await, None, "a build is in progress");
}
