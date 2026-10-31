//! Host-wide factory settings: the `[factory]` table in the per-machine `<config dir>/config.toml`, not in any
//! repo's `.trailhead.toml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Error, Result, UsageConfig};

/// `[factory]` in the user config. Every field is optional: no table means no host cap, no `pause_at` and no repos.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    /// Sandboxes at once across all repos.
    #[serde(default)]
    pub max_sandboxes: Option<usize>,
    /// Usage windows (`five_hour`, `seven_day`) and the percent at which to start no new runs.
    #[serde(default)]
    pub pause_at: BTreeMap<String, u32>,
    /// Directories holding a `.trailhead.toml` with a `[factory]` table. A leading `~` is the home directory.
    #[serde(default)]
    pub repos: Vec<String>,
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
        Ok(config)
    }

    /// `repos` with `~` expanded against `home`.
    pub fn repo_dirs(&self, home: Option<&Path>) -> Vec<PathBuf> {
        self.repos.iter().map(|r| expand_home(r, home)).collect()
    }

    /// The `pause_at` to enforce: this config's value for a window wins; a window it doesn't set takes the strictest
    /// (lowest) value any repo's `[usage]` sets, which is deprecated. The strings are warnings to log.
    pub fn effective_pause_at<'a>(
        &self,
        repos: impl IntoIterator<Item = (&'a str, &'a UsageConfig)>,
    ) -> (BTreeMap<String, u32>, Vec<String>) {
        let mut merged = self.pause_at.clone();
        let mut warnings = Vec::new();
        let mut from_repos: BTreeMap<String, u32> = BTreeMap::new();
        for (name, usage) in repos {
            if usage.pause_at.is_empty() {
                continue;
            }
            warnings.push(format!(
                "{name}: [factory.usage] pause_at is deprecated; set pause_at in [factory] of the user config.toml"
            ));
            for (window, limit) in &usage.pause_at {
                let slot = from_repos.entry(window.clone()).or_insert(*limit);
                if *limit != *slot {
                    warnings.push(format!(
                        "{name}: pause_at for {window} disagrees with another repo; using the strictest"
                    ));
                    *slot = (*slot).min(*limit);
                }
            }
        }
        for (window, limit) in from_repos {
            merged.entry(window).or_insert(limit);
        }
        (merged, warnings)
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

    fn usage(pairs: &[(&str, u32)]) -> UsageConfig {
        UsageConfig { pause_at: pairs.iter().map(|(w, n)| (w.to_string(), *n)).collect() }
    }

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

    #[test]
    fn host_pause_at_wins_and_repos_fall_back_strictest_first() {
        let host = HostConfig { pause_at: [("five_hour".to_string(), 70)].into(), ..Default::default() };
        let (a, b) = (usage(&[("five_hour", 50), ("seven_day", 90)]), usage(&[("seven_day", 85)]));
        let (merged, warnings) = host.effective_pause_at([("a", &a), ("b", &b)]);
        assert_eq!(merged.get("five_hour"), Some(&70));
        assert_eq!(merged.get("seven_day"), Some(&85));
        assert!(warnings.iter().any(|w| w.contains("deprecated")));
        assert!(warnings.iter().any(|w| w.contains("disagrees")));

        let (merged, warnings) = HostConfig::default().effective_pause_at([("a", &UsageConfig::default())]);
        assert!(merged.is_empty() && warnings.is_empty());
    }
}
