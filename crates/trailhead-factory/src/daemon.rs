//! The control plane's side of the daemon contract (`docs/factory/agents.md`).
//! The wire format over iroh comes in M3; until then the testkit hosts a
//! daemon in-process.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{Credential, Endpoint, EventSink, ModelId, PrRef, Repo, Result, Role, SandboxId, TaskSpec, Usage, Verdict};

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
    pub required_checks: Vec<String>,
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

#[async_trait]
pub trait Daemon: Send + Sync {
    /// Runs one agent step in the sandbox, streaming events, until it finishes or is cancelled.
    async fn run(&self, sandbox: &SandboxId, req: RunRequest, events: EventSink) -> Result<RunReport>;
    /// Stops the running agent. A cancelled `run` returns `Error::Cancelled`.
    async fn cancel(&self, sandbox: &SandboxId) -> Result<()>;
}
