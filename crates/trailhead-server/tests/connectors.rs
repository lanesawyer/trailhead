mod common;

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use trailhead_client::creds::{Cred, CredStore};
use trailhead_core::app_data::Declaration;
use trailhead_core::connector::{FAILING, Manifest, secret_cred};
use trailhead_core::{Ctx, NotificationFilter, Principal, Store, TokenOptions};
use trailhead_server::connectors::{Connectors, RunConfig, SchedulerOptions, spawn_scheduler};
use trailhead_server::{AppState, open_local, router};

const HELLO: &str = r#"
id = "hello"
name = "Hello"
every = "1s"
timeout = "5s"
hosts = ["example.com"]
secrets = ["token"]

[settings.greeting]
kind = "string"
required = true

[settings.loud]
kind = "boolean"

[feeds.items]
fields = { title = "string", n = "integer?" }
"#;

struct Rig {
    state: Arc<AppState>,
    app: Router,
    store: Arc<Store>,
    ctx: Ctx,
    human: String,
    creds: CredStore,
    dir: tempfile::TempDir,
}

struct Res {
    status: StatusCode,
    body: Value,
}

impl Rig {
    /// A server that runs connectors, with `hello` run by `program`.
    async fn new(program: Option<&str>) -> Rig {
        Rig::at("http://127.0.0.1:9", program).await
    }

    /// [`Rig::new`], telling a run the server is at `url`.
    async fn at(url: &str, program: Option<&str>) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let cfg = common::local_config(dir.path());
        let (store, ctx) = open_local(&cfg).await.unwrap();
        let human = std::fs::read_to_string(cfg.creds.file(Cred::User)).unwrap().trim().to_string();
        let creds = CredStore { config_dir: dir.path().join("creds"), keyring: false };
        let config = RunConfig {
            url: url.into(),
            data_dir: dir.path().to_path_buf(),
            creds: creds.clone(),
            env: vec![("PATH".into(), std::env::var("PATH").unwrap())],
            overrides: HashMap::new(),
        };
        let connectors = Connectors::new(Some(config));
        let program = program.map(|body| script(dir.path(), "hello", body));
        connectors.insert(Manifest::parse(HELLO).unwrap(), program);
        let state = Arc::new(AppState { connectors: Arc::new(connectors), ..AppState::new(store) });
        let store = state.store.clone();
        Rig { app: router(state.clone()), state, store, ctx, human, creds, dir }
    }

    fn ctl(&self) -> PathBuf {
        self.dir.path().join("ctl")
    }

    async fn call(&self, token: &str, method: &str, path: &str, body: Option<Value>) -> Res {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json");
        let body = body.map_or_else(Body::empty, |b| Body::from(b.to_string()));
        let res = self.app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let body = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
        Res { status, body }
    }

    async fn human(&self, method: &str, path: &str, body: Option<Value>) -> Res {
        self.call(&self.human.clone(), method, path, body).await
    }

    /// Declares `cairn` and `notes`, each with a collection filled from `hello.items`; `notes` wants a field the feed
    /// doesn't give, so every set fails there.
    async fn apps(&self) {
        let cairn =
            r#"{"collections": {"items": {"from": "hello.items", "fields": {"title": "string", "n": "integer?"}}}}"#;
        let notes =
            r#"{"collections": {"items": {"from": "hello.items", "fields": {"title": "string", "x": "integer"}}}}"#;
        for (app, text) in [("cairn", cairn), ("notes", notes)] {
            let declaration: Declaration = serde_json::from_str(text).unwrap();
            self.store.register_app(&self.ctx, app, declaration).await.unwrap();
        }
    }

    /// Turns `hello` on with a greeting and its secret, so it can run.
    async fn ready(&self) {
        self.creds.save(Cred::Secret(&secret_cred("hello", "token")), "s3cret").unwrap();
        let r = self
            .human("PATCH", "/v1/connectors/hello", Some(json!({"enabled": true, "settings": {"greeting": "hi"}})))
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    }

    /// A token of the connector `id`'s agent, made as the server makes it.
    async fn connector_token(&self, id: &str) -> String {
        let agent = self.store.connector_agent(&self.ctx, id).await.unwrap();
        let (_, secret) = self
            .store
            .create_token_with(&self.ctx, Principal::Agent(agent.id), "t", TokenOptions::default())
            .await
            .unwrap();
        secret
    }

    async fn info(&self) -> Value {
        let r = self.human("GET", "/v1/connectors/hello", None).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        r.body
    }

    /// Waits for a run that started after `after` to finish, and gives its record.
    async fn finished_after(&self, after: Option<&str>) -> Value {
        for _ in 0..200 {
            let info = self.info().await;
            let started = info["last_run"]["started_at"].as_str();
            if !info["running"].as_bool().unwrap() && started.is_some() && started != after {
                return info["last_run"].clone();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("no run finished");
    }

    async fn run_now(&self) -> Value {
        let before = self.info().await["last_run"]["started_at"].as_str().map(String::from);
        let r = self.human("POST", "/v1/connectors/hello/run", None).await;
        assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.body);
        self.finished_after(before.as_deref()).await
    }
}

/// A shell script for a connector's program, at `<dir>/<name>`.
fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn error(r: &Res, status: StatusCode, says: &str) {
    assert_eq!(r.status, status, "{}", r.body);
    let msg = r.body["error"]["message"].as_str().unwrap();
    assert!(msg.contains(says), "{msg:?} doesn't say {says:?}");
}

fn items(records: &[(&str, &str)]) -> Value {
    json!({"records": records.iter().map(|(k, t)| json!({"key": k, "data": {"title": t}})).collect::<Vec<_>>()})
}

#[tokio::test]
async fn a_feed_is_written_into_every_app_filled_from_it_and_an_unchanged_set_writes_nothing() {
    let rig = Rig::new(None).await;
    rig.apps().await;
    let token = rig.connector_token("hello").await;
    let r = rig.call(&token, "PUT", "/v1/connectors/hello/feeds/items", Some(items(&[("a", "A"), ("b", "B")]))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["records"], 2);
    let apps = r.body["apps"].as_array().unwrap();
    assert_eq!(apps.len(), 2);
    assert_eq!((apps[0]["app"].as_str(), apps[0]["created"].as_u64()), (Some("cairn"), Some(2)));
    assert_eq!(apps[1]["app"], "notes");
    assert!(apps[1]["error"].as_str().unwrap().contains("`x` is missing"), "one app failing: {}", apps[1]);

    let records = rig.human("GET", "/v1/apps/cairn/records/items", None).await;
    let titles: Vec<&str> =
        records.body.as_array().unwrap().iter().map(|r| r["data"]["title"].as_str().unwrap()).collect();
    assert_eq!(titles, ["A", "B"]);
    let events = rig.store.latest_app_event_id(&rig.ctx, "cairn").await.unwrap();

    let again =
        rig.call(&token, "PUT", "/v1/connectors/hello/feeds/items", Some(items(&[("b", "B"), ("a", "A")]))).await;
    assert_eq!(again.body["apps"][0]["unchanged"], 2, "{}", again.body);
    assert_eq!(rig.store.latest_app_event_id(&rig.ctx, "cairn").await.unwrap(), events, "no event for no change");

    let fewer = rig.call(&token, "PUT", "/v1/connectors/hello/feeds/items", Some(items(&[("b", "B2")]))).await;
    let cairn = &fewer.body["apps"][0];
    assert_eq!((cairn["changed"].as_u64(), cairn["removed"].as_u64()), (Some(1), Some(1)), "{cairn}");
}

#[tokio::test]
async fn only_the_feeds_own_connector_hands_it_over_and_its_token_reaches_nothing_else() {
    let rig = Rig::new(None).await;
    rig.apps().await;
    let set = items(&[("a", "A")]);
    let r = rig.human("PUT", "/v1/connectors/hello/feeds/items", Some(set.clone())).await;
    error(&r, StatusCode::FORBIDDEN, "only connector/hello");
    let r = rig.human("POST", "/v1/agents", Some(json!({"name": "worker"}))).await;
    let worker = r.body["secret"].as_str().unwrap().to_string();
    let r = rig.call(&worker, "PUT", "/v1/connectors/hello/feeds/items", Some(set.clone())).await;
    error(&r, StatusCode::FORBIDDEN, "only connector/hello");
    let other = rig.connector_token("jira").await;
    let r = rig.call(&other, "PUT", "/v1/connectors/hello/feeds/items", Some(set.clone())).await;
    error(&r, StatusCode::FORBIDDEN, "may only hand over its own feeds");

    let token = rig.connector_token("hello").await;
    for (method, path) in [
        ("GET", "/v1/tasks"),
        ("GET", "/v1/apps/cairn/records/items"),
        ("GET", "/v1/connectors/hello"),
        ("POST", "/v1/connectors/hello/run"),
    ] {
        let r = rig.call(&token, method, path, None).await;
        error(&r, StatusCode::FORBIDDEN, "may only hand over its own feeds");
    }
    let r = rig.call(&token, "PUT", "/v1/connectors/hello/feeds/nope", Some(set)).await;
    error(&r, StatusCode::NOT_FOUND, "feed nope");
}

#[tokio::test]
async fn a_bad_record_is_refused_naming_the_feed_its_key_and_the_field() {
    let rig = Rig::new(None).await;
    rig.apps().await;
    let token = rig.connector_token("hello").await;
    let bad =
        json!({"records": [{"key": "a", "data": {"title": "A"}}, {"key": "b", "data": {"title": "B", "n": "two"}}]});
    let r = rig.call(&token, "PUT", "/v1/connectors/hello/feeds/items", Some(bad)).await;
    error(&r, StatusCode::BAD_REQUEST, "feed `items`, record `b`, field `n`");
    let r = rig.human("GET", "/v1/apps/cairn/records/items", None).await;
    assert_eq!(r.body, json!([]), "nothing of a refused set is written");
}

#[tokio::test]
async fn settings_are_checked_against_the_manifest_and_secrets_never_shown() {
    let rig = Rig::new(Some("exit 0")).await;
    let r = rig.human("PATCH", "/v1/connectors/hello", Some(json!({"settings": {"colour": "red"}}))).await;
    error(&r, StatusCode::BAD_REQUEST, "`colour` is not a setting this connector declares");
    let r = rig.human("PATCH", "/v1/connectors/hello", Some(json!({"settings": {"greeting": 3}}))).await;
    error(&r, StatusCode::BAD_REQUEST, "setting `greeting` must be a string");
    let r =
        rig.human("PATCH", "/v1/connectors/hello", Some(json!({"settings": {"greeting": "hi", "loud": true}}))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let r = rig.human("PATCH", "/v1/connectors/hello", Some(json!({"settings": {"loud": null}}))).await;
    let settings = r.body["settings"].as_array().unwrap();
    assert_eq!(settings[0]["value"], "hi", "the others stay as set: {}", r.body);
    assert_eq!(settings[1].get("value"), None, "null unsets one");
    assert_eq!(r.body["secrets"], json!([{"name": "token", "set": false}]));

    rig.creds.save(Cred::Secret(&secret_cred("hello", "token")), "s3cret").unwrap();
    let info = rig.info().await;
    assert_eq!(info["secrets"], json!([{"name": "token", "set": true}]));
    assert!(!info.to_string().contains("s3cret"));

    let r = rig.human("POST", "/v1/agents", Some(json!({"name": "worker"}))).await;
    let worker = r.body["secret"].as_str().unwrap().to_string();
    let r = rig.call(&worker, "PATCH", "/v1/connectors/hello", Some(json!({"enabled": true}))).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "only a person");
}

#[tokio::test]
async fn turning_a_connector_on_makes_its_agent() {
    let rig = Rig::new(Some("exit 0")).await;
    let names = |r: &Res| {
        r.body.as_array().unwrap().iter().map(|a| a["name"].as_str().unwrap().to_string()).collect::<Vec<_>>()
    };
    assert!(!names(&rig.human("GET", "/v1/agents", None).await).contains(&"connector/hello".to_string()));
    let r = rig.human("PATCH", "/v1/connectors/hello", Some(json!({"enabled": true}))).await;
    assert_eq!(r.body["enabled"], true, "{}", r.body);
    let agents = rig.human("GET", "/v1/agents", None).await;
    assert!(names(&agents).contains(&"connector/hello".to_string()), "{}", agents.body);
    let r = rig.human("POST", "/v1/agents", Some(json!({"name": "connector"}))).await;
    error(&r, StatusCode::BAD_REQUEST, "kept for connectors");
}

#[tokio::test]
async fn what_a_connector_still_needs_is_said_and_it_doesnt_run_without_it() {
    let rig = Rig::new(Some("touch \"$TRAILHEAD_CONNECTOR_STATE.ran\"")).await;
    let r = rig.human("POST", "/v1/connectors/hello/run", None).await;
    error(&r, StatusCode::BAD_REQUEST, "is off");
    rig.human("PATCH", "/v1/connectors/hello", Some(json!({"enabled": true}))).await;
    let missing = rig.info().await["missing"].clone();
    let missing: Vec<&str> = missing.as_array().unwrap().iter().map(|m| m.as_str().unwrap()).collect();
    assert_eq!(missing.len(), 2, "{missing:?}");
    assert!(missing[0].starts_with("the setting `greeting`"), "{missing:?}");
    assert!(missing[1].starts_with("the secret `token`"), "{missing:?}");
    let r = rig.human("POST", "/v1/connectors/hello/run", None).await;
    error(&r, StatusCode::BAD_REQUEST, "can't run yet; it needs the setting `greeting`");

    spawn_scheduler(rig.state.clone(), rig.ctx.clone(), SchedulerOptions { tick: Duration::from_millis(20) });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let state = rig.dir.path().join("connectors/hello/state.json.ran");
    assert!(!state.exists(), "the scheduler ran a connector that can't run");
    assert_eq!(rig.info().await.get("next_run"), None);

    let unbuilt = Rig::new(None).await;
    unbuilt.ready().await;
    let missing = unbuilt.info().await["missing"].clone();
    assert!(missing[0].as_str().unwrap().contains("a build of its program"), "{missing}");
}

#[tokio::test]
async fn a_run_gets_its_settings_secrets_state_and_token_and_nothing_else_of_the_servers_environment() {
    let rig = Rig::new(None).await;
    let out = rig.dir.path().join("env.txt");
    let program = script(
        rig.dir.path(),
        "dump",
        &format!("env > {}\necho 'warming up' >&2\necho 'it went wrong' >&2\nexit 3", out.display()),
    );
    rig.state.connectors.insert(Manifest::parse(HELLO).unwrap(), Some(program));
    rig.ready().await;
    let run = rig.run_now().await;
    assert_eq!(run["ok"], false);
    assert_eq!(run["error"], "warming up\nit went wrong", "the end of stderr");

    let env: HashMap<String, String> = std::fs::read_to_string(&out)
        .unwrap()
        .lines()
        .filter_map(|l| l.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
        .collect();
    assert_eq!(env["TRAILHEAD_URL"], "http://127.0.0.1:9");
    assert_eq!(serde_json::from_str::<Value>(&env["TRAILHEAD_CONNECTOR_SETTINGS"]).unwrap(), json!({"greeting": "hi"}));
    assert_eq!(env["TRAILHEAD_CONNECTOR_SECRET_TOKEN"], "s3cret");
    let state = rig.dir.path().join("connectors/hello/state.json");
    assert_eq!(env["TRAILHEAD_CONNECTOR_STATE"], state.display().to_string());
    assert!(env["TRAILHEAD_TOKEN"].starts_with("th_"));
    let names: BTreeSet<&str> = env.keys().map(String::as_str).collect();
    let given: BTreeSet<&str> = [
        "PATH",
        "TRAILHEAD_URL",
        "TRAILHEAD_TOKEN",
        "TRAILHEAD_CONNECTOR_SETTINGS",
        "TRAILHEAD_CONNECTOR_STATE",
        "TRAILHEAD_CONNECTOR_SECRET_TOKEN",
    ]
    .into();
    let shell_own: BTreeSet<&str> = ["PWD", "SHLVL", "_", "OLDPWD"].into();
    let extra: Vec<&&str> = names.difference(&given).filter(|n| !shell_own.contains(**n)).collect();
    assert!(extra.is_empty(), "the run was given more than it should be: {extra:?}");
    assert!(std::env::var("CARGO_MANIFEST_DIR").is_ok() && !names.contains("CARGO_MANIFEST_DIR"));

    let token = &env["TRAILHEAD_TOKEN"];
    let r = rig.call(token, "GET", "/v1/tasks", None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "the run's token reaches nothing but its feeds");
}

#[tokio::test]
async fn runs_are_scheduled_every_every_and_never_overlap() {
    let rig = Rig::new(None).await;
    let ctl = rig.ctl();
    std::fs::create_dir_all(&ctl).unwrap();
    let c = ctl.display();
    let body =
        format!("[ -e {c}/busy ] && touch {c}/overlap\ntouch {c}/busy\necho run >> {c}/runs\nsleep 1.2\nrm {c}/busy");
    let program = script(rig.dir.path(), "slow", &body);
    rig.state.connectors.insert(Manifest::parse(HELLO).unwrap(), Some(program));
    rig.ready().await;
    spawn_scheduler(rig.state.clone(), rig.ctx.clone(), SchedulerOptions { tick: Duration::from_millis(20) });
    let runs = || std::fs::read_to_string(ctl.join("runs")).unwrap_or_default().lines().count();
    for _ in 0..100 {
        if runs() >= 2 && !ctl.join("busy").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(runs() >= 2, "ran {} times", runs());
    assert!(!ctl.join("overlap").exists(), "two runs at once");
    let info = rig.info().await;
    assert_eq!(info["last_run"]["ok"], true, "{info}");
    let r = rig.human("POST", "/v1/connectors/hello/run", None).await;
    if info["running"] == true {
        assert_eq!(r.status, StatusCode::CONFLICT);
    }
}

#[tokio::test]
async fn a_run_is_stopped_at_its_time_limit() {
    let rig = Rig::new(None).await;
    let pid = rig.dir.path().join("pid");
    let program = script(rig.dir.path(), "hang", &format!("echo $$ > {}\necho 'started' >&2\nsleep 30", pid.display()));
    let manifest = Manifest::parse(&HELLO.replace("timeout = \"5s\"", "timeout = \"1s\"")).unwrap();
    rig.state.connectors.insert(manifest, Some(program));
    rig.ready().await;
    let started = std::time::Instant::now();
    let run = rig.run_now().await;
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(run["ok"], false);
    let e = run["error"].as_str().unwrap();
    assert!(e.starts_with("it ran longer than its time limit of 1s and was stopped; it last said: started"), "{e}");
    let pid: i32 = std::fs::read_to_string(pid).unwrap().trim().parse().unwrap();
    // SAFETY: signal 0 only asks whether the process exists.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "the program is gone");
}

#[tokio::test]
async fn three_failures_in_a_row_raise_one_notification_and_a_success_resolves_it() {
    let rig = Rig::new(None).await;
    let ctl = rig.ctl();
    std::fs::create_dir_all(&ctl).unwrap();
    std::fs::write(ctl.join("fail"), "").unwrap();
    let body = format!("if [ -e {c}/fail ]; then echo 'the API said no' >&2; exit 1; fi", c = ctl.display());
    rig.state.connectors.insert(Manifest::parse(HELLO).unwrap(), Some(script(rig.dir.path(), "flaky", &body)));
    rig.ready().await;
    let failing = async || {
        let all = rig.store.list_notifications(&rig.ctx, NotificationFilter::default()).await.unwrap();
        all.into_iter().filter(|n| n.kind == FAILING).collect::<Vec<_>>()
    };
    for _ in 0..2 {
        rig.run_now().await;
    }
    assert!(failing().await.is_empty(), "not after two");
    for _ in 0..2 {
        let run = rig.run_now().await;
        assert_eq!(run["error"], "the API said no");
    }
    let raised = failing().await;
    assert_eq!(raised.len(), 1, "one, and none again until a run succeeds");
    assert_eq!(raised[0].subject.as_deref(), Some("connector/hello"));
    assert!(raised[0].body.contains("the API said no"), "{}", raised[0].body);
    assert_eq!(raised[0].resolved_at, None);

    std::fs::remove_file(ctl.join("fail")).unwrap();
    assert_eq!(rig.run_now().await["ok"], true);
    let after = failing().await;
    assert_eq!(after.len(), 1);
    assert!(after[0].resolved_at.is_some(), "a success resolves it");
}

#[tokio::test]
async fn a_run_hands_its_feeds_to_the_server_and_its_record_says_what_they_did() {
    if std::process::Command::new("curl").arg("--version").output().is_err() {
        return;
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let body = r#"curl -sf -X PUT -H "authorization: Bearer $TRAILHEAD_TOKEN" -H 'content-type: application/json' \
  --data '{"records": [{"key": "a", "data": {"title": "A"}}]}' "$TRAILHEAD_URL/v1/connectors/hello/feeds/items""#;
    let rig = Rig::at(&url, Some(body)).await;
    let app = rig.app.clone();
    tokio::spawn(async move { axum::serve(listener, app).await });
    rig.apps().await;
    rig.ready().await;
    let run = rig.run_now().await;
    assert_eq!(run["ok"], true, "{run}");
    assert_eq!(run["feeds"]["items"]["records"], 1, "{run}");
    assert_eq!(run["feeds"]["items"]["created"], 1, "{run}");
    assert!(run["feeds"]["items"]["errors"][0].as_str().unwrap().starts_with("notes: "), "{run}");
    let r = rig.human("GET", "/v1/apps/cairn/records/items", None).await;
    assert_eq!(r.body[0]["data"]["title"], "A");
}
