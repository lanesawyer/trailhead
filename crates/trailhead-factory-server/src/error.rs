//! The error every fallible step of the control plane's setup and commands returns. The CLI prints its `Display`.

use std::path::{Path, PathBuf};

/// What a lower layer (the store, the wire, a compute, the config parser) failed with.
pub type Source = Box<dyn std::error::Error + Send + Sync>;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("can't find Trailhead's data and config directories")]
    NoDirs,
    #[error("no {file} at or above {}; run from inside the repo or pass --config <path>", dir.display())]
    NoConfig { file: &'static str, dir: PathBuf },
    /// A file that couldn't be read or parsed.
    #[error("{}: {cause}", path.display())]
    At { path: PathBuf, cause: Source },
    /// A step that failed, named by `what`.
    #[error("{what}: {cause}")]
    Context { what: String, cause: Source },
    #[error("no `{agent}` agent yet; create it with `trailhead --as-me agent add {agent}`")]
    NoAgent { agent: &'static str },
    #[error("the worker profile is missing")]
    NoWorkerProfile,
    #[error("bad saved factory keys: {0}")]
    BadKeys(serde_json::Error),
    #[error("bad saved master key")]
    BadMasterKey,
    #[error("Sprites compute needs SPRITES_TOKEN or `trailhead factory login sprites`")]
    NoSpritesToken,
    #[error("Docker compute needs a build with the `docker` feature")]
    DockerNotBuilt,
    #[error("{}: Docker compute needs a build with the `docker` feature (`cargo install --features docker`)", path.display())]
    DockerNotBuiltFor { path: PathBuf },
    /// fnox ran and failed; `why` is its stderr, never a value.
    #[error("fnox failed ({status}){}{why}", if why.is_empty() { "" } else { ": " })]
    FnoxFailed { status: std::process::ExitStatus, why: String },
    #[error("running fnox: {0}")]
    FnoxRun(std::io::Error),
    #[error("reading stdin: {0}")]
    Stdin(std::io::Error),
    #[error("no token on stdin")]
    NoTokenOnStdin,
    #[error(
        "that doesn't look like a Claude token from `claude setup-token` (it starts `{prefix}`); if you are sure it is valid, rerun with `--force`"
    )]
    ClaudeTokenPrefix { prefix: &'static str },
    #[error(
        "the Claude token is only {len} characters, so it looks truncated (a pasted token may have wrapped); nothing was saved; if it is complete, rerun with `--force`"
    )]
    ClaudeTokenShort { len: usize },
    /// The repos in one process can't run together; names both.
    #[error("{0}")]
    Conflict(String),
    #[error("{}", plural(*.0, "check", "checks", "failed"))]
    ChecksFailed(usize),
    #[error("{0} repo loop(s) stopped on an error")]
    LoopsFailed(usize),
    #[error("no user token yet; start the server with `trailhead server start`")]
    NoUserToken,
    /// The server didn't answer a request for tasks.
    #[error("{0}; is the server running? (`trailhead server start`)")]
    ServerDown(Source),
    #[error("the control plane doesn't answer: {0}")]
    Unreachable(reqwest::Error),
    #[error("the control plane refused the stream: {0}")]
    StreamRefused(reqwest::StatusCode),
    #[error("the control plane stopped")]
    StreamEnded,
    /// A task has no sandbox to show.
    #[error("{0}")]
    NoSandbox(String),
    /// A failure from the store, the wire, a compute or the like, shown as it is.
    #[error("{0}")]
    Backend(Source),
}

fn plural(n: usize, one: &str, many: &str, verb: &str) -> String {
    if n == 1 { format!("1 {one} {verb}") } else { format!("{n} {many} {verb}") }
}

impl Error {
    pub fn backend(e: impl Into<Source>) -> Self {
        Error::Backend(e.into())
    }

    pub fn context(what: impl Into<String>, e: impl Into<Source>) -> Self {
        Error::Context { what: what.into(), cause: e.into() }
    }

    pub fn at(path: &Path, e: impl Into<Source>) -> Self {
        Error::At { path: path.to_path_buf(), cause: e.into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrapped_cause_shows_once_in_the_chain() {
        let io = std::io::Error::other("disk on fire");
        let err = Error::at(Path::new("/repo/.trailhead.toml"), io);
        let shown = format!("{:#}", anyhow::Error::from(err));
        assert_eq!(shown, "/repo/.trailhead.toml: disk on fire");
        let err = Error::context("GitHub refused the token", std::io::Error::other("401"));
        assert_eq!(format!("{:#}", anyhow::Error::from(err)), "GitHub refused the token: 401");
        let err = Error::FnoxRun(std::io::Error::other("no fnox"));
        assert_eq!(format!("{:#}", anyhow::Error::from(err)), "running fnox: no fnox");
    }
}
