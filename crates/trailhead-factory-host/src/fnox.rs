//! Looks secrets up through [fnox](https://fnox.jdx.dev) when a `fnox` binary is installed. It's shelled out to, never
//! linked, and never required: no binary, or no fnox config, just means it defines nothing.
//!
//! One `fnox export --format json` call at startup lists every secret its config defines with its value, so fnox's
//! error text is never read to decide whether a name is defined: a name is in the export or it isn't. The call fails,
//! and the error names fnox, only when there is a config to read.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The secrets fnox defines, by name.
#[derive(Debug, Default)]
pub struct Fnox(HashMap<String, String>);

impl Fnox {
    /// Nothing defined: fnox isn't used.
    pub fn none() -> Self {
        Fnox::default()
    }

    /// Runs `program export --format json` in `dir`. Nothing is defined when `program` isn't installed or when no
    /// config exists (`fnox.toml` or `fnox.local.toml` in `dir` or above, or `global`); `Err` (naming fnox, never a
    /// value) when it runs against a config and fails.
    pub fn load(program: &str, dir: &Path, global: Option<&Path>) -> Result<Self, String> {
        if !has_config(dir, global) {
            return Ok(Fnox::none());
        }
        let out = match Command::new(program)
            .args(["export", "--format", "json"])
            .current_dir(dir)
            .stdin(Stdio::null())
            .output()
        {
            Ok(out) => out,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Fnox::none()),
            Err(e) => return Err(format!("running fnox: {e}")),
        };
        if !out.status.success() {
            let why = String::from_utf8_lossy(&out.stderr);
            let why = why.trim();
            return Err(format!("fnox failed ({}){}{why}", out.status, if why.is_empty() { "" } else { ": " }));
        }
        Self::parse(&out.stdout)
    }

    /// The `{"secrets": {NAME: value}}` an export prints (a bare `{NAME: value}` is taken too). Only strings count.
    fn parse(json: &[u8]) -> Result<Self, String> {
        let doc: serde_json::Value =
            serde_json::from_slice(json).map_err(|e| format!("fnox export wasn't JSON: {e}"))?;
        let map = doc.get("secrets").unwrap_or(&doc).as_object().ok_or("fnox export wasn't a JSON object")?;
        Ok(Fnox(map.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.trim().to_string()))).collect()))
    }

    /// The value of `name`, if fnox defines it and it isn't blank.
    pub fn get(&self, name: &str) -> Option<String> {
        self.0.get(name).filter(|v| !v.is_empty()).cloned()
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

fn has_config(dir: &Path, global: Option<&Path>) -> bool {
    global.is_some_and(Path::is_file)
        || dir.ancestors().any(|d| d.join("fnox.toml").is_file() || d.join("fnox.local.toml").is_file())
}

/// Tests that spawn a process, or write a script to spawn, take this: a fork briefly shares its parent's open files,
/// which fails a concurrent test's file lock or makes a freshly written script "text file busy".
#[cfg(test)]
pub static SPAWNING: std::sync::Mutex<()> = std::sync::Mutex::new(());
