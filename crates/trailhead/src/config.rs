//! Per-machine settings in `<config dir>/config.toml`, unlike a repo's
//! `.trailhead.toml`, which is checked in and shared.

use anyhow::{Context, Result};
use trailhead_server::Paths;

pub const DEFAULT_URL: &str = "http://127.0.0.1:7241";

pub use trailhead_server::user_config::{Setting, UserConfig, path};

/// Sets one key of the user config, keeping its comments (`table` is `None` for a top-level key).
pub fn set(paths: &Paths, table: Option<&str>, key: &str, setting: &Setting) -> Result<()> {
    trailhead_server::user_config::set_setting(paths, table, key, setting).map_err(anyhow::Error::msg)
}

pub fn load(paths: &Paths) -> Result<UserConfig> {
    let path = path(paths);
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(UserConfig::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_defaults_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { data_dir: dir.path().join("data"), config_dir: dir.path().join("config") };
        assert!(load(&paths).unwrap().url.is_none());
        set(&paths, None, "url", &Setting::Text("http://127.0.0.1:7242".into())).unwrap();
        assert_eq!(load(&paths).unwrap().url.as_deref(), Some("http://127.0.0.1:7242"));
    }

    #[test]
    fn a_set_keeps_comments_and_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { data_dir: dir.path().join("data"), config_dir: dir.path().join("config") };
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(path(&paths), "# mine\nurl = \"http://a\"\n\n[factory]\n# how many\nmax_sandboxes = 3\n")
            .unwrap();
        set(&paths, Some("factory"), "disabled", &Setting::Flag(true)).unwrap();
        let text = std::fs::read_to_string(path(&paths)).unwrap();
        assert!(text.contains("# mine") && text.contains("# how many") && text.contains("max_sandboxes = 3"), "{text}");
        assert!(text.contains("disabled = true"), "{text}");
        set(&paths, Some("factory"), "disabled", &Setting::Unset).unwrap();
        let text = std::fs::read_to_string(path(&paths)).unwrap();
        assert!(!text.contains("disabled") && text.contains("# how many"), "{text}");
    }

    #[test]
    fn accepts_a_factory_table_but_not_other_unknown_keys() {
        let cfg: UserConfig = toml::from_str("url = \"http://x\"\n[factory]\nmax_sandboxes = 3\n").unwrap();
        assert_eq!(cfg.factory.unwrap()["max_sandboxes"].as_integer(), Some(3));
        assert!(toml::from_str::<UserConfig>("urll = \"http://x\"").is_err());
    }
}
