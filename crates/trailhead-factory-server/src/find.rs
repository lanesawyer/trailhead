//! Finding the factory's settings: the `[factory]` table of the nearest `.trailhead.toml`, found by walking up from the
//! working directory the way the CLI does.

use std::path::{Path, PathBuf};

use trailhead_factory::Repo;

/// The repo file's name; [`config`] is the one place that looks.
const CONFIG: &str = ".trailhead.toml";

/// The nearest `.trailhead.toml` at or above `dir`.
pub fn config(dir: &Path) -> Result<PathBuf, String> {
    dir.ancestors().map(|d| d.join(CONFIG)).find(|p| p.is_file()).ok_or_else(|| {
        format!("no {CONFIG} at or above {}; run from inside the repo or pass --config <path>", dir.display())
    })
}

/// The repo the factory works on: the `[factory]` table of `config`, else of the nearest `.trailhead.toml`.
pub fn repo(config_path: Option<&Path>) -> Result<Repo, String> {
    let path = match config_path {
        Some(path) => path.to_path_buf(),
        None => config(&std::env::current_dir().map_err(|e| e.to_string())?)?,
    };
    let src = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let config = trailhead_factory::FactoryConfig::from_repo_file(&src, remote(dir))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(config.repo.repo())
}

/// The owner and name of the GitHub repo that `dir`'s `origin` remote points at.
pub fn remote(dir: &Path) -> Option<Repo> {
    let out =
        std::process::Command::new("git").arg("-C").arg(dir).args(["remote", "get-url", "origin"]).output().ok()?;
    out.status.success().then(|| parse_remote(String::from_utf8_lossy(&out.stdout).trim())).flatten()
}

fn parse_remote(url: &str) -> Option<Repo> {
    let path = url.split_once("github.com").map(|(_, rest)| rest)?;
    let path = path.trim_start_matches([':', '/']).trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    (!owner.is_empty() && !name.is_empty() && !name.contains('/'))
        .then(|| Repo { owner: owner.into(), name: name.into() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remotes_parse_in_the_usual_forms() {
        for url in [
            "https://github.com/lanesawyer/trailhead.git",
            "https://github.com/lanesawyer/trailhead",
            "git@github.com:lanesawyer/trailhead.git",
            "ssh://git@github.com/lanesawyer/trailhead.git",
        ] {
            let repo = parse_remote(url).unwrap_or_else(|| panic!("{url}"));
            assert_eq!(repo.slug(), "lanesawyer/trailhead", "{url}");
        }
        assert!(parse_remote("https://example.com/a/b.git").is_none());
        assert!(parse_remote("https://github.com/a").is_none());
    }

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
        // A tempdir sits under /tmp, which has no .trailhead.toml above it.
        let err = config(dir.path()).unwrap_err();
        assert!(err.contains("--config"), "{err}");
    }
}
