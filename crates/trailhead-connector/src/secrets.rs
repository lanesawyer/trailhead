//! The secrets a connector declared by name, as the server hands them over: in environment variables.

use std::collections::BTreeMap;
use std::fmt;

use crate::{Manifest, env_var};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SecretError {
    #[error("secret `{name}` is not set: expected it in the environment variable {var}")]
    Missing { name: String, var: String },
    #[error("`{0}` is not a secret this connector declared")]
    Undeclared(String),
}

#[derive(Clone)]
pub struct Secrets(BTreeMap<String, String>);

impl Secrets {
    pub fn from_env(manifest: &Manifest) -> Result<Secrets, SecretError> {
        Secrets::from_lookup(manifest, |var| std::env::var(var).ok())
    }

    pub fn from_pairs<'a>(
        manifest: &Manifest,
        pairs: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Result<Secrets, SecretError> {
        let pairs: BTreeMap<_, _> = pairs.into_iter().collect();
        Secrets::from_lookup(manifest, |var| {
            pairs.iter().find(|(n, _)| env_var(n) == var).map(|(_, v)| (*v).to_string())
        })
    }

    pub(crate) fn from_lookup(
        manifest: &Manifest,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Secrets, SecretError> {
        let mut map = BTreeMap::new();
        for name in &manifest.secrets {
            let var = env_var(name);
            match lookup(&var).filter(|v| !v.is_empty()) {
                Some(value) => map.insert(name.clone(), value),
                None => return Err(SecretError::Missing { name: name.clone(), var }),
            };
        }
        Ok(Secrets(map))
    }

    pub fn get(&self, name: &str) -> Result<&str, SecretError> {
        self.0.get(name).map(String::as_str).ok_or_else(|| SecretError::Undeclared(name.to_string()))
    }
}

impl fmt::Debug for Secrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.0.keys()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::GITHUB;

    #[test]
    fn reads_declared_secrets_and_never_prints_them() {
        let m = Manifest::parse(GITHUB).unwrap();
        let s = Secrets::from_pairs(&m, [("token", "hunter2")]).unwrap();
        assert_eq!(s.get("token").unwrap(), "hunter2");
        assert_eq!(s.get("other"), Err(SecretError::Undeclared("other".into())));
        let shown = format!("{s:?}");
        assert!(shown.contains("token") && !shown.contains("hunter2"));
    }

    #[test]
    fn a_missing_or_empty_secret_names_itself_and_its_variable() {
        let m = Manifest::parse(GITHUB).unwrap();
        for pairs in [vec![], vec![("token", "")]] {
            let e = Secrets::from_pairs(&m, pairs).unwrap_err().to_string();
            assert!(e.contains("`token`") && e.contains("TRAILHEAD_CONNECTOR_SECRET_TOKEN"), "{e}");
        }
    }

    #[test]
    fn from_env_reads_the_process_environment() {
        let m = Manifest::parse(GITHUB).unwrap();
        assert!(matches!(Secrets::from_env(&m), Err(SecretError::Missing { .. })));
    }
}
