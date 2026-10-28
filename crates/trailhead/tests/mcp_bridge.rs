mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::Stdio;

use common::Instance;
use serde_json::{Value, json};

#[test]
fn stdio_bridge_round_trips_json_rpc() {
    let th = Instance::new();
    th.stdout(&["project", "add", "MCP", "Bridge", "--brief", "Bridge brief."]);
    th.stdout(&["add", "first", "-p", "MCP"]);

    let mut child = th.command().arg("mcp").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let input = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "ready", "arguments": {"project": "MCP"}}}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "resources/read", "params": {"uri": "trailhead://projects/MCP/brief"}}),
    ];
    let mut stdin = child.stdin.take().unwrap();
    for msg in &input {
        writeln!(stdin, "{msg}").unwrap();
    }
    writeln!(stdin, "not json").unwrap();
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());

    let replies: Vec<Value> =
        String::from_utf8(out.stdout).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    // The notification and the garbage line produce no output.
    assert_eq!(replies.iter().map(|r| r["id"].as_i64().unwrap()).collect::<Vec<_>>(), [1, 2, 3]);
    assert_eq!(replies[0]["result"]["serverInfo"]["name"], "trailhead");
    assert_eq!(replies[1]["result"]["content"][0]["text"], "MCP-1 open p3 v1 first");
    assert_eq!(replies[2]["result"]["contents"][0]["text"], "Bridge brief.");
}

#[test]
fn bridge_restarts_a_server_that_went_away() {
    let th = Instance::new();
    th.stdout(&["project", "add", "MCP", "Bridge"]);
    th.stdout(&["add", "first", "-p", "MCP"]);

    let mut child =
        th.command().arg("mcp").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut ask = |id: i64| {
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": "ready", "arguments": {"project": "MCP"}}});
        writeln!(stdin, "{msg}").unwrap();
        let reply: Value = serde_json::from_str(&stdout.next().unwrap().unwrap()).unwrap();
        reply["result"]["content"][0]["text"].as_str().map(String::from)
    };
    assert_eq!(ask(1).as_deref(), Some("MCP-1 open p3 v1 first"));

    // Reinstall under the running bridge, then lose the server, as when whatever
    // started it exits and takes it along.
    th.install();
    assert!(th.stdout(&["server", "stop"]).starts_with("stopped"));
    assert_eq!(ask(2).as_deref(), Some("MCP-1 open p3 v1 first"));
    assert!(th.pid().is_some());

    drop(ask);
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("starting it"));
}
