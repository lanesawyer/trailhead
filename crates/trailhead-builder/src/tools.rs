//! The builder's reach outside itself: git and cargo. [`Tools`] is the seam its tests fill with a fake; [`System`]
//! runs the real ones.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use crate::layout::BINARY;

pub type Boxed<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Where the apps repo is fetched from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub url: String,
    pub branch: String,
}

/// One build: the builder's clone, checked out at the commit to build.
#[derive(Debug, Clone)]
pub struct Job {
    pub src: PathBuf,
    pub target: PathBuf,
    /// Where cargo's output goes, truncated first.
    pub log: PathBuf,
    pub package: Option<String>,
    pub profile: String,
    pub jobs: u32,
}

/// git and cargo, as the builder uses them. Errors are a line for a person.
pub trait Tools: Send + Sync + 'static {
    /// The checkout's `origin` URL and its default branch.
    fn origin<'a>(&'a self, repo: &'a Path) -> Boxed<'a, Result<Origin, String>>;

    /// The commit at the head of the base branch.
    fn head<'a>(&'a self, origin: &'a Origin) -> Boxed<'a, Result<String, String>>;

    /// Clones into `src` when it isn't there, fetches, and checks out `rev` detached.
    fn checkout<'a>(&'a self, src: &'a Path, origin: &'a Origin, rev: &'a str) -> Boxed<'a, Result<(), String>>;

    /// Builds the shell, and says where the binary is.
    fn build<'a>(&'a self, job: &'a Job) -> Boxed<'a, Result<PathBuf, String>>;

    /// The files of `rev` in the clone at `src`, as a gzipped tar, for a machine that builds the shell itself.
    fn archive<'a>(&'a self, src: &'a Path, rev: &'a str) -> Boxed<'a, Result<Vec<u8>, String>>;

    /// The subject lines of the commits in the clone at `src` after `since` up to `until`, newest first. Both are
    /// commit hashes the caller has checked.
    fn log<'a>(&'a self, src: &'a Path, since: &'a str, until: &'a str) -> Boxed<'a, Result<Vec<String>, String>>;
}

/// The system's git and cargo.
pub struct System {
    cargo: PathBuf,
}

/// Longer than any fetch should take; a git that hangs (a credential prompt nobody sees) would otherwise stop every
/// build after it.
const GIT_TIMEOUT: Duration = Duration::from_secs(600);

impl System {
    pub fn new() -> System {
        System { cargo: find_cargo() }
    }
}

impl Default for System {
    fn default() -> Self {
        Self::new()
    }
}

/// `cargo` on the `PATH`, else rustup's.
fn find_cargo() -> PathBuf {
    let on_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|dir| dir.join("cargo"))
        .find(|p| p.is_file());
    let home = || std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo/bin/cargo"));
    on_path.or_else(|| home().filter(|p| p.is_file())).unwrap_or_else(|| "cargo".into())
}

async fn git(dir: Option<&Path>, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    if let Some(dir) = dir {
        cmd.arg("-C").arg(dir);
    }
    // Never ask: the server has nobody to answer.
    cmd.args(args).env("GIT_TERMINAL_PROMPT", "0").stdin(Stdio::null()).kill_on_drop(true);
    let what = format!("git {}", args.first().copied().unwrap_or_default());
    let out = tokio::time::timeout(GIT_TIMEOUT, cmd.output())
        .await
        .map_err(|_| format!("{what} took longer than {}s", GIT_TIMEOUT.as_secs()))?
        .map_err(|e| format!("{what}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!("{what} failed ({}): {}", out.status, String::from_utf8_lossy(&out.stderr).trim()))
    }
}

impl Tools for System {
    fn origin<'a>(&'a self, repo: &'a Path) -> Boxed<'a, Result<Origin, String>> {
        Box::pin(async move {
            let url = git(Some(repo), &["remote", "get-url", "origin"]).await?;
            let head = git(Some(repo), &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]).await.ok();
            let branch = head.as_deref().and_then(|h| h.strip_prefix("origin/")).unwrap_or("main").to_string();
            Ok(Origin { url, branch })
        })
    }

    fn head<'a>(&'a self, origin: &'a Origin) -> Boxed<'a, Result<String, String>> {
        Box::pin(async move {
            let refname = format!("refs/heads/{}", origin.branch);
            let out = git(None, &["ls-remote", &origin.url, &refname]).await?;
            out.split_whitespace()
                .next()
                .map(String::from)
                .ok_or_else(|| format!("{} has no branch {}", origin.url, origin.branch))
        })
    }

    fn checkout<'a>(&'a self, src: &'a Path, origin: &'a Origin, rev: &'a str) -> Boxed<'a, Result<(), String>> {
        Box::pin(async move {
            if src.join(".git").exists() {
                git(Some(src), &["remote", "set-url", "origin", &origin.url]).await?;
            } else {
                if let Some(parent) = src.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| format!("creating {}: {e}", parent.display()))?;
                }
                let into = src.to_string_lossy();
                git(None, &["clone", "--quiet", "--no-checkout", &origin.url, &into]).await?;
            }
            let refspec = format!("+refs/heads/{0}:refs/remotes/origin/{0}", origin.branch);
            git(Some(src), &["fetch", "--quiet", "origin", &refspec]).await?;
            git(Some(src), &["checkout", "--quiet", "--force", "--detach", rev]).await?;
            git(Some(src), &["clean", "-ffdq"]).await?;
            Ok(())
        })
    }

    fn build<'a>(&'a self, job: &'a Job) -> Boxed<'a, Result<PathBuf, String>> {
        Box::pin(async move {
            let package = match &job.package {
                Some(p) => p.clone(),
                None => self.package(&job.src).await?,
            };
            let log = std::fs::File::create(&job.log).map_err(|e| format!("creating {}: {e}", job.log.display()))?;
            let err = log.try_clone().map_err(|e| e.to_string())?;
            let jobs = job.jobs.to_string();
            let mut cmd = Command::new(&self.cargo);
            cmd.args(["build", "--locked", "--profile", &job.profile, "-p", &package, "-j", &jobs])
                .current_dir(&job.src)
                .env("CARGO_TARGET_DIR", &job.target)
                .env("CARGO_TERM_COLOR", "never")
                .stdin(Stdio::null())
                .stdout(log)
                .stderr(err)
                // Its own process group, so stopping the build stops the compilers cargo started too.
                .process_group(0)
                .kill_on_drop(true);
            let mut child = cmd.spawn().map_err(|e| format!("starting {}: {e}", self.cargo.display()))?;
            let mut group = KillGroup(child.id());
            let status = child.wait().await.map_err(|e| format!("waiting for cargo: {e}"))?;
            group.0 = None;
            if !status.success() {
                return Err(format!("cargo build failed ({status})"));
            }
            let binary = job.target.join(profile_dir(&job.profile)).join(BINARY);
            if binary.is_file() { Ok(binary) } else { Err(format!("cargo built no {}", binary.display())) }
        })
    }

    fn archive<'a>(&'a self, src: &'a Path, rev: &'a str) -> Boxed<'a, Result<Vec<u8>, String>> {
        Box::pin(async move {
            let mut cmd = Command::new("git");
            cmd.arg("-C").arg(src).args(["archive", "--format=tar.gz", rev]).stdin(Stdio::null()).kill_on_drop(true);
            let out = tokio::time::timeout(GIT_TIMEOUT, cmd.output())
                .await
                .map_err(|_| format!("git archive took longer than {}s", GIT_TIMEOUT.as_secs()))?
                .map_err(|e| format!("git archive: {e}"))?;
            if out.status.success() {
                Ok(out.stdout)
            } else {
                Err(format!("git archive failed ({}): {}", out.status, String::from_utf8_lossy(&out.stderr).trim()))
            }
        })
    }

    fn log<'a>(&'a self, src: &'a Path, since: &'a str, until: &'a str) -> Boxed<'a, Result<Vec<String>, String>> {
        Box::pin(async move {
            let range = format!("{since}..{until}");
            let out = git(Some(src), &["log", "--format=%s", &range, "--"]).await?;
            Ok(out.lines().map(String::from).collect())
        })
    }
}

impl System {
    /// The workspace package with a binary named `trailhead-shell`.
    async fn package(&self, src: &Path) -> Result<String, String> {
        let out = Command::new(&self.cargo)
            .args(["metadata", "--format-version", "1", "--no-deps", "--locked"])
            .current_dir(src)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| format!("cargo metadata: {e}"))?;
        if !out.status.success() {
            return Err(format!("cargo metadata failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        let meta: serde_json::Value =
            serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata: {e}"))?;
        package_with_shell(&meta).ok_or_else(|| {
            format!("no package in the apps repo has a binary named {BINARY}; set [shell] package in the user config")
        })
    }
}

fn package_with_shell(meta: &serde_json::Value) -> Option<String> {
    meta["packages"].as_array()?.iter().find_map(|p| {
        let targets = p["targets"].as_array()?;
        let has = targets
            .iter()
            .any(|t| t["name"] == BINARY && t["kind"].as_array().is_some_and(|k| k.iter().any(|k| k == "bin")));
        has.then(|| p["name"].as_str().map(String::from)).flatten()
    })
}

/// The directory under the target directory a profile's output lands in.
fn profile_dir(profile: &str) -> &str {
    match profile {
        "dev" | "test" => "debug",
        "bench" => "release",
        other => other,
    }
}

/// Stops a build's process group when the build is dropped before it finishes (the server stopping).
struct KillGroup(Option<u32>);

impl Drop for KillGroup {
    fn drop(&mut self) {
        if let Some(pid) = self.0.and_then(|p| i32::try_from(p).ok()) {
            // SAFETY: kill takes no pointers; a group that has already gone gets ESRCH, which is fine.
            unsafe {
                libc::kill(-pid, libc::SIGTERM);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_package_is_the_one_with_the_shells_binary() {
        let meta = serde_json::json!({"packages": [
            {"name": "ticklist", "targets": [{"name": "ticklist", "kind": ["lib"]}]},
            {"name": "trailhead-apps-shell", "targets": [
                {"name": "trailhead_apps_shell", "kind": ["lib"]},
                {"name": "trailhead-shell", "kind": ["bin"]}
            ]}
        ]});
        assert_eq!(package_with_shell(&meta).as_deref(), Some("trailhead-apps-shell"));
        assert_eq!(package_with_shell(&serde_json::json!({"packages": []})), None);
        assert_eq!((profile_dir("release"), profile_dir("dev"), profile_dir("fast")), ("release", "debug", "fast"));
    }

    fn sh(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "init.defaultBranch=main"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// The real git, on repos in a temporary directory: the person's checkout, its origin, and the builder's clone.
    #[tokio::test]
    async fn git_finds_the_origin_and_its_head_and_checks_a_commit_out_in_its_own_clone() {
        let dir = tempfile::tempdir().unwrap();
        let upstream = dir.path().join("upstream");
        std::fs::create_dir_all(&upstream).unwrap();
        sh(&upstream, &["init", "--quiet"]);
        std::fs::write(upstream.join("a"), "one").unwrap();
        sh(&upstream, &["add", "a"]);
        sh(&upstream, &["commit", "--quiet", "-m", "one"]);
        let checkout = dir.path().join("checkout");
        sh(dir.path(), &["clone", "--quiet", upstream.to_str().unwrap(), checkout.to_str().unwrap()]);

        let system = System::new();
        let origin = system.origin(&checkout).await.unwrap();
        assert_eq!(origin.branch, "main");
        let first = system.head(&origin).await.unwrap();
        assert_eq!(first, sh(&upstream, &["rev-parse", "HEAD"]));

        let src = dir.path().join("shell").join("src");
        system.checkout(&src, &origin, &first).await.unwrap();
        assert_eq!(std::fs::read_to_string(src.join("a")).unwrap(), "one");

        std::fs::write(upstream.join("a"), "two").unwrap();
        sh(&upstream, &["commit", "--quiet", "-am", "two"]);
        let second = system.head(&origin).await.unwrap();
        assert_ne!(second, first);
        system.checkout(&src, &origin, &second).await.unwrap();
        assert_eq!(std::fs::read_to_string(src.join("a")).unwrap(), "two");
        assert_eq!(sh(&src, &["rev-parse", "HEAD"]), second);
        assert_eq!(sh(&checkout, &["rev-parse", "HEAD"]), first, "the person's checkout is untouched");

        std::fs::write(upstream.join("a"), "three").unwrap();
        sh(&upstream, &["commit", "--quiet", "-am", "feat(a): three: the third [A-3]"]);
        let third = system.head(&origin).await.unwrap();
        system.checkout(&src, &origin, &third).await.unwrap();
        let log = system.log(&src, &first, &third).await.unwrap();
        assert_eq!(log, ["feat(a): three: the third [A-3]", "two"], "newest first, after `since`");
        assert_eq!(system.log(&src, &third, &third).await.unwrap(), Vec::<String>::new());
        assert!(system.log(&src, &"0".repeat(40), &third).await.is_err(), "a commit the clone doesn't have");
    }
}
