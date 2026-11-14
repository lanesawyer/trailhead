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
    Check::new(name, command)
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
        branch_if_present: false,
        trim: None,
        disk_warn_bytes: None,
        disk_limit_bytes: None,
        setup: Some(check("setup", "touch .setup-ran")),
        tools: Default::default(),
        tool_hosts: Default::default(),
        env: Default::default(),
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
        branch_if_present: false,
        trim: None,
        disk_warn_bytes: None,
        disk_limit_bytes: None,
        setup: None,
        tools: Default::default(),
        tool_hosts: Default::default(),
        env: Default::default(),
    };
    ws.prepare(&req).await.unwrap();
    assert_eq!(ws.head().await.unwrap(), rev(&src, "factory/w-1"), "the work starts from the PR head");
    assert!(ws.dir().join("feature.txt").exists());

    ws.prepare(&PrepareRequest { branch: None, ..req }).await.unwrap();
    assert_eq!(ws.head().await.unwrap(), rev(&src, "main"), "a later plain prepare is back on the base");
}

#[tokio::test]
async fn stack_env_reaches_checks_and_the_agent() {
    let root = tempfile::tempdir().unwrap();
    let ws = GitWorkspace::new(root.path());
    ws.set_env(&[("CARGO_BUILD_JOBS".to_string(), "2".to_string())].into());
    let result = ws.check(&check("test", "echo jobs=$CARGO_BUILD_JOBS")).await;
    assert_eq!(result.output.trim(), "jobs=2");
    assert_eq!(ws.agent_env().await["CARGO_BUILD_JOBS"], "2");
}

#[tokio::test]
async fn prepare_runs_setup_and_trim_with_the_request_env() {
    let root = tempfile::tempdir().unwrap();
    upstream(root.path());
    let remote = format!("file://{}", root.path().join("remote").display());
    let ws = GitWorkspace::with_remote(root.path().join("work/widget"), remote);
    let req = PrepareRequest {
        repo: Repo { owner: "acme".into(), name: "widget".into() },
        base_branch: "main".into(),
        branch: None,
        merge_base: false,
        branch_if_present: false,
        setup: Some(check("setup", "echo $CARGO_BUILD_JOBS > .setup-jobs")),
        trim: Some(check("trim", "echo $CARGO_BUILD_JOBS > ../trim-jobs")),
        disk_warn_bytes: None,
        disk_limit_bytes: None,
        tools: Default::default(),
        tool_hosts: Default::default(),
        env: [("CARGO_BUILD_JOBS".to_string(), "3".to_string())].into(),
    };
    ws.prepare(&req).await.unwrap();
    assert_eq!(std::fs::read_to_string(ws.dir().join(".setup-jobs")).unwrap().trim(), "3");
    assert_eq!(std::fs::read_to_string(ws.dir().parent().unwrap().join("trim-jobs")).unwrap().trim(), "3");
    assert_eq!(ws.agent_env().await["CARGO_BUILD_JOBS"], "3");
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
        branch_if_present: false,
        trim: None,
        disk_warn_bytes: None,
        disk_limit_bytes: None,
        setup: None,
        tools: Default::default(),
        tool_hosts: Default::default(),
        env: Default::default(),
    };
    ws.prepare(&req).await.unwrap();
    let start = ws.head().await.unwrap();
    assert_eq!(start, rev(&src, "factory/w-1"), "still at the PR head, merge uncommitted");
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

    // Redo the merge and let the agent finish it: once committed, no merge is under way and the base is in HEAD.
    let merge = Command::new("git")
        .args(["-c", "user.name=agent", "-c", "user.email=a@example.com", "merge", "--no-commit", base.as_str()])
        .current_dir(ws.dir())
        .output()
        .unwrap();
    assert!(!merge.status.success(), "the README conflicts");
    assert_eq!(ws.merge_head().await.unwrap().unwrap(), base);
    std::fs::write(ws.dir().join("README.md"), "resolved\n").unwrap();
    git(&ws.dir(), &["add", "README.md"]);
    git(&ws.dir(), &["-c", "user.name=agent", "-c", "user.email=a@example.com", "commit", "--quiet", "--no-edit"]);
    assert_eq!(ws.merge_head().await.unwrap(), None);
    assert!(ws.contains(&base).await.unwrap());
    assert!(ws.contains(&ws.head().await.unwrap()).await.unwrap());
    let head = ws.head().await.unwrap();
    assert_ne!(head, start, "the merge made a new commit");
    assert_eq!(rev(&ws.dir(), &format!("{head}^1")), start);
    assert_eq!(rev(&ws.dir(), &format!("{head}^2")), base, "a merge commit: the old tip and the base");
    assert_eq!(ws.merged_base(&start, "main").await.unwrap(), Some(base.clone()));

    // A later prepare, as from a warm checkpoint taken mid-merge, drops the merge.
    ws.prepare(&PrepareRequest { branch: None, merge_base: false, ..req }).await.unwrap();
    assert_eq!(ws.merge_head().await.unwrap(), None);
    assert_eq!(ws.head().await.unwrap(), rev(&src, "main"));
}

/// A workspace prepared on `factory/w-1`, whose branch and `main` each have a commit of their own touching different
/// files, so merging one into the other never conflicts. Returns the workspace and the start commit.
async fn branch_behind_main(root: &Path) -> (GitWorkspace, String) {
    let src = upstream(root);
    let bare = root.join("remote/acme/widget.git");
    git(&src, &["checkout", "--quiet", "-b", "factory/w-1"]);
    std::fs::write(src.join("branch.txt"), "branch\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "--quiet", "-m", "branch edit"]);
    git(&src, &["push", "--quiet", bare.to_str().unwrap(), "factory/w-1"]);
    git(&src, &["checkout", "--quiet", "main"]);
    std::fs::write(src.join("main.txt"), "main\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "--quiet", "-m", "main edit"]);
    git(&src, &["push", "--quiet", bare.to_str().unwrap(), "main"]);

    let remote = format!("file://{}", root.join("remote").display());
    let ws = GitWorkspace::with_remote(root.join("work/widget"), remote);
    let req = PrepareRequest {
        repo: Repo { owner: "acme".into(), name: "widget".into() },
        base_branch: "main".into(),
        branch: Some("factory/w-1".into()),
        merge_base: false,
        branch_if_present: false,
        trim: None,
        disk_warn_bytes: None,
        disk_limit_bytes: None,
        setup: None,
        tools: Default::default(),
        tool_hosts: Default::default(),
        env: Default::default(),
    };
    ws.prepare(&req).await.unwrap();
    let start = ws.head().await.unwrap();
    (ws, start)
}

fn agent_git(dir: &Path, args: &[&str]) {
    let mut full = vec!["-c", "user.name=agent", "-c", "user.email=a@example.com"];
    full.extend(args);
    git(dir, &full);
}

#[tokio::test]
async fn merged_base_is_the_tip_of_the_base_once_the_agent_commits_its_merge() {
    let root = tempfile::tempdir().unwrap();
    let (ws, start) = branch_behind_main(root.path()).await;
    assert_eq!(ws.merged_base(&start, "main").await.unwrap(), None, "nothing merged yet");
    agent_git(&ws.dir(), &["merge", "--quiet", "--no-edit", "origin/main"]);
    let tip = rev(&ws.dir(), "origin/main");
    assert_eq!(ws.merged_base(&start, "main").await.unwrap(), Some(tip));
}

#[tokio::test]
async fn merged_base_is_none_after_an_ordinary_commit() {
    let root = tempfile::tempdir().unwrap();
    let (ws, start) = branch_behind_main(root.path()).await;
    std::fs::write(ws.dir().join("work.txt"), "work\n").unwrap();
    git(&ws.dir(), &["add", "."]);
    agent_git(&ws.dir(), &["commit", "--quiet", "-m", "work"]);
    assert_eq!(ws.merged_base(&start, "main").await.unwrap(), None);
}

#[tokio::test]
async fn merged_base_is_none_when_the_base_branch_does_not_exist() {
    let root = tempfile::tempdir().unwrap();
    let (ws, start) = branch_behind_main(root.path()).await;
    agent_git(&ws.dir(), &["merge", "--quiet", "--no-edit", "origin/main"]);
    assert_eq!(ws.merged_base(&start, "no-such-branch").await.unwrap(), None);
}

#[tokio::test]
async fn merged_base_is_none_when_the_merge_is_of_something_other_than_the_base() {
    let root = tempfile::tempdir().unwrap();
    let (ws, start) = branch_behind_main(root.path()).await;
    let dir = ws.dir();
    git(&dir, &["checkout", "--quiet", "-b", "side", &start]);
    std::fs::write(dir.join("side.txt"), "side\n").unwrap();
    git(&dir, &["add", "."]);
    agent_git(&dir, &["commit", "--quiet", "-m", "side"]);
    git(&dir, &["checkout", "--quiet", "--detach", &start]);
    std::fs::write(dir.join("work.txt"), "work\n").unwrap();
    git(&dir, &["add", "."]);
    agent_git(&dir, &["commit", "--quiet", "-m", "work"]);
    agent_git(&dir, &["merge", "--quiet", "--no-ff", "--no-edit", "side"]);
    assert_eq!(ws.merged_base(&start, "main").await.unwrap(), None);
}

/// A stand-in for mise that logs its arguments, and runs `exec -- sh -c` with a marker in the environment.
fn fake_mise(dir: &Path) -> std::path::PathBuf {
    let bin = dir.join("mise");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\necho \"$PWD: $*\" >> {log}\ncase \"$1\" in\n  env) echo '{{\"PATH\":\"/pinned/bin:/usr/bin\"}}' ;;\n  exec) shift 2; PINNED=1 exec \"$@\" ;;\nesac\n",
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
        branch_if_present: false,
        setup: Some(check("setup", "echo \"pinned=$PINNED\" > .setup-env")),
        tools: [("node".to_string(), "26".to_string())].into(),
        tool_hosts: Default::default(),
        env: Default::default(),
        trim: None,
        disk_warn_bytes: None,
        disk_limit_bytes: None,
    };
    ws.prepare(&req).await.unwrap();

    let generated = std::fs::read_to_string(conf.join("trailhead-factory.toml")).unwrap();
    assert!(generated.contains("node = \"26\""), "{generated}");
    let log = std::fs::read_to_string(tools.join("mise.log")).unwrap();
    assert!(log.contains("conf.d: bootstrap --only tools"), "installed from outside the checkout: {log}");
    assert_eq!(
        std::fs::read_to_string(ws.dir().join(".setup-env")).unwrap().trim(),
        "pinned=1",
        "setup runs under mise"
    );
    assert_eq!(ws.agent_env().await["PATH"], "/pinned/bin:/usr/bin");
    let result = ws.check(&check("test", "echo $PINNED $MISE_IGNORED_CONFIG_PATHS")).await;
    assert!(result.passed, "{result:?}");
    assert_eq!(result.output.trim(), format!("1 {}", ws.dir().display()), "the repo's own mise config is ignored");

    // Dropping the stack removes the factory's tools again.
    ws.prepare(&PrepareRequest { tools: Default::default(), setup: None, ..req }).await.unwrap();
    assert!(!conf.join("trailhead-factory.toml").exists());
}

#[tokio::test]
async fn the_stack_install_is_retried_while_egress_opens() {
    let root = tempfile::tempdir().unwrap();
    upstream(root.path());
    let tools = root.path().join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    let conf = root.path().join("conf.d");
    let remote = format!("file://{}", root.path().join("remote").display());
    let ws = GitWorkspace::with_remote(root.path().join("work/widget"), remote)
        .with_mise(Mise::with(fake_mise(&tools), &conf));
    let script = std::fs::read_to_string(tools.join("mise")).unwrap();
    let once =
        format!("#!/bin/sh\nif [ ! -e {seen} ]; then touch {seen}; exit 1; fi\n", seen = tools.join("seen").display());
    std::fs::write(tools.join("mise"), script.replacen("#!/bin/sh\n", &once, 1)).unwrap();
    let req = PrepareRequest {
        repo: Repo { owner: "acme".into(), name: "widget".into() },
        base_branch: "main".into(),
        branch: None,
        merge_base: false,
        branch_if_present: false,
        setup: None,
        tools: [("node".to_string(), "26".to_string())].into(),
        tool_hosts: Default::default(),
        env: Default::default(),
        trim: None,
        disk_warn_bytes: None,
        disk_limit_bytes: None,
    };
    ws.prepare(&req).await.unwrap();
    assert!(std::fs::read_to_string(tools.join("mise.log")).unwrap().contains("bootstrap --only tools"));
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

#[tokio::test]
async fn conflict_markers_the_merged_in_side_already_has_are_not_the_agents() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("work");
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "--quiet", "-b", "main"]);
    git(&dir, &["config", "user.email", "a@b.c"]);
    git(&dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("a.md"), "a\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "--quiet", "-m", "one"]);
    git(&dir, &["checkout", "--quiet", "-b", "pr"]);
    std::fs::write(dir.join("pr.txt"), "pr\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "--quiet", "-m", "pr"]);
    git(&dir, &["checkout", "--quiet", "main"]);
    std::fs::write(dir.join("doc.md"), ">>>>>>> quoted\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "--quiet", "-m", "base"]);
    git(&dir, &["checkout", "--quiet", "pr"]);
    let ws = GitWorkspace::new(&dir);
    let start = ws.head().await.unwrap();

    git(&dir, &["merge", "--quiet", "--no-ff", "--no-commit", "main"]);
    assert!(ws.conflict_markers(&start).await.unwrap().is_empty(), "uncommitted merge");
    std::fs::write(dir.join("pr.txt"), "<<<<<<< HEAD\n").unwrap();
    assert_eq!(ws.conflict_markers(&start).await.unwrap(), ["pr.txt"]);
    std::fs::write(dir.join("pr.txt"), "pr\n").unwrap();
    git(&dir, &["commit", "--quiet", "-am", "merge"]);
    assert!(ws.conflict_markers(&start).await.unwrap().is_empty(), "committed merge");
}

#[tokio::test]
async fn prepare_resets_a_checkout_left_in_a_conflicted_merge() {
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
    let ws = GitWorkspace::with_remote(root.path().join("work/widget"), remote);
    let req = PrepareRequest {
        repo: Repo { owner: "acme".into(), name: "widget".into() },
        base_branch: "main".into(),
        branch: Some("factory/w-1".into()),
        merge_base: true,
        branch_if_present: false,
        trim: None,
        disk_warn_bytes: None,
        disk_limit_bytes: None,
        setup: None,
        tools: Default::default(),
        tool_hosts: Default::default(),
        env: Default::default(),
    };
    let status = || {
        let out = Command::new("git").args(["status", "--porcelain"]).current_dir(ws.dir()).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    ws.prepare(&req).await.unwrap();
    // As a warm checkpoint would hold it: conflicted index, MERGE_HEAD and all.
    assert!(ws.merge_head().await.unwrap().is_some());
    assert!(status().contains("UU README.md"), "{}", status());

    // Prepared again without aborting by hand: a fresh merge, not the old one.
    ws.prepare(&req).await.unwrap();
    assert!(ws.merge_head().await.unwrap().is_some());

    ws.prepare(&PrepareRequest { branch: None, merge_base: false, ..req }).await.unwrap();
    assert_eq!(ws.merge_head().await.unwrap(), None);
    assert_eq!(ws.head().await.unwrap(), rev(&src, "main"));
    assert_eq!(status(), "");
}

#[tokio::test]
async fn prepare_runs_the_trim_check_after_setup_and_survives_its_failure() {
    let root = tempfile::tempdir().unwrap();
    upstream(root.path());
    let remote = format!("file://{}", root.path().join("remote").display());
    let ws = GitWorkspace::with_remote(root.path().join("work/widget"), remote);
    let req = PrepareRequest {
        repo: Repo { owner: "acme".into(), name: "widget".into() },
        base_branch: "main".into(),
        branch: None,
        merge_base: false,
        branch_if_present: false,
        setup: Some(check("setup", "mkdir -p cache && touch cache/big")),
        trim: Some(check("trim", "rm -rf cache")),
        disk_warn_bytes: None,
        disk_limit_bytes: None,
        tools: Default::default(),
        tool_hosts: Default::default(),
        env: Default::default(),
    };
    ws.prepare(&req).await.unwrap();
    assert!(!ws.dir().join("cache").exists());

    let failing = PrepareRequest { trim: Some(check("trim", "echo nope; exit 1")), ..req };
    ws.prepare(&failing).await.unwrap();
}

#[tokio::test]
async fn prepare_refuses_when_the_disk_is_over_the_limit() {
    let root = tempfile::tempdir().unwrap();
    upstream(root.path());
    let remote = format!("file://{}", root.path().join("remote").display());
    let ws = GitWorkspace::with_remote(root.path().join("work/widget"), remote);
    let req = PrepareRequest {
        repo: Repo { owner: "acme".into(), name: "widget".into() },
        base_branch: "main".into(),
        branch: None,
        merge_base: false,
        branch_if_present: false,
        setup: None,
        trim: None,
        disk_warn_bytes: Some(1),
        disk_limit_bytes: Some(1),
        tools: Default::default(),
        tool_hosts: Default::default(),
        env: Default::default(),
    };
    let err = ws.prepare(&req).await.unwrap_err();
    assert!(err.to_string().contains("disk_limit_bytes"), "{err}");
    let ok = PrepareRequest { disk_limit_bytes: Some(u64::MAX), ..req };
    ws.prepare(&ok).await.unwrap();
}

#[tokio::test]
async fn changed_files_lists_committed_uncommitted_and_new_files_against_the_base() {
    let root = tempfile::tempdir().unwrap();
    upstream(root.path());
    let remote = format!("file://{}", root.path().join("remote").display());
    let ws = GitWorkspace::with_remote(root.path().join("work/widget"), remote);
    let req = PrepareRequest {
        repo: Repo { owner: "acme".into(), name: "widget".into() },
        base_branch: "main".into(),
        branch: None,
        merge_base: false,
        branch_if_present: false,
        trim: None,
        disk_warn_bytes: None,
        disk_limit_bytes: None,
        setup: None,
        tools: Default::default(),
        tool_hosts: Default::default(),
        env: Default::default(),
    };
    ws.prepare(&req).await.unwrap();
    assert!(ws.changed_files("main").await.unwrap().is_empty());

    let dir = ws.dir();
    git(&dir, &["config", "user.email", "t@example.com"]);
    git(&dir, &["config", "user.name", "t"]);
    std::fs::create_dir_all(dir.join("clients/ui")).unwrap();
    std::fs::write(dir.join("clients/ui/main.rs"), "fn main() {}\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "--quiet", "-m", "ui"]);
    std::fs::write(dir.join("README.md"), "edited\n").unwrap();
    std::fs::write(dir.join("new.txt"), "new\n").unwrap();

    let mut files = ws.changed_files("main").await.unwrap();
    files.sort();
    assert_eq!(files, ["README.md", "clients/ui/main.rs", "new.txt"]);
}

#[tokio::test]
async fn unpublished_sees_uncommitted_and_unpushed_work_but_not_a_clean_checkout() {
    let root = tempfile::tempdir().unwrap();
    upstream(root.path());
    let remote = format!("file://{}", root.path().join("remote").display());
    let ws = GitWorkspace::with_remote(root.path().join("work/widget"), remote);
    let req = PrepareRequest {
        repo: Repo { owner: "acme".into(), name: "widget".into() },
        base_branch: "main".into(),
        branch: None,
        merge_base: false,
        branch_if_present: false,
        trim: None,
        disk_warn_bytes: None,
        disk_limit_bytes: None,
        setup: None,
        tools: Default::default(),
        tool_hosts: Default::default(),
        env: Default::default(),
    };
    ws.prepare(&req).await.unwrap();
    assert!(!ws.unpublished("main").await.unwrap(), "a fresh checkout has nothing to lose");

    let dir = ws.dir();
    git(&dir, &["config", "user.email", "t@example.com"]);
    git(&dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("new.txt"), "new\n").unwrap();
    assert!(ws.unpublished("main").await.unwrap(), "an untracked file");
    git(&dir, &["add", "."]);
    assert!(ws.unpublished("main").await.unwrap(), "a staged one");
    git(&dir, &["commit", "--quiet", "-m", "new"]);
    assert!(ws.unpublished("main").await.unwrap(), "a commit nobody has");
    assert!(ws.unpublished("nonesuch").await.is_err(), "a remote branch it can't compare with is an error");
}

#[tokio::test]
async fn changed_files_leaves_out_base_changes_merged_in_by_a_rework() {
    let root = tempfile::tempdir().unwrap();
    let src = upstream(root.path());
    let bare = root.path().join("remote/acme/widget.git");
    git(&src, &["checkout", "--quiet", "-b", "factory/w-1"]);
    std::fs::write(src.join("pr.txt"), "pr\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "--quiet", "-m", "pr"]);
    git(&src, &["push", "--quiet", bare.to_str().unwrap(), "factory/w-1"]);
    git(&src, &["checkout", "--quiet", "main"]);
    std::fs::write(src.join("base.txt"), "base\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "--quiet", "-m", "base"]);
    git(&src, &["push", "--quiet", bare.to_str().unwrap(), "main"]);

    let remote = format!("file://{}", root.path().join("remote").display());
    let ws = GitWorkspace::with_remote(root.path().join("work/widget"), remote);
    let req = PrepareRequest {
        repo: Repo { owner: "acme".into(), name: "widget".into() },
        base_branch: "main".into(),
        branch: Some("factory/w-1".into()),
        merge_base: true,
        branch_if_present: false,
        trim: None,
        disk_warn_bytes: None,
        disk_limit_bytes: None,
        setup: None,
        tools: Default::default(),
        tool_hosts: Default::default(),
        env: Default::default(),
    };
    ws.prepare(&req).await.unwrap();
    assert!(ws.merge_head().await.unwrap().is_some());
    assert!(ws.dir().join("base.txt").exists(), "the merge brought the base change in");
    assert_eq!(ws.changed_files("main").await.unwrap(), ["pr.txt"]);

    // The agent fetches mid-session and the base moves on: the diff stays pinned to the commit that was merged.
    std::fs::write(src.join("later.txt"), "later\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "--quiet", "-m", "later"]);
    git(&src, &["push", "--quiet", bare.to_str().unwrap(), "main"]);
    git(&ws.dir(), &["fetch", "--quiet", "origin"]);
    assert_eq!(ws.changed_files("main").await.unwrap(), ["pr.txt"]);
}

#[tokio::test]
async fn a_branch_published_through_the_api_counts_as_published_and_a_resume_builds_on_it() {
    let root = tempfile::tempdir().unwrap();
    upstream(root.path());
    let bare = root.path().join("remote/acme/widget.git");
    let remote = format!("file://{}", root.path().join("remote").display());
    let ws = GitWorkspace::with_remote(root.path().join("work/widget"), remote);
    let req = PrepareRequest {
        repo: Repo { owner: "acme".into(), name: "widget".into() },
        base_branch: "main".into(),
        branch: Some("factory/trail-1".into()),
        branch_if_present: true,
        merge_base: false,
        trim: None,
        disk_warn_bytes: None,
        disk_limit_bytes: None,
        setup: None,
        tools: Default::default(),
        tool_hosts: Default::default(),
        env: Default::default(),
    };
    ws.prepare(&req).await.unwrap();
    let dir = ws.dir();
    assert_eq!(ws.head().await.unwrap(), rev(&dir, "origin/main"), "a branch the remote lacks leaves it on the base");
    assert!(ws.unpublished("factory/trail-1").await.is_err(), "and there is nothing to compare with");

    git(&dir, &["config", "user.email", "t@example.com"]);
    git(&dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("new.txt"), "new\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "--quiet", "-m", "new"]);
    assert!(ws.unpublished("main").await.unwrap());

    // The API makes its own commit for the same content, so the local one is no ancestor of the branch.
    let tmp = root.path().join("api");
    git(root.path(), &["clone", "--quiet", bare.to_str().unwrap(), tmp.to_str().unwrap()]);
    git(&tmp, &["config", "user.email", "api@example.com"]);
    git(&tmp, &["config", "user.name", "api"]);
    std::fs::write(tmp.join("new.txt"), "new\n").unwrap();
    git(&tmp, &["add", "."]);
    git(&tmp, &["commit", "--quiet", "-m", "published"]);
    git(&tmp, &["push", "--quiet", "origin", "HEAD:refs/heads/factory/trail-1"]);
    assert!(!ws.unpublished("factory/trail-1").await.unwrap(), "the same content is on the branch");

    std::fs::write(dir.join("new.txt"), "newer\n").unwrap();
    git(&dir, &["commit", "--quiet", "-am", "more"]);
    assert!(ws.unpublished("factory/trail-1").await.unwrap(), "different content isn't");

    // A resume now builds on the pushed branch.
    ws.prepare(&req).await.unwrap();
    assert_eq!(ws.head().await.unwrap(), rev(&tmp, "HEAD"));
}

#[tokio::test]
async fn merged_base_follows_a_fetch_that_moves_the_base_after_the_merge() {
    let root = tempfile::tempdir().unwrap();
    let (ws, start) = branch_behind_main(root.path()).await;
    agent_git(&ws.dir(), &["merge", "--quiet", "--no-edit", "origin/main"]);
    let merged = rev(&ws.dir(), "origin/main");
    let src = root.path().join("src");
    let bare = root.path().join("remote/acme/widget.git");
    git(&src, &["checkout", "--quiet", "main"]);
    std::fs::write(src.join("later.txt"), "later\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "--quiet", "-m", "later"]);
    git(&src, &["push", "--quiet", bare.to_str().unwrap(), "main"]);
    git(&ws.dir(), &["fetch", "--quiet", "origin"]);
    assert_ne!(rev(&ws.dir(), "origin/main"), merged);
    assert_eq!(ws.merged_base(&start, "main").await.unwrap(), Some(merged));
}

#[tokio::test]
async fn merged_base_sees_a_fast_forward_to_the_base() {
    let root = tempfile::tempdir().unwrap();
    let (ws, _) = branch_behind_main(root.path()).await;
    let dir = ws.dir();
    // A branch with nothing of its own, behind main, so the merge fast-forwards.
    git(&dir, &["checkout", "--quiet", "--detach", "origin/main~1"]);
    let start = ws.head().await.unwrap();
    agent_git(&dir, &["merge", "--quiet", "--no-edit", "origin/main"]);
    assert_eq!(ws.head().await.unwrap(), rev(&dir, "origin/main"));
    assert_eq!(ws.merged_base(&start, "main").await.unwrap(), Some(rev(&dir, "origin/main")));
}
