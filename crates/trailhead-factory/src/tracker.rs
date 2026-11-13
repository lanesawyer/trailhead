use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{Error, GateKind, PrGrade, PrRef, Result, Usd, Verdict};

pub use trailhead_model::{Grade, Priority, Status, TaskKey, key_number};

/// The key in task metadata that the factory's run fields ([`RunRecord`]) live under.
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
    /// The `factory` object in the task's metadata ([`RunRecord`] fields).
    pub run: RunRecord,
}

impl Task {
    /// The factory's view of a Trailhead task. `holder` is the name of the agent holding its claim, if any.
    pub fn from_model(task: trailhead_model::Task, holder: Option<String>) -> Result<Task> {
        let run = RunRecord::of(&task).map_err(|e| Error::Other(format!("{}: bad factory metadata: {e}", task.key)))?;
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

/// What a write does to one field of the run record.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Patch<T> {
    /// Leave it as it is.
    #[default]
    Keep,
    /// Remove it.
    Clear,
    Set(T),
}

impl<T> Patch<T> {
    pub fn is_keep(&self) -> bool {
        matches!(self, Patch::Keep)
    }

    /// Puts a field back to a value read earlier: set if there was one, cleared if there wasn't.
    pub fn restore(was: Option<T>) -> Patch<T> {
        was.map_or(Patch::Clear, Patch::Set)
    }

    fn apply(self, field: &mut Option<T>) {
        match self {
            Patch::Keep => {}
            Patch::Clear => *field = None,
            Patch::Set(value) => *field = Some(value),
        }
    }
}

/// Set when there is a value, otherwise kept: for a field a write only sometimes has something to say about.
impl<T> From<Option<T>> for Patch<T> {
    fn from(value: Option<T>) -> Patch<T> {
        value.map_or(Patch::Keep, Patch::Set)
    }
}

/// In the stored metadata's merge patch a cleared field is `null`, which removes the key, and a kept one is absent.
impl<T: Serialize> Serialize for Patch<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Patch::Set(value) => value.serialize(serializer),
            Patch::Keep | Patch::Clear => serializer.serialize_none(),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Patch<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Patch<T>, D::Error> {
        Ok(Option::<T>::deserialize(deserializer)?.map_or(Patch::Clear, Patch::Set))
    }
}

/// Declares the factory's fields once, as the record that is read ([`RunRecord`]) and the write that changes it
/// ([`RunPatch`]).
macro_rules! run_fields {
    ($($(#[$doc:meta])* $name:ident: $ty:ty,)*) => {
        /// The factory's fields in task metadata, as they are read.
        #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
        pub struct RunRecord {
            $(
                $(#[$doc])*
                #[serde(default, skip_serializing_if = "Option::is_none")]
                pub $name: Option<$ty>,
            )*
        }

        /// A write to a task's [`RunRecord`]: each field kept, cleared or set.
        #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
        pub struct RunPatch {
            $(
                #[serde(default, skip_serializing_if = "Patch::is_keep")]
                pub $name: Patch<$ty>,
            )*
        }

        impl RunRecord {
            /// The record after `patch`.
            pub fn apply(&mut self, patch: RunPatch) {
                $( patch.$name.apply(&mut self.$name); )*
            }
        }
    };
}

run_fields! {
    attempt: u32,
    /// The run's name, which its sub-agent shares (`trail-42-a1`).
    sandbox: String,
    /// The pooled sandbox the run is on, when it isn't one named after the run.
    host: String,
    /// The target the run's sandbox is on (`docs/factory/targets.md`), beside `host`.
    target: String,
    /// When the run's agent was started, in unix seconds, so a control plane that reattaches after a restart keeps
    /// the run's wall-clock budget honest.
    started: i64,
    profile: String,
    pr: PrRef,
    /// Review rounds started on the current attempt.
    review_rounds: u32,
    /// The attempt whose PR the agent review last finished on.
    reviewed: u32,
    /// Total spend across attempts and reviews.
    spend: Usd,
    /// The run whose report was handled and its spend counted, so a control plane that restarts before the run's
    /// sandbox is released doesn't reattach and count it again (TRAIL-255).
    concluded: String,
    /// Why the last attempt failed, which the next attempt is told.
    failure: String,
    /// Runs that failed before their agent started (TRAIL-159). They don't count as attempts, but each revoked a
    /// sub-agent, so a retry's name carries this count to stay unique.
    retries: u32,
    /// Runs in a row that failed on the infrastructure, before or after the agent started (TRAIL-172). They don't
    /// count as attempts; `max_infra_failures` caps them. Reset when an agent ends a run, or when the cap parks the task.
    infra_failures: u32,
    /// The review that last asked for changes, which the rework on the PR branch is told.
    changes_requested: ChangesRequested,
    /// A reviewer breach the task is to be parked for, written before the task is reopened so that reconcile can
    /// finish a park that was cut short (TRAIL-366).
    review_breach: ReviewBreach,
    /// A worker's budget breach the task is to be parked for, written with the run's spend so that reconcile can
    /// finish a park that was cut short instead of requeueing the task on a fresh budget (TRAIL-451).
    worker_breach: WorkerBreach,
    /// Conflict reworks of the task's PR (TRAIL-221). They don't count as review rounds; `max_conflict_reworks` caps them.
    conflict_reworks: u32,
    /// The attempt the last review asked to change. `max_attempts` counts the runs after it, so reworks don't use up
    /// the attempts of the task as a whole.
    attempt_base: u32,
    /// The attempt at which the control plane parked the task for hitting `max_attempts`. A run that starts on the
    /// reopened task counts from it (see `attempt_floor`), so a human only has to reopen it (TRAIL-308).
    gave_up: u32,
    /// The attempt that ended in a question. A give-up recorded in `failure` from before it isn't "the attempt
    /// before" the next one for the backstop on repeated give-ups, though the next attempt is still told of it
    /// (TRAIL-805).
    asked: u32,
    /// How the task's PR was merged, once it has been.
    merged: Merged,
    /// A person merged the PR without the approval core requires to finish the task, so the task is being parked for a
    /// person to mark it done. Written before the park's reopen, claim and ask, so a pass that finds a task with it
    /// outside `needs_input` finishes the park rather than closing the task (TRAIL-729). `merged` alone can't say
    /// this: it is also recorded by a close that failed before the park.
    unapproved_merge: bool,
    /// The PR's grade and what raised it above the task's.
    grade: PrGrade,
    /// The task was skipped for having no grade, and said so (`set one to queue it`).
    ungraded: bool,
    /// The PR head commit the agent review last ran on, so a verdict on an older head isn't counted.
    reviewed_head: String,
    /// The merger's last decision on the PR.
    merge: MergeDecision,
    /// The head the merger is about to merge, written before `CodeHost::merge` so that a merge whose close failed is
    /// still attributed to the merger. Cleared once the host said the merge didn't happen.
    merging: String,
    /// The PR's head from before a conflict rework pushed (its request's `head`), until the host answers `mergeable`
    /// for another head (TRAIL-446). A reading while the PR still shows this head is the old head's, so it doesn't
    /// count. Cleared once settled.
    stale_head: String,
    /// When the control plane gave up the parked task's pooled sandbox for waiting work (unix seconds), so reconcile
    /// and `factory status` don't look for it (TRAIL-601). Cleared when a new run starts.
    released: i64,
}

impl RunRecord {
    /// The factory's fields in `task`'s metadata; the defaults for a task it hasn't touched.
    pub fn of(task: &trailhead_model::Task) -> serde_json::Result<RunRecord> {
        match task.metadata.get(METADATA_KEY) {
            Some(v) => serde_json::from_value(v.clone()).map(RunRecord::without_stand_ins),
            None => Ok(RunRecord::default()),
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
    /// The gates still to pass.
    pub waiting: Vec<GateKind>,
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
    /// What the status comment has said, oldest first and capped at `HISTORY_LIMIT`, so the comment can list when
    /// the decision changed.
    #[serde(default)]
    pub history: Vec<Said>,
}

/// How many statuses the merger's status comment lists in its History.
pub const HISTORY_LIMIT: usize = 8;

/// One status the merger said on the PR, and when (UTC, to the minute).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Said {
    pub at: String,
    pub message: String,
}

/// A merged PR: who merged it, which PR, and the commit it became. Written whole, like [`ChangesRequested`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Merged {
    pub by: MergedBy,
    pub pr: PrRef,
    #[serde(default)]
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

/// Who a rework asked for by failing CI is attributed to, in the worker's feedback and the park question.
pub const CI_REVIEWER: &str = "CI";

/// What asked for a PR to be changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    /// A reviewer. Its rework uses a review round.
    #[default]
    Review,
    /// The PR no longer merges cleanly with the base branch, and the rework merges it in (TRAIL-199).
    Conflict,
    /// CI failed on the PR's head (TRAIL-390). Its rework merges the base in too.
    Ci,
}

impl RequestKind {
    /// Whether a reviewer asked. The other two are counted as conflict reworks (`max_conflict_reworks`), and their
    /// rework starts with the base branch merged in.
    pub fn is_review(self) -> bool {
        self == RequestKind::Review
    }
}

/// A request for changes to a PR: who asked, what they said, and the attempt whose PR it was about. Written whole:
/// every field serializes, unset ones as `null` or `false`, because the store merges a written record into the one
/// it holds and a field left out would keep the older request's value (TRAIL-1471).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "StoredRequest")]
pub struct ChangesRequested {
    pub reviewer: String,
    pub body: String,
    pub attempt: u32,
    pub kind: RequestKind,
    /// The PR's head when this was requested. A head that has moved since means someone pushed to the PR, so the
    /// request is stale: a person settled it (TRAIL-338). Unknown for requests recorded before this, or when the
    /// host couldn't say.
    #[serde(default)]
    pub head: Option<String>,
    /// A person has taken the request over: the task was parked on it and then answered or reopened, which settles
    /// it whether or not they pushed. A settled request is kept for the record but is no longer pending.
    #[serde(default)]
    pub settled: bool,
}

/// A request as task metadata holds it. Records from before `kind` say a conflict with a flag, and CI by the
/// reviewer's name; tasks in flight still carry those.
#[derive(Deserialize)]
struct StoredRequest {
    reviewer: String,
    body: String,
    #[serde(default)]
    attempt: u32,
    #[serde(default)]
    kind: Option<RequestKind>,
    #[serde(default)]
    conflict: bool,
    #[serde(default)]
    head: Option<String>,
    #[serde(default)]
    settled: bool,
}

impl From<StoredRequest> for ChangesRequested {
    fn from(stored: StoredRequest) -> ChangesRequested {
        let kind = stored.kind.unwrap_or(match (stored.conflict, stored.reviewer.as_str()) {
            (false, _) => RequestKind::Review,
            (true, CI_REVIEWER) => RequestKind::Ci,
            (true, _) => RequestKind::Conflict,
        });
        ChangesRequested {
            reviewer: stored.reviewer,
            body: stored.body,
            attempt: stored.attempt,
            kind,
            head: stored.head,
            settled: stored.settled,
        }
    }
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
    /// The sandbox couldn't be cut off or destroyed and is still live, so the park doesn't keep it: reconcile
    /// retries the destroy even though the task is parked.
    #[serde(default)]
    pub stuck: bool,
}

impl RunRecord {
    /// Records from before a write could clear a field (TRAIL-1459) cleared one by writing a stand-in: an empty
    /// string, or zero. A task still in flight may carry one, and it reads as the absence it stood for.
    fn without_stand_ins(mut self) -> RunRecord {
        self.merging = self.merging.filter(|head| !head.is_empty());
        self.stale_head = self.stale_head.filter(|head| !head.is_empty());
        self.released = self.released.filter(|at| *at > 0);
        self.gave_up = self.gave_up.filter(|attempt| *attempt > 0);
        self.failure = self.failure.filter(|failure| !failure.is_empty());
        self
    }

    /// The last request for changes, unless a later review has since finished on another attempt. It may be settled:
    /// a worker that takes the task over after a person answered a park still needs to know it was a conflict.
    pub fn last_changes(&self) -> Option<&ChangesRequested> {
        self.changes_requested.as_ref().filter(|c| self.reviewed.is_none_or(|r| r == c.attempt))
    }

    /// The request still waiting for a worker: the last one, unless a person settled it.
    pub fn pending_changes(&self) -> Option<&ChangesRequested> {
        self.last_changes().filter(|c| !c.settled)
    }

    /// The attempt `max_attempts` counts from: the last review that asked for changes, or the attempt the control
    /// plane gave up at, whichever is later.
    pub fn attempt_floor(&self) -> u32 {
        self.attempt_base.unwrap_or(0).max(self.gave_up.unwrap_or(0))
    }

    pub fn is_empty(&self) -> bool {
        *self == RunRecord::default()
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

/// The last step of [`Tracker::ready`]: puts the tasks in order, by priority and within a priority reworks
/// ([`Task::changes_requested`]) ahead of new work, then cuts the list to `limit`. A tracker merges its reworks in and
/// raises each task to the priority of what it blocks before this, so a pile of low-priority reworks can't hide a
/// higher-priority new task, nor the cut a low-priority blocker of urgent work. The sort is stable, so the order the
/// tasks came in holds among equals.
pub fn in_ready_order(mut tasks: Vec<Task>, limit: usize) -> Vec<Task> {
    tasks.sort_by_key(|t| (t.priority, !t.changes_requested));
    tasks.truncate(limit);
    tasks
}

/// A Trailhead sub-agent and its bearer token, handed to a sandbox at start. `Debug` leaves the token out, since a
/// credential rides in a [`RunRequest`](crate::RunRequest) that gets logged.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credential {
    pub agent: String,
    pub token: String,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential").field("agent", &self.agent).finish_non_exhaustive()
    }
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
    async fn report(&self, task: &TaskKey, update: RunPatch) -> Result<()>;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_patch_writes_only_what_it_changes_and_a_clear_as_null() {
        let patch = RunPatch { attempt: Patch::Set(2), merging: Patch::Clear, ..RunPatch::default() };
        let written = serde_json::to_value(&patch).unwrap();
        assert_eq!(written, serde_json::json!({"attempt": 2, "merging": null}));
        assert_eq!(serde_json::from_value::<RunPatch>(written).unwrap(), patch);

        let mut run = RunRecord { attempt: Some(1), merging: Some("abc".into()), ..RunRecord::default() };
        run.host = Some("w1".into());
        run.apply(patch);
        assert_eq!((run.attempt, run.merging, run.host.as_deref()), (Some(2), None, Some("w1")));
    }

    #[test]
    fn a_stand_in_from_before_clears_reads_as_absent() {
        let task: trailhead_model::Task = serde_json::from_value(serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000001", "workspace_id": "00000000-0000-0000-0000-000000000002",
            "project_id": "00000000-0000-0000-0000-000000000003", "key": "TRAIL-7", "title": "t", "description": "",
            "status": "open", "priority": 2, "version": 1, "created_by": "00000000-0000-0000-0000-000000000004",
            "created_at": "2026-10-01T00:00:00Z", "updated_at": "2026-10-01T00:00:00Z",
            "metadata": {"factory": {"attempt": 3, "merging": "", "stale_head": "", "released": 0, "gave_up": 0, "failure": ""}},
        }))
        .unwrap();
        let run = RunRecord::of(&task).unwrap();
        assert_eq!(run, RunRecord { attempt: Some(3), ..RunRecord::default() });
    }

    #[test]
    fn a_request_recorded_before_kinds_reads_its_kind_from_the_flag_and_the_reviewer() {
        let read = |v: serde_json::Value| serde_json::from_value::<ChangesRequested>(v).unwrap().kind;
        let old = |reviewer: &str, conflict: bool| serde_json::json!({"reviewer": reviewer, "body": "b", "attempt": 1, "conflict": conflict});
        assert_eq!(read(old("factory/trail-7-r1", false)), RequestKind::Review);
        assert_eq!(read(serde_json::json!({"reviewer": "r", "body": "b"})), RequestKind::Review);
        assert_eq!(read(old("factory", true)), RequestKind::Conflict);
        assert_eq!(read(old(CI_REVIEWER, true)), RequestKind::Ci);
    }

    #[test]
    fn a_request_is_written_with_its_kind_and_without_the_old_flag() {
        let request = ChangesRequested {
            reviewer: CI_REVIEWER.into(),
            body: "test failed".into(),
            attempt: 2,
            kind: RequestKind::Ci,
            head: None,
            settled: false,
        };
        let written = serde_json::to_value(&request).unwrap();
        assert_eq!(written["kind"], "ci");
        assert!(written.get("conflict").is_none());
        assert_eq!(serde_json::from_value::<ChangesRequested>(written).unwrap(), request);
    }

    fn task(key: &str, priority: u8, changes_requested: bool) -> Task {
        Task {
            key: TaskKey(key.into()),
            project: "TRAIL".into(),
            title: key.into(),
            description: String::new(),
            status: if changes_requested { Status::InReview } else { Status::Open },
            grade: None,
            priority: Priority::new(priority).unwrap(),
            holder: None,
            changes_requested,
            run: RunRecord::default(),
        }
    }

    #[test]
    fn ready_order_is_priority_then_reworks_first_then_arrival_and_cut_last() {
        let tasks = vec![
            task("TRAIL-1", 3, false),
            task("TRAIL-2", 2, false),
            task("TRAIL-3", 2, true),
            task("TRAIL-4", 2, false),
            task("TRAIL-5", 3, true),
        ];
        let keys = |tasks: Vec<Task>| tasks.into_iter().map(|t| t.key.0).collect::<Vec<_>>();
        assert_eq!(keys(in_ready_order(tasks.clone(), 10)), ["TRAIL-3", "TRAIL-2", "TRAIL-4", "TRAIL-5", "TRAIL-1"]);
        assert_eq!(keys(in_ready_order(tasks, 2)), ["TRAIL-3", "TRAIL-2"]);
    }
}
