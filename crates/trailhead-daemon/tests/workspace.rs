//! `GitWorkspace` against a local repo standing in for GitHub.

use std::path::Path;
use std::process::Command;

use trailhead_daemon::{GitWorkspace, Mise, Workspace};
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

/// Adds a dev-machine `mise.toml` to the upstream repo.
fn add_mise_toml(root: &Path, src: &Path) {
    std::fs::write(src.join("mise.toml"), "[tools]\nlima = \"latest\"\n").unwrap();
    git(src, &["add", "."]);
    git(src, &["commit", "--quiet", "-m", "mise"]);
    git(src, &["push", "--quiet", root.join("remote/acme/widget.git").to_str().unwrap(), "main"]);
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
        tools: Default::default(),
        tool_hosts: Default::default(),
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
        tools: Default::default(),
        tool_hosts: Default::default(),
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
        tools: Default::default(),
        tool_hosts: Default::default(),
    };
    ws.prepare(&req).await.unwrap();
    assert_eq!(ws.head().await.unwrap(), rev(&src, "factory/w-1"), "still at the PR head, merge uncommitted");
    assert_eq!(ws.merge_head().await.unwrap().unwrap(), rev(&src, "main"));
    let readme = std::fs::read_to_string(ws.dir().join("README.md")).unwrap();
    assert!(readme.contains("<<<<<<<") && readme.contains("main") && readme.contains("branch"), "{readme}");

    // The base isn't in HEAD until the merge is committed, and aborting it never puts it there.
    let base = rev(&src, "main");
    assert!(!ws.contains(&base).await.unwrap());
    git(&ws.dir(), &["merge", "--abort"]);
    assert_eq!(ws.merge_head().await.unwrap(), None);
    assert!(!ws.contains(&base).await.unwrap());
    assert!(ws.contains(&ws.head().await.unwrap()).await.unwrap());

    // A later prepare, as from a warm checkpoint taken mid-merge, drops the merge.
    ws.prepare(&PrepareRequest { branch: None, merge_base: false, ..req }).await.unwrap();
    assert_eq!(ws.merge_head().await.unwrap(), None);
    assert_eq!(ws.head().await.unwrap(), rev(&src, "main"));
}

/// A stand-in for mise that logs its arguments, and runs `exec -- sh -c` with a marker in the environment.
fn fake_mise(dir: &Path) -> std::path::PathBuf {
    let bin = dir.join("mise");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\necho \"$PWD: $*\" >> {log}\ncase \"$1\" in\n  bootstrap) if [ \"$2\" = plan ]; then echo 'plan: tools'; fi ;;\n  \
             env) echo '{{\"PATH\":\"/pinned/bin:/usr/bin\"}}' ;;\n  exec) shift 2; PINNED=1 exec \"$@\" ;;\nesac\n",
            log = dir.join("mise.log").display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

#[tokio::test]
async fn prepare_installs_the_stack_with_mise_and_checks_run_under_it() {
    let root = tempfile::tempdir().unwrap();
    let src = upstream(root.path());
    add_mise_toml(root.path(), &src);
    let tools = root.path().join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    let conf = root.path().join("conf.d");
    let remote = format!("file://{}", root.path().join("remote").display());
    let ws = GitWorkspace::with_remote(root.path().join("work/widget"), remote)
        .with_mise(Mise::with(fake_mise(&tools), &conf));
    let req = PrepareRequest {
        repo: Repo { owner: "acme".into(), name: "widget".into() },
        base_branch: "main".into(),
        branch: None,
        merge_base: false,
        setup: Some(check("setup", "echo \"pinned=$PINNED\" > .setup-env")),
        tools: [("node".to_string(), "22".to_string())].into(),
        tool_hosts: Default::default(),
    };
    ws.prepare(&req).await.unwrap();

    let generated = std::fs::read_to_string(conf.join("trailhead-factory.toml")).unwrap();
    assert!(generated.contains("node = \"22\""), "{generated}");
    let log = std::fs::read_to_string(tools.join("mise.log")).unwrap();
    let plan = log.find("bootstrap plan").expect("the plan is logged first");
    let apply = log.find("bootstrap --only packages,tools,task").expect("only the allowed parts run");
    assert!(plan < apply, "{log}");
    assert_eq!(
        std::fs::read_to_string(ws.dir().join(".setup-env")).unwrap().trim(),
        "pinned=1",
        "setup runs under mise"
    );
    assert_eq!(ws.agent_env().await["PATH"], "/pinned/bin:/usr/bin");
    let result = ws.check(&check("test", "echo $PINNED")).await;
    assert!(result.passed && result.output.trim() == "1", "{result:?}");

    // Dropping the stack removes the factory's tools again.
    ws.prepare(&PrepareRequest { tools: Default::default(), setup: None, ..req }).await.unwrap();
    assert!(!conf.join("trailhead-factory.toml").exists());
}

#[tokio::test]
async fn a_repos_own_mise_config_alone_bootstraps_best_effort() {
    let root = tempfile::tempdir().unwrap();
    let src = upstream(root.path());
    add_mise_toml(root.path(), &src);
    let tools = root.path().join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    let conf = root.path().join("conf.d");
    let remote = format!("file://{}", root.path().join("remote").display());
    let ws = GitWorkspace::with_remote(root.path().join("work/widget"), remote)
        .with_mise(Mise::with(fake_mise(&tools), &conf));
    let req = PrepareRequest {
        repo: Repo { owner: "acme".into(), name: "widget".into() },
        base_branch: "main".into(),
        branch: None,
        merge_base: false,
        setup: None,
        tools: Default::default(),
        tool_hosts: Default::default(),
    };
    ws.prepare(&req).await.unwrap();

    // The repo's bootstrap fails (a dev-machine tool that can't install here): logged, not fatal.
    std::fs::write(tools.join("mise"), "#!/bin/sh\necho \"$*\" >> \"$(dirname \"$0\")/mise.log\"\nexit 1\n").unwrap();
    ws.prepare(&req).await.unwrap();
    let log = std::fs::read_to_string(tools.join("mise.log")).unwrap();
    assert!(log.contains("bootstrap --only packages,tools,task"), "{log}");
    assert!(!conf.join("trailhead-factory.toml").exists());
}

#[tokio::test]
async fn a_failing_repo_config_does_not_fail_the_stack() {
    let root = tempfile::tempdir().unwrap();
    let src = upstream(root.path());
    add_mise_toml(root.path(), &src);
    let tools = root.path().join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    let conf = root.path().join("conf.d");
    let remote = format!("file://{}", root.path().join("remote").display());
    let ws = GitWorkspace::with_remote(root.path().join("work/widget"), remote)
        .with_mise(Mise::with(fake_mise(&tools), &conf));
    let req = PrepareRequest {
        repo: Repo { owner: "acme".into(), name: "widget".into() },
        base_branch: "main".into(),
        branch: None,
        merge_base: false,
        setup: None,
        tools: [("node".to_string(), "22".to_string())].into(),
        tool_hosts: Default::default(),
    };
    ws.prepare(&req).await.unwrap();
    // Succeeds for the factory's tools (run outside the checkout), fails inside it.
    std::fs::write(
        tools.join("mise"),
        "#!/bin/sh\necho \"$PWD: $*\" >> \"$(dirname \"$0\")/mise.log\"\ncase \"$PWD\" in */widget) exit 1;; esac\n",
    )
    .unwrap();
    ws.prepare(&req).await.unwrap();
    let log = std::fs::read_to_string(tools.join("mise.log")).unwrap();
    assert!(log.contains("conf.d: bootstrap --only tools"), "{log}");
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
