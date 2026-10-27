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

pub use codehost::{CodeHost, Commit, FileChange, MergeOutcome, NewPr, PrRef, Repo};
pub use compute::{
    CheckpointId, Compute, Egress, IROH_DOMAINS, Labels, Program, Sandbox, SandboxFile, SandboxId, SandboxSpec, label,
};
pub use config::{Budget, Checks, FactoryConfig, Gate, Profile, Role};
pub use daemon::{CheckResult, Daemon, RunReport, RunRequest, RunResult};
pub use harness::{EventSink, Feedback, Harness, RunContext, RunEvent, RunOutcome, TaskSpec, Verdict, VerdictKind};
pub use money::{Usage, Usd};
pub use provider::{Endpoint, ModelAlias, ModelId, ModelProvider};
pub use tracker::{Credential, RunUpdate, Status, Task, TaskKey, Tracker};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("invalid config: {0}")]
    Config(String),
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
