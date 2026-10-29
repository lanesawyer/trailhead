//! The factory's domain types, the daemon contract and the five plugin seams
//! (`docs/factory/seams.md`). Vendor crates implement the traits; the control
//! plane and the in-sandbox daemon depend only on this crate.

pub mod codehost;
pub mod compute;
pub mod config;
pub mod daemon;
pub mod harness;
pub mod money;
pub mod provider;
pub mod tracker;

pub use codehost::{CodeHost, Commit, FileChange, MergeOutcome, NewPr, PrRef, PrState, Repo};
pub use compute::{
    CheckpointId, Compute, Egress, IROH_DOMAINS, Labels, Program, Sandbox, SandboxFile, SandboxId, SandboxSpec, label,
};
pub use config::{Budget, Checks, ComputeConfig, FactoryConfig, Gate, Profile, Role, UsageConfig};
pub use daemon::{
    Check, CheckResult, Daemon, DaemonSecrets, FromDaemon, PrepareRequest, RunReport, RunRequest, RunResult, ToDaemon,
};
pub use harness::{
    EventSink, Feedback, FollowUp, Harness, RunContext, RunEvent, RunOutcome, TaskSpec, Verdict, VerdictKind,
    conventional_title, is_conventional,
};
pub use money::{Usage, Usd};
pub use provider::{Endpoint, ModelAlias, ModelId, ModelProvider};
pub use tracker::{
    Answered, ChangesRequested, Credential, HumanContext, Merged, MergedBy, RunUpdate, Status, Task, TaskKey, TaskNote,
    Tracker, is_factory_agent,
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
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Other(String),
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
