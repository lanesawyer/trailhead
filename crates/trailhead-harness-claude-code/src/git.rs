//! What the agent changed, read from its checkout with git: the head, and every file that differs from where the
//! run started.

use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Stdio;

use tokio::process::Command;
use trailhead_factory::{Error, FileChange, FileMode, Result};

pub(crate) async fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git").args(args).current_dir(dir).stdin(Stdio::null()).output().await;
    let out = out.map_err(|e| Error::Other(format!("git {}: {e}", args.join(" "))))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(Error::Other(format!("git {} failed: {}", args.join(" "), stderr.trim())));
    }
    Ok(out.stdout)
}

pub(crate) async fn git_head(dir: &Path) -> Result<String> {
    Ok(String::from_utf8_lossy(&git(dir, &["rev-parse", "HEAD"]).await?).trim().to_string())
}

/// Every path that differs from `start`, committed or not, including new files, with its contents now.
pub(crate) async fn changes_since(dir: &Path, start: &str) -> Result<Vec<FileChange>> {
    unnest_commitless_repositories(dir).await;
    git(dir, &["add", "--all"]).await?;
    let names = git(dir, &["diff", "--cached", "--name-only", "--no-renames", "-z", start]).await?;
    let mut changes = Vec::new();
    for path in names.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        read_change(dir, String::from_utf8_lossy(path).into_owned(), &mut changes).await?;
    }
    // A tracked directory that became an embedded repository is listed as its old files and as the gitlink, which
    // expands to the same files again: publish each path once.
    let mut seen = std::collections::HashSet::new();
    changes.retain(|c| seen.insert(c.path.clone()));
    Ok(changes)
}

/// A nested repository with no commit can't be staged as an embedded repository: `git add` fails on it. Remove the
/// `.git` of each one (git lists a nested repository as a `dir/` entry among the untracked paths), so its files are
/// staged as ordinary files, and log it. One with a commit is left alone: `read_change` publishes its files.
pub(crate) async fn unnest_commitless_repositories(dir: &Path) {
    let listed = match git(dir, &["ls-files", "-z", "--others", "--exclude-standard"]).await {
        Ok(listed) => listed,
        Err(e) => {
            tracing::warn!(error = %e, "could not look for nested repositories");
            return;
        }
    };
    for name in listed.split(|b| *b == 0).filter(|n| n.ends_with(b"/")) {
        let nested = dir.join(String::from_utf8_lossy(name).trim_end_matches('/'));
        if git(&nested, &["rev-parse", "--verify", "--quiet", "HEAD"]).await.is_ok() {
            continue;
        }
        let dot_git = nested.join(".git");
        let removed = match tokio::fs::symlink_metadata(&dot_git).await {
            Ok(m) if m.is_dir() => tokio::fs::remove_dir_all(&dot_git).await,
            Ok(_) => tokio::fs::remove_file(&dot_git).await,
            Err(e) => Err(e),
        };
        match removed {
            Ok(()) => {
                tracing::warn!(path = %nested.display(), "removed the .git of a nested repository with no commit");
            }
            Err(e) => tracing::warn!(path = %nested.display(), error = %e, "could not remove a nested .git"),
        }
    }
}

/// Reads one changed path into `changes`. Git lists a directory only as an embedded repository (a gitlink),
/// so a directory is replaced by the files its own git tracks or would add, never `.git`. A symlink is never
/// followed: it's published as a link whose contents are its target path, wherever that points, so a link to a file
/// outside the worktree can't copy that file into the commit. A file the embedded repository lists but that is gone
/// from disk (staged, then deleted) is skipped: the parent's base tree never had it, so publishing a deletion could
/// be rejected.
pub(crate) async fn read_change(dir: &Path, path: String, changes: &mut Vec<FileChange>) -> Result<()> {
    let mut pending = vec![(path, false)];
    while let Some((path, expanded)) = pending.pop() {
        let full = dir.join(&path);
        let fail = |e: std::io::Error| Error::Other(format!("{path}: {e}"));
        let meta = match tokio::fs::symlink_metadata(&full).await {
            Ok(meta) => Some(meta),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(fail(e)),
        };
        if meta.as_ref().is_some_and(|m| m.is_dir()) {
            let listed = git(&full, &["ls-files", "-z", "--cached", "--others", "--exclude-standard"]).await?;
            for name in listed.split(|b| *b == 0).filter(|n| !n.is_empty()) {
                let name = String::from_utf8_lossy(name);
                if name != ".git" && !name.starts_with(".git/") {
                    pending.push((format!("{path}/{name}"), true));
                }
            }
            continue;
        }
        if meta.as_ref().is_some_and(|m| m.is_symlink()) {
            let target = tokio::fs::read_link(&full).await.map_err(fail)?;
            let contents = Some(target.into_os_string().into_vec());
            changes.push(FileChange { path, contents, mode: FileMode::Symlink });
            continue;
        }
        let contents = match tokio::fs::read(&full).await {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && expanded => continue,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(fail(e)),
        };
        let executable = meta.is_some_and(|m| m.permissions().mode() & 0o111 != 0);
        let mode = if executable { FileMode::Executable } else { FileMode::Regular };
        changes.push(FileChange { path, contents, mode });
    }
    Ok(())
}
