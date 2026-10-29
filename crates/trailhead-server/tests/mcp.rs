mod common;

use std::sync::Arc;

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientConfig, ReadResourceRequestParams, ResourceContents};
use rmcp::service::RunningService;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use trailhead_core::{NewProject, NewTask, Principal, TokenOptions};
use trailhead_server::creds::Cred;
use trailhead_server::{AppState, open_local, router};

struct Server {
    url: String,
    human: String,
    agent: String,
    /// An agent token limited to AUTH.
    scoped: String,
    _dir: tempfile::TempDir,
}

async fn start() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let cfg = common::local_config(dir.path());
    let (store, ctx) = open_local(&cfg).await.unwrap();
    let human = std::fs::read_to_string(cfg.creds.file(Cred::User)).unwrap().trim().to_string();
    store
        .create_project(
            &ctx,
            NewProject { key: "AUTH".into(), name: "Auth".into(), brief: "Ship login.".into(), ..Default::default() },
        )
        .await
        .unwrap();
    for title in ["first", "second"] {
        store
            .create_task(&ctx, NewTask { project: "AUTH".into(), title: title.into(), ..Default::default() })
            .await
            .unwrap();
    }
    let agent = store.create_agent(&ctx, "claude").await.unwrap();
    let (_, agent) = store.create_token(&ctx, Principal::Agent(agent.id), "claude").await.unwrap();
    let bot = store.create_agent(&ctx, "scoped").await.unwrap();
    let opts = TokenOptions { projects: Some(vec!["AUTH".into()]), ..Default::default() };
    let (_, scoped) = store.create_token_with(&ctx, Principal::Agent(bot.id), "scoped", opts).await.unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let app = router(Arc::new(AppState::new(store)));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Server { url, human, agent, scoped, _dir: dir }
}

async fn connect(url: &str, token: &str) -> RunningService<rmcp::RoleClient, ClientConfig> {
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(url).auth_header(token),
    );
    ClientConfig::default().serve(transport).await.unwrap()
}

async fn call(client: &RunningService<rmcp::RoleClient, ClientConfig>, tool: &str, args: Value) -> (bool, String) {
    let args = args.as_object().cloned().unwrap_or_default();
    let res = client.call_tool(CallToolRequestParams::new(tool.to_string()).with_arguments(args)).await.unwrap();
    let text = res.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect::<Vec<_>>().join("\n");
    (res.is_error == Some(true), text)
}

#[tokio::test]
async fn agent_works_a_task_over_mcp() {
    let s = start().await;
    let client = connect(&s.url, &s.agent).await;

    let tools = client.list_all_tools().await.unwrap();
    // Every tool says whether it's read-only, so clients can skip approval for safe calls.
    for t in &tools {
        let a = t.annotations.as_ref().unwrap_or_else(|| panic!("{} has no annotations", t.name));
        let expect = matches!(
            t.name.as_ref(),
            "ready" | "list_tasks" | "show" | "search" | "my_claims" | "list_projects" | "show_project"
        );
        assert_eq!(a.read_only_hint, Some(expect), "{}", t.name);
    }
    let names: Vec<_> = tools.iter().map(|t| t.name.to_string()).collect();
    for expected in ["ready", "claim", "renew", "release", "show", "create_task", "update_task", "comment", "search"] {
        assert!(names.contains(&expected.to_string()), "missing tool {expected}: {names:?}");
    }

    let (err, ready) = call(&client, "ready", json!({"project": "AUTH"})).await;
    assert!(!err);
    assert_eq!(ready, "AUTH-1 open p3 v1 first\nAUTH-2 open p3 v1 second");

    let (err, out) =
        call(&client, "create_task", json!({"project": "AUTH", "title": "blocker", "blocks": ["AUTH-1"]})).await;
    assert!(!err, "{out}");
    let (err, out) = call(&client, "claim", json!({"task": "AUTH-1"})).await;
    assert!(err);
    assert!(out.starts_with("error[blocked_by]"), "{out}");

    let (err, out) = call(&client, "claim", json!({"task": "AUTH-2", "ttl": 600})).await;
    assert!(!err, "{out}");
    assert!(out.starts_with("AUTH-2 in_progress"));
    let (err, out) =
        call(&client, "update_task", json!({"task": "AUTH-2", "status": "done", "handoff": "done, tests pass"})).await;
    assert!(!err, "{out}");
    let (_, out) = call(&client, "show", json!({"task": "AUTH-2"})).await;
    assert!(out.contains("[handoff]") && out.contains("done, tests pass"), "{out}");
    assert!(out.contains(" UTC claude: done, tests pass"), "{out}");

    let (err, out) = call(
        &client,
        "comment",
        json!({"task": "AUTH-2", "body": "not yet", "kind": "review", "verdict": "request_changes", "model": "m-1"}),
    )
    .await;
    assert!(!err, "{out}");
    let (_, out) = call(&client, "show", json!({"task": "AUTH-2"})).await;
    assert!(out.contains("[review, request changes, m-1] ") && out.contains(" claude: not yet"), "{out}");

    // `addressed_to` takes a name as `show` prints it.
    let (err, out) =
        call(&client, "comment", json!({"task": "AUTH-1", "body": "?", "kind": "question", "addressed_to": "nobody"}))
            .await;
    assert!(err && out.contains("nobody"), "{out}");
    let (err, out) = call(
        &client,
        "comment",
        json!({"task": "AUTH-1", "body": "ship it?", "kind": "question", "addressed_to": "lane"}),
    )
    .await;
    assert!(!err, "{out}");
    let question = out.split_whitespace().last().unwrap().to_string();
    let (_, out) = call(&client, "show", json!({"task": "AUTH-1"})).await;
    assert!(out.contains("[question, to lane, open] ") && out.contains(" claude: ship it?"), "{out}");
    let (err, out) = call(&client, "resolve_question", json!({"comment_id": question, "answer": "yes"})).await;
    assert!(!err, "{out}");
    let (_, out) = call(&client, "show", json!({"task": "AUTH-1"})).await;
    assert!(out.contains("[question, to lane, resolved] "), "{out}");
    assert!(out.contains("[note, to claude] "), "the answer goes back to the asker: {out}");

    let (err, out) = call(&client, "create_task", json!({"project": "AUTH", "title": "oops"})).await;
    assert!(!err, "{out}");
    let (err, out) = call(&client, "undo_last", json!({})).await;
    assert!(!err && out == "reverted 1 change(s)", "{out}");
    let (_, out) = call(&client, "search", json!({"query": "oops"})).await;
    assert_eq!(out, "no matches");

    let (err, out) = call(&client, "update_task", json!({"task": "AUTH-3", "status": "sideways"})).await;
    assert!(err && out.starts_with("error[invalid]"), "{out}");

    let resources = client.list_all_resources().await.unwrap();
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].uri, "trailhead://projects/AUTH/brief");
    let brief = client.read_resource(ReadResourceRequestParams::new("trailhead://projects/AUTH/brief")).await.unwrap();
    match &brief.contents[0] {
        ResourceContents::TextResourceContents { text, .. } => assert_eq!(text, "Ship login."),
        other => panic!("unexpected contents {other:?}"),
    }
    client.cancel().await.unwrap();

    // The human sees the agent's work attributed to the agent.
    let human = connect(&s.url, &s.human).await;
    let (_, out) = call(&human, "search", json!({"query": "tests pass"})).await;
    assert_eq!(out, "AUTH-2 done p3 v3 second");
    human.cancel().await.unwrap();
}

#[tokio::test]
async fn mcp_requires_a_token() {
    let s = start().await;
    let res = reqwest_post(&s.url).await;
    assert_eq!(res, 401);
}

async fn reqwest_post(url: &str) -> u16 {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).to_string();
    let addr = url.trim_start_matches("http://").split('/').next().unwrap().to_string();
    let mut stream = tokio::net::TcpStream::connect(&addr).await.unwrap();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let req = format!(
        "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = String::new();
    stream.read_to_string(&mut buf).await.unwrap();
    buf.split_whitespace().nth(1).unwrap().parse().unwrap()
}

#[tokio::test]
async fn idempotency_keys_make_mcp_retries_safe() {
    let s = start().await;
    let client = connect(&s.url, &s.agent).await;
    let args = json!({"project": "AUTH", "title": "once", "idempotency_key": "retry-1"});
    let (err, first) = call(&client, "create_task", args.clone()).await;
    assert!(!err, "{first}");
    let (err, again) = call(&client, "create_task", args).await;
    assert!(!err);
    assert_eq!(first, again);
    let (_, found) = call(&client, "search", json!({"query": "once"})).await;
    assert_eq!(found.lines().count(), 1, "{found}");

    let (err, out) =
        call(&client, "create_task", json!({"project": "AUTH", "title": "different", "idempotency_key": "retry-1"}))
            .await;
    assert!(err && out.starts_with("error[idempotency_key_reused]"), "{out}");

    // The same key in a different tool is a different key.
    let (err, out) =
        call(&client, "comment", json!({"task": "AUTH-1", "body": "hi", "idempotency_key": "retry-1"})).await;
    assert!(!err, "{out}");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn agents_schedule_tasks_over_mcp() {
    let s = start().await;
    let client = connect(&s.url, &s.agent).await;
    let args =
        json!({"project": "AUTH", "title": "plan", "scheduled_on": "2026-10-17", "due_at": "2026-11-01T12:00:00Z"});
    let (err, out) = call(&client, "create_task", args).await;
    assert!(!err && out.contains("(scheduled 2026-10-17) (due 2026-11-01 12:00 UTC)"), "{out}");
    let (_, out) = call(&client, "list_tasks", json!({"scheduled_through": "2026-10-16"})).await;
    assert_eq!(out, "no tasks");
    let (_, out) = call(&client, "list_tasks", json!({"scheduled_through": "2026-10-17"})).await;
    assert!(out.starts_with("AUTH-3 ") && out.lines().count() == 1, "{out}");
    let at =
        json!({"task": "AUTH-3", "scheduled_at": "2026-10-17T13:00:00-07:00", "scheduled_tz": "America/Los_Angeles"});
    let (err, out) = call(&client, "update_task", at).await;
    assert!(!err && out.contains("(scheduled 2026-10-17 13:00 PDT)"), "{out}");
    let (err, out) = call(&client, "update_task", json!({"task": "AUTH-3", "scheduled_at": ""})).await;
    assert!(!err && out.contains("(scheduled 2026-10-17)"), "{out}");
    let (err, out) = call(&client, "update_task", json!({"task": "AUTH-3", "due_at": ""})).await;
    assert!(!err && out.contains("(due 2026-11-01)"), "{out}");
    let (err, out) = call(&client, "update_task", json!({"task": "AUTH-3", "due_on": ""})).await;
    assert!(!err && !out.contains("due"), "{out}");
    let (err, out) = call(&client, "update_task", json!({"task": "AUTH-3", "scheduled_on": ""})).await;
    assert!(!err && !out.contains("scheduled"), "{out}");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn agents_move_tasks_between_parents_over_mcp() {
    let s = start().await;
    let client = connect(&s.url, &s.agent).await;
    call(&client, "create_task", json!({"project": "AUTH", "title": "queue"})).await;
    call(&client, "create_task", json!({"project": "AUTH", "title": "work"})).await;
    let under = json!({"parent": "AUTH-3"});
    let (err, out) = call(&client, "update_task", json!({"task": "AUTH-4", "parent": "AUTH-3"})).await;
    assert!(!err, "{out}");
    let (_, out) = call(&client, "list_tasks", under.clone()).await;
    assert!(out.starts_with("AUTH-4 ") && out.lines().count() == 1, "{out}");
    let (err, out) = call(&client, "update_task", json!({"task": "AUTH-4", "parent": ""})).await;
    assert!(!err, "{out}");
    let (_, out) = call(&client, "list_tasks", under).await;
    assert_eq!(out, "no tasks");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn agents_create_projects_and_write_briefs() {
    let s = start().await;
    let client = connect(&s.url, &s.agent).await;

    let (err, out) = call(
        &client,
        "create_project",
        json!({"key": "tb", "name": "Tutti Belli", "brief": "Choir app.", "require_review": true}),
    )
    .await;
    assert!(!err, "{out}");
    assert_eq!(out, "TB v1 Tutti Belli [review]");
    let (err, out) = call(&client, "create_project", json!({"key": "TB", "name": "again"})).await;
    assert!(err && out.starts_with("error[already_exists]"), "{out}");
    let (err, out) = call(&client, "create_project", json!({"key": "1X", "name": "bad key"})).await;
    assert!(err && out.starts_with("error[invalid]"), "{out}");

    let (_, out) = call(&client, "list_projects", json!({})).await;
    assert_eq!(out, "AUTH v1 Auth\nTB v1 Tutti Belli [review]");

    let (err, out) = call(&client, "update_project", json!({"project": "TB", "brief": "Choir app. Ship small."})).await;
    assert!(!err, "{out}");
    let (err, out) = call(&client, "update_project", json!({"project": "TB", "name": "x", "version": 1})).await;
    assert!(err && out.starts_with("error[version_conflict]"), "{out}");
    let (_, out) = call(&client, "show_project", json!({"project": "tb"})).await;
    assert_eq!(out, "TB v2 Tutti Belli [review]\n\nChoir app. Ship small.");

    let (err, out) = call(&client, "undo_last", json!({})).await;
    assert!(!err && out == "reverted 1 change(s)", "{out}");
    let (_, out) = call(&client, "show_project", json!({"project": "TB"})).await;
    assert!(out.ends_with("\n\nChoir app."), "{out}");

    let (err, out) = call(&client, "create_task", json!({"project": "TB", "title": "first"})).await;
    assert!(!err && out.starts_with("TB-1 open"), "{out}");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn project_scoped_tokens_cant_create_or_edit_other_projects() {
    let s = start().await;
    let human = connect(&s.url, &s.human).await;
    let (err, out) = call(&human, "create_project", json!({"key": "WEB", "name": "Web"})).await;
    assert!(!err, "{out}");
    human.cancel().await.unwrap();

    let client = connect(&s.url, &s.scoped).await;
    let (err, out) = call(&client, "create_project", json!({"key": "NEW", "name": "New"})).await;
    assert!(err && out.starts_with("error[forbidden]"), "{out}");
    let (err, out) = call(&client, "update_project", json!({"project": "WEB", "brief": "mine now"})).await;
    assert!(err && out.starts_with("error[forbidden]"), "{out}");
    let (_, out) = call(&client, "list_projects", json!({})).await;
    assert_eq!(out, "AUTH v1 Auth");
    let (err, out) = call(&client, "update_project", json!({"project": "AUTH", "brief": "Ship login fast."})).await;
    assert!(!err, "{out}");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn one_connection_serves_several_registered_sub_agents() {
    let s = start().await;
    let client = connect(&s.url, &s.agent).await;
    let (err, out) = call(&client, "ready", json!({"as_agent": "researcher"})).await;
    assert!(err && out.starts_with("error[not_found]"), "unregistered names are refused: {out}");

    let (err, out) = call(&client, "register_agent", json!({"name": "researcher"})).await;
    assert!(!err && out.starts_with("registered claude/researcher"), "{out}");
    let (err, out) = call(&client, "claim", json!({"task": "AUTH-1", "as_agent": "researcher"})).await;
    assert!(!err, "{out}");
    let (_, out) = call(&client, "my_claims", json!({"as_agent": "researcher"})).await;
    assert!(out.starts_with("AUTH-1"), "{out}");
    let (_, out) = call(&client, "my_claims", json!({})).await;
    assert_eq!(out, "you hold no claims", "the parent doesn't hold its sub-agent's claim");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn grade_is_set_cleared_and_filtered_over_mcp() {
    let s = start().await;
    let human = connect(&s.url, &s.human).await;
    let (err, out) = call(&human, "update_task", json!({"task": "AUTH-1", "grade": "3"})).await;
    assert!(!err && out.contains("(grade 3)"), "{out}");
    let (_, out) = call(&human, "list_tasks", json!({"project": "AUTH", "grade": "3"})).await;
    assert!(out.contains("AUTH-1") && !out.contains("AUTH-2"), "{out}");
    let (_, out) = call(&human, "ready", json!({"project": "AUTH", "grade": "ungraded"})).await;
    assert!(out.contains("AUTH-2") && !out.contains("AUTH-1"), "{out}");
    let (err, out) = call(&human, "ready", json!({"grade": "nope"})).await;
    assert!(err && out.starts_with("error[invalid]"), "{out}");
    let (err, out) = call(&human, "update_task", json!({"task": "AUTH-1", "grade": "9"})).await;
    assert!(err && out.starts_with("error[invalid]"), "{out}");

    let (err, out) = call(&human, "update_task", json!({"task": "AUTH-1", "grade": "none"})).await;
    assert!(!err, "{out}");
    assert!(!out.contains("grade"), "{out}");
    let (_, out) = call(&human, "ready", json!({"project": "AUTH", "grade": "graded"})).await;
    assert!(!out.contains("AUTH-"), "{out}");
    human.cancel().await.unwrap();

    let agent = connect(&s.url, &s.agent).await;
    let (err, out) = call(&agent, "update_task", json!({"task": "AUTH-1", "grade": "2"})).await;
    assert!(err && out.starts_with("error[forbidden]"), "{out}");
    agent.cancel().await.unwrap();
}
