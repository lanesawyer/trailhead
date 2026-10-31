//! Keeps a cargo `target` directory under a size cap, so a warm checkpoint of the sandbox stays small.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// What a trim did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Trimmed {
    pub before: u64,
    pub after: u64,
}

/// Brings `target` down to `cap` bytes. Incremental caches go first: they're the biggest part and the least reusable
/// across commits. If it is still over, the least recently modified files go until it fits; cargo rebuilds whatever
/// it finds missing, so this costs build time, never correctness. A missing `target` is fine.
pub fn trim_target(target: &Path, cap: u64) -> io::Result<Trimmed> {
    if !target.is_dir() {
        return Ok(Trimmed::default());
    }
    let before = dir_size(target)?;
    if before <= cap {
        return Ok(Trimmed { before, after: before });
    }
    let mut size = before;
    for profile in fs::read_dir(target)? {
        let incremental = profile?.path().join("incremental");
        if incremental.is_dir() {
            size = size.saturating_sub(dir_size(&incremental)?);
            fs::remove_dir_all(&incremental)?;
        }
    }
    if size > cap {
        let mut files = Vec::new();
        collect(target, &mut files)?;
        files.sort_by_key(|f| f.1);
        for (path, _, len) in files {
            if size <= cap {
                break;
            }
            fs::remove_file(&path)?;
            size = size.saturating_sub(len);
        }
    }
    Ok(Trimmed { before, after: size })
}

fn dir_size(dir: &Path) -> io::Result<u64> {
    let mut files = Vec::new();
    collect(dir, &mut files)?;
    Ok(files.iter().map(|f| f.2).sum())
}

/// Every regular file under `dir` with its modification time and size. Symlinks are not followed.
fn collect(dir: &Path, out: &mut Vec<(PathBuf, SystemTime, u64)>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect(&entry.path(), out)?;
        } else if kind.is_file() {
            let meta = entry.metadata()?;
            out.push((entry.path(), meta.modified()?, meta.len()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn file(path: &Path, len: usize, age_secs: u64) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![0u8; len]).unwrap();
        let f = fs::File::options().write(true).open(path).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(age_secs)).unwrap();
    }

    #[test]
    fn under_the_cap_nothing_goes() {
        let dir = tempfile::tempdir().unwrap();
        file(&dir.path().join("debug/deps/a"), 100, 10);
        assert_eq!(trim_target(dir.path(), 1000).unwrap(), Trimmed { before: 100, after: 100 });
        assert!(dir.path().join("debug/deps/a").exists());
    }

    #[test]
    fn incremental_goes_first() {
        let dir = tempfile::tempdir().unwrap();
        file(&dir.path().join("debug/incremental/x"), 600, 1);
        file(&dir.path().join("debug/deps/a"), 300, 100);
        let t = trim_target(dir.path(), 400).unwrap();
        assert_eq!(t, Trimmed { before: 900, after: 300 });
        assert!(dir.path().join("debug/deps/a").exists());
        assert!(!dir.path().join("debug/incremental").exists());
    }

    #[test]
    fn then_the_oldest_files() {
        let dir = tempfile::tempdir().unwrap();
        file(&dir.path().join("debug/deps/old"), 300, 300);
        file(&dir.path().join("debug/deps/mid"), 300, 200);
        file(&dir.path().join("debug/deps/new"), 300, 100);
        let t = trim_target(dir.path(), 500).unwrap();
        assert_eq!(t.after, 300);
        assert!(dir.path().join("debug/deps/new").exists());
        assert!(!dir.path().join("debug/deps/old").exists());
        assert!(!dir.path().join("debug/deps/mid").exists());
    }

    #[test]
    fn no_target_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(trim_target(&dir.path().join("target"), 10).unwrap(), Trimmed::default());
    }
}
