//! The subscription usage readings that pause new runs (`docs/factory/budgets.md`), kept in a file so a restart
//! doesn't lift a pause until the next reading arrives.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// One window's latest reading, as kept in the file.
#[derive(Serialize, Deserialize)]
struct Reading {
    used_percent: u32,
    /// Unix seconds.
    resets_at: i64,
}

/// The readings kept in `path` whose window hasn't reset by `now`. A file that can't be read is logged and starts
/// from no readings.
pub(crate) fn load(path: &Path, now: i64) -> HashMap<String, (u32, i64)> {
    let readings = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str::<HashMap<String, Reading>>(&text).unwrap_or_else(|e| {
            tracing::warn!(path = %path.display(), error = %e, "can't read the usage file; starting with no readings");
            HashMap::new()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "can't read the usage file; starting with no readings");
            HashMap::new()
        }
    };
    readings
        .into_iter()
        .filter(|(_, r)| r.resets_at > now)
        .map(|(window, r)| (window, (r.used_percent, r.resets_at)))
        .collect()
}

/// Writes `readings` to `path`. A failure is logged, and a restart then forgets them.
pub(crate) fn save(path: &Path, readings: &HashMap<String, (u32, i64)>) {
    let file: BTreeMap<_, _> = readings
        .iter()
        .map(|(window, &(used_percent, resets_at))| (window, Reading { used_percent, resets_at }))
        .collect();
    let write = || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(&file).map_err(std::io::Error::other)?)?;
        std::fs::rename(&tmp, path)
    };
    if let Err(e) = write() {
        tracing::warn!(path = %path.display(), error = %e, "can't save the usage file; a restart will forget the readings");
    }
}

#[cfg(test)]
mod tests {
    use trailhead_factory::HostConfig;

    use crate::Host;

    #[test]
    fn only_a_changed_reading_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        let host = Host::with_usage_file(&HostConfig::default(), path.clone());
        host.record_usage("five_hour".into(), 50, i64::MAX);
        assert!(path.exists());
        std::fs::remove_file(&path).unwrap();
        host.record_usage("five_hour".into(), 50, i64::MAX);
        assert!(!path.exists(), "the same reading again isn't written");
        host.record_usage("five_hour".into(), 51, i64::MAX);
        assert_eq!(super::load(&path, 0)["five_hour"], (51, i64::MAX));
    }
}
