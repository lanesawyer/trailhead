use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::Duration;

struct Instance {
    dir: tempfile::TempDir,
    url: String,
    exe: PathBuf,
}

impl Instance {
    fn new() -> Instance {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("trailhead");
        let th = Instance { dir, url: format!("http://127.0.0.1:{port}"), exe };
        th.install();
        th
    }

    /// Like `cargo install`: write a fresh copy and rename it over the old one, so a
    /// running server keeps its old inode while the path gets a newer mtime.
    fn install(&self) {
        let staged = self.dir.path().join("trailhead.new");
        std::fs::copy(env!("CARGO_BIN_EXE_trailhead"), &staged).unwrap();
        std::fs::rename(&staged, &self.exe).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(&self.exe)
            .args(args)
            .env("TRAILHEAD_DATA_DIR", self.dir.path().join("data"))
            .env("TRAILHEAD_CONFIG_DIR", self.dir.path().join("config"))
            .env("TRAILHEAD_URL", &self.url)
            .env_remove("TRAILHEAD_TOKEN")
            .env_remove("TRAILHEAD_AGENT")
            .env_remove("TRAILHEAD_PROJECT")
            .output()
            .unwrap()
    }

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(out.status.success(), "{args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }

    fn pid(&self) -> Option<String> {
        let v: serde_json::Value = serde_json::from_str(&self.stdout(&["--json", "server", "status"])).unwrap();
        v["pid"].as_u64().map(|p| p.to_string())
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.run(&["server", "stop"]);
    }
}

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
