use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::process::Command;
use trailhead_factory::{Check, CheckResult, Error, PrepareRequest, Result};

use crate::Workspace;

/// Keep the end of a check's output, where failures usually are.
const OUTPUT_TAIL: usize = 16 * 1024;

/// How long to wait for narrowed egress to take effect. Sprites took under a second in M0's tests.
const BLOCK_PATIENCE: Duration = Duration::from_secs(15);

/// A git checkout of a public GitHub repo, cloned anonymously during prepare.
pub struct GitWorkspace {
    dir: PathBuf,
    /// Where repos are cloned from; tests point it at a local directory.
    remote_base: String,
    block_patience: Duration,
    /// Extra environment for git; tests use it to hide the machine's git config.
    git_env: Vec<(String, String)>,
}

impl GitWorkspace {
    pub fn new(dir: impl Into<PathBuf>) -> GitWorkspace {
        GitWorkspace::with_remote(dir, "https://github.com")
    }

    pub fn with_remote(dir: impl Into<PathBuf>, remote_base: impl Into<String>) -> GitWorkspace {
        GitWorkspace {
            dir: dir.into(),
            remote_base: remote_base.into(),
            block_patience: BLOCK_PATIENCE,
            git_env: Vec::new(),
        }
    }

    pub fn with_block_patience(mut self, patience: Duration) -> GitWorkspace {
        self.block_patience = patience;
        self
    }

    pub fn with_git_env(mut self, key: impl Into<String>, value: impl Into<String>) -> GitWorkspace {
        self.git_env.push((key.into(), value.into()));
        self
    }

    async fn git(&self, cwd: &Path, args: &[&str]) -> Result<()> {
        self.git_output(cwd, args).await.map(drop)
    }

    async fn git_output(&self, cwd: &Path, args: &[&str]) -> Result<String> {
        let out = Command::new("git")
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

    /// Clones the base branch, or resets an existing checkout to it, then moves to the request's branch if it has one.
    /// Ignored files such as build output stay, so a sandbox reused for another round keeps its caches.
    async fn prepare(&self, req: &PrepareRequest) -> Result<()> {
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
            let url = format!("{}/{}/{}.git", self.remote_base.trim_end_matches('/'), req.repo.owner, req.repo.name);
            let dir = self.dir.to_string_lossy();
            self.git(parent, &["clone", "--quiet", "--branch", base, &url, &dir]).await?;
        }
        if let Some(branch) = &req.branch {
            // A rework builds on the PR branch's head, so what it publishes lands on top of it.
            self.git(&self.dir, &["fetch", "--quiet", "origin", branch]).await?;
            self.git(&self.dir, &["checkout", "--quiet", "--force", "-B", branch, &format!("origin/{branch}")]).await?;
        }
        if let Some(setup) = &req.setup {
            let result = self.check(setup).await;
            if !result.passed {
                return Err(Error::Other(format!("setup failed: {}", result.output.trim())));
            }
        }
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

    async fn merge_head(&self) -> Result<Option<String>> {
        Ok(self.git_output(&self.dir, &["rev-parse", "--quiet", "--verify", "MERGE_HEAD"]).await.ok())
    }

    async fn contains(&self, commit: &str) -> Result<bool> {
        let status = Command::new("git")
            .args(["merge-base", "--is-ancestor", commit, "HEAD"])
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
            _ => Err(Error::Other(format!("git merge-base --is-ancestor {commit} HEAD failed"))),
        }
    }

    async fn conflict_markers(&self, start: &str) -> Result<Vec<String>> {
        // Tracked files only, but a merge's result is all tracked once added; the agent's new files are added too.
        let _ = self.git(&self.dir, &["add", "--intent-to-add", "--all"]).await;
        let diff = self.git_output(&self.dir, &["diff", "--unified=0", "--no-color", "--no-ext-diff", start]).await?;
        Ok(marker_files(&diff))
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
        let out = Command::new("sh")
            .arg("-c")
            .arg(format!("exec 2>&1\n{}", check.command))
            .current_dir(&self.dir)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output()
            .await;
        let (passed, output) = match out {
            Ok(out) => (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned()),
            Err(e) => (false, format!("couldn't run `{}`: {e}", check.command)),
        };
        CheckResult { name: check.name.clone(), passed, output: tail(output) }
    }
}

/// The files a unified diff adds a `<<<<<<<` or `>>>>>>>` line to.
fn marker_files(diff: &str) -> Vec<String> {
    let mut files: Vec<String> = Vec::new();
    let mut file = None;
    for line in diff.lines() {
        if let Some(name) = line.strip_prefix("+++ ") {
            file = Some(name.strip_prefix("b/").unwrap_or(name).to_string());
        } else if let Some(added) = line.strip_prefix('+')
            && (added.starts_with("<<<<<<<") || added.starts_with(">>>>>>>"))
            && let Some(file) = &file
            && !files.contains(file)
        {
            files.push(file.clone());
        }
    }
    files
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
