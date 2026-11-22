//! Where the builder keeps things, under `<data dir>/shell`, and the state it writes there for the server's API and
//! the launcher to read.

use std::io::Write;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use trailhead_model::shell::{BuildError, ShellBuild};

/// The name of the shell's binary, in the apps repo and in each build's directory.
pub const BINARY: &str = "trailhead-shell";

/// `<data dir>/shell`:
///
/// - `src/`: the builder's own clone of the apps repo.
/// - `target/`: cargo's target directory for it.
/// - `builds/<rev>/trailhead-shell`: the current build and the one before it.
/// - `state.json`: [`State`], written only by the builder.
/// - `bad`: builds the launcher fell back from, one commit a line, written only by the launcher.
/// - `build.log`: cargo's output for the last build.
#[derive(Debug, Clone)]
pub struct Layout {
    pub dir: PathBuf,
}

impl Layout {
    pub fn new(data_dir: &Path) -> Layout {
        Layout { dir: data_dir.join("shell") }
    }

    pub fn src(&self) -> PathBuf {
        self.dir.join("src")
    }

    pub fn target(&self) -> PathBuf {
        self.dir.join("target")
    }

    pub fn builds(&self) -> PathBuf {
        self.dir.join("builds")
    }

    /// The binary built from `rev`.
    pub fn binary(&self, rev: &str) -> PathBuf {
        self.builds().join(rev).join(BINARY)
    }

    pub fn state_file(&self) -> PathBuf {
        self.dir.join("state.json")
    }

    pub fn log(&self) -> PathBuf {
        self.dir.join("build.log")
    }

    fn bad_file(&self) -> PathBuf {
        self.dir.join("bad")
    }

    /// What the builder wrote last; nothing built when there is no file or it doesn't read.
    pub fn load(&self) -> State {
        std::fs::read_to_string(self.state_file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    /// Writes the state beside its file and moves it over, so a reader never sees half of it.
    pub fn save(&self, state: &State) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.state_file();
        let beside = path.with_extension("json.new");
        std::fs::write(&beside, serde_json::to_string_pretty(state)?)?;
        std::fs::rename(&beside, &path)
    }

    /// The builds the launcher marked bad.
    pub fn bad(&self) -> Vec<String> {
        std::fs::read_to_string(self.bad_file())
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect()
    }

    /// Marks `rev` bad, so the launcher doesn't run it again and the builder doesn't build it again.
    pub fn mark_bad(&self, rev: &str) -> std::io::Result<()> {
        if self.bad().iter().any(|b| b == rev) {
            return Ok(());
        }
        std::fs::create_dir_all(&self.dir)?;
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(self.bad_file())?;
        writeln!(file, "{rev}")
    }

    /// The state and the bad builds, as `GET /v1/shell/build` and `trailhead app` show them.
    pub fn read(&self) -> ShellBuild {
        let state = self.load();
        ShellBuild {
            configured: true,
            current: state.current.map(|b| b.rev),
            previous: state.previous.map(|b| b.rev),
            building: state.building,
            error: state.error,
            bad: self.bad(),
            changes: Vec::new(),
            changes_total: 0,
            repo: None,
        }
    }
}

/// `state.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub current: Option<Built>,
    pub previous: Option<Built>,
    pub building: Option<String>,
    pub error: Option<BuildError>,
}

/// A build in `builds/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Built {
    pub rev: String,
    pub at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_and_the_bad_builds_are_read_back_together() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        assert_eq!(layout.read(), ShellBuild { configured: true, ..Default::default() });
        let at = Timestamp::UNIX_EPOCH;
        let state = State {
            current: Some(Built { rev: "b".into(), at }),
            previous: Some(Built { rev: "a".into(), at }),
            building: Some("c".into()),
            error: None,
        };
        layout.save(&state).unwrap();
        layout.mark_bad("b").unwrap();
        layout.mark_bad("b").unwrap();
        assert_eq!(layout.load(), state);
        let read = layout.read();
        assert_eq!((read.current.as_deref(), read.previous.as_deref()), (Some("b"), Some("a")));
        assert_eq!((read.building.as_deref(), read.bad), (Some("c"), vec!["b".to_string()]));
    }
}
