use std::sync::Arc;

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientConfig, ReadResourceRequestParams, ResourceContents};
use rmcp::service::RunningService;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use trailhead_core::{NewProject, NewTask, Principal};
use trailhead_server::{AppState, LocalConfig, open_local, router};

struct Server {
    url: String,
    human: String,
    agent: String,
    _dir: tempfile::TempDir,
}

async fn start() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let cfg = LocalConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        db: dir.path().join("t.db"),
        credentials: dir.path().join("credentials"),
        user_name: "lane".into(),
        pid_file: None,
    };
    let (store, ctx) = open_local(&cfg).await.unwrap();
    let human = std::fs::read_to_string(&cfg.credentials).unwrap().trim().to_string();
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

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let app = router(Arc::new(AppState::new(store)));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Server { url, human, agent, _dir: dir }
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
    let (err, _) =
        call(&client, "comment", json!({"task": "AUTH-2", "kind": "handoff", "body": "done, tests pass"})).await;
    assert!(!err);
    let (err, out) = call(&client, "update_task", json!({"task": "AUTH-2", "status": "done"})).await;
    assert!(!err, "{out}");
    let (_, out) = call(&client, "show", json!({"task": "AUTH-2"})).await;
    assert!(out.contains("[handoff]") && out.contains("done, tests pass"), "{out}");

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
