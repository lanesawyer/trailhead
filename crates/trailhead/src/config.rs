//! Per-machine settings in `<config dir>/config.toml`, unlike a repo's
//! `.trailhead.toml`, which is checked in and shared.

use std::path::PathBuf;

use anyhow::{Context, Result};
use trailhead_server::Paths;

pub const DEFAULT_URL: &str = "http://127.0.0.1:7241";

#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct UserConfig {
    /// The server to use when neither `--url` nor `TRAILHEAD_URL` is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

pub fn path(paths: &Paths) -> PathBuf {
    paths.config_dir.join("config.toml")
}

pub fn load(paths: &Paths) -> Result<UserConfig> {
    let path = path(paths);
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(UserConfig::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn save(paths: &Paths, cfg: &UserConfig) -> Result<()> {
    std::fs::create_dir_all(&paths.config_dir)?;
    std::fs::write(path(paths), toml::to_string(cfg)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_defaults_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { data_dir: dir.path().join("data"), config_dir: dir.path().join("config") };
        assert!(load(&paths).unwrap().url.is_none());
        save(&paths, &UserConfig { url: Some("http://127.0.0.1:7242".into()) }).unwrap();
        assert_eq!(load(&paths).unwrap().url.as_deref(), Some("http://127.0.0.1:7242"));
    }
}
