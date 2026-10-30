//! Looks secrets up through [fnox](https://fnox.jdx.dev) when a `fnox` binary is installed. It's shelled out to, never
//! linked, and never required: no binary, or no entry for the name, is just "not found".

use std::io::ErrorKind;
use std::process::{Command, Stdio};

/// What `fnox get` says on stderr when its config doesn't define the name, or there is no config at all. Anything
/// else it fails with (a provider that refused a login, say) is an error, so a broken login isn't skipped.
const UNDEFINED: &[&str] =
    &["not found in config", "not defined", "no configuration", "no config file", "configuration file not found"];

/// The value of `name` from `fnox get`, run as `program`. `Ok(None)` when fnox isn't installed or doesn't define the
/// name; `Err` (naming fnox, never the value) when it does and fails.
pub fn get(program: &str, name: &str) -> Result<Option<String>, String> {
    let out = match Command::new(program).args(["get", name]).stdin(Stdio::null()).output() {
        Ok(out) => out,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("running fnox for {name}: {e}")),
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if UNDEFINED.iter().any(|p| stderr.to_lowercase().contains(p)) {
            return Ok(None);
        }
        let why = stderr.trim();
        return Err(format!("fnox failed for {name} ({}){}{why}", out.status, if why.is_empty() { "" } else { ": " }));
    }
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(Some(value).filter(|v| !v.is_empty()))
}

/// Tests that spawn a process, or write a script to spawn, take this: a fork briefly shares its parent's open files,
/// which fails a concurrent test's file lock or makes a freshly written script "text file busy".
#[cfg(test)]
pub static SPAWNING: std::sync::Mutex<()> = std::sync::Mutex::new(());
