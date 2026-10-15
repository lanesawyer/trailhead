#![allow(dead_code)]

use std::path::PathBuf;
use std::process::{Command, Output};

pub struct Instance {
    pub dir: tempfile::TempDir,
    pub url: String,
    exe: PathBuf,
}

impl Instance {
    pub fn new() -> Instance {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("trailhead");
        let th = Instance { dir, url: format!("http://127.0.0.1:{port}"), exe };
        th.install();
        th
    }

    /// Like `cargo install`: write a fresh copy and rename it over the old one, so a
    /// running server keeps its old inode while the path gets a newer mtime.
    pub fn install(&self) {
        let staged = self.dir.path().join("trailhead.new");
        std::fs::copy(env!("CARGO_BIN_EXE_trailhead"), &staged).unwrap();
        std::fs::rename(&staged, &self.exe).unwrap();
    }

    pub fn command(&self) -> Command {
        let mut cmd = Command::new(&self.exe);
        cmd.env("TRAILHEAD_DATA_DIR", self.dir.path().join("data"))
            .env("TRAILHEAD_CONFIG_DIR", self.dir.path().join("config"))
            .env("TRAILHEAD_URL", &self.url)
            .env_remove("TRAILHEAD_TOKEN")
            .env_remove("TRAILHEAD_AGENT")
            .env_remove("TRAILHEAD_PROJECT")
            .env_remove("TRAILHEAD_DB")
            .env_remove("TRAILHEAD_BIND");
        cmd
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    pub fn stdout(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(out.status.success(), "{args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }

    pub fn pid(&self) -> Option<String> {
        let v: serde_json::Value = serde_json::from_str(&self.stdout(&["--json", "server", "status"])).unwrap();
        v["pid"].as_u64().map(|p| p.to_string())
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.run(&["server", "stop"]);
    }
}
