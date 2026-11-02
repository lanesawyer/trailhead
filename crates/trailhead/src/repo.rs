//! Per-repo defaults from a `.trailhead.toml` in the working directory or any
//! parent, so agent sessions in a repo act as the agent without `--as`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub const FILE: &str = ".trailhead.toml";

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    /// Act as this local agent unless `--token`, `--as` or `--as-me` says otherwise.
    pub agent: Option<String>,
    /// Default `-p` for `ready`, `ls` and `add`.
    pub project: Option<String>,
    /// The factory's settings (`docs/factory/`). Accepted but not read: the factory parses them, so the CLI doesn't
    /// depend on it (PLAN principle 10).
    #[serde(rename = "factory")]
    pub _factory: Option<toml::Value>,
    #[serde(skip)]
    pub path: PathBuf,
}

/// The nearest `.trailhead.toml` at or above `dir`, if any.
pub fn find(dir: &Path) -> Result<Option<RepoConfig>> {
    let Some(path) = dir.ancestors().map(|d| d.join(FILE)).find(|p| p.is_file()) else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut cfg: RepoConfig = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    cfg.path = path;
    Ok(Some(cfg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn found_from_a_nested_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), "agent = \"claude\"\nproject = \"TRAIL\"\n").unwrap();
        let nested = dir.path().join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        let cfg = find(&nested).unwrap().unwrap();
        assert_eq!(cfg.agent.as_deref(), Some("claude"));
        assert_eq!(cfg.project.as_deref(), Some("TRAIL"));
        assert_eq!(cfg.path, dir.path().join(FILE));
    }

    #[test]
    fn the_factory_table_is_accepted_without_being_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), "project = \"TRAIL\"\n[factory.repo]\nqueue = \"TRAIL-1\"\n").unwrap();
        assert_eq!(find(dir.path()).unwrap().unwrap().project.as_deref(), Some("TRAIL"));
    }

    #[test]
    fn unknown_keys_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), "agnet = \"claude\"\n").unwrap();
        assert!(find(dir.path()).is_err());
    }
}
