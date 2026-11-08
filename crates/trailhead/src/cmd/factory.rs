//! `trailhead factory <args>`: runs the `trailhead-factory` binary with those args, the way `cargo foo` runs
//! `cargo-foo`. The factory stays a separate binary so this crate doesn't depend on it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::{client, config};

const BIN: &str = "trailhead-factory";
const INSTALL: &str = "cargo install --locked --features docker --path crates/trailhead-factory-host";

/// `beside` (this executable's directory) first, then each of `path`.
fn find(name: &str, beside: Option<&Path>, path: &[PathBuf]) -> Option<PathBuf> {
    let name = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    beside.into_iter().chain(path.iter().map(PathBuf::as_path)).map(|d| d.join(&name)).find(|p| p.is_file())
}

/// `Some(true)` for `trailhead factory enable`, `Some(false)` for `disable`: the two subcommands the CLI handles
/// itself, since they are how to recover when a broken factory configuration keeps the server from starting.
pub fn toggle(args: &[OsString]) -> Option<bool> {
    match args {
        [cmd] if cmd == "enable" => Some(true),
        [cmd] if cmd == "disable" => Some(false),
        _ => None,
    }
}

/// Sets or clears `disabled` in `[factory]` of the user config. Needs no server, and touches no other factory key.
pub async fn set_enabled(enable: bool) -> Result<()> {
    let paths = trailhead_server::Paths::from_env().context("no home directory")?;
    let mut cfg = config::load(&paths)?;
    let table = cfg.factory.get_or_insert_with(Default::default);
    let was_enabled = !table.get("disabled").and_then(toml::Value::as_bool).unwrap_or(false);
    if enable {
        table.remove("disabled");
    } else {
        table.insert("disabled".into(), toml::Value::Boolean(true));
    }
    config::save(&paths, &cfg)?;
    let word = if enable { "enabled" } else { "disabled" };
    if was_enabled == enable {
        println!("the factory was already {word}");
        return Ok(());
    }
    println!("the factory is {word} ({})", config::path(&paths).display());
    let url = cfg.url.clone().unwrap_or_else(|| config::DEFAULT_URL.into());
    let (http, base) = client::http_and_base(&url)?;
    if client::is_local(&base) && client::health(&http, &base, None).await.is_some() {
        println!("the running server picks this up when it restarts: `trailhead server restart`");
    }
    Ok(())
}

pub fn run(args: &[OsString]) -> Result<()> {
    let beside = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf));
    let path: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    let Some(bin) = find(BIN, beside.as_deref(), &path) else {
        bail!("{BIN} isn't installed next to trailhead or on PATH; install it with `{INSTALL}`");
    };
    exec(&bin, args)
}

#[cfg(unix)]
fn exec(bin: &Path, args: &[OsString]) -> Result<()> {
    use std::os::unix::process::CommandExt;
    // Replaces this process, so Ctrl-C and exit codes reach the factory directly.
    let err = std::process::Command::new(bin).args(args).exec();
    bail!("running {}: {err}", bin.display())
}

#[cfg(not(unix))]
fn exec(bin: &Path, args: &[OsString]) -> Result<()> {
    let status = std::process::Command::new(bin).args(args).status()?;
    std::process::exit(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enable_and_disable_are_handled_here_and_nothing_else() {
        let args = |a: &[&str]| a.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(toggle(&args(&["disable"])), Some(false));
        assert_eq!(toggle(&args(&["enable"])), Some(true));
        assert_eq!(toggle(&args(&["run"])), None);
        assert_eq!(toggle(&args(&["disable", "--help"])), None);
        assert_eq!(toggle(&args(&[])), None);
    }

    #[test]
    fn looks_beside_the_executable_before_the_path() {
        let (beside, on_path) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let path = vec![on_path.path().to_path_buf()];
        assert_eq!(find(BIN, Some(beside.path()), &path), None);

        let name = format!("{BIN}{}", std::env::consts::EXE_SUFFIX);
        std::fs::write(on_path.path().join(&name), "").unwrap();
        assert_eq!(find(BIN, Some(beside.path()), &path), Some(on_path.path().join(&name)));

        std::fs::write(beside.path().join(&name), "").unwrap();
        assert_eq!(find(BIN, Some(beside.path()), &path), Some(beside.path().join(&name)));
    }
}
