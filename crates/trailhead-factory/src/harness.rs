use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::{CheckpointId, Commit, Endpoint, ModelId, Result, Role, TaskKey, Usage};

/// What the agent is asked to do: the task's spec as Trailhead holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSpec {
    pub key: TaskKey,
    pub title: String,
    pub description: String,
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
}

pub type EventSink = mpsc::UnboundedSender<RunEvent>;

/// An agent running inside the sandbox: Claude Code, Codex, an own loop.
#[async_trait]
pub trait Harness: Send + Sync {
    async fn run(&self, ctx: RunContext, events: EventSink) -> Result<RunOutcome>;
    async fn resume(&self, ctx: RunContext, feedback: Feedback, events: EventSink) -> Result<RunOutcome>;
}
