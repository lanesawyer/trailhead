//! `[shell]` in the user config: which apps repo the builder builds the shell from, and how.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// `[shell]` in `<config dir>/config.toml`. Without `repo` there is no builder.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellConfig {
    /// The apps repo's checkout. Its `origin` remote and default branch say what to build; the builder builds in a
    /// clone of its own and never touches this one. A leading `~` is the home directory.
    pub repo: Option<String>,
    /// The cargo package to build. Without it, the package in the repo's workspace with a binary named
    /// `trailhead-shell`.
    pub package: Option<String>,
    #[serde(default = "default_profile")]
    pub profile: String,
    /// Cargo's `-j`: the build shares the machine with the server and the factory's sandboxes.
    #[serde(default = "default_jobs")]
    pub jobs: u32,
    /// How often to look for a new commit on the base branch.
    #[serde(default = "default_poll_secs")]
    pub poll_secs: u64,
}

fn default_profile() -> String {
    "release".into()
}

fn default_jobs() -> u32 {
    3
}

fn default_poll_secs() -> u64 {
    60
}

impl ShellConfig {
    /// `[shell]` out of a `config.toml`'s text: `None` when there is no table or it names no repo.
    pub fn from_user_config(toml_src: &str) -> Result<Option<ShellConfig>, String> {
        #[derive(Deserialize)]
        struct File {
            shell: Option<toml::Table>,
        }
        let file: File = toml::from_str(toml_src).map_err(|e| e.to_string())?;
        match file.shell {
            None => Ok(None),
            Some(table) => Ok(Some(Self::from_table(table)?).filter(|c| c.repo.is_some())),
        }
    }

    /// The `[shell]` table, as the CLI keeps it unparsed.
    pub fn from_table(table: toml::Table) -> Result<ShellConfig, String> {
        let config = ShellConfig::deserialize(table).map_err(|e| format!("[shell]: {e}"))?;
        if config.jobs == 0 {
            return Err("[shell] jobs must be at least 1".into());
        }
        if config.poll_secs == 0 {
            return Err("[shell] poll_secs must be at least 1".into());
        }
        if config.profile.is_empty() {
            return Err("[shell] profile is empty".into());
        }
        Ok(config)
    }

    /// The `[shell]` table of the user config in `config_dir`; `None` when there is no file, table or repo.
    pub fn read(config_dir: &Path) -> Result<Option<ShellConfig>, String> {
        let path = config_dir.join("config.toml");
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::from_user_config(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// The repo's directory, with a leading `~` made the home directory.
    pub fn repo_dir(&self, home: Option<&Path>) -> Option<PathBuf> {
        let repo = self.repo.as_deref()?;
        Some(match (repo.strip_prefix('~'), home) {
            (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
                home.join(rest.trim_start_matches('/'))
            }
            _ => PathBuf::from(repo),
        })
    }

    /// The repo's directory as the builder needs it: there, and a git checkout. `Ok(None)` when no repo is named.
    pub fn checked_repo_dir(&self, home: Option<&Path>) -> Result<Option<PathBuf>, String> {
        let Some(repo) = self.repo_dir(home) else { return Ok(None) };
        if !repo.is_dir() {
            return Err(format!("[shell] repo {} isn't a directory", repo.display()));
        }
        if !repo.join(".git").exists() {
            return Err(format!("[shell] repo {} isn't a git checkout", repo.display()));
        }
        Ok(Some(repo))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shell_table_with_a_repo_is_read_with_its_defaults() {
        let config = ShellConfig::from_user_config("url = \"x\"\n[factory]\nrepos = []\n[shell]\nrepo = \"~/apps\"\n")
            .unwrap()
            .unwrap();
        assert_eq!((config.profile.as_str(), config.jobs, config.poll_secs), ("release", 3, 60));
        assert_eq!(config.package, None);
        assert_eq!(config.repo_dir(Some(Path::new("/home/me"))), Some(PathBuf::from("/home/me/apps")));
    }

    #[test]
    fn no_table_or_no_repo_is_no_builder_and_unknown_keys_are_refused() {
        assert_eq!(ShellConfig::from_user_config("url = \"x\"\n").unwrap(), None);
        assert_eq!(ShellConfig::from_user_config("[shell]\njobs = 2\n").unwrap(), None);
        assert!(ShellConfig::from_user_config("[shell]\nrepo = \"a\"\nrepoo = \"b\"\n").is_err());
        assert!(ShellConfig::from_user_config("[shell]\nrepo = \"a\"\njobs = 0\n").is_err());
    }

    #[test]
    fn the_repo_must_be_a_directory_with_a_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let config =
            |repo: &Path| ShellConfig::from_table(toml::toml! { repo = (repo.display().to_string()) }).unwrap();
        let missing = config(&dir.path().join("nope")).checked_repo_dir(None).unwrap_err();
        assert!(missing.ends_with("isn't a directory"), "{missing}");
        let plain = config(dir.path()).checked_repo_dir(None).unwrap_err();
        assert!(plain.ends_with("isn't a git checkout"), "{plain}");
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        assert_eq!(config(dir.path()).checked_repo_dir(None).unwrap().as_deref(), Some(dir.path()));
        let none = ShellConfig::from_table(toml::Table::new()).unwrap();
        assert_eq!(none.checked_repo_dir(None).unwrap(), None);
    }
}
