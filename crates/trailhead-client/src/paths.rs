//! Where a Trailhead instance keeps its data and config, and where its local server listens. The server, the CLI and
//! the factory share these, so they live here rather than in the server.

use std::path::{Path, PathBuf};

use crate::creds;

/// Port 7241 spells T-R-A-I on a phone keypad.
pub const DEFAULT_BIND: &str = "127.0.0.1:7241";

/// OS-standard locations for Trailhead's data and config.
#[derive(Debug, Clone)]
pub struct Paths {
    pub data_dir: PathBuf,
    pub config_dir: PathBuf,
}

impl Paths {
    /// `TRAILHEAD_DATA_DIR` and `TRAILHEAD_CONFIG_DIR` override the OS defaults,
    /// which is how tests and side-by-side instances stay isolated.
    pub fn from_env() -> Option<Paths> {
        let dirs = directories::ProjectDirs::from("", "", "trailhead");
        let pick = |var: &str, default: Option<&Path>| {
            std::env::var_os(var).map(PathBuf::from).or_else(|| default.map(Path::to_path_buf))
        };
        Some(Paths {
            data_dir: pick("TRAILHEAD_DATA_DIR", dirs.as_ref().map(|d| d.data_dir()))?,
            config_dir: pick("TRAILHEAD_CONFIG_DIR", dirs.as_ref().map(|d| d.config_dir()))?,
        })
    }

    pub fn db(&self) -> PathBuf {
        self.data_dir.join("trailhead.db")
    }

    pub fn creds(&self) -> creds::CredStore {
        creds::CredStore::new(self.config_dir.clone())
    }

    pub fn pid_file(&self) -> PathBuf {
        self.data_dir.join("server.pid")
    }

    pub fn instance_file(&self) -> PathBuf {
        self.data_dir.join("instance")
    }

    /// This data directory's instance id, if a server has ever run from it.
    pub fn instance(&self) -> Option<String> {
        let id = std::fs::read_to_string(self.instance_file()).ok()?;
        Some(id.trim().to_string()).filter(|id| !id.is_empty())
    }

    /// Like [`Paths::instance`], creating the id on first use.
    pub fn instance_or_create(&self) -> std::io::Result<String> {
        if let Some(id) = self.instance() {
            return Ok(id);
        }
        std::fs::create_dir_all(&self.data_dir)?;
        let id = uuid::Uuid::now_v7().simple().to_string();
        std::fs::write(self.instance_file(), format!("{id}\n"))?;
        Ok(id)
    }

    pub fn server_log(&self) -> PathBuf {
        self.data_dir.join("server.log")
    }
}
