//! One launcher at a time: the shell directory and the lock a running `trailhead app` holds in it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use trailhead_server::Paths;

/// Where the window keeps its endpoint key and this launcher keeps its own: `TRAILHEAD_SHELL_DIR`, else `shell` in
/// Trailhead's config directory, as the window reads it.
pub(crate) fn shell_dir(paths: &Paths) -> PathBuf {
    std::env::var_os("TRAILHEAD_SHELL_DIR").map_or_else(|| paths.config_dir.join("shell"), PathBuf::from)
}

/// Held by the one launcher running from a shell directory, until it exits.
pub(crate) struct OnlyLauncher {
    _lock: std::fs::File,
}

/// Takes the shell directory's lock, or says another launcher has it. The launcher and its window dial the server as
/// the keys in that directory, and a second pair under the same keys doesn't get a second connection: the relay
/// sends the server's answers to whichever of the two dialed last, so the other's dials time out for as long as both
/// run (TRAIL-1665: a Mac whose launcher never connected while an earlier one was still up). The lock is the
/// operating system's, so it goes when the process does, however it ends.
pub(crate) fn only_launcher(dir: &Path) -> Result<OnlyLauncher> {
    std::fs::create_dir_all(dir).with_context(|| dir.display().to_string())?;
    let path = dir.join("launcher.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| path.display().to_string())?;
    match file.try_lock() {
        Ok(()) => Ok(OnlyLauncher { _lock: file }),
        Err(std::fs::TryLockError::WouldBlock) => bail!(
            "the Trailhead window is already running here (another `trailhead app` holds {}). Use that window; a second one would dial the server under the same identity and cut both off.",
            path.display()
        ),
        Err(std::fs::TryLockError::Error(e)) => Err(e).with_context(|| format!("locking {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_launcher_is_refused_until_the_first_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let shell = dir.path().join("shell");
        let first = only_launcher(&shell).unwrap();
        let refused = only_launcher(&shell).err().expect("the lock is held").to_string();
        assert!(refused.contains("already running") && refused.contains("launcher.lock"), "{refused}");
        drop(first);
        only_launcher(&shell).expect("free once the first launcher is gone");
    }
}
