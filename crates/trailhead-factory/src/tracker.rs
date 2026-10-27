use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{PrRef, Result, Usd, Verdict};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TaskKey(pub String);

/// Mirrors Trailhead's task statuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Open,
    InProgress,
    NeedsInput,
    InReview,
    Done,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub key: TaskKey,
    pub project: String,
    pub title: String,
    pub description: String,
    pub status: Status,
    /// The agent holding the claim, if any.
    pub holder: Option<String>,
    /// The `factory` object in the task's metadata ([`RunUpdate`] fields).
    pub run: RunUpdate,
}

/// The factory's fields in task metadata. Writes merge: `None` leaves a field as it is.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<PrRef>,
    /// Review rounds started on the current attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_rounds: Option<u32>,
    /// The attempt whose PR the agent review last finished on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed: Option<u32>,
    /// Total spend across attempts and reviews.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spend: Option<Usd>,
}

impl RunUpdate {
    pub fn merge(&mut self, update: RunUpdate) {
        let RunUpdate { attempt, sandbox, profile, pr, review_rounds, reviewed, spend } = update;
        self.attempt = attempt.or(self.attempt.take());
        self.sandbox = sandbox.or(self.sandbox.take());
        self.profile = profile.or(self.profile.take());
        self.pr = pr.or(self.pr.take());
        self.review_rounds = review_rounds.or(self.review_rounds.take());
        self.reviewed = reviewed.or(self.reviewed.take());
        self.spend = spend.or(self.spend.take());
    }

    pub fn is_empty(&self) -> bool {
        *self == RunUpdate::default()
    }
}

/// A Trailhead sub-agent and its bearer token, handed to a sandbox at start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credential {
    pub agent: String,
    pub token: String,
}

/// A task tracker, bound to one identity. The control plane holds one for its
/// own agent and reaches its sandboxes' sub-agents through [`Tracker::sub_agent`];
/// a daemon holds one for its sub-agent's token.
///
/// Writes by the claim holder are the norm: Trailhead refuses edits to a task
/// another actor holds, which is why the control plane acts as the sub-agent
/// to park or requeue a sandbox's task.
#[async_trait]
pub trait Tracker: Send + Sync {
    /// Who this handle acts as.
    fn agent(&self) -> &str;
    /// A handle acting as one of this agent's sub-agents, by the part after the parent's name.
    fn sub_agent(&self, name: &str) -> Arc<dyn Tracker>;

    /// Ready tasks in `project`; with `queue`, only the queue task's subtasks, which is how work is handed to the
    /// factory.
    async fn ready(&self, project: &str, queue: Option<&TaskKey>, limit: usize) -> Result<Vec<Task>>;
    async fn show(&self, task: &TaskKey) -> Result<Task>;
    /// Open, in-progress, parked or in-review tasks that carry factory metadata.
    async fn in_flight(&self, project: &str) -> Result<Vec<Task>>;

    async fn claim(&self, task: &TaskKey) -> Result<()>;
    async fn renew(&self, task: &TaskKey) -> Result<()>;
    async fn release(&self, task: &TaskKey) -> Result<()>;
    async fn report(&self, task: &TaskKey, update: RunUpdate) -> Result<()>;
    async fn request_review(&self, task: &TaskKey) -> Result<()>;
    async fn record_verdict(&self, task: &TaskKey, verdict: &Verdict) -> Result<()>;
    async fn ask(&self, task: &TaskKey, question: &str) -> Result<()>;
    async fn finish(&self, task: &TaskKey, handoff: &str) -> Result<()>;

    /// Creates (or reuses) a project-scoped sub-agent and a fresh token for it that expires after `ttl`,
    /// in case revoking it is missed.
    async fn create_sub_agent(&self, name: &str, project: &str, ttl: Duration) -> Result<Credential>;
    /// Revokes a sub-agent and its tokens. Revoking one that's already gone is fine.
    async fn revoke_sub_agent(&self, name: &str) -> Result<()>;
}
