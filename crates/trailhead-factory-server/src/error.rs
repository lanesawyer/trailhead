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
    #[error("that doesn't look like a Claude token from `claude setup-token` (it starts `{prefix}`)")]
    ClaudeTokenPrefix { prefix: &'static str },
    #[error(
        "the Claude token is only {len} characters, so it looks truncated (a pasted token may have wrapped); nothing was saved"
    )]
    ClaudeTokenShort { len: usize },
    /// The repos in one process can't run together; names both and why.
    #[error("{}", .0)]
    Conflict(Box<RepoConflict>),
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
    /// A task has no sandbox to show; `unreachable` lists the targets that couldn't be listed, where it may be.
    #[error(
        "{task} has no sandbox right now (it isn't running, or its sandbox was released or destroyed){}",
        unreachable_note(unreachable)
    )]
    NoSandbox { task: String, unreachable: Vec<String> },
    /// A failure from the store, the wire, a compute or the like, shown as it is.
    #[error("{0}")]
    Backend(Source),
}

/// Two repos that can't share a process, and why.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{} ({}) and {} ({}) {kind}", first.dir.display(), first.slug, second.dir.display(), second.slug)]
pub struct RepoConflict {
    pub first: RepoRef,
    pub second: RepoRef,
    pub kind: ConflictKind,
}

/// A repo named in a [`Error::Conflict`]: where it is and which GitHub repo it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRef {
    pub dir: PathBuf,
    pub slug: String,
}

/// Why two repos can't share a process. `Display` completes "<repo> and <repo> ...".
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConflictKind {
    #[error(
        "differ in [factory.compute], [factory.targets] or [factory.daemon]; repos in one process share the sandbox providers and daemon launch"
    )]
    SharedConfigDiffers,
    #[error("use different profile adapters; repos in one process share the daemon launch, which builds one harness")]
    DifferentAdapters,
    #[error("have the same repo, {slug}; they would collide, so run only one of them")]
    SameRepo { slug: String },
    #[error(
        "have the same repo name, {name} (pool sandboxes are named from it); they would collide, so run only one of them"
    )]
    SameName { name: String },
    #[error("have the same project and queue, {project} under {task}; they would collide, so run only one of them")]
    SameQueue { project: String, task: String },
    #[error("share project {project} and one has no queue, so it would take the other's tasks; give each a queue")]
    NoQueue { project: String },
}

fn unreachable_note(targets: &[String]) -> String {
    if targets.is_empty() {
        String::new()
    } else {
        format!("; target(s) {} couldn't be reached, so it may be there", targets.join(", "))
    }
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
    fn a_conflict_and_a_missing_sandbox_keep_their_fields() {
        let repo = |dir: &str, slug: &str| RepoRef { dir: dir.into(), slug: slug.into() };
        let err = Error::Conflict(Box::new(RepoConflict {
            first: repo("/a", "o/app"),
            second: repo("/b", "o/app"),
            kind: ConflictKind::SameRepo { slug: "o/app".into() },
        }));
        assert_eq!(
            err.to_string(),
            "/a (o/app) and /b (o/app) have the same repo, o/app; they would collide, so run only one of them"
        );
        let err = Error::NoSandbox { task: "TRAIL-1".into(), unreachable: vec!["sprites".into(), "docker".into()] };
        assert!(err.to_string().ends_with("; target(s) sprites, docker couldn't be reached, so it may be there"));
        let err = Error::NoSandbox { task: "TRAIL-1".into(), unreachable: vec![] };
        assert!(err.to_string().ends_with("destroyed)"));
    }

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
