//! Where local tokens live: the OS keyring when one is reachable, otherwise a
//! `0600` file in the config directory. The server and the CLI share this, so the
//! bootstrap credential and agent tokens follow the same rules.

use std::path::PathBuf;

/// A saved credential: the local user's, or a local agent's.
#[derive(Clone, Copy, Debug)]
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
}

pub enum Saved {
    Keyring,
    File(PathBuf),
}

/// Credentials for one config directory.
#[derive(Clone, Debug)]
pub struct CredStore {
    pub config_dir: PathBuf,
    /// Off in tests, or with `TRAILHEAD_NO_KEYRING=1` on machines whose secret
    /// service prompts.
    pub keyring: bool,
}

impl CredStore {
    pub fn new(config_dir: PathBuf) -> Self {
        let disabled = std::env::var("TRAILHEAD_NO_KEYRING").is_ok_and(|v| !v.is_empty() && v != "0");
        CredStore { config_dir, keyring: !disabled }
    }

    pub fn file(&self, cred: Cred) -> PathBuf {
        match cred {
            Cred::User => self.config_dir.join("credentials"),
            // `parent/child` names flatten to `parent.child`: a parent's token is a
            // file, so it can't also be a directory, and names never contain `.`.
            Cred::Agent(name) => self.config_dir.join("agents").join(name.replace('/', ".")),
        }
    }

    /// One keyring service per config directory, so side-by-side instances (and
    /// test runs) never see each other's tokens.
    fn service(&self) -> String {
        let dir = std::path::absolute(&self.config_dir).unwrap_or_else(|_| self.config_dir.clone());
        format!("trailhead:{}", dir.display())
    }

    fn entry(&self, cred: Cred) -> Option<keyring::Entry> {
        if !self.keyring {
            return None;
        }
        keyring::Entry::new(&self.service(), &cred.account()).ok()
    }

    /// Keyring first, then the file. Keyring errors (no secret service, locked,
    /// D-Bus down) fall through to the file.
    pub fn read(&self, cred: Cred) -> Option<(String, Saved)> {
        if let Some(secret) = self.entry(cred).and_then(|e| e.get_password().ok()) {
            return Some((secret.trim().to_string(), Saved::Keyring));
        }
        let path = self.file(cred);
        let secret = std::fs::read_to_string(&path).ok()?;
        Some((secret.trim().to_string(), Saved::File(path)))
    }

    /// To the keyring if it's available, else to the `0600` file. A keyring save
    /// removes any older file copy so no plaintext token is left behind.
    pub fn save(&self, cred: Cred, secret: &str) -> std::io::Result<Saved> {
        let path = self.file(cred);
        if self.entry(cred).is_some_and(|e| e.set_password(secret).is_ok()) {
            let _ = std::fs::remove_file(&path);
            return Ok(Saved::Keyring);
        }
        crate::write_secret(&path, secret)?;
        Ok(Saved::File(path))
    }

    #[cfg(test)]
    fn delete(&self, cred: Cred) {
        if let Some(e) = self.entry(cred) {
            let _ = e.delete_credential();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_is_tied_to_the_config_dir() {
        let a = CredStore { config_dir: "/tmp/a".into(), keyring: true };
        let b = CredStore { config_dir: "/tmp/b".into(), keyring: true };
        assert_ne!(a.service(), b.service());
        assert_eq!(a.service(), format!("trailhead:{}", std::path::absolute("/tmp/a").unwrap().display()));
    }

    #[test]
    fn without_a_keyring_tokens_go_to_files() {
        let dir = tempfile::tempdir().unwrap();
        let s = CredStore { config_dir: dir.path().into(), keyring: false };
        assert!(matches!(s.save(Cred::Agent("a"), "tok").unwrap(), Saved::File(_)));
        assert_eq!(s.read(Cred::Agent("a")).unwrap().0, "tok");
        assert!(s.read(Cred::User).is_none());
    }

    /// Talks to the real OS keyring, under a service named after a temp dir, and
    /// deletes what it wrote. Run with `cargo test -- --ignored` on a desktop.
    #[test]
    #[ignore = "needs an OS keyring"]
    fn keyring_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let s = CredStore { config_dir: dir.path().into(), keyring: true };
        let file = s.file(Cred::Agent("kr-test"));
        crate::write_secret(&file, "old").unwrap();
        assert!(matches!(s.save(Cred::Agent("kr-test"), "s3cret").unwrap(), Saved::Keyring));
        assert!(!file.exists(), "a keyring save removes the plaintext copy");
        assert_eq!(s.read(Cred::Agent("kr-test")).unwrap().0, "s3cret");
        s.delete(Cred::Agent("kr-test"));
        assert!(s.read(Cred::Agent("kr-test")).is_none());
    }
}
