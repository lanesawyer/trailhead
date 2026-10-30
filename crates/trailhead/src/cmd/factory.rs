//! `trailhead factory <args>`: runs the `trailhead-factory` binary with those args, the way `cargo foo` runs
//! `cargo-foo`. The factory stays a separate binary so this crate doesn't depend on it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

const BIN: &str = "trailhead-factory";
const INSTALL: &str = "cargo install --locked --path crates/trailhead-factory-host";

/// `beside` (this executable's directory) first, then each of `path`.
fn find(name: &str, beside: Option<&Path>, path: &[PathBuf]) -> Option<PathBuf> {
    let name = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    beside.into_iter().chain(path.iter().map(PathBuf::as_path)).map(|d| d.join(&name)).find(|p| p.is_file())
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
