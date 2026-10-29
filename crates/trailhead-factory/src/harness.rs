use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::{CheckResult, CheckpointId, Commit, Endpoint, HumanContext, ModelId, Result, Role, TaskKey, Usage};

/// What the agent is asked to do: the task's spec as Trailhead holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSpec {
    pub key: TaskKey,
    pub title: String,
    pub description: String,
    /// Why the previous attempt at the task failed, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_failure: Option<String>,
    /// Lane's answers to the agent's earlier questions, and the task's decisions and notes.
    #[serde(default, skip_serializing_if = "HumanContext::is_empty")]
    pub context: HumanContext,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunContext {
    pub role: Role,
    pub task: TaskSpec,
    pub workdir: PathBuf,
    pub model: ModelId,
    pub endpoint: Endpoint,
    pub max_steps: u32,
    /// For reviewers: the change under review. Never the author's transcript.
    pub diff: Option<String>,
    /// For reviewers: the checks the worker already ran in its own sandbox, which the reviewer shouldn't rerun.
    #[serde(default)]
    pub checks: Vec<CheckResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feedback {
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerdictKind {
    Approve,
    RequestChanges,
}

/// Something a reviewer noticed that shouldn't hold up the change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowUp {
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// Trailhead's scale, 1 (most urgent) to 4; the project's default when absent.
    #[serde(default)]
    pub priority: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub kind: VerdictKind,
    pub body: String,
    pub reviewer: String,
    pub model: Option<ModelId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunOutcome {
    /// A worker finished: its local commits, to be published by the daemon.
    Changes {
        commits: Vec<Commit>,
        summary: String,
    },
    /// A reviewer finished. The daemon fills in `reviewer` and `model`.
    Verdict {
        kind: VerdictKind,
        body: String,
        /// Non-blocking suggestions, to be filed as tasks when the review approves.
        #[serde(default)]
        follow_ups: Vec<FollowUp>,
    },
    NeedsInput {
        question: String,
    },
    Failed {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunEvent {
    StepStarted {
        step: u32,
    },
    ToolCall {
        name: String,
        /// A short, one-line summary of the call (the command, file or pattern), for progress logs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    CheckpointTaken {
        checkpoint: CheckpointId,
    },
    /// Usage of one model call, as reported in its response.
    Usage {
        model: ModelId,
        usage: Usage,
    },
    NeedsInput {
        question: String,
    },
    /// How full one of the subscription's usage windows is (Claude's `five_hour` or `seven_day`), as the harness
    /// last heard. The control plane stops starting runs past a threshold until the window resets.
    Quota {
        window: String,
        used_percent: u32,
        /// Unix seconds.
        resets_at: i64,
    },
}

pub type EventSink = mpsc::UnboundedSender<RunEvent>;

/// An agent running inside the sandbox: Claude Code, Codex, an own loop.
#[async_trait]
pub trait Harness: Send + Sync {
    async fn run(&self, ctx: RunContext, events: EventSink) -> Result<RunOutcome>;
    async fn resume(&self, ctx: RunContext, feedback: Feedback, events: EventSink) -> Result<RunOutcome>;
}
