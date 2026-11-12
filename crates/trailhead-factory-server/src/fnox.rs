//! Looks secrets up through [fnox](https://fnox.jdx.dev) when a `fnox` binary is installed. It's shelled out to, never
//! linked, and never required: no binary, or no fnox config, just means it defines nothing.
//!
//! Only the secrets the factory needs are fetched, never the whole config, so one broken provider for a secret the
//! factory doesn't use can't stop it. `fnox list --complete` names what the config defines, so fnox's error text is
//! never read to decide whether a name is defined; a defined name is then read with `fnox get NAME`. The list is read
//! at most once per `Fnox`, and only when a name is asked for. A defined name that fails is an error naming fnox; one
//! the config doesn't define is just not found.

use crate::error::{Error, Result};
use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;

/// What `fnox list` says the config defines, read on first use.
#[derive(Debug)]
enum Names {
    Unread,
    /// No `fnox` binary: nothing is defined.
    NoBinary,
    Defined(HashSet<String>),
}

/// A fnox config to look secrets up in.
#[derive(Debug)]
pub struct Fnox {
    /// The program and the directory to run it in; `None` when fnox isn't used.
    run: Option<(String, PathBuf)>,
    /// Whether a config fnox would read was found; when not, fnox is still run, but a failure means nothing is defined.
    configured: bool,
    names: Mutex<Names>,
}

impl Fnox {
    /// Nothing defined: fnox isn't used.
    pub fn none() -> Self {
        Fnox { run: None, configured: false, names: Mutex::new(Names::NoBinary) }
    }

    /// Looks names up with `program` run in `dir`, the directory of the repo whose config is loaded. Fnox is run
    /// whenever it's installed, as its own discovery is the authority; `dir`, `global` and the environment are only
    /// checked for a config it would read, to tell a failing fnox from one with nothing to read. Runs nothing until
    /// [`Fnox::get`].
    pub fn new(program: &str, dir: &Path, global: Option<&Path>) -> Self {
        let configured = has_config(dir, global);
        Fnox { run: Some((program.to_string(), dir.to_path_buf())), configured, names: Mutex::new(Names::Unread) }
    }

    /// `Ok(None)` when `program` isn't installed; `Err` (naming fnox) when it runs and fails.
    fn command(program: &str, dir: &Path, args: &[&str]) -> Result<Option<Output>> {
        match Command::new(program).args(args).current_dir(dir).stdin(Stdio::null()).output() {
            Ok(out) if out.status.success() => Ok(Some(out)),
            Ok(out) => {
                let why = String::from_utf8_lossy(&out.stderr);
                let why = why.trim();
                Err(Error::FnoxFailed { status: out.status, why: why.to_string() })
            }
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::FnoxRun(e)),
        }
    }

    /// The value of `name`, if fnox defines it and it isn't blank. `Err` (naming fnox, never a value) when fnox fails
    /// to list the config or to read a name it defines.
    pub fn get(&self, name: &str) -> Result<Option<String>> {
        let Some((program, dir)) = &self.run else {
            return Ok(None);
        };
        let mut names = self.names.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*names, Names::Unread) {
            let listed = match Self::command(program, dir, &["list", "--complete"]) {
                Err(e) if !self.configured => {
                    tracing::debug!("fnox is on PATH but found no config from {}: {e}", dir.display());
                    Ok(None)
                }
                listed => listed,
            }?;
            if !self.configured && listed.is_some() {
                tracing::debug!("fnox is on PATH but no config was found from {}", dir.display());
            }
            *names = match listed {
                None => Names::NoBinary,
                Some(out) => Names::Defined(
                    String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .filter_map(|l| l.split_whitespace().next())
                        .map(str::to_string)
                        .collect(),
                ),
            };
        }
        let Names::Defined(defined) = &*names else {
            return Ok(None);
        };
        if !defined.contains(name) {
            return Ok(None);
        }
        let Some(out) = Self::command(program, dir, &["get", name])? else {
            return Ok(None);
        };
        let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if value.is_empty() {
            // `if_missing = "warn"` lets fnox exit 0 with nothing for a secret it defines but couldn't resolve.
            tracing::warn!("fnox defines {name} but returned nothing for it; trying the next source");
            return Ok(None);
        }
        Ok(Some(value))
    }
}

/// Where fnox keeps its global config: `$XDG_CONFIG_HOME/fnox/config.toml`, else `~/.config/fnox/config.toml`.
pub fn global_config() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("fnox").join("config.toml"))
}

/// Whether a config fnox would read exists: `FNOX_CONFIG`, the global config, or a config file in `dir` or above.
fn has_config(dir: &Path, global: Option<&Path>) -> bool {
    let mut names = vec!["fnox.toml".to_string(), ".fnox.toml".into(), "fnox.local.toml".into()];
    if let Some(profile) = std::env::var("FNOX_PROFILE").ok().filter(|p| !p.is_empty()) {
        names.push(format!("fnox.{profile}.toml"));
    }
    std::env::var_os("FNOX_CONFIG").is_some_and(|c| !c.is_empty())
        || global.is_some_and(Path::is_file)
        || dir.ancestors().any(|d| names.iter().any(|n| d.join(n).is_file()))
}

/// Tests that spawn a process, or write a script to spawn, take this: a fork briefly shares its parent's open files,
/// which fails a concurrent test's file lock or makes a freshly written script "text file busy".
#[cfg(test)]
pub static SPAWNING: std::sync::Mutex<()> = std::sync::Mutex::new(());
