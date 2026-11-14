mod common;

use std::time::Duration;

use common::{Instance, output};

#[test]
fn auto_start_stop_and_restart_on_new_binary() {
    let th = Instance::new();
    assert_eq!(th.stdout(&["server", "status"]).trim(), "not running");

    // Any command starts the server.
    assert!(th.stdout(&["whoami"]).contains('('));
    let first = th.pid().expect("running after auto-start");
    assert!(th.dir.path().join("data/server.pid").exists());

    // A rebuilt binary at the same path gets the server restarted on the next command.
    std::thread::sleep(Duration::from_millis(20));
    th.install();
    let out = th.run(&["whoami"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("restarting the local server"));
    let second = th.pid().expect("running after restart");
    assert_ne!(first, second);

    let restarted = th.stdout(&["server", "restart"]);
    assert!(restarted.starts_with("restarted"));
    assert_ne!(th.pid().unwrap(), second);

    assert!(th.stdout(&["server", "stop"]).starts_with("stopped"));
    assert_eq!(th.stdout(&["server", "status"]).trim(), "not running");
    assert!(!th.dir.path().join("data/server.pid").exists());
    assert_eq!(th.stdout(&["server", "stop"]).trim(), "not running");
}

#[test]
fn stop_refuses_a_server_for_another_data_dir() {
    let a = Instance::new();
    a.stdout(&["whoami"]);
    let b = Instance::new();
    let out = output(b.command().args(["--url", &a.url, "server", "stop"]));
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("refusing to stop it"));
    assert!(a.pid().is_some(), "a's server should still be running");
}

/// `b` stands in for another machine reaching `a`'s server through a tunnel on a
/// loopback port.
#[test]
fn a_tunnelled_server_is_used_but_never_managed_or_replaced() {
    let a = Instance::new();
    let added: serde_json::Value = serde_json::from_str(&a.stdout(&["--json", "agent", "add", "laptop"])).unwrap();
    let secret = added["secret"].as_str().unwrap();
    let b = Instance::new();
    let remote = |args: &[&str]| output(b.command().args(["--url", &a.url]).args(args).env("TRAILHEAD_TOKEN", secret));

    let out = remote(&["whoami"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("laptop"));
    let status = String::from_utf8(remote(&["server", "status"]).stdout).unwrap();
    assert!(status.contains("a tunnel?"), "{status}");
    let out = remote(&["server", "stop"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("refusing to stop it"));
    let out = remote(&["server", "start"]);
    assert!(!out.status.success());

    // The tunnel goes down: b must not start an empty server of its own in its place.
    a.stdout(&["server", "stop"]);
    let out = remote(&["whoami"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("through a tunnel"));
    assert_eq!(a.stdout(&["server", "status"]).trim(), "not running");
    assert!(!b.dir.path().join("data/trailhead.db").exists());

    // Unless asked to, which makes the port b's own from then on.
    assert!(remote(&["server", "start"]).status.success());
    let out = output(b.command().args(["--url", &a.url, "whoami"]));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(remote(&["server", "stop"]).status.success());
    let out = output(b.command().args(["--url", &a.url, "whoami"]));
    assert!(out.status.success(), "b's own server should auto-start again");
    assert!(output(b.command().args(["--url", &a.url, "server", "stop"])).status.success());
}

#[test]
fn another_machine_logs_in_with_a_printed_token() {
    let a = Instance::new();
    let secret = a.stdout(&["agent", "add", "laptop", "--print-token"]).trim().to_string();
    assert!(secret.starts_with("th_"), "{secret}");
    assert!(!a.dir.path().join("config/agents/laptop").exists(), "--print-token doesn't save");

    let b = Instance::new();
    let remote = |args: &[&str]| output(b.command().args(["--url", &a.url]).args(args));
    let out = remote(&["whoami"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("login"), "{}", String::from_utf8_lossy(&out.stderr));

    let pasted = b.dir.path().join("pasted");
    std::fs::write(&pasted, format!("{secret}\n")).unwrap();
    let out = output(b.command().args(["--url", &a.url, "login"]).stdin(std::fs::File::open(&pasted).unwrap()));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("logged in as agent laptop"));

    let out = remote(&["--as", "laptop", "whoami"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("laptop"));
    // Saved under a's instance, so b's own server and its tokens are untouched.
    let instance = std::fs::read_to_string(a.dir.path().join("data/instance")).unwrap();
    assert!(b.dir.path().join("config/servers").join(instance.trim()).join("agents/laptop").exists());
    assert!(!b.dir.path().join("config/agents/laptop").exists());
}

#[test]
fn a_human_logs_in_on_another_machine_with_their_own_token() {
    let a = Instance::new();
    let secret = a.stdout(&["token", "add", "laptop"]).trim().to_string();
    assert!(secret.starts_with("th_"), "{secret}");
    let b = Instance::new();
    let pasted = b.dir.path().join("pasted");
    std::fs::write(&pasted, format!("{secret}\n")).unwrap();
    let out = output(b.command().args(["--url", &a.url, "login"]).stdin(std::fs::File::open(&pasted).unwrap()));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let me = a.stdout(&["whoami"]);
    let out = output(b.command().args(["--url", &a.url, "whoami"]));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), me);
}

#[test]
fn login_makes_the_other_server_this_machines_default() {
    let a = Instance::new();
    let secret = a.stdout(&["agent", "add", "laptop", "--print-token"]).trim().to_string();
    let b = Instance::new();
    let pasted = b.dir.path().join("pasted");
    std::fs::write(&pasted, format!("{secret}\n")).unwrap();
    let out = output(b.command().args(["--url", &a.url, "login"]).stdin(std::fs::File::open(&pasted).unwrap()));
    assert!(String::from_utf8_lossy(&out.stdout).contains("default server"));
    let out = output(b.command().env_remove("TRAILHEAD_URL").args(["--as", "laptop", "whoami"]));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("laptop"));
}

#[test]
fn health_says_an_upgrade_restart_can_be_asked_for() {
    let th = Instance::new();
    th.stdout(&["whoami"]);
    let v: serde_json::Value = serde_json::from_str(&th.stdout(&["--json", "server", "status"])).unwrap();
    assert_eq!(v["upgrade_pending"], serde_json::json!(false), "{v}");
}

/// `trailhead serve` at a terminal, on the instance's port; killed when dropped.
struct Foreground(std::process::Child);

impl Drop for Foreground {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn serve_in_foreground(th: &Instance) -> Foreground {
    let bind = th.url.trim_start_matches("http://").to_string();
    let mut cmd = th.command();
    cmd.args(["serve", "--bind", &bind]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    // As `common::output` does: another test's fork can briefly hold the fresh binary open for writing.
    let child = (0..50)
        .find_map(|_| match cmd.spawn() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(Duration::from_millis(20));
                None
            }
            r => Some(r.unwrap()),
        })
        .expect("the binary stayed busy");
    let server = Foreground(child);
    for _ in 0..100 {
        if th.pid().is_some() {
            return server;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("the foreground server didn't come up");
}

#[test]
fn a_stale_foreground_server_is_left_running() {
    let th = Instance::new();
    let _server = serve_in_foreground(&th);
    let pid = th.pid().unwrap();

    std::thread::sleep(Duration::from_millis(20));
    th.install();
    let out = th.run(&["whoami"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("running in the foreground"), "{err}");
    assert_eq!(th.pid().unwrap(), pid);
}

#[test]
fn a_second_server_on_the_same_data_dir_is_refused() {
    let th = Instance::new();
    let _server = serve_in_foreground(&th);
    let out = output(th.command().args(["serve", "--bind", "127.0.0.1:0"]));
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("another Trailhead process is using this data directory"), "{err}");
}

/// A server whose `/v1/me` reports an `agent_id` with no `agent`, the answer for a valid agent token whose agent can't
/// be looked up. The real server can't be put in that state (a token whose agent row is gone is refused outright), so
/// this stands in for it, and records the request lines it gets.
struct UnresolvedAgentServer {
    url: String,
    agent_id: uuid::Uuid,
    user_id: uuid::Uuid,
    requests: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl UnresolvedAgentServer {
    fn start() -> Self {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (agent_id, user_id) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = requests.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                // Drain the headers; none of the requests this serves has a body.
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap_or(0) <= 2 {
                        break;
                    }
                }
                let target = line.split_whitespace().nth(1).unwrap_or_default().to_string();
                log.lock().unwrap().push(target.clone());
                let body = if target.starts_with("/v1/me") {
                    serde_json::json!({
                        "workspace_id": uuid::Uuid::now_v7(),
                        "user": {
                            "id": user_id,
                            "workspace_id": uuid::Uuid::now_v7(),
                            "name": "lane",
                            "email": null,
                            "created_at": "2026-01-01T00:00:00Z",
                        },
                        "agent_id": agent_id,
                        "agent": null,
                    })
                    .to_string()
                } else if target.starts_with("/v1/events") {
                    "[]".to_string()
                } else {
                    "{}".to_string()
                };
                let res = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = reader.get_mut().write_all(res.as_bytes());
            }
        });
        UnresolvedAgentServer { url, agent_id, user_id, requests }
    }
}

#[test]
fn login_refuses_and_undo_uses_the_id_when_the_tokens_agent_doesnt_resolve() {
    let server = UnresolvedAgentServer::start();
    let secret = "th_unresolved";

    // Login refuses and saves nothing, not even as the user's token.
    let th = Instance::new();
    let pasted = th.dir.path().join("pasted");
    std::fs::write(&pasted, format!("{secret}\n")).unwrap();
    let out = output(th.command().args(["--url", &server.url, "login"]).stdin(std::fs::File::open(&pasted).unwrap()));
    assert!(!out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(&server.agent_id.to_string()) && stderr.contains("can't look that agent up"), "{stderr}");
    let saved: Vec<_> = walk(&th.dir.path().join("config"))
        .into_iter()
        .filter(|p| p.file_name().is_some_and(|n| n != "config.toml"))
        .collect();
    assert!(saved.is_empty(), "nothing should be saved, found {saved:?}");

    // Undo with no arguments looks for the caller's last batch by the agent's id, not its owner's.
    server.requests.lock().unwrap().clear();
    let out = output(th.command().args(["--url", &server.url, "undo"]).env("TRAILHEAD_TOKEN", secret));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("nothing to undo"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let requests = server.requests.lock().unwrap();
    let events = requests.iter().find(|r| r.starts_with("/v1/events")).unwrap_or_else(|| panic!("{requests:?}"));
    assert!(events.contains(&server.agent_id.to_string()), "{events}");
    assert!(!events.contains(&server.user_id.to_string()), "{events}");
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries.flatten().flat_map(|e| if e.path().is_dir() { walk(&e.path()) } else { vec![e.path()] }).collect()
}
