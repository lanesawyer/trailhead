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

    /// One process at a time serves a data directory, and so runs its factory: see [`lock`].
    pub fn lock_file(&self) -> PathBuf {
        self.data_dir.join("server.lock")
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

/// Takes the exclusive lock at `path`, naming `holder` in it for whoever is refused. A server holds it while it runs,
/// so a second server on the same data directory (on another port, say) can't start, and with it a second factory
/// control plane on the same database, iroh key and sandboxes; `trailhead factory check`, which binds that key,
/// holds it too. The OS lets go when the returned file drops, when the process dies, and on exec (the descriptor is
/// close-on-exec), which is how a server restarting onto a new binary hands it on.
pub fn lock(path: &Path, holder: &str) -> Result<std::fs::File, String> {
    use std::io::{Read, Seek, Write};
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    match file.try_lock() {
        Ok(()) => {
            let line = format!("pid {}, {holder}\n", std::process::id());
            let wrote = file.set_len(0).and_then(|()| file.rewind()).and_then(|()| file.write_all(line.as_bytes()));
            wrote.map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(file)
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            let mut held = String::new();
            let _ = file.read_to_string(&mut held);
            let held = held.trim();
            let by = if held.is_empty() { String::new() } else { format!(" ({held})") };
            Err(format!(
                "another Trailhead process is using this data directory{by}: one server at a time, and `trailhead factory check` only while none runs ({})",
                path.display()
            ))
        }
        Err(std::fs::TryLockError::Error(e)) => Err(format!("{}: {e}", path.display())),
    }
}

#[cfg(test)]
mod lock_tests {
    #[test]
    fn a_second_holder_is_refused_naming_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.lock");
        let held = super::lock(&path, "a server at 127.0.0.1:7241").unwrap();
        let err = super::lock(&path, "a server at 127.0.0.1:7300").unwrap_err();
        assert!(err.contains(&format!("pid {}, a server at 127.0.0.1:7241", std::process::id())), "{err}");
        drop(held);
        super::lock(&path, "a server at 127.0.0.1:7300").unwrap();
    }
}
