//! The daemon contract (`docs/factory/agents.md`): the control plane's
//! [`Daemon`] trait and the messages it exchanges with the daemon in a sandbox.
//! `trailhead-daemon` carries them over iroh; the testkit runs a daemon in-process.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{
    Credential, Endpoint, Error, EventSink, ModelId, PrRef, Repo, Result, Role, RunEvent, SandboxId, TaskSpec, Usage,
    Verdict,
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
    pub setup: Option<Check>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRequest {
    pub role: Role,
    pub task: TaskSpec,
    pub repo: Repo,
    pub base_branch: String,
    /// The branch a worker publishes to; a reviewer reads the PR instead.
    pub branch: String,
    pub pr: Option<PrRef>,
    pub model: ModelId,
    pub endpoint: Endpoint,
    pub max_steps: u32,
    /// The checks that must pass before a worker opens a PR.
    pub required_checks: Vec<Check>,
    /// Hosts that egress was just narrowed to exclude. Policy changes take effect a moment after the call that makes
    /// them, so the daemon waits until these stop resolving before the agent starts.
    pub blocked_hosts: Vec<String>,
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
/// `Run` followed by at most one `Cancel`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToDaemon {
    Prepare(PrepareRequest),
    Run(Box<RunRequest>),
    Cancel,
}

/// Daemon to control plane: `Event`s while a run streams, then exactly one of the rest.
#[derive(Debug, Serialize, Deserialize)]
pub enum FromDaemon {
    Event(RunEvent),
    Prepared,
    Report(RunReport),
    Failed(Error),
}

/// What the control plane hands a daemon at start, as a file the daemon reads
/// and deletes: its iroh key, and the one peer it accepts.
#[derive(Clone, Serialize, Deserialize)]
pub struct DaemonSecrets {
    /// The daemon's iroh secret key, hex. Derived per sandbox by the control
    /// plane, so it can dial the daemon by id without being told it.
    pub secret_key: String,
    /// The control plane's iroh endpoint id, hex.
    pub control_plane: String,
}

impl std::fmt::Debug for DaemonSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonSecrets").field("control_plane", &self.control_plane).finish_non_exhaustive()
    }
}

#[async_trait]
pub trait Daemon: Send + Sync {
    /// Starts the sandbox's daemon if needed and readies its checkout.
    async fn prepare(&self, sandbox: &SandboxId, req: PrepareRequest) -> Result<()>;
    /// Runs one agent step in the sandbox, streaming events, until it finishes or is cancelled.
    async fn run(&self, sandbox: &SandboxId, req: RunRequest, events: EventSink) -> Result<RunReport>;
    /// Stops the running agent. A cancelled `run` returns `Error::Cancelled`.
    async fn cancel(&self, sandbox: &SandboxId) -> Result<()>;
}
