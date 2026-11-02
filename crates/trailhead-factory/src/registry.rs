//! Name-keyed registries for the static plugins (`docs/factory/seams.md`): a profile's `adapter` picks a
//! [`HarnessKind`] and its `provider` a [`ProviderKind`], so an unknown name fails when the config loads.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::{Error, FactoryConfig, Harness, ModelProvider, Result};

/// Credentials and settings a harness needs in the sandbox, by name. Each harness defines its own keys; the
/// control plane carries them to the daemon without reading them.
pub type HarnessSecrets = BTreeMap<String, String>;

/// What the daemon hands a harness when it builds one.
#[derive(Debug, Clone)]
pub struct HarnessSetup {
    /// The agent CLI to run, as given to the daemon.
    pub executable: PathBuf,
    pub secrets: HarnessSecrets,
}

/// A harness implementation the daemon can build by name.
#[derive(Clone, Copy)]
pub struct HarnessKind {
    /// Builds the harness in the sandbox.
    pub build: fn(&HarnessSetup) -> Arc<dyn Harness>,
    /// The hosts a sandbox must reach for this harness with these secrets, besides the gateway.
    pub egress: fn(&HarnessSecrets) -> Vec<String>,
}

/// A model provider the control plane can build by name, from the model endpoint the sandboxes call.
pub type ProviderKind = fn(model_endpoint: String) -> Arc<dyn ModelProvider>;

/// Plugins by the name config gives them.
pub struct Registry<T> {
    kind: &'static str,
    entries: BTreeMap<&'static str, T>,
}

impl<T> Registry<T> {
    /// An empty registry; `kind` ("adapter", "provider") names what it holds in errors.
    pub fn new(kind: &'static str) -> Registry<T> {
        Registry { kind, entries: BTreeMap::new() }
    }

    pub fn with(mut self, name: &'static str, entry: T) -> Registry<T> {
        self.entries.insert(name, entry);
        self
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.entries.keys().copied().collect()
    }

    pub fn get(&self, name: &str) -> Result<&T> {
        self.entries.get(name).ok_or_else(|| {
            Error::Config(format!("unknown {} `{name}` (known: {})", self.kind, self.names().join(", ")))
        })
    }
}

impl FactoryConfig {
    /// Refuses a profile naming an adapter or provider that isn't registered, and a worker and reviewer that
    /// differ in either: a repo's sandboxes run one harness and its factory has one provider.
    pub fn check_plugins(&self, harnesses: &[&str], providers: &[&str]) -> Result<()> {
        let known = |kind: &str, name: &str, known: &[&str]| {
            if known.contains(&name) {
                Ok(())
            } else {
                Err(Error::Config(format!("unknown {kind} `{name}` (known: {})", known.join(", "))))
            }
        };
        for (name, p) in &self.profiles {
            known("adapter", &p.adapter, harnesses).map_err(|e| in_profile(name, e))?;
            known("provider", &p.provider, providers).map_err(|e| in_profile(name, e))?;
        }
        let (worker, reviewer) = (self.worker_profile(), self.reviewer_profile());
        if let (Some(w), Some(r)) = (worker, reviewer)
            && (w.adapter != r.adapter || w.provider != r.provider)
        {
            return Err(Error::Config(
                "the worker and reviewer profiles must use the same adapter and provider".into(),
            ));
        }
        Ok(())
    }

    /// The `adapter` and `provider` the repo's worker profile names.
    pub fn plugins(&self) -> Option<(&str, &str)> {
        self.worker_profile().map(|p| (p.adapter.as_str(), p.provider.as_str()))
    }

    fn worker_profile(&self) -> Option<&crate::Profile> {
        self.profiles.get(&self.repo.worker)
    }

    fn reviewer_profile(&self) -> Option<&crate::Profile> {
        self.profiles.get(&self.repo.reviewer)
    }
}

fn in_profile(profile: &str, e: Error) -> Error {
    match e {
        Error::Config(msg) => Error::Config(format!("[factory.profiles.{profile}]: {msg}")),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(reviewer_adapter: &str) -> FactoryConfig {
        let src = format!(
            "[repo]\nowner = \"o\"\nname = \"n\"\nworker = \"w\"\nreviewer = \"r\"\n\
             [profiles.w]\nrole = \"worker\"\nadapter = \"claude-code\"\nprovider = \"anthropic\"\nmodel = \"sonnet\"\nbudget = {{ usd = 5, minutes = 30, steps = 50 }}\n\
             [profiles.r]\nrole = \"reviewer\"\nadapter = \"{reviewer_adapter}\"\nprovider = \"anthropic\"\nmodel = \"opus\"\nbudget = {{ usd = 2, minutes = 20, steps = 20 }}\n"
        );
        FactoryConfig::parse_table(&src, "P", None).unwrap()
    }

    #[test]
    fn an_unknown_adapter_or_provider_is_refused() {
        let c = config("claude-code");
        assert_eq!(c.check_plugins(&["claude-code"], &["anthropic"]), Ok(()));
        let e = c.check_plugins(&["other"], &["anthropic"]).unwrap_err().to_string();
        assert!(e.contains("unknown adapter `claude-code`") && e.contains("known: other"), "{e}");
        let e = c.check_plugins(&["claude-code"], &[]).unwrap_err().to_string();
        assert!(e.contains("unknown provider `anthropic`"), "{e}");
    }

    #[test]
    fn a_worker_and_reviewer_must_share_a_harness() {
        let c = config("other");
        let e = c.check_plugins(&["claude-code", "other"], &["anthropic"]).unwrap_err().to_string();
        assert!(e.contains("same adapter"), "{e}");
    }

    #[test]
    fn a_registry_looks_plugins_up_by_name() {
        let r = Registry::new("adapter").with("a", 1).with("b", 2);
        assert_eq!(r.get("b"), Ok(&2));
        assert!(r.get("c").unwrap_err().to_string().contains("unknown adapter `c` (known: a, b)"));
    }
}
