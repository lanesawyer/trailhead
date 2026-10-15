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
