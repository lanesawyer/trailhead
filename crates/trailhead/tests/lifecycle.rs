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
