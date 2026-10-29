//! The daemon contract (`docs/factory/agents.md`): the control plane's
//! [`Daemon`] trait and the messages it exchanges with the daemon in a sandbox.
//! `trailhead-daemon` carries them over iroh; the testkit runs a daemon in-process.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{
    Credential, Endpoint, Error, EventSink, Feedback, ModelId, PrGrade, PrRef, Repo, Result, Role, RunEvent, SandboxId,
    TaskSpec, Usage, Verdict,
};

/// A named check and the shell command that runs it in the repo (`docs/factory/sandbox.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub command: String,
}

/// Gets a sandbox's checkout ready before any agent runs: clone or refresh the
/// repo at the base branch, then run the repo's `setup` check. Runs while the
/// sandbox has install-phase egress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrepareRequest {
    pub repo: Repo,
    pub base_branch: String,
    /// A branch to check out at its remote head instead of staying on the base: a rework's PR branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Merge the base branch into `branch` after setup, leaving the merge uncommitted for the agent to finish: a
    /// rework of a PR that conflicts with it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub merge_base: bool,
    pub setup: Option<Check>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRequest {
    /// Names the run, so a control plane that loses its connection can reattach to it (`ToDaemon::Attach`).
    pub run: String,
    pub role: Role,
    pub task: TaskSpec,
    pub repo: Repo,
    pub base_branch: String,
    /// The branch a worker publishes to; a reviewer reads the PR instead.
    pub branch: String,
    pub pr: Option<PrRef>,
    /// For a reviewer: the PR's grade, which its comment states.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade: Option<PrGrade>,
    /// For a rework: the review that asked for changes, which the worker is told.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<Feedback>,
    pub model: ModelId,
    pub endpoint: Endpoint,
    pub max_steps: u32,
    /// The checks that must pass before a worker opens a PR.
    pub required_checks: Vec<Check>,
    /// For a reviewer: the worker's check results, passed on so it needn't build or test.
    #[serde(default)]
    pub worker_checks: Vec<CheckResult>,
    /// Hosts that egress was just narrowed to exclude. Policy changes take effect a moment after the call that makes
    /// them, so the daemon waits until these stop resolving before the agent starts.
    pub blocked_hosts: Vec<String>,
    /// Hosts the agent may reach, which the daemon keeps resolving: a Sprite only lets an IP through while its own
    /// DNS has recently resolved it, and a long-lived connection whose allowance lapses gets reset.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    /// The sandbox's own Trailhead sub-agent.
    pub credential: Credential,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    pub name: String,
    pub passed: bool,
    pub output: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunResult {
    /// A worker published its branch, opened a PR and moved the task to review.
    PrOpened(PrRef),
    /// A reviewer posted its verdict on the PR and recorded it in Trailhead.
    Reviewed(Verdict),
    /// The agent parked the task with a question.
    NeedsInput(String),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunReport {
    pub result: RunResult,
    pub checks: Vec<CheckResult>,
    pub usage: Usage,
}

/// Control plane to daemon. One iroh stream carries one request: `Prepare`, or
/// `Run` or `Attach` followed by `Cancel`s.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToDaemon {
    Prepare(PrepareRequest),
    Run(Box<RunRequest>),
    /// Resumes following a run after a dropped connection: the daemon sends the events after the first `seen`,
    /// then the rest of the run as usual. A run the daemon doesn't know ends with `Failed(NotFound)`.
    Attach {
        run: String,
        seen: u64,
    },
    Cancel,
    /// Asks which build of the daemon is answering. A daemon that predates this fails the request.
    Version,
}

/// Which build of the daemon is running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonVersion {
    /// The daemon crate's version.
    pub version: String,
    /// SHA-256 of the daemon's executable, hex. The control plane compares it with the binary it uploads, so
    /// it catches a stale process as well as a stale protocol.
    pub build: String,
}

/// Daemon to control plane: `Event`s while a run streams, then exactly one of the rest.
#[derive(Debug, Serialize, Deserialize)]
pub enum FromDaemon {
    Event(RunEvent),
    Prepared,
    Version(DaemonVersion),
    Report(RunReport),
    Failed(Error),
}

/// What the control plane hands a daemon at start, as a `0600` file: its iroh
/// key, and the one peer it accepts. The file stays, because a supervised
/// restart (a Sprite's cold wake) reads it again.
#[derive(Clone, Serialize, Deserialize)]
pub struct DaemonSecrets {
    /// The daemon's iroh secret key, hex. Derived per sandbox by the control
    /// plane, so it can dial the daemon by id without being told it.
    pub secret_key: String,
    /// The control plane's iroh endpoint id, hex.
    pub control_plane: String,
    /// Subscription mode (`docs/factory/sandbox.md`): a Claude Code OAuth token from `claude setup-token`, used in
    /// place of a model connector. The one exception to sandboxes holding no model credential.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_oauth_token: Option<String>,
}

impl std::fmt::Debug for DaemonSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonSecrets")
            .field("control_plane", &self.control_plane)
            .field("subscription", &self.claude_oauth_token.is_some())
            .finish_non_exhaustive()
    }
}

#[async_trait]
pub trait Daemon: Send + Sync {
    /// Starts the sandbox's daemon if needed and readies its checkout.
    async fn prepare(&self, sandbox: &SandboxId, req: PrepareRequest) -> Result<()>;
    /// Drops any connection to the sandbox's daemon, after something (a restore) restarted it.
    async fn forget(&self, _sandbox: &SandboxId) {}
    /// Runs one agent step in the sandbox, streaming events, until it finishes or is cancelled.
    async fn run(&self, sandbox: &SandboxId, req: RunRequest, events: EventSink) -> Result<RunReport>;
    /// Stops the running agent. A cancelled `run` returns `Error::Cancelled`.
    async fn cancel(&self, sandbox: &SandboxId) -> Result<()>;
    /// Cancels every run in flight, because the control plane is stopping on purpose. Without this, a daemon would
    /// keep an unobserved run going while it waits for a reattach.
    async fn shutdown(&self) {}
}
