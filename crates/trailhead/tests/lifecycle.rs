mod common;

use std::time::Duration;

use common::Instance;

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
    let out = b.command().args(["--url", &a.url, "server", "stop"]).output().unwrap();
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
    let remote =
        |args: &[&str]| b.command().args(["--url", &a.url]).args(args).env("TRAILHEAD_TOKEN", secret).output().unwrap();

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
    let out = b.command().args(["--url", &a.url, "whoami"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(remote(&["server", "stop"]).status.success());
    let out = b.command().args(["--url", &a.url, "whoami"]).output().unwrap();
    assert!(out.status.success(), "b's own server should auto-start again");
    assert!(b.command().args(["--url", &a.url, "server", "stop"]).output().unwrap().status.success());
}
