//! `GitWorkspace` against a local repo standing in for GitHub.

use std::path::Path;
use std::process::Command;

use trailhead_daemon::{GitWorkspace, Workspace};
use trailhead_factory::{Check, PrepareRequest, Repo};

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// `<root>/acme/widget.git` with one commit on `main`.
fn upstream(root: &Path) -> std::path::PathBuf {
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "--quiet", "--initial-branch", "main"]);
    git(&src, &["config", "user.email", "t@example.com"]);
    git(&src, &["config", "user.name", "t"]);
    std::fs::write(src.join("README.md"), "v1\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "--quiet", "-m", "v1"]);
    let bare = root.join("remote/acme/widget.git");
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    git(root, &["clone", "--quiet", "--bare", src.to_str().unwrap(), bare.to_str().unwrap()]);
    src
}

fn check(name: &str, command: &str) -> Check {
    Check { name: name.into(), command: command.into() }
}

#[tokio::test]
async fn prepare_clones_runs_setup_and_resets() {
    let root = tempfile::tempdir().unwrap();
    let src = upstream(root.path());
    let remote = format!("file://{}", root.path().join("remote").display());
    let ws = GitWorkspace::with_remote(root.path().join("work/widget"), remote);
    let req = PrepareRequest {
        repo: Repo { owner: "acme".into(), name: "widget".into() },
        base_branch: "main".into(),
        setup: Some(check("setup", "touch .setup-ran")),
    };

    ws.prepare(&req).await.unwrap();
    assert_eq!(std::fs::read_to_string(ws.dir().join("README.md")).unwrap(), "v1\n");
    assert!(ws.dir().join(".setup-ran").exists());

    // A later prepare picks up new upstream commits and drops the agent's leftovers, but keeps ignored files.
    std::fs::write(src.join("README.md"), "v2\n").unwrap();
    std::fs::write(src.join(".gitignore"), "target/\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "--quiet", "-m", "v2"]);
    git(&src, &["push", "--quiet", root.path().join("remote/acme/widget.git").to_str().unwrap(), "main"]);
    std::fs::write(ws.dir().join("scratch.txt"), "leftover").unwrap();
    std::fs::create_dir_all(ws.dir().join("target")).unwrap();
    std::fs::write(ws.dir().join("target/cache"), "keep").unwrap();
    ws.prepare(&req).await.unwrap();
    assert_eq!(std::fs::read_to_string(ws.dir().join("README.md")).unwrap(), "v2\n");
    assert!(!ws.dir().join("scratch.txt").exists());
    assert!(ws.dir().join("target/cache").exists(), "build caches survive");

    let failing = PrepareRequest { setup: Some(check("setup", "echo boom; exit 3")), ..req };
    let err = ws.prepare(&failing).await.unwrap_err().to_string();
    assert!(err.contains("setup failed") && err.contains("boom"), "{err}");
}

#[tokio::test]
async fn checks_report_exit_status_and_output() {
    let root = tempfile::tempdir().unwrap();
    let ws = GitWorkspace::new(root.path());
    let pass = ws.check(&check("fmt", "echo formatted")).await;
    assert!(pass.passed);
    assert_eq!(pass.output, "formatted\n");
    let fail = ws.check(&check("test", "echo out; echo err >&2; exit 1")).await;
    assert!(!fail.passed);
    assert_eq!(fail.output, "out\nerr\n", "stderr is folded into the output");
}
