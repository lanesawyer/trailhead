//! The factory's domain types, the daemon contract and the five plugin seams
//! (`docs/factory/seams.md`). Vendor crates implement the traits; the control
//! plane and the in-sandbox daemon depend only on this crate.

pub mod codehost;
pub mod compute;
pub mod config;
pub mod daemon;
pub mod gateway;
pub mod grade;
pub mod harness;
pub mod host;
pub mod money;
pub mod provider;
pub mod target;
pub mod tracker;

pub use codehost::{
    BranchUpdate, Ci, CiState, CodeHost, Commit, FileChange, MergeOutcome, NewPr, PrRef, PrState, Repo,
};
pub use compute::{
    CheckpointId, Compute, Egress, IROH_DOMAINS, Labels, Program, Sandbox, SandboxFile, SandboxId, SandboxSpec, label,
};
pub use config::{
    Budget, Checks, ComputeConfig, FactoryConfig, GATEWAY_ANTHROPIC_ID, GATEWAY_GITHUB_ID, GATEWAY_HOST, GATEWAY_PORT,
    Gate, GateKind, GradesConfig, MergeConfig, MergeMode, Profile, Role, UsageConfig,
};
pub use daemon::{
    Check, CheckResult, Daemon, DaemonSecrets, DaemonVersion, FromDaemon, PrepareRequest, RunReport, RunRequest,
    RunResult, ToDaemon,
};
pub use gateway::{
    AuthScheme, GatewayConnection, GatewayConnectionInfo, GatewayRequest, GatewayResponse, GatewaySandbox,
    GatewayState, Secret,
};
pub use grade::{ChangedFile, FileStatus, Grader, PrGrade, glob_match};
pub use harness::{
    EventSink, Feedback, FollowUp, Harness, RaiseGrade, RunContext, RunEvent, RunOutcome, TaskSpec, Verdict,
    VerdictKind, conventional_title, is_conventional,
};
pub use host::HostConfig;
pub use money::{Usage, Usd};
pub use provider::{Endpoint, ModelAlias, ModelId, ModelProvider};
pub use target::Target;
pub use tracker::{
    Answered, ChangesRequested, Credential, HumanContext, METADATA_KEY, MergeDecision, Merged, MergedBy, RunUpdate,
    Status, Task, TaskKey, TaskNote, Tracker, is_factory_agent,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, serde::Serialize, serde::Deserialize)]
pub enum Error {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("invalid config: {0}")]
    Config(String),
    /// The remote refused the request and repeating it won't change that (a 400 or 422, say), unlike
    /// [`Error::Other`], which may be a blip worth retrying.
    #[error("rejected: {0}")]
    Rejected(String),
    /// A resource the run needs is short right now (a full disk, say) and will be again only when something is
    /// freed. The work is worth retrying later, and waiting doesn't count as a failure of the task or the sandbox.
    #[error("unavailable: {0}")]
    Unavailable(String),
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}

impl Error {
    /// [`Error::Rejected`] for a client error the remote won't answer differently next time (any 4xx but 408 and
    /// 429), otherwise [`Error::Other`].
    pub fn from_status(status: u16, msg: String) -> Error {
        if (400..500).contains(&status) && !matches!(status, 408 | 429) {
            Error::Rejected(msg)
        } else {
            Error::Other(msg)
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// The deterministic name for a task's sandbox and sub-agent on one attempt
/// (`TRAIL-42`, attempt 1 → `trail-42-a1`), so the reconciler can match them
/// after a crash between creating one and recording it (`docs/factory/state.md`).
pub fn run_name(task: &TaskKey, attempt: u32) -> String {
    format!("{}-a{attempt}", task.0.to_ascii_lowercase())
}

/// The reviewer's sandbox and sub-agent for one review round of an attempt.
pub fn review_name(task: &TaskKey, attempt: u32, round: u32) -> String {
    format!("{}-r{round}", run_name(task, attempt))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_names_are_deterministic() {
        let key = TaskKey("TRAIL-42".into());
        assert_eq!(run_name(&key, 1), "trail-42-a1");
        assert_eq!(review_name(&key, 2, 1), "trail-42-a2-r1");
    }
}
