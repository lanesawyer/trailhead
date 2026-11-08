//! Host-wide factory settings: the `[factory]` table in the per-machine `<config dir>/config.toml`, not in any
//! repo's `.trailhead.toml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// The usage windows `pause_at` can name.
pub const USAGE_WINDOWS: [&str; 2] = ["five_hour", "seven_day"];

/// `[factory]` in the user config. Every field is optional: no table means no host cap, no `pause_at` and no repos.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    /// Sandboxes at once across all repos.
    #[serde(default)]
    pub max_sandboxes: Option<usize>,
    /// Usage windows (`five_hour`, `seven_day`) and the percent at which to start no new runs. Host-wide, since every
    /// repo shares the subscription (`docs/factory/budgets.md`).
    #[serde(default)]
    pub pause_at: BTreeMap<String, u32>,
    /// Directories holding a `.trailhead.toml` with a `[factory]` table. A leading `~` is the home directory.
    #[serde(default)]
    pub repos: Vec<String>,
    /// Set by `trailhead factory disable`: the server runs no control plane, whatever `repos` lists.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
}

impl HostConfig {
    /// The `[factory]` table out of a `config.toml`'s text. The CLI's own keys (`url`) are left alone; a missing
    /// file's empty text or a missing table is the default.
    pub fn from_user_config(toml_src: &str) -> Result<HostConfig> {
        #[derive(Deserialize)]
        struct File {
            factory: Option<toml::Table>,
        }
        let file: File = toml::from_str(toml_src).map_err(|e| Error::Config(e.to_string()))?;
        match file.factory {
            None => Ok(HostConfig::default()),
            Some(table) => Self::from_table(table),
        }
    }

    /// The `[factory]` table, as the CLI keeps it unparsed.
    pub fn from_table(table: toml::Table) -> Result<HostConfig> {
        let config = HostConfig::deserialize(table).map_err(|e| Error::Config(format!("[factory]: {e}")))?;
        if config.max_sandboxes == Some(0) {
            return Err(Error::Config("[factory] max_sandboxes must be at least 1".into()));
        }
        for (window, percent) in &config.pause_at {
            if !USAGE_WINDOWS.contains(&window.as_str()) {
                return Err(Error::Config(format!(
                    "[factory] pause_at has unknown window `{window}`; the windows are {}",
                    USAGE_WINDOWS.join(" and ")
                )));
            }
            if *percent > 100 {
                return Err(Error::Config(format!(
                    "[factory] pause_at for {window} is {percent}; it's a percent, 0 to 100"
                )));
            }
        }
        Ok(config)
    }

    /// `repos` with `~` expanded against `home`.
    pub fn repo_dirs(&self, home: Option<&Path>) -> Vec<PathBuf> {
        self.repos.iter().map(|r| expand_home(r, home)).collect()
    }
}

fn expand_home(path: &str, home: Option<&Path>) -> PathBuf {
    match (path.strip_prefix('~'), home) {
        (Some(""), Some(home)) => home.to_path_buf(),
        (Some(rest), Some(home)) if rest.starts_with('/') => home.join(&rest[1..]),
        _ => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_table_and_ignores_the_clis_keys() {
        let c = HostConfig::from_user_config(
            "url = \"http://x\"\n[factory]\nmax_sandboxes = 3\npause_at = { five_hour = 80, seven_day = 90 }\nrepos = [\"~/Dev/a\", \"/b\"]\n",
        )
        .unwrap();
        assert_eq!(c.max_sandboxes, Some(3));
        assert_eq!(c.pause_at.get("seven_day"), Some(&90));
        assert_eq!(c.repos, ["~/Dev/a", "/b"]);
    }

    #[test]
    fn missing_file_or_table_is_empty() {
        assert_eq!(HostConfig::from_user_config("").unwrap(), HostConfig::default());
        assert_eq!(HostConfig::from_user_config("url = \"http://x\"").unwrap(), HostConfig::default());
    }

    #[test]
    fn typos_are_rejected() {
        assert!(HostConfig::from_user_config("[factory]\nmax_sandbox = 3\n").is_err());
        assert!(HostConfig::from_user_config("[factory]\nmax_sandboxes = 0\n").is_err());
        assert!(HostConfig::from_user_config("[factory]\npause_at = { five_hours = 80 }\n").is_err());
        assert!(HostConfig::from_user_config("[factory]\npause_at = { five_hour = 101 }\n").is_err());
        assert!(HostConfig::from_user_config("[factory]\npause_at = { five_hour = 100 }\n").is_ok());
    }

    #[test]
    fn tilde_expands_only_at_the_start() {
        let c = HostConfig {
            repos: vec!["~/Dev/a".into(), "~".into(), "/x/~/y".into(), "~bob/z".into()],
            ..Default::default()
        };
        let home = Path::new("/home/me");
        assert_eq!(
            c.repo_dirs(Some(home)),
            [Path::new("/home/me/Dev/a"), home, Path::new("/x/~/y"), Path::new("~bob/z")]
        );
        assert_eq!(c.repo_dirs(None)[0], Path::new("~/Dev/a"));
    }
}
