use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::process::Command;
use trailhead_factory::{Check, CheckResult, Error, InstallHost, PrepareRequest, Result};

use crate::{Mise, Workspace};

/// Keep the end of a check's output, where failures usually are.
const OUTPUT_TAIL: usize = 16 * 1024;

/// Git sends a request body bigger than `http.postBuffer` chunked, which the gateway refuses (it wants a
/// Content-Length, 32 MiB at most). Set to that limit, a fetch is chunked only if the gateway would refuse it anyway.
const POST_BUFFER: &str = "http.postBuffer=33554432";

/// How long to wait for narrowed egress to take effect. Sprites took under a second in M0's tests.
const BLOCK_PATIENCE: Duration = Duration::from_secs(15);

/// Finds where repos are cloned from, asked each time a clone needs it.
pub type FindRemote =
    Arc<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send>> + Send + Sync>;

enum Remote {
    Fixed(String),
    Found(FindRemote),
}

/// A git checkout of a GitHub repo, cloned during prepare: anonymously from github.com, or from wherever
/// `with_remote` or `with_found_remote` points (a gateway git connection, for private repos).
pub struct GitWorkspace {
    dir: PathBuf,
    /// Where repos are cloned from; tests point it at a local directory.
    remote: Remote,
    block_patience: Duration,
    /// Extra environment for git; tests use it to hide the machine's git config.
    git_env: Vec<(String, String)>,
    mise: Mise,
    /// `[factory.stack] env`, from the latest prepare or run request.
    env: std::sync::Mutex<std::collections::BTreeMap<String, String>>,
}

impl GitWorkspace {
    async fn is_ancestor(&self, commit: &str, of: &str) -> Result<bool> {
        let status = Command::new("git")
            .args(["merge-base", "--is-ancestor", commit, of])
            .envs(self.git_env.clone())
            .current_dir(&self.dir)
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| Error::Other(format!("git merge-base: {e}")))?
            .status;
        match status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(Error::Other(format!("git merge-base --is-ancestor {commit} {of} failed"))),
        }
    }

    pub fn new(dir: impl Into<PathBuf>) -> GitWorkspace {
        GitWorkspace::with_remote(dir, "https://github.com")
    }

    pub fn with_remote(dir: impl Into<PathBuf>, remote_base: impl Into<String>) -> GitWorkspace {
        GitWorkspace::cloning_from(dir, Remote::Fixed(remote_base.into()))
    }

    /// Clones from wherever `find` says at the time of the clone. An error from it fails that prepare, not every
    /// later one: an origin set from a wrong guess would stay with the checkout for good.
    pub fn with_found_remote(dir: impl Into<PathBuf>, find: FindRemote) -> GitWorkspace {
        GitWorkspace::cloning_from(dir, Remote::Found(find))
    }

    fn cloning_from(dir: impl Into<PathBuf>, remote: Remote) -> GitWorkspace {
        GitWorkspace {
            dir: dir.into(),
            remote,
            block_patience: BLOCK_PATIENCE,
            git_env: Vec::new(),
            mise: Mise::new(),
            env: Default::default(),
        }
    }

    pub fn with_block_patience(mut self, patience: Duration) -> GitWorkspace {
        self.block_patience = patience;
        self
    }

    /// A different mise binary and config directory; tests use it to stand in for the real thing.
    pub fn with_mise(mut self, mise: Mise) -> GitWorkspace {
        self.mise = mise;
        self
    }

    pub fn with_git_env(mut self, key: impl Into<String>, value: impl Into<String>) -> GitWorkspace {
        self.git_env.push((key.into(), value.into()));
        self
    }

    /// Compares the checkout's filesystem use with the request's limits, which are stack-neutral: whatever a new
    /// stack leaves behind counts. Over the warn size logs; over the limit fails, so no warm checkpoint is taken.
    /// A `df` that can't be read is logged and ignored.
    async fn check_disk(&self, req: &PrepareRequest) -> Result<()> {
        if req.disk_warn_bytes.is_none() && req.disk_limit_bytes.is_none() {
            return Ok(());
        }
        let used = match self.disk_used().await {
            Ok(used) => used,
            Err(e) => {
                tracing::warn!(error = %e, "couldn't read the sandbox's disk use");
                return Ok(());
            }
        };
        if let Some(limit) = req.disk_limit_bytes
            && used > limit
        {
            return Err(Error::Other(format!(
                "the sandbox uses {used} bytes of disk after setup, over disk_limit_bytes ({limit}); not taking a warm checkpoint (add or tighten the repo's `trim` check)"
            )));
        }
        if let Some(warn) = req.disk_warn_bytes
            && used > warn
        {
            tracing::warn!(
                used,
                warn,
                "the sandbox is over disk_warn_bytes after setup; add or tighten the repo's `trim` check"
            );
        }
        Ok(())
    }

    /// Bytes used on the filesystem holding the checkout.
    async fn disk_used(&self) -> Result<u64> {
        let out = Command::new("df")
            .args(["-Pk"])
            .arg(&self.dir)
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| Error::Other(format!("df: {e}")))?;
        let text = String::from_utf8_lossy(&out.stdout);
        let used_kib = out.status.success().then(|| parse_df_used(&text)).flatten();
        used_kib.map(|k| k * 1024).ok_or_else(|| Error::Other(format!("unreadable df output: {}", text.trim())))
    }

    async fn git(&self, cwd: &Path, args: &[&str]) -> Result<()> {
        self.git_output(cwd, args).await.map(drop)
    }

    async fn git_output(&self, cwd: &Path, args: &[&str]) -> Result<String> {
        let out = Command::new("git")
            .args(["-c", POST_BUFFER])
            .args(args)
            .envs(self.git_env.clone())
            .current_dir(cwd)
            .stdin(Stdio::null())
            .output()
            .await;
        let out = out.map_err(|e| Error::Other(format!("git {}: {e}", args.join(" "))))?;
        if out.status.success() {
            return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(Error::Other(format!("git {} failed: {}", args.join(" "), stderr.trim())))
    }
}

#[async_trait]
impl Workspace for GitWorkspace {
    fn dir(&self) -> PathBuf {
        self.dir.clone()
    }

    async fn install_hosts(&self, req: &PrepareRequest) -> Result<Vec<InstallHost>> {
        self.mise.install_hosts(&req.tools, &req.tool_hosts)
    }

    /// Clones the base branch, or resets an existing checkout to it, then moves to the request's branch if it has one.
    /// Ignored files such as build output stay, so a sandbox reused for another round keeps its caches.
    async fn prepare(&self, req: &PrepareRequest) -> Result<()> {
        self.set_env(&req.env);
        let base = &req.base_branch;
        if self.dir.join(".git").exists() {
            // A merge left under way (a warm checkpoint taken mid-rework) would block the checkout. Abort it if
            // there is one; whatever that does, the hard reset clears the index, the merge state and any conflicted
            // files, which `--abort` can fail to do.
            if self.merge_head().await?.is_some()
                && let Err(e) = self.git(&self.dir, &["merge", "--abort"]).await
            {
                tracing::warn!(error = %e, "couldn't abort the merge a checkout was left in; resetting it instead");
            }
            self.git(&self.dir, &["reset", "--quiet", "--hard"]).await?;
            self.git(&self.dir, &["fetch", "--quiet", "origin", base]).await?;
            self.git(&self.dir, &["checkout", "--quiet", "--force", "-B", base, &format!("origin/{base}")]).await?;
            self.git(&self.dir, &["clean", "--quiet", "-fd"]).await?;
        } else {
            let parent = self.dir.parent().unwrap_or(Path::new("/"));
            tokio::fs::create_dir_all(parent).await.map_err(|e| Error::Other(format!("{}: {e}", parent.display())))?;
            let remote_base = match &self.remote {
                Remote::Fixed(base) => base.clone(),
                Remote::Found(find) => find().await?,
            };
            let url = format!("{}/{}/{}.git", remote_base.trim_end_matches('/'), req.repo.owner, req.repo.name);
            let dir = self.dir.to_string_lossy();
            self.git(parent, &["clone", "--quiet", "--branch", base, &url, &dir]).await?;
        }
        if let Some(branch) = &req.branch {
            // A rework builds on the PR branch's head, so what it publishes lands on top of it; a
            // reviewer checks out the same head, so it reads the code the PR proposes.
            let fetched = self.git(&self.dir, &["fetch", "--quiet", "origin", branch]).await;
            // A branch that was never pushed isn't an error when the request allows it: the work starts at the base.
            if fetched.is_ok() || !req.branch_if_present {
                fetched?;
                self.git(&self.dir, &["checkout", "--quiet", "--force", "-B", branch, &format!("origin/{branch}")])
                    .await?;
            }
        }
        // Before setup, so the repo's own build runs with the pinned tools.
        self.mise.bootstrap(&req.tools).await?;
        if let Some(setup) = &req.setup {
            tracing::info!(command = %setup.command, "running the setup step");
            let began = std::time::Instant::now();
            let result = self.check(setup).await;
            tracing::info!(passed = result.passed, elapsed = ?began.elapsed(), "the setup step finished");
            if !result.passed {
                return Err(Error::SetupFailed(result.output.trim().to_string()));
            }
        }
        if let Some(trim) = &req.trim {
            // The sandbox works whatever the trim does, so a failure is logged, not returned.
            tracing::info!(command = %trim.command, "running the trim step");
            let result = self.check(trim).await;
            if !result.passed {
                tracing::warn!(output = %result.output.trim(), "the trim check failed; carrying on untrimmed");
            }
        }
        self.check_disk(req).await?;
        if req.merge_base && req.branch.is_some() {
            // After setup, which would trip over conflict markers. Conflicts are the point: the agent resolves them.
            let base = format!("origin/{}", req.base_branch);
            // Even --no-commit wants a committer identity, and a sandbox or CI runner may have none. The daemon
            // publishes through the GitHub API, so this one never lands in history.
            let merged = self
                .git(
                    &self.dir,
                    &[
                        "-c",
                        "user.name=trailhead-factory",
                        "-c",
                        "user.email=factory@trailhead.invalid",
                        "merge",
                        "--no-commit",
                        "--no-ff",
                        "--quiet",
                        &base,
                    ],
                )
                .await;
            if let Err(e) = merged
                && self.merge_head().await?.is_none()
            {
                return Err(e);
            }
        }
        Ok(())
    }

    async fn agent_env(&self) -> std::collections::BTreeMap<String, String> {
        let mut env = self.mise.env(&self.dir).await;
        env.extend(self.env.lock().unwrap().clone());
        env
    }

    fn set_env(&self, env: &std::collections::BTreeMap<String, String>) {
        *self.env.lock().unwrap() = env.clone();
    }

    async fn merge_head(&self) -> Result<Option<String>> {
        Ok(self.git_output(&self.dir, &["rev-parse", "--quiet", "--verify", "MERGE_HEAD"]).await.ok())
    }

    async fn merged_base(&self, start: &str, base: &str) -> Result<Option<String>> {
        let tip = format!("origin/{base}");
        let Ok(tip) = self.git_output(&self.dir, &["rev-parse", "--verify", "--quiet", &tip]).await else {
            return Ok(None);
        };
        // The newest commit of the base that HEAD has. If `start` has it too, nothing of the base came in. Otherwise
        // the agent merged the base, whether as a merge commit, a fast-forward, or before fetching again moved
        // `origin/<base>` on.
        let Ok(have) = self.git_output(&self.dir, &["merge-base", "HEAD", &tip]).await else {
            return Ok(None);
        };
        let have = have.trim().to_string();
        Ok((!self.is_ancestor(&have, start).await?).then_some(have))
    }

    async fn contains(&self, commit: &str) -> Result<bool> {
        self.is_ancestor(commit, "HEAD").await
    }

    async fn conflict_markers(&self, start: &str) -> Result<Vec<String>> {
        // Tracked files only, but a merge's result is all tracked once added; the agent's new files are added too.
        let _ = self.git(&self.dir, &["add", "--intent-to-add", "--all"]).await;
        let diff = self.git_output(&self.dir, &["diff", "--unified=0", "--no-color", "--no-ext-diff", start]).await?;
        let mut found = marker_lines(&diff);
        if found.is_empty() {
            return Ok(vec![]);
        }
        // Lines the merged-in side already has aren't the agent's: a fixture or a deep blockquote on the base branch.
        let mut sides = Vec::new();
        if let Some(head) = self.merge_head().await? {
            sides.push(head);
        }
        let range = format!("{start}..HEAD");
        let merges = self.git_output(&self.dir, &["rev-list", "--merges", "--parents", &range]).await?;
        for line in merges.lines() {
            sides.extend(line.split_whitespace().skip(2).map(str::to_string));
        }
        for side in sides {
            let theirs = self
                .git_output(&self.dir, &["diff", "--unified=0", "--no-color", "--no-ext-diff", start, &side])
                .await?;
            for line in marker_lines(&theirs) {
                found.retain(|l| *l != line);
            }
        }
        let mut files: Vec<String> = Vec::new();
        for (file, _) in found {
            if !files.contains(&file) {
                files.push(file);
            }
        }
        Ok(files)
    }

    async fn changed_files(&self, base: &str) -> Result<Vec<String>> {
        // New files count, as in conflict_markers.
        let _ = self.git(&self.dir, &["add", "--intent-to-add", "--all"]).await;
        let remote = format!("origin/{base}");
        // While the rework's merge of the base is in progress, diff against the base tip itself: against the
        // merge-base, every base change since the branch point would count as the PR's and trigger scoped checks.
        let from = if self.merge_head().await?.is_some() {
            remote
        } else {
            self.git_output(&self.dir, &["merge-base", &remote, "HEAD"]).await?
        };
        let names =
            self.git_output(&self.dir, &["diff", "--name-only", "--no-renames", "--no-ext-diff", "-z", &from]).await?;
        Ok(names.split('\0').filter(|f| !f.is_empty()).map(str::to_string).collect())
    }

    async fn unpublished(&self, remote_branch: &str) -> Result<bool> {
        // Untracked files count; ignored ones (build output) don't.
        if !self.git_output(&self.dir, &["status", "--porcelain"]).await?.trim().is_empty() {
            return Ok(true);
        }
        // The factory publishes through the GitHub API, which makes new commits for the same content: the ref may not
        // be here yet and the local commits may not be its ancestors. Fetch it, then compare content as well. A branch
        // the remote lacks fails the fetch.
        self.git(&self.dir, &["fetch", "--quiet", "origin", remote_branch]).await?;
        let remote = format!("origin/{remote_branch}");
        let ahead = self.git_output(&self.dir, &["rev-list", "-n1", "HEAD", "--not", &remote]).await?;
        if ahead.is_empty() {
            return Ok(false);
        }
        let differs = self.git_output(&self.dir, &["diff", "--name-only", "--no-ext-diff", &remote, "HEAD"]).await?;
        Ok(!differs.is_empty())
    }

    async fn head(&self) -> Result<String> {
        self.git_output(&self.dir, &["rev-parse", "HEAD"]).await
    }

    async fn confirm_blocked(&self, hosts: &[String]) -> Result<()> {
        let deadline = tokio::time::Instant::now() + self.block_patience;
        for host in hosts {
            while tokio::net::lookup_host((host.as_str(), 443)).await.is_ok() {
                if tokio::time::Instant::now() >= deadline {
                    return Err(Error::Forbidden(format!("egress still reaches {host}; not starting the agent")));
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
        Ok(())
    }

    async fn check(&self, check: &Check) -> CheckResult {
        let env = self.env.lock().unwrap().clone();
        let ooms_before = oom_kills();
        let out = self
            .mise
            .shell(&self.dir)
            .arg(format!("exec 2>&1\n{}", check.command))
            .current_dir(&self.dir)
            .stdin(Stdio::null())
            .envs(env)
            .envs(std::env::current_exe().ok().map(|exe| ("TRAILHEAD_DAEMON", exe)))
            .kill_on_drop(true)
            .output()
            .await;
        let (passed, output) = match out {
            Ok(out) => (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned()),
            Err(e) => (false, format!("couldn't run `{}`: {e}", check.command)),
        };
        let output = if passed || !oom_killed(ooms_before, oom_kills(), &output) {
            output
        } else {
            format!("{output}\n{OOM_NOTE}")
        };
        CheckResult { name: check.name.clone(), passed, output: tail(output), skipped: false }
    }
}

/// Appended to a failed check's output when the kernel's OOM killer, not the check itself, probably ended it.
const OOM_NOTE: &str = "The sandbox ran out of memory: the kernel's OOM killer ended a process of this check (the \
\"signal 9\" above is its doing, not a bug in the code). Raise `memory_mib` in [factory.compute], or lower the build's \
parallelism (CARGO_BUILD_JOBS) or the number of sandboxes (max_sandboxes).";

/// How many processes the sandbox's memory cgroup has OOM-killed so far, when it can be read.
fn oom_kills() -> Option<u64> {
    let events = std::fs::read_to_string("/sys/fs/cgroup/memory.events").ok()?;
    events.lines().find_map(|l| l.strip_prefix("oom_kill ")?.trim().parse().ok())
}

/// Whether a failed check was OOM-killed: the cgroup's count rose, or, where it can't be read, the output says a
/// process died of SIGKILL.
fn oom_killed(before: Option<u64>, after: Option<u64>, output: &str) -> bool {
    match (before, after) {
        (Some(before), Some(after)) if after > before => true,
        (Some(_), Some(_)) => false,
        _ => output.contains("terminated with signal 9") || output.contains("signal: 9"),
    }
}

/// The "Used" column (KiB) of `df -P`'s one data line.
fn parse_df_used(out: &str) -> Option<u64> {
    out.lines().nth(1)?.split_whitespace().nth(2)?.parse().ok()
}

/// The `(file, line)` pairs of the `<<<<<<<` and `>>>>>>>` lines a unified diff adds.
///
/// A file's name is read only from its header, between its `diff --git` line and its first hunk, so an added line
/// that happens to read `+++ ...` is never taken for one.
fn marker_lines(diff: &str) -> Vec<(String, String)> {
    let mut lines = Vec::new();
    let mut file = None;
    let mut in_header = false;
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            in_header = true;
            file = None;
        } else if in_header {
            if let Some(name) = line.strip_prefix("+++ ") {
                file = diff_path(name.strip_suffix('\t').unwrap_or(name));
            } else if line.starts_with("@@") {
                in_header = false;
            }
        } else if let Some(added) = line.strip_prefix('+')
            && (added.starts_with("<<<<<<<") || added.starts_with(">>>>>>>"))
            && let Some(file) = &file
        {
            lines.push((file.clone(), added.to_string()));
        }
    }
    lines
}

/// The path on a `+++` header: git quotes unusual ones C-style, and prefixes the new side with `b/`.
fn diff_path(name: &str) -> Option<String> {
    let name = match name.strip_prefix('"').and_then(|n| n.strip_suffix('"')) {
        Some(quoted) => unquote(quoted),
        None => name.to_string(),
    };
    if name == "/dev/null" {
        return None;
    }
    Some(name.strip_prefix("b/").map(str::to_string).unwrap_or(name))
}

/// Undoes git's C-style quoting of a path (`\"`, `\\`, `\t`, `\n`, and `\ooo` octal bytes).
fn unquote(s: &str) -> String {
    let mut out = Vec::new();
    let mut bytes = s.bytes().peekable();
    while let Some(b) = bytes.next() {
        if b != b'\\' {
            out.push(b);
            continue;
        }
        match bytes.next() {
            Some(d @ b'0'..=b'7') => {
                let mut n = u32::from(d - b'0');
                for _ in 0..2 {
                    match bytes.next_if(|c| (b'0'..=b'7').contains(c)) {
                        Some(c) => n = n * 8 + u32::from(c - b'0'),
                        None => break,
                    }
                }
                out.push(n as u8);
            }
            Some(b'n') => out.push(b'\n'),
            Some(b't') => out.push(b'\t'),
            Some(b'r') => out.push(b'\r'),
            Some(other) => out.push(other),
            None => out.push(b'\\'),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn tail(mut s: String) -> String {
    if s.len() > OUTPUT_TAIL {
        let mut cut = s.len() - OUTPUT_TAIL;
        while !s.is_char_boundary(cut) {
            cut += 1;
        }
        s.replace_range(..cut, "…");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::{marker_lines, oom_killed, parse_df_used};

    #[test]
    fn marker_lines_names_the_right_file() {
        let diff = [
            "diff --git a/a.txt b/a.txt",
            "--- a/a.txt",
            "+++ b/a.txt",
            "@@ -0,0 +2 @@",
            "++ not a header",
            "+<<<<<<< HEAD",
            r#"diff --git "a/q \"x.txt" "b/q \"x.txt""#,
            r#"--- "a/q \"x.txt""#,
            r#"+++ "b/q \"x.txt""#,
            "@@ -0,0 +1 @@",
            "+>>>>>>> main",
            "diff --git a/c d.txt b/c d.txt",
            "+++ b/c d.txt\t",
            "@@ -0,0 +1 @@",
            "+fine",
        ]
        .join("\n");
        assert_eq!(
            marker_lines(&diff),
            vec![
                ("a.txt".to_string(), "<<<<<<< HEAD".to_string()),
                ("q \"x.txt".to_string(), ">>>>>>> main".to_string()),
            ]
        );
    }

    #[test]
    fn an_oom_kill_is_told_apart_from_other_failures() {
        assert!(oom_killed(Some(0), Some(1), "ld terminated with signal 9"));
        assert!(!oom_killed(Some(2), Some(2), "ld terminated with signal 9"), "the cgroup's count is the truth");
        assert!(oom_killed(None, None, "error: linking with `cc` failed\n  = note: ld terminated with signal 9"));
        assert!(oom_killed(None, Some(1), "process didn't exit successfully (signal: 9, SIGKILL)"));
        assert!(!oom_killed(None, None, "error[E0308]: mismatched types"));
    }

    #[test]
    fn df_used_is_the_third_column_of_the_data_line() {
        let out = "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/vda1 100000 4242 95000 5% /\n";
        assert_eq!(parse_df_used(out), Some(4242));
        assert_eq!(parse_df_used("garbage"), None);
    }
}
