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

fn rev(dir: &Path, rev: &str) -> String {
    let out = Command::new("git").args(["rev-parse", rev]).current_dir(dir).output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
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
        branch: None,
        merge_base: false,
        setup: Some(check("setup", "touch .setup-ran")),
    };

    ws.prepare(&req).await.unwrap();
    assert_eq!(std::fs::read_to_string(ws.dir().join("README.md")).unwrap(), "v1\n");
    assert_eq!(ws.head().await.unwrap(), rev(&src, "main"), "the commit the work starts from");
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
    assert_eq!(ws.head().await.unwrap(), rev(&src, "main"));
    assert!(!ws.dir().join("scratch.txt").exists());
    assert!(ws.dir().join("target/cache").exists(), "build caches survive");

    let failing = PrepareRequest { setup: Some(check("setup", "echo boom; exit 3")), ..req };
    let err = ws.prepare(&failing).await.unwrap_err().to_string();
    assert!(err.contains("setup failed") && err.contains("boom"), "{err}");
}

#[tokio::test]
async fn prepare_can_check_out_a_branchs_remote_head() {
    let root = tempfile::tempdir().unwrap();
    let src = upstream(root.path());
    let bare = root.path().join("remote/acme/widget.git");
    git(&src, &["checkout", "--quiet", "-b", "factory/w-1"]);
    std::fs::write(src.join("feature.txt"), "work\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "--quiet", "-m", "feature"]);
    git(&src, &["push", "--quiet", bare.to_str().unwrap(), "factory/w-1"]);

    let remote = format!("file://{}", root.path().join("remote").display());
    let ws = GitWorkspace::with_remote(root.path().join("work/widget"), remote);
    let req = PrepareRequest {
        repo: Repo { owner: "acme".into(), name: "widget".into() },
        base_branch: "main".into(),
        branch: Some("factory/w-1".into()),
        merge_base: false,
        setup: None,
    };
    ws.prepare(&req).await.unwrap();
    assert_eq!(ws.head().await.unwrap(), rev(&src, "factory/w-1"), "the work starts from the PR head");
    assert!(ws.dir().join("feature.txt").exists());

    ws.prepare(&PrepareRequest { branch: None, ..req }).await.unwrap();
    assert_eq!(ws.head().await.unwrap(), rev(&src, "main"), "a later plain prepare is back on the base");
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

#[tokio::test]
async fn confirm_blocked_waits_for_hosts_to_stop_resolving() {
    let root = tempfile::tempdir().unwrap();
    let ws = GitWorkspace::new(root.path()).with_block_patience(std::time::Duration::from_millis(500));
    ws.confirm_blocked(&["nothing-here.invalid".into()]).await.unwrap();
    let err = ws.confirm_blocked(&["localhost".into()]).await.unwrap_err();
    assert!(matches!(err, trailhead_factory::Error::Forbidden(ref m) if m.contains("localhost")), "{err:?}");
}

#[tokio::test]
async fn sprite_holds_heartbeat_then_linger() {
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use trailhead_daemon::awake::SpriteHolds;

    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("api.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = seen.clone();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            let mut buf = vec![0u8; 4096];
            let n = s.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            let line = req.lines().next().unwrap().to_string();
            let body = req.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
            log.lock().unwrap().push(format!("{line} {body}").trim().to_string());
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await.unwrap();
        }
    });

    let holds = SpriteHolds::new(&socket);
    let hold = holds.hold();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    drop(hold);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let seen = seen.lock().unwrap().clone();
    assert_eq!(
        seen,
        [
            r#"PUT /v1/tasks/trailhead-daemon-1 HTTP/1.1 {"expire":"5m"}"#,
            r#"PUT /v1/tasks/trailhead-daemon-1 HTTP/1.1 {"expire":"90s"}"#,
        ]
    );
}

#[tokio::test]
async fn prepare_can_start_merging_the_base_into_the_branch_leaving_the_conflicts() {
    let root = tempfile::tempdir().unwrap();
    let src = upstream(root.path());
    let bare = root.path().join("remote/acme/widget.git");
    git(&src, &["checkout", "--quiet", "-b", "factory/w-1"]);
    std::fs::write(src.join("README.md"), "branch\n").unwrap();
    git(&src, &["commit", "--quiet", "-am", "branch edit"]);
    git(&src, &["push", "--quiet", bare.to_str().unwrap(), "factory/w-1"]);
    git(&src, &["checkout", "--quiet", "main"]);
    std::fs::write(src.join("README.md"), "main\n").unwrap();
    git(&src, &["commit", "--quiet", "-am", "main edit"]);
    git(&src, &["push", "--quiet", bare.to_str().unwrap(), "main"]);

    let remote = format!("file://{}", root.path().join("remote").display());
    // No global git identity, as on a CI runner: the merge must not depend on the machine's config.
    let home = root.path().join("empty-home");
    std::fs::create_dir_all(&home).unwrap();
    let ws = GitWorkspace::with_remote(root.path().join("work/widget"), remote)
        .with_git_env("HOME", home.to_str().unwrap())
        .with_git_env("XDG_CONFIG_HOME", home.to_str().unwrap())
        .with_git_env("GIT_CONFIG_GLOBAL", "/dev/null")
        .with_git_env("GIT_CONFIG_NOSYSTEM", "1");
    let req = PrepareRequest {
        repo: Repo { owner: "acme".into(), name: "widget".into() },
        base_branch: "main".into(),
        branch: Some("factory/w-1".into()),
        merge_base: true,
        setup: None,
    };
    ws.prepare(&req).await.unwrap();
    assert_eq!(ws.head().await.unwrap(), rev(&src, "factory/w-1"), "still at the PR head, merge uncommitted");
    assert_eq!(ws.merge_head().await.unwrap().unwrap(), rev(&src, "main"));
    let readme = std::fs::read_to_string(ws.dir().join("README.md")).unwrap();
    assert!(readme.contains("<<<<<<<") && readme.contains("main") && readme.contains("branch"), "{readme}");

    // A later prepare, as from a warm checkpoint taken mid-merge, drops the merge.
    ws.prepare(&PrepareRequest { branch: None, merge_base: false, ..req }).await.unwrap();
    assert_eq!(ws.merge_head().await.unwrap(), None);
    assert_eq!(ws.head().await.unwrap(), rev(&src, "main"));
}

#[tokio::test]
async fn conflict_markers_added_since_the_start_are_found_in_any_file() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("work");
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "--quiet", "-b", "main"]);
    git(&dir, &["config", "user.email", "a@b.c"]);
    git(&dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("a.md"), "a\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "--quiet", "-m", "one"]);
    let ws = GitWorkspace::new(&dir);
    let start = ws.head().await.unwrap();
    assert!(ws.conflict_markers(&start).await.unwrap().is_empty());

    std::fs::write(dir.join("a.md"), "a\n=======\n").unwrap();
    assert!(ws.conflict_markers(&start).await.unwrap().is_empty(), "a rule alone isn't a marker");
    std::fs::write(dir.join("a.md"), "a\n<<<<<<< HEAD\nx\n=======\ny\n>>>>>>> main\n").unwrap();
    std::fs::write(dir.join("new.toml"), ">>>>>>> main\n").unwrap();
    assert_eq!(ws.conflict_markers(&start).await.unwrap(), ["a.md", "new.toml"]);
}
