//! Where the CLI keeps tokens: the OS keyring when one is reachable, otherwise a
//! `0600` file in the config directory. `TRAILHEAD_TOKEN` is handled by the caller
//! and beats both.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use trailhead_server::Paths;

/// A saved credential: the local user's, or a local agent's.
#[derive(Clone, Copy)]
pub enum Cred<'a> {
    User,
    Agent(&'a str),
}

impl Cred<'_> {
    fn account(self) -> String {
        match self {
            Cred::User => "user".into(),
            Cred::Agent(name) => format!("agent:{name}"),
        }
    }

    fn file(self, paths: &Paths) -> PathBuf {
        match self {
            Cred::User => paths.credentials(),
            Cred::Agent(name) => paths.agent_token(name),
        }
    }
}

pub enum Saved {
    Keyring,
    File(PathBuf),
}

/// `TRAILHEAD_NO_KEYRING=1` turns the keyring off, e.g. in tests or on machines
/// whose secret service prompts.
fn keyring_disabled() -> bool {
    std::env::var("TRAILHEAD_NO_KEYRING").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// One keyring service per config directory, so side-by-side instances (and test
/// runs) never see each other's tokens.
fn service(config_dir: &Path) -> String {
    let dir = std::path::absolute(config_dir).unwrap_or_else(|_| config_dir.to_path_buf());
    format!("trailhead:{}", dir.display())
}

fn entry(paths: &Paths, cred: Cred) -> Option<keyring::Entry> {
    if keyring_disabled() {
        return None;
    }
    keyring::Entry::new(&service(&paths.config_dir), &cred.account()).ok()
}

/// Reads a credential: keyring first, then the file. Keyring errors (no secret
/// service, locked, D-Bus down) fall through to the file.
pub fn read(paths: &Paths, cred: Cred) -> Result<String> {
    if let Some(secret) = entry(paths, cred).and_then(|e| e.get_password().ok()) {
        return Ok(secret.trim().to_string());
    }
    let path = cred.file(paths);
    match std::fs::read_to_string(&path) {
        Ok(s) => Ok(s.trim().to_string()),
        Err(_) => match cred {
            Cred::User => bail!("no token: set TRAILHEAD_TOKEN or run a local server to create {}", path.display()),
            Cred::Agent(name) => bail!("no saved token for agent {name:?} in the keyring or at {}", path.display()),
        },
    }
}

/// Saves a credential to the keyring if it's available, else to the `0600` file.
/// A keyring save removes any older file copy so no plaintext token is left behind.
pub fn save(paths: &Paths, cred: Cred, secret: &str) -> Result<Saved> {
    let path = cred.file(paths);
    if entry(paths, cred).is_some_and(|e| e.set_password(secret).is_ok()) {
        let _ = std::fs::remove_file(&path);
        return Ok(Saved::Keyring);
    }
    trailhead_server::write_secret(&path, secret)?;
    Ok(Saved::File(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &Path) -> Paths {
        Paths { data_dir: dir.join("data"), config_dir: dir.join("config") }
    }

    #[test]
    fn service_is_tied_to_the_config_dir() {
        assert_ne!(service(Path::new("/tmp/a")), service(Path::new("/tmp/b")));
        assert_eq!(service(Path::new("/tmp/a")), "trailhead:/tmp/a");
    }

    /// Talks to the real OS keyring, under a service named after a temp dir, and
    /// deletes what it wrote. Run with `cargo test -- --ignored` on a desktop.
    #[test]
    #[ignore = "needs an OS keyring"]
    fn keyring_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let file = p.agent_token("kr-test");
        trailhead_server::write_secret(&file, "old").unwrap();
        assert!(matches!(save(&p, Cred::Agent("kr-test"), "s3cret").unwrap(), Saved::Keyring));
        assert!(!file.exists(), "a keyring save removes the plaintext copy");
        assert_eq!(read(&p, Cred::Agent("kr-test")).unwrap(), "s3cret");
        entry(&p, Cred::Agent("kr-test")).unwrap().delete_credential().unwrap();
        assert!(read(&p, Cred::Agent("kr-test")).is_err());
    }
}
