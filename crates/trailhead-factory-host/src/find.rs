//! Finding the factory file by walking up from the working directory, the way `.trailhead.toml` is found.

use std::path::{Path, PathBuf};

/// The factory file's name. TRAIL-163 moves it into `.trailhead.toml`; [`config`] is the one place that looks.
const CONFIG: &str = "factory.toml";

/// The nearest `factory.toml` at or above `dir`.
pub fn config(dir: &Path) -> Result<PathBuf, String> {
    dir.ancestors().map(|d| d.join(CONFIG)).find(|p| p.is_file()).ok_or_else(|| {
        format!("no {CONFIG} at or above {}; run from inside the repo or pass --config <path>", dir.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_is_found_from_a_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(CONFIG), "").unwrap();
        let nested = dir.path().join("crates/a");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(config(&nested).unwrap(), dir.path().join(CONFIG));
    }

    #[test]
    fn the_nearest_config_wins() {
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("inner");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(dir.path().join(CONFIG), "").unwrap();
        std::fs::write(inner.join(CONFIG), "").unwrap();
        assert_eq!(config(&inner).unwrap(), inner.join(CONFIG));
    }

    #[test]
    fn a_missing_config_names_the_flag() {
        let dir = tempfile::tempdir().unwrap();
        // A tempdir sits under /tmp, which has no factory.toml above it.
        let err = config(dir.path()).unwrap_err();
        assert!(err.contains("--config"), "{err}");
    }
}
