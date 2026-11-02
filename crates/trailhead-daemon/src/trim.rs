//! Keeps a cargo `target` directory under a size cap, so a warm checkpoint of the sandbox stays small.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// What a trim did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Trimmed {
    pub before: u64,
    pub after: u64,
}

/// Brings `target` down to `cap` bytes. Incremental caches go first: they're the biggest part and the least reusable
/// across commits. If it is still over, whole compilation units go, least recently used first. A unit is a crate's
/// `.fingerprint/<name>-<hash>` with its `deps/<name>-<hash>.*` files and its `build/<name>-<hash>` directory, and it
/// is removed together or not at all: cargo judges a build script fresh by its fingerprint and `output` file and never
/// looks inside `OUT_DIR`, so deleting single files could leave a "fresh" script whose generated files or native
/// libraries are gone. Without the fingerprint cargo rebuilds the unit, build script included. A unit's last use is
/// the mtime of its fingerprint's `invoked.timestamp`, which cargo touches whenever a build considers the unit.
/// A missing `target` is fine.
pub fn trim_target(target: &Path, cap: u64) -> io::Result<Trimmed> {
    if !target.is_dir() {
        return Ok(Trimmed::default());
    }
    let before = dir_size(target)?;
    if before <= cap {
        return Ok(Trimmed { before, after: before });
    }
    let profiles = profile_dirs(target)?;
    for profile in &profiles {
        let incremental = profile.join("incremental");
        if incremental.is_dir() {
            fs::remove_dir_all(&incremental)?;
        }
    }
    let mut size = dir_size(target)?;
    if size > cap {
        let mut units = Vec::new();
        for profile in &profiles {
            units.extend(units_of(profile)?);
        }
        units.sort_by(|a, b| a.used.cmp(&b.used).then_with(|| a.paths.cmp(&b.paths)));
        for unit in units {
            if size <= cap {
                break;
            }
            for path in &unit.paths {
                if path.is_dir() {
                    fs::remove_dir_all(path)?;
                } else {
                    fs::remove_file(path)?;
                }
            }
            size = size.saturating_sub(unit.size);
        }
        size = dir_size(target)?;
    }
    Ok(Trimmed { before, after: size })
}

/// One compilation unit's paths in a profile directory.
struct Unit {
    paths: Vec<PathBuf>,
    size: u64,
    /// `UNIX_EPOCH` for leftovers with no fingerprint, which cargo would rebuild anyway.
    used: SystemTime,
}

/// The profile directories (`target/debug`, `target/<triple>/release`, ...): those holding a `.fingerprint`.
fn profile_dirs(target: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(target)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path();
        if path.join(".fingerprint").is_dir() {
            out.push(path);
            continue;
        }
        for inner in fs::read_dir(&path)? {
            let inner = inner?;
            if inner.file_type()?.is_dir() && inner.path().join(".fingerprint").is_dir() {
                out.push(inner.path());
            }
        }
    }
    Ok(out)
}

fn units_of(profile: &Path) -> io::Result<Vec<Unit>> {
    let mut units: BTreeMap<String, Unit> = BTreeMap::new();
    let fresh = || Unit { paths: Vec::new(), size: 0, used: SystemTime::UNIX_EPOCH };
    for entry in fs::read_dir(profile.join(".fingerprint"))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let unit = units.entry(entry.file_name().to_string_lossy().into_owned()).or_insert_with(fresh);
        unit.used = last_used(&entry.path())?;
        unit.size += dir_size(&entry.path())?;
        unit.paths.push(entry.path());
    }
    let build = profile.join("build");
    if build.is_dir() {
        for entry in fs::read_dir(&build)? {
            let entry = entry?;
            let unit = units.entry(entry.file_name().to_string_lossy().into_owned()).or_insert_with(fresh);
            unit.size += dir_size(&entry.path())?;
            unit.paths.push(entry.path());
        }
    }
    let deps = profile.join("deps");
    if deps.is_dir() {
        let links = outside_links(profile)?;
        for entry in fs::read_dir(&deps)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let stem = name.split('.').next().unwrap_or(&name).to_string();
            let key = match stem.strip_prefix("lib") {
                Some(rest) if !units.contains_key(&stem) && units.contains_key(rest) => rest.to_string(),
                _ => stem,
            };
            let unit = units.entry(key).or_insert_with(fresh);
            unit.size += dir_size(&entry.path())?;
            unit.paths.push(entry.path());
            // Deleting a `deps/` file frees nothing while another hard link survives, so those go with it.
            let meta = entry.metadata()?;
            if meta.is_file()
                && meta.nlink() > 1
                && let Some(paths) = links.get(&(meta.dev(), meta.ino()))
            {
                unit.paths.extend(paths.iter().cloned());
            }
        }
    }
    Ok(units.into_values().collect())
}

/// The hard-linked files directly in the profile root and in `examples/`, by identity: where cargo links the artifacts
/// of `deps/`.
fn outside_links(profile: &Path) -> io::Result<HashMap<(u64, u64), Vec<PathBuf>>> {
    let mut out: HashMap<(u64, u64), Vec<PathBuf>> = HashMap::new();
    for dir in [profile.to_path_buf(), profile.join("examples")] {
        if !dir.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let meta = entry.metadata()?;
            if meta.nlink() > 1 {
                out.entry((meta.dev(), meta.ino())).or_default().push(entry.path());
            }
        }
    }
    Ok(out)
}

/// `invoked.timestamp` when cargo wrote one, else the newest file in the fingerprint directory.
fn last_used(fingerprint: &Path) -> io::Result<SystemTime> {
    if let Ok(meta) = fs::metadata(fingerprint.join("invoked.timestamp")) {
        return meta.modified();
    }
    let mut files = Vec::new();
    collect(fingerprint, &mut files)?;
    Ok(files.iter().map(|f| f.1).max().unwrap_or(SystemTime::UNIX_EPOCH))
}

/// Bytes under `path`, a file or a directory. Hard links (cargo links `examples/` and the profile root to `deps/`)
/// count once.
fn dir_size(path: &Path) -> io::Result<u64> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() {
        return Ok(if meta.is_file() { meta.len() } else { 0 });
    }
    let mut files = Vec::new();
    collect(path, &mut files)?;
    let mut seen = HashSet::new();
    Ok(files.iter().filter(|f| f.3.is_none_or(|id| seen.insert(id))).map(|f| f.2).sum())
}

type FileInfo = (PathBuf, SystemTime, u64, Option<(u64, u64)>);

/// Every regular file under `dir` with its modification time, size and, if hard-linked, its identity. Symlinks are
/// not followed.
fn collect(dir: &Path, out: &mut Vec<FileInfo>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect(&entry.path(), out)?;
        } else if kind.is_file() {
            let meta = entry.metadata()?;
            let id = (meta.nlink() > 1).then(|| (meta.dev(), meta.ino()));
            out.push((entry.path(), meta.modified()?, meta.len(), id));
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

    /// A unit as cargo lays it out: a fingerprint stamped `age_secs` ago, a deps artifact, and a build dir with an
    /// `out` file.
    fn unit(profile: &Path, key: &str, len: usize, age_secs: u64) {
        file(&profile.join(format!(".fingerprint/{key}/invoked.timestamp")), 0, age_secs);
        file(&profile.join(format!(".fingerprint/{key}/lib-x.json")), 10, age_secs);
        file(&profile.join(format!("deps/lib{key}.rlib")), len, age_secs);
        file(&profile.join(format!("deps/{key}.d")), 10, age_secs);
        file(&profile.join(format!("build/{key}/out/native.a")), len, age_secs);
        file(&profile.join(format!("build/{key}/output")), 10, age_secs);
    }

    /// Whether the fingerprint, the deps artifact and the build dir are there.
    fn present(profile: &Path, key: &str) -> [bool; 3] {
        [
            profile.join(".fingerprint").join(key).exists(),
            profile.join(format!("deps/lib{key}.rlib")).exists(),
            profile.join("build").join(key).exists(),
        ]
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
        let p = dir.path().join("debug");
        unit(&p, "a-1", 300, 100);
        file(&p.join("incremental/x"), 600, 1);
        let before = dir_size(dir.path()).unwrap();
        let t = trim_target(dir.path(), before - 600).unwrap();
        assert_eq!(t.after, before - 600);
        assert_eq!(present(&p, "a-1"), [true; 3]);
        assert!(!p.join("incremental").exists());
    }

    #[test]
    fn then_whole_units_least_recently_used_first() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("debug");
        unit(&p, "old-1", 300, 300);
        unit(&p, "mid-2", 300, 200);
        unit(&p, "new-3", 300, 100);
        let t = trim_target(dir.path(), 1000).unwrap();
        assert!(t.after <= 1000 && t.after > 0);
        assert_eq!(present(&p, "old-1"), [false; 3]);
        assert_eq!(present(&p, "new-3"), [true; 3]);
    }

    #[test]
    fn a_unit_is_never_left_half_there() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x86_64-unknown-linux-gnu/release");
        let keys = ["a-1", "b-2", "c-3", "d-4", "e-5"];
        for (i, key) in keys.iter().enumerate() {
            unit(&p, key, 200 + i * 10, 500 - i as u64 * 50);
        }
        for cap in [1500, 1000, 500, 0] {
            trim_target(dir.path(), cap).unwrap();
            for key in keys {
                let there = present(&p, key);
                assert!(there.iter().all(|&b| b) || there.iter().all(|&b| !b), "{key} at cap {cap}: {there:?}");
                // The build script's generated files stay exactly as long as the fingerprint that vouches for them.
                assert_eq!(p.join("build").join(key).join("out/native.a").exists(), there[0], "{key} at cap {cap}");
            }
        }
    }

    #[test]
    fn leftovers_without_a_fingerprint_go_before_used_units() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("debug");
        unit(&p, "used-1", 300, 1000);
        file(&p.join("deps/libstray-9.rlib"), 300, 1);
        trim_target(dir.path(), 800).unwrap();
        assert!(!p.join("deps/libstray-9.rlib").exists());
        assert_eq!(present(&p, "used-1"), [true; 3]);
    }

    #[test]
    fn hard_links_outside_deps_go_with_the_unit() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("debug");
        unit(&p, "old-1", 300, 300);
        unit(&p, "new-2", 300, 100);
        fs::hard_link(p.join("deps/libold-1.rlib"), p.join("libold-1.rlib")).unwrap();
        fs::create_dir_all(p.join("examples")).unwrap();
        fs::hard_link(p.join("deps/libold-1.rlib"), p.join("examples/libold-1.rlib")).unwrap();
        fs::hard_link(p.join("deps/libnew-2.rlib"), p.join("libnew-2.rlib")).unwrap();
        let before = dir_size(dir.path()).unwrap();
        // Dropping old-1 frees its 630 bytes only if its links go too.
        let t = trim_target(dir.path(), before - 630).unwrap();
        assert_eq!(t.after, before - 630);
        assert!(!p.join("libold-1.rlib").exists());
        assert!(!p.join("examples/libold-1.rlib").exists());
        assert!(p.join("libnew-2.rlib").exists());
        assert_eq!(present(&p, "new-2"), [true; 3]);
    }

    #[test]
    fn no_target_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(trim_target(&dir.path().join("target"), 10).unwrap(), Trimmed::default());
    }
}
