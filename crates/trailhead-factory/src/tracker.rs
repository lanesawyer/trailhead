use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{Error, GateKind, PrGrade, PrRef, Result, Usd, Verdict};

pub use trailhead_model::{Grade, Priority, Status, TaskKey};

/// The key in task metadata that the factory's run fields ([`RunUpdate`]) live under.
pub const METADATA_KEY: &str = "factory";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub key: TaskKey,
    pub project: String,
    pub title: String,
    pub description: String,
    pub status: Status,
    /// The factory works only graded tasks.
    pub grade: Option<Grade>,
    /// `Tracker::ready` raises it to the highest among the task and the open tasks
    /// it blocks, directly or through a chain, so a low-priority blocker is listed as urgent as the work it holds up.
    pub priority: Priority,
    /// The agent holding the claim, if any.
    pub holder: Option<String>,
    /// The task is in review and waiting on a rework: its newest review asks for changes and no handoff has come
    /// since (`trailhead_model::changes_requested`). Computed by the tracker, never stored.
    #[serde(default)]
    pub changes_requested: bool,
    /// The `factory` object in the task's metadata ([`RunUpdate`] fields).
    pub run: RunUpdate,
}

impl Task {
    /// The factory's view of a Trailhead task. `holder` is the name of the agent holding its claim, if any.
    pub fn from_model(task: trailhead_model::Task, holder: Option<String>) -> Result<Task> {
        let run = RunUpdate::of(&task).map_err(|e| Error::Other(format!("{}: bad factory metadata: {e}", task.key)))?;
        let key = task.key;
        Ok(Task {
            project: key.project().to_string(),
            key,
            title: task.title,
            description: task.description,
            status: task.status,
            grade: task.grade,
            priority: task.priority,
            holder,
            changes_requested: false,
            run,
        })
    }
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
    /// The target the run's sandbox is on (`docs/factory/targets.md`), beside `host`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// When the run's agent was started, in unix seconds, so a control plane that reattaches after a restart keeps
    /// the run's wall-clock budget honest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<i64>,
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
    /// The run whose report was handled and its spend counted, so a control plane that restarts before the run's
    /// sandbox is released doesn't reattach and count it again (TRAIL-255).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concluded: Option<String>,
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
    /// A reviewer breach the task is to be parked for, written before the task is reopened so that reconcile can
    /// finish a park that was cut short (TRAIL-366).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_breach: Option<ReviewBreach>,
    /// A worker's budget breach the task is to be parked for, written with the run's spend so that reconcile can
    /// finish a park that was cut short instead of requeueing the task on a fresh budget (TRAIL-451).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_breach: Option<WorkerBreach>,
    /// Conflict reworks of the task's PR (TRAIL-221). They don't count as review rounds; `max_conflict_reworks` caps them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict_reworks: Option<u32>,
    /// The attempt the last review asked to change. `max_attempts` counts the runs after it, so reworks don't use up
    /// the attempts of the task as a whole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_base: Option<u32>,
    /// The attempt at which the control plane parked the task for hitting `max_attempts`. A run that starts on the
    /// reopened task counts from it (see `attempt_floor`), so a human only has to reopen it (TRAIL-308).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gave_up: Option<u32>,
    /// How the task's PR was merged, once it has been.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged: Option<Merged>,
    /// The PR's grade and what raised it above the task's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade: Option<PrGrade>,
    /// The task was skipped for having no grade, and said so (`set one to queue it`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ungraded: Option<bool>,
    /// The PR head commit the agent review last ran on, so a verdict on an older head isn't counted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_head: Option<String>,
    /// The merger's last decision on the PR.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge: Option<MergeDecision>,
    /// The head the merger is about to merge, written before `CodeHost::merge` so that a merge whose close failed is
    /// still attributed to the merger. Empty once the host said the merge didn't happen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merging: Option<String>,
    /// The PR's head from before a conflict rework pushed (its request's `head`), until the host answers `mergeable`
    /// for another head (TRAIL-446). A reading while the PR still shows this head is the old head's, so it doesn't
    /// count. Empty once settled; written empty to clear it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_head: Option<String>,
    /// When the control plane gave up the parked task's pooled sandbox for waiting work (unix seconds; 0 once a new run
    /// has started, since a null write leaves the field as it is), so reconcile and `factory status` don't look for
    /// it (TRAIL-601).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released: Option<i64>,
}

impl RunUpdate {
    /// The factory's fields in `task`'s metadata; the defaults for a task it hasn't touched.
    pub fn of(task: &trailhead_model::Task) -> serde_json::Result<RunUpdate> {
        match task.metadata.get(METADATA_KEY) {
            Some(v) => serde_json::from_value(v.clone()),
            None => Ok(RunUpdate::default()),
        }
    }
}

/// The merger's decision on a PR's current head (`docs/factory/review.md#the-merger`). Written whole, and its
/// optional fields serialize as `null` when unset so that a write clears them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeDecision {
    pub grade: Grade,
    /// The head commit the gates were evaluated on.
    pub head: String,
    /// The gates that passed: `checks`, `ci`, `agent_review`, `human`.
    pub passed: Vec<GateKind>,
    /// The gates still to pass, and for CI that failed, the names of the failing checks.
    pub waiting: Vec<GateKind>,
    #[serde(default)]
    pub failing: Vec<String>,
    /// Every gate passed and the grade auto-merges, so a merger that was on would merge now.
    pub would_merge: bool,
    /// The head the control plane moved the task to `approved` on, which tells its approval from Lane's.
    pub approved_head: Option<String>,
    /// The head Lane approved the task on.
    pub human_head: Option<String>,
    /// Why the control plane couldn't act on the decision, such as not being a delegated reviewer.
    pub problem: Option<String>,
    /// Why the code host last refused to merge this head, which is said once per reason and tried again each pass.
    #[serde(default)]
    pub refused: Option<String>,
    /// The id of the merger's status comment on the PR, which is edited as the decision changes.
    #[serde(default)]
    pub comment: Option<u64>,
    /// What was last said on the PR about this, so it's said once per change.
    pub message: String,
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
    /// Not a review: the PR no longer merges cleanly with the base branch, and the rework merges it in (TRAIL-199).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub conflict: bool,
    /// The PR's head when this was requested. A head that has moved since means someone pushed to the PR, so the
    /// request is stale: a person settled it (TRAIL-338). Unknown for requests recorded before this, or when the
    /// host couldn't say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
}

/// A reviewer that overran its budget, and whether the task was parked for it yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewBreach {
    /// The review round the breach happened in.
    pub round: u32,
    /// The attempt whose PR was being reviewed. A breach only parks the task while it is still on that attempt; a
    /// worker run that has started since has left the breach behind.
    #[serde(default)]
    pub attempt: u32,
    pub reason: String,
    /// The task was parked with the question; until then, reconcile finishes the park.
    #[serde(default)]
    pub parked: bool,
}

/// A worker run that overran its budget. Recorded unparked with the run's spend, and marked parked just before the
/// question is asked, so reconcile finishes a park that was cut short and never asks again once it's answered (an
/// answer within the run's claim moves the task back to in progress under the same sub-agent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerBreach {
    /// The run that breached, by name; a breach only parks the task while that run is the task's current one.
    pub run: String,
    pub reason: String,
    #[serde(default)]
    pub parked: bool,
}

impl RunUpdate {
    pub fn merge(&mut self, update: RunUpdate) {
        let RunUpdate {
            attempt,
            sandbox,
            host,
            target,
            started,
            profile,
            pr,
            review_rounds,
            conflict_reworks,
            reviewed,
            spend,
            concluded,
            failure,
            retries,
            infra_failures,
            changes_requested,
            review_breach,
            worker_breach,
            attempt_base,
            gave_up,
            merged,
            grade,
            ungraded,
            reviewed_head,
            merge,
            merging,
            stale_head,
            released,
        } = update;
        self.attempt = attempt.or(self.attempt.take());
        self.sandbox = sandbox.or(self.sandbox.take());
        self.host = host.or(self.host.take());
        self.target = target.or(self.target.take());
        self.started = started.or(self.started.take());
        self.profile = profile.or(self.profile.take());
        self.pr = pr.or(self.pr.take());
        self.review_rounds = review_rounds.or(self.review_rounds.take());
        self.conflict_reworks = conflict_reworks.or(self.conflict_reworks.take());
        self.reviewed = reviewed.or(self.reviewed.take());
        self.spend = spend.or(self.spend.take());
        self.concluded = concluded.or(self.concluded.take());
        self.failure = failure.or(self.failure.take());
        self.retries = retries.or(self.retries.take());
        self.infra_failures = infra_failures.or(self.infra_failures.take());
        self.changes_requested = changes_requested.or(self.changes_requested.take());
        self.review_breach = review_breach.or(self.review_breach.take());
        self.worker_breach = worker_breach.or(self.worker_breach.take());
        self.attempt_base = attempt_base.or(self.attempt_base.take());
        self.gave_up = gave_up.or(self.gave_up.take());
        self.merged = merged.or(self.merged.take());
        self.grade = grade.or(self.grade.take());
        self.ungraded = ungraded.or(self.ungraded.take());
        self.reviewed_head = reviewed_head.or(self.reviewed_head.take());
        self.merge = merge.or(self.merge.take());
        self.merging = merging.or(self.merging.take());
        self.stale_head = stale_head.or(self.stale_head.take());
        self.released = released.or(self.released.take());
    }

    /// The review the next run has to address: the last one that asked for changes, unless a later review has
    /// since finished on another attempt.
    pub fn pending_changes(&self) -> Option<&ChangesRequested> {
        self.changes_requested.as_ref().filter(|c| self.reviewed.is_none_or(|r| r == c.attempt))
    }

    /// The attempt `max_attempts` counts from: the last review that asked for changes, or the attempt the control
    /// plane gave up at, whichever is later.
    pub fn attempt_floor(&self) -> u32 {
        self.attempt_base.unwrap_or(0).max(self.gave_up.unwrap_or(0))
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
    /// For a reviewer: the unfinished follow-ups that earlier reviews of the task filed.
    #[serde(default)]
    pub follow_ups: Vec<FollowUpTask>,
}

impl HumanContext {
    pub fn is_empty(&self) -> bool {
        self.answers.is_empty() && self.notes.is_empty() && self.follow_ups.is_empty()
    }
}

/// A follow-up task an earlier review of a task filed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowUpTask {
    pub key: TaskKey,
    pub title: String,
    pub description: String,
}

/// The line a follow-up's description carries to say which task's review raised it.
pub fn raised_by(source: &TaskKey) -> String {
    format!("Raised by the reviewer of {}", source.0)
}

/// Whether a task description says the review of `source` raised it. The key must end there (at the PR link's
/// parenthesis or the full stop), so TRAIL-1 doesn't match TRAIL-12.
pub fn is_raised_by(description: &str, source: &TaskKey) -> bool {
    let marker = raised_by(source);
    description.match_indices(&marker).any(|(i, _)| {
        description[i + marker.len()..].chars().next().is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '-'))
    })
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

    /// Ready tasks in `project`: open and unheld, and in-review ones waiting on a rework ([`Task::changes_requested`]),
    /// those first. With `queue`, only the direct subtasks of any of those tasks, best first across all of
    /// them and each once, which is how work is handed to the factory.
    async fn ready(&self, project: &str, queue: &[TaskKey], limit: usize) -> Result<Vec<Task>>;
    async fn show(&self, task: &TaskKey) -> Result<Task>;
    /// The task's resolved questions with their answers, and its `decision` and `note` comments, oldest first,
    /// without the factory's own bookkeeping.
    async fn human_context(&self, task: &TaskKey) -> Result<HumanContext>;
    /// The task's latest `review` comment, whoever left it, if it has one.
    async fn last_review(&self, task: &TaskKey) -> Result<Option<Verdict>>;
    /// Open, in-progress, parked or in-review tasks that carry factory metadata, limited to the queues' subtasks when
    /// there are any, so repos sharing a project through different queues never see each other's runs.
    async fn in_flight(&self, project: &str, queue: &[TaskKey]) -> Result<Vec<Task>>;
    /// Open, in-progress, parked or in-review tasks in `project` that carry factory metadata but aren't direct subtasks
    /// of any of `queues`: runs re-parented out of their queue, which [`Tracker::in_flight`] no longer sees (TRAIL-531).
    /// A tracker that can't tell says there are none.
    async fn strays(&self, _project: &str, _queues: &[TaskKey]) -> Result<Vec<Task>> {
        Ok(Vec::new())
    }

    async fn claim(&self, task: &TaskKey) -> Result<()>;
    async fn renew(&self, task: &TaskKey) -> Result<()>;
    async fn release(&self, task: &TaskKey) -> Result<()>;
    async fn report(&self, task: &TaskKey, update: RunUpdate) -> Result<()>;
    /// Moves a task to review, leaving `handoff` on it first. The handoff is what answers a review that asked for
    /// changes, so a task coming back from a rework stops being one waiting on it.
    async fn request_review(&self, task: &TaskKey, handoff: &str) -> Result<()>;
    /// Answers a review that asked for changes without a new push, as when the request no longer applies.
    async fn handoff(&self, task: &TaskKey, body: &str) -> Result<()>;
    /// Moves a task in review to `approved`. Only a human or a project's delegated reviewer can; anyone else gets
    /// `Forbidden`.
    async fn approve(&self, task: &TaskKey) -> Result<()>;
    /// Sends an approved task back to review, as a push after its approval needs a new one.
    async fn unapprove(&self, task: &TaskKey) -> Result<()>;
    /// Moves any task core lets reach `open` (in review, approved or parked on a question) back to the queue, unheld.
    /// Reworks don't use it: they stay in review (changes requested).
    async fn reopen(&self, task: &TaskKey) -> Result<()>;
    /// Raises the task's grade. Trailhead lets an agent raise a grade but never set the first or lower it.
    async fn raise_grade(&self, task: &TaskKey, grade: Grade) -> Result<()>;
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
        priority: Option<Priority>,
    ) -> Result<TaskKey>;
    /// The unfinished follow-ups that reviews of `task` filed: tasks in its project whose description says
    /// ([`is_raised_by`]) that its review raised them.
    async fn follow_ups(&self, task: &TaskKey) -> Result<Vec<FollowUpTask>>;
    /// An unfinished task in `project` with this title (ignoring case and surrounding space), if there is one.
    async fn find_open_by_title(&self, project: &str, title: &str) -> Result<Option<TaskKey>>;

    /// Creates (or reuses) a project-scoped sub-agent and a fresh token for it that expires after `ttl`,
    /// in case revoking it is missed.
    async fn create_sub_agent(&self, name: &str, project: &str, ttl: Duration) -> Result<Credential>;
    /// Revokes a sub-agent and its tokens. Revoking one that's already gone is fine.
    async fn revoke_sub_agent(&self, name: &str) -> Result<()>;
}
