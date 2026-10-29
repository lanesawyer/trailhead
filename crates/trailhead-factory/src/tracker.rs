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
    Approved,
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
    /// The run's name, which its sub-agent shares (`trail-42-a1`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<String>,
    /// The pooled sandbox the run is on, when it isn't one named after the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
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
    /// Why the last attempt failed, which the next attempt is told.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    /// Runs that failed before their agent started (TRAIL-159). They don't count as attempts, but each revoked a
    /// sub-agent, so a retry's name carries this count to stay unique.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<u32>,
    /// Runs in a row that failed on the infrastructure, before or after the agent started (TRAIL-172). They don't
    /// count as attempts; `max_infra_failures` caps them. Reset when an agent ends a run, or when the cap parks the task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub infra_failures: Option<u32>,
    /// The review that last asked for changes, which the rework on the PR branch is told.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changes_requested: Option<ChangesRequested>,
    /// The attempt the last review asked to change. `max_attempts` counts the runs after it, so reworks don't use up
    /// the attempts of the task as a whole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_base: Option<u32>,
    /// How the task's PR was merged, once it has been.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged: Option<Merged>,
}

/// A merged PR: who merged it, which PR, and the commit it became.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Merged {
    pub by: MergedBy,
    pub pr: PrRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergedBy {
    /// Someone merged it outside the factory, such as Lane on GitHub.
    Human,
    /// The control plane merged it, once the tier's gates passed.
    Merger,
}

/// A review that asked for changes: who, what they said, and the attempt whose PR it reviewed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangesRequested {
    pub reviewer: String,
    pub body: String,
    #[serde(default)]
    pub attempt: u32,
}

impl RunUpdate {
    pub fn merge(&mut self, update: RunUpdate) {
        let RunUpdate {
            attempt,
            sandbox,
            host,
            profile,
            pr,
            review_rounds,
            reviewed,
            spend,
            failure,
            retries,
            infra_failures,
            changes_requested,
            attempt_base,
            merged,
        } = update;
        self.attempt = attempt.or(self.attempt.take());
        self.sandbox = sandbox.or(self.sandbox.take());
        self.host = host.or(self.host.take());
        self.profile = profile.or(self.profile.take());
        self.pr = pr.or(self.pr.take());
        self.review_rounds = review_rounds.or(self.review_rounds.take());
        self.reviewed = reviewed.or(self.reviewed.take());
        self.spend = spend.or(self.spend.take());
        self.failure = failure.or(self.failure.take());
        self.retries = retries.or(self.retries.take());
        self.infra_failures = infra_failures.or(self.infra_failures.take());
        self.changes_requested = changes_requested.or(self.changes_requested.take());
        self.attempt_base = attempt_base.or(self.attempt_base.take());
        self.merged = merged.or(self.merged.take());
    }

    /// The review the next run has to address: the last one that asked for changes, unless a later review has
    /// since finished on another attempt.
    pub fn pending_changes(&self) -> Option<&ChangesRequested> {
        self.changes_requested.as_ref().filter(|c| self.reviewed.is_none_or(|r| r == c.attempt))
    }

    pub fn is_empty(&self) -> bool {
        *self == RunUpdate::default()
    }
}

/// A resolved question the factory's agent asked and the answer it got.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answered {
    pub question: String,
    pub answer: String,
    /// Who answered.
    pub by: String,
    /// When it was answered, as `YYYY-MM-DD`.
    pub date: String,
}

/// A `decision` or `note` comment on a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskNote {
    pub author: String,
    /// `YYYY-MM-DD`.
    pub date: String,
    pub body: String,
}

/// What people have said on a task that the agent should know: answers and notes, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HumanContext {
    pub answers: Vec<Answered>,
    pub notes: Vec<TaskNote>,
}

impl HumanContext {
    pub fn is_empty(&self) -> bool {
        self.answers.is_empty() && self.notes.is_empty()
    }
}

/// Whether an author is the factory itself or one of its run sub-agents.
pub fn is_factory_agent(name: &str) -> bool {
    name == "factory" || name.starts_with("factory/")
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
    /// The task's resolved questions with their answers, and its `decision` and `note` comments, oldest first,
    /// without the factory's own bookkeeping.
    async fn human_context(&self, task: &TaskKey) -> Result<HumanContext>;
    /// Open, in-progress, parked or in-review tasks that carry factory metadata.
    async fn in_flight(&self, project: &str) -> Result<Vec<Task>>;

    async fn claim(&self, task: &TaskKey) -> Result<()>;
    async fn renew(&self, task: &TaskKey) -> Result<()>;
    async fn release(&self, task: &TaskKey) -> Result<()>;
    async fn report(&self, task: &TaskKey, update: RunUpdate) -> Result<()>;
    async fn request_review(&self, task: &TaskKey) -> Result<()>;
    /// Sends a task in review back to the queue, unheld, for a rework run to pick up.
    async fn reopen(&self, task: &TaskKey) -> Result<()>;
    async fn record_verdict(&self, task: &TaskKey, verdict: &Verdict) -> Result<()>;
    async fn ask(&self, task: &TaskKey, question: &str) -> Result<()>;
    /// A note on the task, such as why an attempt failed, without changing it.
    async fn note(&self, task: &TaskKey, body: &str) -> Result<()>;
    async fn finish(&self, task: &TaskKey, handoff: &str) -> Result<()>;
    /// Files a new task in `project`, not under the factory's queue, and returns its key.
    async fn create_task(
        &self,
        project: &str,
        title: &str,
        description: &str,
        priority: Option<i64>,
    ) -> Result<TaskKey>;
    /// An unfinished task in `project` with this title (ignoring case and surrounding space), if there is one.
    async fn find_open_by_title(&self, project: &str, title: &str) -> Result<Option<TaskKey>>;

    /// Creates (or reuses) a project-scoped sub-agent and a fresh token for it that expires after `ttl`,
    /// in case revoking it is missed.
    async fn create_sub_agent(&self, name: &str, project: &str, ttl: Duration) -> Result<Credential>;
    /// Revokes a sub-agent and its tokens. Revoking one that's already gone is fine.
    async fn revoke_sub_agent(&self, name: &str) -> Result<()>;
}
