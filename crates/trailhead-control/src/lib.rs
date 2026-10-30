//! The factory's control plane: starts sandboxes for ready tasks, enforces
//! budgets, dispatches reviews and reconciles on boot. It keeps no state of its
//! own; everything it needs is in the tracker and the compute host
//! (`docs/factory/state.md`).

mod core_tracker;
pub mod daemon_binary;

pub use core_tracker::{CoreTracker, METADATA_KEY};

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::FutureExt;
use tokio::sync::Notify;
use tokio::task::{self, JoinSet};
use tokio::time::Instant;

use tokio::sync::mpsc;
use trailhead_factory::harness::is_transient_api_error;
use trailhead_factory::{
    BranchUpdate, ChangesRequested, CheckResult, CheckpointId, CiState, CodeHost, Compute, Daemon, Egress, Error,
    FactoryConfig, Feedback, HumanContext, IROH_DOMAINS, Labels, MergeDecision, MergeMode, MergeOutcome, Merged,
    MergedBy, ModelProvider, PrGrade, PrRef, PrState, PrepareRequest, Profile, Result, Role, RunEvent, RunReport,
    RunRequest, RunResult, RunUpdate, Sandbox, SandboxSpec, Status, Task, TaskKey, TaskSpec, Tracker, Usd, Verdict,
    VerdictKind, label, review_name, run_name,
};

/// How long a daemon has to stop after a cancel before the control plane cuts its sandbox off regardless.
pub const CANCEL_GRACE: Duration = Duration::from_secs(60);

/// How long a stopping control plane waits for its cancelled runs to end before abandoning them.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(15);

/// The `factory` label value on every sandbox this control plane starts.
pub const FACTORY_LABEL: &str = "trailhead";

/// How many times reconcile tries to release a pooled sandbox before giving up on it and starting a new one.
pub const RELEASE_ATTEMPTS: u32 = 5;

/// How long a pooled sandbox may go on failing to release before the factory gives up on it, however few tries
/// that took.
pub const RELEASE_PATIENCE: Duration = Duration::from_secs(10 * 60);

/// The first wait between tries at a troubled sandbox, doubling up to [`MAX_BACKOFF`].
const FIRST_BACKOFF: Duration = Duration::from_secs(30);
const MAX_BACKOFF: Duration = Duration::from_secs(5 * 60);

/// The first wait before retrying a task after an infrastructure failure, doubling up to [`MAX_INFRA_BACKOFF`].
const FIRST_INFRA_BACKOFF: Duration = Duration::from_secs(60);
const MAX_INFRA_BACKOFF: Duration = Duration::from_secs(15 * 60);

/// Where a sandbox clones a public GitHub repo from during prepare (`docs/factory/sandbox.md`).
const CLONE_DOMAINS: &[&str] = &["github.com", "codeload.github.com"];

/// Who a rework asked for by failing CI is attributed to, in the worker's feedback and the park question.
const CI_REVIEWER: &str = "CI";

/// Reads the current `[merge] mode`, or says why it can't.
pub type MergeModeSource = Box<dyn Fn() -> std::result::Result<MergeMode, String> + Send + Sync>;

/// Checks that the Trailhead server the sandboxes' daemons claim through is reachable, or says why not.
pub type ServerCheck =
    Box<dyn Fn() -> futures_util::future::BoxFuture<'static, std::result::Result<(), String>> + Send + Sync>;

pub struct Factory {
    pub config: FactoryConfig,
    pub compute: Arc<dyn Compute>,
    pub tracker: Arc<dyn Tracker>,
    pub provider: Arc<dyn ModelProvider>,
    pub daemon: Arc<dyn Daemon>,
    /// Reads PRs from the control plane, which sandboxes' own code host access can't serve. Without one, reconcile
    /// doesn't notice merges or closed PRs.
    pub codehost: Option<Arc<dyn CodeHost>>,
    /// Where the merge mode is re-read from before each merge decision, so it can be switched without a restart.
    merge_mode_source: Option<MergeModeSource>,
    /// The merge mode last read from `merge_mode_source`, kept when a later read fails.
    merge_mode: Mutex<MergeMode>,
    /// Run before starting runs: the control plane reads the database directly, but each sandbox's daemon needs the
    /// server, so a run started while it is down fails on its first claim.
    server_check: Option<ServerCheck>,
    /// Whether the server was last found down, so an outage is reported loudly once and its end once.
    server_down: Mutex<bool>,
    troubled: Mutex<HashMap<String, Trouble>>,
    /// Tasks whose last runs failed on the infrastructure: how many in a row, and when to try again.
    setbacks: Mutex<HashMap<TaskKey, (u32, Instant)>>,
    /// Tasks whose PR a run just pushed to: the host's next `mergeable = false` for each may be a verdict on the head
    /// before the push, so it is read as not yet worked out, once (TRAIL-241).
    fresh_pushes: Mutex<HashSet<TaskKey>>,
    /// The latest reading of each subscription usage window: percent used, and when it resets (unix seconds).
    quota: Mutex<HashMap<String, (u32, i64)>>,
    /// Tasks already reported as ungraded, so a poll doesn't say it again.
    ungraded: Mutex<HashSet<TaskKey>>,
    /// The reset a pause was last logged for, so a pause is logged once rather than on every poll.
    pause_logged: Mutex<Option<i64>>,
    /// Held while a worker picks an idle pooled sandbox (or a name for a new one) and labels it busy, so runs
    /// starting together never take the same sandbox.
    pool_pick: tokio::sync::Mutex<()>,
}

/// A pooled sandbox that keeps failing to release. Kept in memory only, so a restart counts again from zero.
struct Trouble {
    failures: u32,
    since: Instant,
    next_try: Instant,
    /// Given up on: out of the pool and the sandbox cap, even if labelling it broken failed too.
    abandoned: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The PR is open and the agent review is recorded; a human merges.
    Reviewed { pr: PrRef, verdict: Verdict },
    /// The PR is open but the review didn't finish; the reconciler retries it.
    AwaitingReview { pr: PrRef },
    /// The task is parked for a human; its sandbox is kept for resuming.
    Parked { reason: String },
    /// The attempt failed and the task went back to the queue.
    Failed { reason: String },
    /// The task's PR had already merged or closed, so no run started: a merge finished the task, a close parked it.
    Settled { reason: String },
}

/// How a metered run gets going: a new one, or one the daemon already has under way.
enum Begin {
    Start(Box<RunRequest>),
    Attach(String),
}

/// What reconcile found to carry on with, for the caller to run beside whatever else it does.
#[derive(Default)]
struct Due {
    /// Tasks whose PR is due a review.
    reviews: Vec<Task>,
    /// Worker runs still going in their sandboxes, to follow again.
    runs: Vec<(Task, Sandbox)>,
}

/// What one sandbox run came to, before the lifecycle acts on it.
enum Run {
    Report(RunReport, Usd),
    Breach(String, Usd),
}

#[derive(Debug, Default, PartialEq)]
pub struct Reconciled {
    pub destroyed: Vec<String>,
    /// Pooled sandboxes an interrupted run left busy, returned to idle.
    pub released: Vec<String>,
    /// Tasks whose run was still going in its sandbox, followed again instead of requeued (TRAIL-137).
    pub reattached: Vec<String>,
    pub requeued: Vec<String>,
    pub reviewed: Vec<String>,
    /// Tasks whose review asked for changes but stayed in review, now reopened for a rework or parked.
    pub reworked: Vec<String>,
    /// In-review tasks whose PR no longer merges cleanly, now reopened to merge the base branch in or parked.
    pub conflicted: Vec<String>,
    /// In-review tasks whose PR was merged, now done.
    pub merged: Vec<String>,
    /// In-review tasks whose PR was closed without merging, now parked for a human.
    pub closed: Vec<String>,
    /// Pooled sandboxes that kept failing to release, now out of the pool.
    pub abandoned: Vec<String>,
}

/// Who merged a PR that is found merged: the merger when it recorded the intent before merging (TRAIL-289), else a person.
fn merged_by(task: &Task) -> MergedBy {
    if task.run.merging.as_deref().is_some_and(|h| !h.is_empty()) { MergedBy::Merger } else { MergedBy::Human }
}

/// What the merger says on the PR about its decision, e.g. `Grade 1: checks, CI and review passed; would merge
/// (shadow).` or `Grade 3 (Cargo.lock changed): waiting for a person.` `merging` is whether the merger is on and will
/// try to merge now; the message then says the merge happened, so it is only posted once it has.
fn decision_message(grade: &PrGrade, d: &MergeDecision, auto_merge: bool, merging: bool) -> String {
    fn join(items: Vec<String>) -> String {
        match items.as_slice() {
            [] => String::new(),
            [one] => one.clone(),
            [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
        }
    }
    let head = grade.describe();
    if !d.waiting.is_empty() {
        let mut parts = Vec::new();
        let mut waiting = Vec::new();
        for gate in &d.waiting {
            match gate.as_str() {
                "ci" if !d.failing.is_empty() => parts.push(format!("CI failed ({})", d.failing.join(", "))),
                "ci" => waiting.push("CI".to_string()),
                "agent_review" => waiting.push("review".into()),
                "human" => waiting.push("a person".into()),
                other => waiting.push(other.to_string()),
            }
        }
        if !waiting.is_empty() {
            parts.push(format!("waiting for {}", join(waiting)));
        }
        return format!("{head}: {}.", parts.join("; "));
    }
    let passed = d.passed.iter().map(|g| match g.as_str() {
        "ci" => "CI".to_string(),
        "agent_review" => "review".into(),
        "human" => "a person's approval".into(),
        other => other.to_string(),
    });
    let passed = join(passed.collect());
    match (&d.problem, d.would_merge, auto_merge) {
        (Some(problem), ..) => format!("{head}: {passed} passed, but {problem}."),
        _ if merging => format!("{head}: {passed} passed; merged."),
        (None, true, _) => format!("{head}: {passed} passed; would merge (shadow)."),
        (None, false, true) => format!("{head}: {passed} passed; a person merges."),
        (None, false, false) => format!("{head}: {passed} passed; not auto-merged at this grade, so a person merges."),
    }
}

/// A worker run's name: the attempt's, with the count of retries that gave their attempt back.
fn worker_name(task: &TaskKey, attempt: u32, retries: u32) -> String {
    match retries {
        0 => run_name(task, attempt),
        n => format!("{}-{n}", run_name(task, attempt)),
    }
}

/// A reviewer's name: the round's, with the count of infrastructure retries so a retried round gets a fresh one.
fn reviewer_name(task: &TaskKey, attempt: u32, round: u32, retries: u32) -> String {
    match retries {
        0 => review_name(task, attempt, round),
        n => format!("{}-{n}", review_name(task, attempt, round)),
    }
}

impl Factory {
    pub fn new(
        config: FactoryConfig,
        compute: Arc<dyn Compute>,
        tracker: Arc<dyn Tracker>,
        provider: Arc<dyn ModelProvider>,
        daemon: Arc<dyn Daemon>,
    ) -> Factory {
        let merge_mode = Mutex::new(config.merge.mode);
        Factory {
            config,
            compute,
            tracker,
            provider,
            daemon,
            codehost: None,
            merge_mode_source: None,
            merge_mode,
            server_check: None,
            server_down: Mutex::new(false),
            troubled: Mutex::default(),
            setbacks: Mutex::default(),
            fresh_pushes: Mutex::default(),
            quota: Mutex::default(),
            ungraded: Mutex::default(),
            pause_logged: Mutex::default(),
            pool_pick: tokio::sync::Mutex::default(),
        }
    }

    /// Re-reads the merge mode from `source` before each merge decision, so switching it (to `off` as the quick stop)
    /// takes effect on the next pass. A failed read keeps the last mode read and is logged.
    pub fn with_merge_mode_source(mut self, source: MergeModeSource) -> Factory {
        self.merge_mode_source = Some(source);
        self
    }

    /// Checks the server is reachable before starting runs, and starts none while it isn't.
    pub fn with_server_check(mut self, check: ServerCheck) -> Factory {
        self.server_check = Some(check);
        self
    }

    /// Whether the server the daemons need is up. While it isn't, the outage is logged as an error when it starts
    /// and again when it ends, and no new runs start (running ones carry on).
    async fn server_up(&self) -> bool {
        let Some(check) = &self.server_check else { return true };
        let result = check().await;
        let mut down = self.server_down.lock().unwrap();
        match result {
            Ok(()) => {
                if std::mem::take(&mut *down) {
                    tracing::info!("the Trailhead server is reachable again; starting runs again");
                }
                true
            }
            Err(why) => {
                if !std::mem::replace(&mut *down, true) {
                    tracing::error!(%why, "the Trailhead server is down; starting no new runs until it is back (start it with `trailhead server start`)");
                }
                false
            }
        }
    }

    /// The merge mode now in force.
    fn merge_mode(&self) -> MergeMode {
        let mut mode = self.merge_mode.lock().unwrap();
        if let Some(source) = &self.merge_mode_source {
            match source() {
                Ok(read) => {
                    if read != *mode {
                        tracing::info!(from = ?*mode, to = ?read, "merge mode changed");
                    }
                    *mode = read;
                }
                Err(why) => tracing::warn!(%why, mode = ?*mode, "can't re-read the merge mode; keeping the last one"),
            }
        }
        *mode
    }

    /// Lets reconcile check each in-review task's PR (`docs/factory/operating.md`).
    pub fn with_codehost(mut self, codehost: Arc<dyn CodeHost>) -> Factory {
        self.codehost = Some(codehost);
        self
    }

    fn project(&self) -> &str {
        &self.config.repo.project
    }

    fn queue(&self) -> Option<TaskKey> {
        self.config.repo.queue.clone().map(TaskKey)
    }

    fn base_labels(&self) -> Labels {
        Labels::from([
            (label::FACTORY.into(), FACTORY_LABEL.into()),
            (label::REPO.into(), self.config.repo.repo().slug()),
        ])
    }

    fn agent_egress(&self) -> Egress {
        let iroh = IROH_DOMAINS.iter().map(|d| d.to_string());
        Egress { domains: iroh.chain(self.config.egress.always.iter().cloned()).collect() }
    }

    /// Install-phase hosts the agent phase drops, which the daemon confirms are unreachable before the agent starts.
    /// Wildcards can't be looked up, so they're left out.
    fn blocked_hosts(&self) -> Vec<String> {
        let agent = self.agent_egress().domains;
        let install = self.install_egress().domains;
        install.into_iter().filter(|d| !d.contains('*') && !agent.contains(d)).collect()
    }

    /// The agent phase's named hosts (wildcards can't be looked up), which the daemon keeps resolved.
    fn allowed_hosts(&self) -> Vec<String> {
        self.agent_egress().domains.into_iter().filter(|d| !d.contains('*')).collect()
    }

    fn install_egress(&self) -> Egress {
        let mut egress = self.agent_egress();
        let extra = self.config.egress.install.iter().cloned().chain(CLONE_DOMAINS.iter().map(|d| d.to_string()));
        egress.domains.extend(extra);
        egress
    }

    fn branch(task: &Task) -> String {
        format!("factory/{}", task.key.0.to_ascii_lowercase())
    }

    /// The control plane's main loop: reconcile, then keep up to the repo's cap of tasks running, checking for
    /// ready work when `wake` fires (on tracker events) and every `poll`. On `shutdown` it stops starting runs
    /// and abandons the ones in flight; the next boot's reconcile requeues them.
    pub async fn run(
        self: Arc<Self>,
        wake: Arc<Notify>,
        poll: Duration,
        shutdown: impl Future<Output = ()>,
    ) -> Result<()> {
        self.run_draining(wake, poll, std::future::pending(), shutdown).await
    }

    /// [`Factory::run`] with a first, gentler stop: once `drain` fires no run or review is started (reconcile keeps
    /// putting things right), the runs in flight finish as usual, and the loop returns when none are left. `shutdown`
    /// still cancels them, drain or not.
    pub async fn run_draining(
        self: Arc<Self>,
        wake: Arc<Notify>,
        poll: Duration,
        drain: impl Future<Output = ()>,
        shutdown: impl Future<Output = ()>,
    ) -> Result<()> {
        tokio::pin!(shutdown);
        tokio::pin!(drain);
        let mut draining = false;
        let mut running: JoinSet<Result<Outcome>> = JoinSet::new();
        let mut keys: HashMap<task::Id, TaskKey> = HashMap::new();
        // Reviews reconcile found due, running beside the worker runs without taking a worker's place in the cap.
        let mut reviews: HashSet<task::Id> = HashSet::new();
        // A failed boot reconcile (the compute API down, say) is retried in the loop rather than ending the process.
        let mut dirty = true;
        loop {
            // Shutdown is otherwise only noticed while idle; a stop during a long reconcile mustn't start new work.
            if (&mut shutdown).now_or_never().is_some() {
                break;
            }
            if draining && running.is_empty() {
                tracing::info!("drained; no runs left");
                break;
            }
            if dirty {
                dirty = false;
                match self.reconcile_deferring(&keys.values().cloned().collect()).await {
                    Ok((done, due)) => {
                        if done != Reconciled::default() || !due.reviews.is_empty() {
                            let reviewing: Vec<&str> = due.reviews.iter().map(|t| t.key.0.as_str()).collect();
                            tracing::info!(?done, ?reviewing, "reconciled");
                        }
                        // Due reviews are new work, picked up again by the next start. Due runs are runs the daemon is
                        // still working on, so a drain keeps following them and waits for them too.
                        let due = if draining { Due { reviews: Vec::new(), ..due } } else { due };
                        for (task, sandbox) in due.runs {
                            let key = task.key.clone();
                            let factory = self.clone();
                            let handle = running.spawn(async move { factory.resume_run(&task, &sandbox).await });
                            keys.insert(handle.id(), key);
                        }
                        // A review is a session on the same subscription window, so a pause holds them back too; the
                        // next reconcile finds them due again (the resume below triggers one).
                        let reviews_due = if self.usage_paused() { Vec::new() } else { due.reviews };
                        for task in reviews_due {
                            let key = task.key.clone();
                            let factory = self.clone();
                            let handle = running.spawn(async move { factory.resume_review(&task).await });
                            keys.insert(handle.id(), key);
                            reviews.insert(handle.id());
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "reconcile failed"),
                }
                if (&mut shutdown).now_or_never().is_some() {
                    break;
                }
            }
            if !draining && let Err(e) = self.start_ready(&mut running, &mut keys, reviews.len()).await {
                tracing::warn!(error = %e, "couldn't start ready tasks");
            }
            let resume = self.paused_until().map(|(_, _, resets_at)| until(resets_at));
            tokio::select! {
                _ = &mut shutdown => break,
                _ = &mut drain, if !draining => {
                    draining = true;
                    let mut flying: Vec<&str> = keys.values().map(|k| k.0.as_str()).collect();
                    flying.sort();
                    tracing::info!(
                        "draining: {} runs in flight ({}); Ctrl-C again to cancel them",
                        flying.len(),
                        flying.join(", ")
                    );
                }
                _ = wake.notified() => {}
                _ = tokio::time::sleep(resume.unwrap_or_default()), if resume.is_some() => dirty = true,
                _ = tokio::time::sleep(poll) => dirty = true,
                Some(joined) = running.join_next_with_id() => match joined {
                    Ok((id, Ok(outcome))) => {
                        reviews.remove(&id);
                        let key = keys.remove(&id);
                        tracing::info!(task = ?key, ?outcome, "run finished");
                    }
                    Ok((id, Err(e))) => {
                        reviews.remove(&id);
                        let key = keys.remove(&id);
                        tracing::warn!(task = ?key, error = %e, "run failed; reconciling");
                        dirty = true;
                    }
                    Err(e) => {
                        reviews.remove(&e.id());
                        let key = keys.remove(&e.id());
                        tracing::error!(task = ?key, error = %e, "run panicked; reconciling");
                        dirty = true;
                    }
                },
            }
        }
        // Cancel rather than just drop the runs: a daemon that loses its stream keeps the run going for a while in
        // case the control plane reattaches, and a stop on purpose mustn't leave it working unobserved (TRAIL-142).
        if !running.is_empty() {
            tracing::info!(runs = running.len(), "cancelling runs in flight");
            self.daemon.shutdown().await;
            let drained = tokio::time::timeout(SHUTDOWN_GRACE, async { while running.join_next().await.is_some() {} });
            if drained.await.is_err() {
                tracing::warn!(runs = running.len(), "runs didn't stop in time; abandoning them");
            }
        }
        running.shutdown().await;
        Ok(())
    }

    async fn start_ready(
        self: &Arc<Self>,
        running: &mut JoinSet<Result<Outcome>>,
        keys: &mut HashMap<task::Id, TaskKey>,
        reviewing: usize,
    ) -> Result<()> {
        if self.usage_paused() || !self.server_up().await {
            return Ok(());
        }
        let busy: HashSet<TaskKey> = keys.values().cloned().collect();
        let others = self
            .occupied_after_release(&busy)
            .await?
            .into_iter()
            .filter(|s| s.labels.get(label::TASK).is_none_or(|t| !busy.contains(&TaskKey(t.clone()))))
            .count();
        let room = self.config.repo.max_sandboxes.saturating_sub(keys.len() - reviewing + others);
        if room == 0 {
            return Ok(());
        }
        let ready = self.ready_graded(room).await?;
        for task in ready.into_iter().filter(|t| !busy.contains(&t.key) && self.setback_over(&t.key)) {
            let key = task.key.clone();
            let factory = self.clone();
            let handle = running.spawn(async move { factory.run_task(&task).await });
            keys.insert(handle.id(), key);
        }
        Ok(())
    }

    /// Up to `room` ready tasks that are the factory's work. Under a queue that means graded ones: grading a task is
    /// what queues it. An ungraded one is skipped and reported once, in the log and on the task, rather than on every
    /// poll (`docs/factory/review.md#grades`).
    async fn ready_graded(&self, room: usize) -> Result<Vec<Task>> {
        let queue = self.queue();
        if queue.is_none() {
            return self.tracker.ready(self.project(), None, room).await;
        }
        // Ungraded tasks mustn't crowd graded ones out of the limit.
        let ready = self.tracker.ready(self.project(), queue.as_ref(), usize::MAX).await?;
        let (graded, ungraded): (Vec<Task>, Vec<Task>) = ready.into_iter().partition(|t| t.grade.is_some());
        for task in ungraded {
            self.report_ungraded(&task).await;
        }
        for task in &graded {
            self.clear_ungraded(task).await;
        }
        Ok(graded.into_iter().take(room).collect())
    }

    /// A task that has been graded is no longer ungraded, so it's told again if its grade is ever cleared.
    async fn clear_ungraded(&self, task: &Task) {
        let was_reported = self.ungraded.lock().unwrap().remove(&task.key);
        if task.run.ungraded != Some(true) {
            return;
        }
        let update = RunUpdate { ungraded: Some(false), ..RunUpdate::default() };
        if let Err(e) = self.tracker.report(&task.key, update).await {
            // Keep the key out of the set only if nothing was cleared, so a failed write is retried next poll.
            if was_reported {
                self.ungraded.lock().unwrap().insert(task.key.clone());
            }
            tracing::warn!(task = %task.key.0, error = %e, "couldn't clear the task's ungraded flag");
        }
    }

    async fn report_ungraded(&self, task: &Task) {
        if task.run.ungraded == Some(true) || !self.ungraded.lock().unwrap().insert(task.key.clone()) {
            return;
        }
        tracing::info!(task = %task.key.0, "no grade; skipping it until a human sets one");
        let told = async {
            self.tracker.note(&task.key, "no grade; set one to queue it").await?;
            self.tracker.report(&task.key, RunUpdate { ungraded: Some(true), ..RunUpdate::default() }).await
        };
        if let Err(e) = told.await {
            tracing::warn!(task = %task.key.0, error = %e, "couldn't note that the task has no grade");
        }
    }

    /// The worker sandboxes that take a place in the cap: any with a worker role, and busy pooled ones, which
    /// include a parked task's whose kill switch took its role away. Abandoned ones don't count (TRAIL-135).
    async fn occupied(&self) -> Result<Vec<Sandbox>> {
        let mut workers = self.base_labels();
        workers.insert(label::ROLE.into(), Role::Worker.as_str().into());
        let mut busy = self.pool_labels();
        busy.insert(label::STATE.into(), label::BUSY.into());
        let mut out: Vec<Sandbox> = Vec::new();
        for s in self.compute.list(&workers).await?.into_iter().chain(self.compute.list(&busy).await?) {
            if !self.abandoned(&s.name) && !out.iter().any(|o| o.name == s.name) {
                out.push(s);
            }
        }
        Ok(out)
    }

    /// [`Factory::occupied`] once the sandboxes held for tasks a human has reopened, cancelled or finished are
    /// released. A parked task keeps its sandbox, so without this the place stays taken until the next reconcile
    /// (TRAIL-108, TRAIL-257). A reopened task's next attempt starts from a fresh checkout with the answer in its prompt.
    async fn occupied_after_release(&self, running: &HashSet<TaskKey>) -> Result<Vec<Sandbox>> {
        let held = self.occupied().await?;
        let candidates: Vec<&Sandbox> = held
            .iter()
            .filter(|s| s.labels.contains_key(label::POOL))
            .filter(|s| s.labels.get(label::TASK).is_some_and(|t| !running.contains(&TaskKey(t.clone()))))
            .collect();
        if candidates.is_empty() {
            return Ok(held);
        }
        let mut released = Vec::new();
        for sandbox in candidates {
            let Some(key) = sandbox.labels.get(label::TASK) else { continue };
            let task = match self.tracker.show(&TaskKey(key.clone())).await {
                Ok(task) if matches!(task.status, Status::Open | Status::Cancelled | Status::Done) => task,
                Ok(_) => continue,
                Err(e) => {
                    tracing::warn!(task = %key, error = %e, "couldn't look up a parked task's status");
                    continue;
                }
            };
            let mut done = Reconciled::default();
            self.reconcile_pooled(sandbox, &mut done).await;
            if !done.released.is_empty() || !done.abandoned.is_empty() || !done.destroyed.is_empty() {
                tracing::info!(task = %task.key.0, sandbox = %sandbox.name, "released the sandbox of a task no longer parked");
                if let Err(e) = self.reconcile_task(&task, &mut done).await {
                    tracing::warn!(task = %task.key.0, error = %e, "couldn't clean up a released task; reconcile will");
                }
                released.push(sandbox.name.clone());
            }
        }
        if released.is_empty() {
            return Ok(held);
        }
        Ok(held.into_iter().filter(|s| !released.contains(&s.name)).collect())
    }

    /// Starts runs for ready tasks, up to the repo's sandbox cap, one after another. For tests and one-shot use.
    pub async fn tick(&self) -> Result<Vec<(Task, Outcome)>> {
        if self.usage_paused() || !self.server_up().await {
            return Ok(Vec::new());
        }
        let room =
            self.config.repo.max_sandboxes.saturating_sub(self.occupied_after_release(&HashSet::new()).await?.len());
        if room == 0 {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let ready = self.ready_graded(room).await?;
        for task in ready.into_iter().filter(|t| self.setback_over(&t.key)) {
            let outcome = self.run_task(&task).await?;
            out.push((task, outcome));
        }
        Ok(out)
    }

    /// One attempt at a task: a fresh sandbox, the worker, then the reviewer.
    pub async fn run_task(&self, task: &Task) -> Result<Outcome> {
        // A rework on a PR that has merged or closed since the task was queued would push to a branch nobody wants
        // (TRAIL-304). The sub-agent isn't made yet, so the factory's own identity holds the task while it's settled.
        if let (Some(pr), Some(codehost)) = (&task.run.pr, &self.codehost) {
            match codehost.pr_state(pr).await? {
                PrState::Merged { commit } => {
                    self.tracker.claim(&task.key).await?;
                    let by = merged_by(task);
                    self.close_merged(task, Merged { by, pr: pr.clone(), commit }).await?;
                    return Ok(Outcome::Settled { reason: format!("{} was merged", pr.url) });
                }
                PrState::Closed => {
                    self.park_closed(task, pr).await?;
                    return Ok(Outcome::Settled { reason: format!("{} was closed", pr.url) });
                }
                PrState::Open { .. } => {}
            }
        }
        let attempt = task.run.attempt.unwrap_or(0) + 1;
        let profile = self.config.worker().clone();
        // A run that failed before its agent started gave its attempt back but revoked its sub-agent, which can't be
        // undone, so a retry's name carries the count of those to stay unique.
        let retries = task.run.retries.unwrap_or(0);
        let name = worker_name(&task.key, attempt, retries);
        let credential = match self.tracker.create_sub_agent(&name, self.project(), token_ttl(&profile)).await {
            Ok(credential) => credential,
            // The name was used by a run whose give-back didn't land; skip past it rather than retry it forever.
            Err(e @ Error::Forbidden(_)) => {
                self.set_back(&task.key, 1);
                let update = RunUpdate { retries: Some(retries + 1), ..RunUpdate::default() };
                if let Err(e) = self.tracker.report(&task.key, update).await {
                    tracing::warn!(task = %task.key.0, error = %e, "couldn't skip a used run name");
                }
                return Err(e);
            }
            // No sub-agent, so nothing holds the claim: back off and count it, as the factory's own identity. Revoking
            // the sub-agent that was never created fails, and that warning is expected.
            Err(e) => {
                // Parking and releasing need the claim, so take it as the factory's own identity first.
                if let Err(claim) = self.tracker.claim(&task.key).await {
                    tracing::warn!(task = %task.key.0, error = %claim, "couldn't claim the task to back it off");
                }
                return Err(self.infra_failed(task, &self.tracker, &name, e, task.run.infra_failures, None).await);
            }
        };
        let sub = self.tracker.sub_agent(&name);
        let infra_prior = task.run.infra_failures;
        // Claiming as the sub-agent before creating the sandbox keeps two control planes from racing for one task.
        if let Err(e) = sub.claim(&task.key).await {
            return Err(self.infra_failed(task, &sub, &name, e, infra_prior, None).await);
        }
        let started = RunUpdate {
            attempt: Some(attempt),
            // A task parked for giving up and reopened starts its count again.
            attempt_base: task.run.gave_up.filter(|g| *g > task.run.attempt_base.unwrap_or(0)),
            sandbox: Some(name.clone()),
            profile: Some(self.config.repo.worker.clone()),
            started: Some(unix_now()),
            ..RunUpdate::default()
        };
        if let Err(e) = sub.report(&task.key, started).await {
            return Err(self.infra_failed(task, &sub, &name, e, infra_prior, None).await);
        }
        tracing::info!(task = %task.key.0, run = %name, "preparing a worker sandbox");
        // A task with a PR is a rework: the checkout starts from the PR branch's head, and the worker is told what the
        // review asked for.
        let rework = task.run.pr.is_some();
        let prepared = async {
            let context = self.tracker.human_context(&task.key).await?;
            // A review that never went through request_rework (an older one, or a task reopened by hand) left no
            // changes_requested; the review comment is the feedback then (TRAIL-218).
            let feedback = match task.run.pending_changes().filter(|_| rework) {
                Some(c) => Some(Feedback { body: format!("Review by {}:\n\n{}", c.reviewer, c.body) }),
                None if rework => match self.tracker.last_review(&task.key).await? {
                    Some(v) if v.kind == VerdictKind::RequestChanges => {
                        Some(Feedback { body: format!("Review by {}:\n\n{}", v.reviewer, v.body) })
                    }
                    _ => None,
                },
                None => None,
            };
            let merge_base = task.run.pending_changes().is_some_and(|c| c.conflict);
            let sandbox = self.acquire_worker(task, rework.then(|| Self::branch(task)), merge_base).await?;
            sub.report(&task.key, RunUpdate { host: Some(sandbox.name.clone()), ..RunUpdate::default() }).await?;
            Ok::<_, Error>((sandbox, context, feedback))
        };
        let (sandbox, context, feedback) = match prepared.await {
            Ok(prepared) => prepared,
            Err(e) => return Err(self.infra_failed(task, &sub, &name, e, infra_prior, None).await),
        };
        tracing::info!(task = %task.key.0, run = %name, sandbox = %sandbox.name, "starting worker");
        let req = RunRequest {
            run: name.clone(),
            role: Role::Worker,
            task: spec(task, context),
            repo: self.config.repo.repo(),
            base_branch: self.config.repo.base_branch.clone(),
            branch: Self::branch(task),
            grade: None,
            pr: task.run.pr.clone(),
            feedback,
            model: self.provider.resolve(profile.model),
            endpoint: self.provider.endpoint(),
            max_steps: profile.budget.steps,
            required_checks: self.config.required_checks(),
            worker_checks: Vec::new(),
            blocked_hosts: self.blocked_hosts(),
            allowed_hosts: self.allowed_hosts(),
            credential,
        };
        let prior = task.run.spend.unwrap_or_default();
        let run =
            self.run_metered(&sandbox, Begin::Start(Box::new(req)), &task.key, &profile, prior, Duration::ZERO).await;
        self.conclude(task, &sub, &sandbox, &name, attempt, run).await
    }

    /// Follows a worker run that was going when the last control plane stopped (TRAIL-137): the daemon replays the
    /// run's events, which are priced again on top of the spend of the runs before it, and the deadline counts from
    /// when the run started. What the run comes to is handled as if this process had started it.
    pub async fn resume_run(&self, task: &Task, sandbox: &Sandbox) -> Result<Outcome> {
        let name =
            task.run.sandbox.clone().ok_or_else(|| Error::Other(format!("{} has no run to resume", task.key.0)))?;
        let attempt = task.run.attempt.unwrap_or(1);
        let sub = self.tracker.sub_agent(&name);
        // The attempt this run took was already counted; if it's given back, it goes back to the number before it.
        let mut before = task.clone();
        before.run.attempt = Some(attempt.saturating_sub(1));
        let task = &before;
        let profile = self.config.worker().clone();
        let prior = task.run.spend.unwrap_or_default();
        let elapsed = task
            .run
            .started
            .map_or(Duration::ZERO, |t| Duration::from_secs(u64::try_from(unix_now() - t).unwrap_or(0)));
        tracing::info!(task = %task.key.0, run = %name, sandbox = %sandbox.name, ?elapsed, "reattaching to a run in flight");
        let run = self.run_metered(sandbox, Begin::Attach(name.clone()), &task.key, &profile, prior, elapsed).await;
        self.conclude(task, &sub, sandbox, &name, attempt, run).await
    }

    /// What a worker's run came to: the report acted on, or the failure given back to the queue.
    async fn conclude(
        &self,
        task: &Task,
        sub: &Arc<dyn Tracker>,
        sandbox: &Sandbox,
        name: &str,
        attempt: u32,
        run: Result<Run>,
    ) -> Result<Outcome> {
        let infra_prior = task.run.infra_failures;
        let run = match run {
            Ok(run) => run,
            Err(Error::Cancelled) => {
                // A stop on purpose cancelled the run; the next boot's reconcile requeues it without a strike.
                self.give_back_attempt(task, sub).await;
                return Err(Error::Cancelled);
            }
            // The daemon was lost without reattaching, or the like: the agent didn't give up, so the attempt is
            // given back (TRAIL-172).
            Err(e) => return Err(self.infra_failed(task, sub, name, e, infra_prior, None).await),
        };

        // From here the agent has ended the run, so its attempt counts. Errors go to reconcile as before, except after
        // the agent's own API trouble, which gives the attempt back (TRAIL-172). A breach is the agent's doing too.
        let (report, spend) = match run {
            Run::Breach(reason, spend) => {
                tracing::warn!(task = %task.key.0, %reason, "budget breached; parking");
                // The spend is recorded before any teardown. A record that fails doesn't skip the kill switch or the
                // park; its error is returned after them.
                let recorded = sub.report(&task.key, RunUpdate { spend: Some(spend), ..RunUpdate::default() }).await;
                if let Err(e) = &recorded {
                    tracing::warn!(task = %task.key.0, error = %e, "couldn't record the breach's spend; cutting off anyway");
                }
                if let Err(e) = self.compute.checkpoint(&sandbox.id, &format!("parked: {reason}")).await {
                    tracing::warn!(task = %task.key.0, sandbox = %sandbox.name, error = %e, "couldn't checkpoint the breached sandbox");
                }
                if let Err(e) = self.cut_off(sandbox).await {
                    // Reconcile leaves a parked task's sandbox alone, so a sandbox that couldn't be cut off is
                    // destroyed: that loses the suspended state, which beats leaving it reachable.
                    tracing::warn!(task = %task.key.0, sandbox = %sandbox.name, error = %e, "couldn't cut off the breached sandbox; destroying it");
                    self.daemon.forget(&sandbox.id).await;
                    if let Err(e) = self.compute.destroy(&sandbox.id).await {
                        tracing::warn!(task = %task.key.0, sandbox = %sandbox.name, error = %e, "couldn't destroy the breached sandbox; reconcile will");
                    }
                }
                let asked = sub
                    .ask(
                        &task.key,
                        &format!(
                            "Budget breached: {reason}. The sandbox {} is suspended; raise the budget or cancel.",
                            sandbox.name
                        ),
                    )
                    .await;
                recorded?;
                asked?;
                return Ok(Outcome::Parked { reason });
            }
            Run::Report(report, spend) => (report, spend),
        };
        // An agent that ends the run itself shows the infrastructure working again, unless what it ended with is its
        // API trouble.
        let api_trouble = matches!(&report.result, RunResult::Failed(reason) if is_transient_api_error(reason));
        if !api_trouble {
            self.setbacks.lock().unwrap().remove(&task.key);
        }
        let infra_failures = if api_trouble { None } else { task.run.infra_failures.filter(|n| *n > 0).map(|_| 0) };
        let update =
            RunUpdate { spend: Some(spend), concluded: Some(name.to_string()), infra_failures, ..RunUpdate::default() };
        match sub.report(&task.key, update).await {
            Err(e) if api_trouble => return Err(self.infra_failed(task, sub, name, e, infra_prior, None).await),
            reported => reported?,
        }

        match report.result {
            RunResult::PrOpened(pr) => {
                if task.run.pending_changes().is_some_and(|c| c.conflict) {
                    self.fresh_pushes.lock().unwrap().insert(task.key.clone());
                }
                // MVP: no suspend during review; requested changes reopen the task (`request_rework`) and a new run
                // starts from the PR branch.
                if let Err(e) = self.release_worker(sandbox).await {
                    tracing::warn!(task = %task.key.0, sandbox = %sandbox.name, error = %e, "couldn't release the worker's sandbox; reconcile will");
                }
                if let Err(e) = self.tracker.revoke_sub_agent(name).await {
                    tracing::warn!(task = %task.key.0, error = %e, "couldn't revoke the worker's sub-agent; its token will expire");
                }
                self.review(task, &pr, attempt, &report.checks).await
            }
            RunResult::NeedsInput(question) => Ok(Outcome::Parked { reason: question }),
            RunResult::Failed(reason) => {
                tracing::warn!(task = %task.key.0, %reason, "attempt failed");
                match self.release_worker(sandbox).await {
                    Err(e) if api_trouble => {
                        return Err(self.infra_failed(task, sub, name, e, infra_prior, None).await);
                    }
                    // Any other failure to release leaves the sandbox to reconcile; the park below still happens.
                    Err(e) => {
                        tracing::warn!(task = %task.key.0, sandbox = %sandbox.name, error = %e, "couldn't release the worker's sandbox; reconcile will");
                    }
                    Ok(()) => {}
                }
                if api_trouble {
                    // The agent's own API trouble says nothing about the task. The daemon released the task before
                    // this control plane could reclaim it; within one control plane the task stays in `busy`, so
                    // nothing else takes it in between. Claiming it again as the sub-agent gives the attempt back.
                    let error = match sub.claim(&task.key).await {
                        Ok(()) => Error::Other(reason.clone()),
                        Err(e) => e,
                    };
                    let restore = Some(task.run.failure.clone().unwrap_or_default());
                    self.infra_failed(task, sub, name, error, infra_prior, restore).await;
                    return Ok(Outcome::Failed { reason });
                }
                let max = self.config.repo.max_attempts;
                // Counted from the last review that asked for changes, so reworks get attempts of their own.
                let failed = attempt.saturating_sub(task.run.attempt_floor());
                if failed >= max {
                    // The daemon released the task; claiming it again as the sub-agent is how it gets parked.
                    // A task that can't be read still parks, with this attempt's reason alone.
                    let last = match self.tracker.show(&task.key).await {
                        Ok(shown) => shown.run.failure.filter(|f| !f.is_empty()),
                        Err(e) => {
                            tracing::warn!(task = %task.key.0, error = %e, "couldn't read the last failure; parking without it");
                            None
                        }
                    }
                    .unwrap_or_else(|| reason.clone());
                    sub.claim(&task.key).await?;
                    // Recorded before the question, so a task that is parked has the marker; reopening it is all a
                    // human has to do.
                    sub.report(&task.key, RunUpdate { gave_up: Some(attempt), ..RunUpdate::default() }).await?;
                    sub.ask(
                        &task.key,
                        &format!(
                            "Gave up after {failed} failed attempts (the limit is {max}). The last failure: \
                             {reason}\n\n{last}\n\nFix the cause, then reopen the task; the attempt count starts again from \
                             here. Or raise `max_attempts` to allow more tries."
                        ),
                    )
                    .await?;
                    self.tracker.revoke_sub_agent(name).await?;
                    return Ok(Outcome::Parked { reason });
                }
                self.tracker.revoke_sub_agent(name).await?;
                Ok(Outcome::Failed { reason })
            }
            RunResult::Reviewed(_) => Err(Error::Other("a worker returned a review".into())),
        }
    }

    /// A run that failed on the infrastructure, not the task (TRAIL-159, TRAIL-172): the task goes back to the queue
    /// without the attempt counting toward `max_attempts`, and waits out a backoff that grows with each such failure.
    /// At `max_infra_failures` in a row it's parked instead, saying so. The run's sub-agent must hold the claim. Returns the
    /// error to surface.
    async fn infra_failed(
        &self,
        task: &Task,
        sub: &Arc<dyn Tracker>,
        name: &str,
        error: Error,
        prior: Option<u32>,
        failure: Option<String>,
    ) -> Error {
        // The in-memory count too, so the backoff still grows when the tracker is what's failing and the persisted
        // count didn't get written.
        let held = self.setbacks.lock().unwrap().get(&task.key).map_or(0, |(n, _)| *n);
        let failures = prior.unwrap_or(0).max(held) + 1;
        let max = self.config.repo.max_infra_failures.max(1);
        let park = failures >= max;
        tracing::warn!(task = %task.key.0, %error, failures, max, "infrastructure failure; the attempt doesn't count");
        self.set_back(&task.key, failures);
        // Before the release: the run's sub-agent holds the claim, so only it can report. A parked task starts its
        // count again when Lane reopens it.
        let update = RunUpdate {
            attempt: Some(task.run.attempt.unwrap_or(0)),
            retries: Some(task.run.retries.unwrap_or(0) + 1),
            infra_failures: Some(if park { 0 } else { failures }),
            // Set only when this run's agent wrote text that isn't about the task (its API trouble): the failure the
            // task had before the run is put back, so the next attempt is still told what failed.
            failure,
            ..RunUpdate::default()
        };
        if let Err(e) = sub.report(&task.key, update).await {
            tracing::warn!(task = %task.key.0, error = %e, "couldn't give the attempt back");
        }
        if park {
            let question = format!(
                "The infrastructure failed {failures} times in a row (the limit is {max}); it's not the task. The \
                 last error: {error}\n\nFix the cause, then reopen the task; raise `max_infra_failures` to allow \
                 more tries."
            );
            match sub.ask(&task.key, &question).await {
                // Reopened by Lane, it starts again with no wait, as its count does.
                Ok(()) => {
                    self.setbacks.lock().unwrap().remove(&task.key);
                }
                Err(e) => {
                    tracing::warn!(task = %task.key.0, error = %e, "couldn't park the task; reconcile will requeue it");
                    // Not parked, so the count mustn't start again: the next failure tries to park once more.
                    let keep = RunUpdate { infra_failures: Some(failures), ..RunUpdate::default() };
                    if let Err(e) = sub.report(&task.key, keep).await {
                        tracing::warn!(task = %task.key.0, error = %e, "couldn't keep the infrastructure failure count");
                    }
                }
            }
        } else if let Err(e) = sub.release(&task.key).await {
            tracing::warn!(task = %task.key.0, error = %e, "couldn't release the task; reconcile will");
        }
        if let Err(e) = self.tracker.revoke_sub_agent(name).await {
            tracing::warn!(task = %task.key.0, error = %e, "couldn't revoke the run's sub-agent; reconcile will");
        }
        error
    }

    /// Restores the attempt count the task had before this run took the next number, reporting as the run's
    /// sub-agent, which holds the claim.
    async fn give_back_attempt(&self, task: &Task, sub: &Arc<dyn Tracker>) {
        let update = RunUpdate {
            attempt: Some(task.run.attempt.unwrap_or(0)),
            retries: Some(task.run.retries.unwrap_or(0) + 1),
            ..RunUpdate::default()
        };
        if let Err(e) = sub.report(&task.key, update).await {
            tracing::warn!(task = %task.key.0, error = %e, "couldn't give the attempt back");
        }
    }

    /// Holds a task back for a backoff that grows with each infrastructure failure in a row.
    fn set_back(&self, key: &TaskKey, failures: u32) {
        let wait = backoff(failures, FIRST_INFRA_BACKOFF, MAX_INFRA_BACKOFF);
        self.setbacks.lock().unwrap().insert(key.clone(), (failures, Instant::now() + wait));
    }

    /// Whether a task that failed on the infrastructure has waited out its backoff.
    fn setback_over(&self, key: &TaskKey) -> bool {
        self.setbacks.lock().unwrap().get(key).is_none_or(|(_, next_try)| Instant::now() >= *next_try)
    }

    /// Runs the reviewer on an open PR in its own fresh sandbox.
    pub async fn review(&self, task: &Task, pr: &PrRef, attempt: u32, checks: &[CheckResult]) -> Result<Outcome> {
        let shown = self.tracker.show(&task.key).await?;
        // Graded before anything starts, so a code host that fails here leaves the review to be tried again whole.
        let grade = self.grade_pr(&shown, pr).await?;
        if let Some(grade) = &grade {
            self.record_grade(&shown, grade).await?;
        }
        // Read before anything is created, so a tracker that fails here holds no sub-agent or sandbox.
        let context = self.tracker.human_context(&task.key).await?;
        // The commit the review is of, so a verdict isn't counted for a head pushed after it.
        let head = match &self.codehost {
            Some(codehost) => Some(codehost.head(pr).await?),
            None => None,
        };
        let before = shown.run.review_rounds.unwrap_or(0);
        // The re-review after a conflict rework doesn't use a round (TRAIL-221); it does if it asks for changes.
        let after_conflict = shown.run.pending_changes().is_some_and(|c| c.conflict);
        let round = if after_conflict { before } else { before + 1 };
        let retries = shown.run.retries.unwrap_or(0);
        let infra_prior = shown.run.infra_failures;
        let name = reviewer_name(&task.key, attempt, before + 1, retries);
        self.tracker.report(&task.key, RunUpdate { review_rounds: Some(round), ..RunUpdate::default() }).await?;
        let profile = self.config.reviewer().clone();
        let credential = match self.tracker.create_sub_agent(&name, self.project(), token_ttl(&profile)).await {
            Ok(credential) => credential,
            Err(e) => return Err(self.review_infra_failed(task, &name, e, before, retries, infra_prior).await),
        };
        let sub = self.tracker.sub_agent(&name);
        let sandbox = match self.create_sandbox(&name, task, Role::Reviewer, Some(Self::branch(task))).await {
            Ok(sandbox) => sandbox,
            Err(e) => return Err(self.review_infra_failed(task, &name, e, before, retries, infra_prior).await),
        };
        tracing::info!(task = %task.key.0, sandbox = %name, "starting reviewer");
        let req = RunRequest {
            run: name.clone(),
            role: Role::Reviewer,
            task: spec(task, context),
            repo: self.config.repo.repo(),
            base_branch: self.config.repo.base_branch.clone(),
            branch: Self::branch(task),
            grade: grade.clone(),
            pr: Some(pr.clone()),
            feedback: None,
            model: self.provider.resolve(profile.model),
            endpoint: self.provider.endpoint(),
            max_steps: profile.budget.steps,
            required_checks: Vec::new(),
            worker_checks: checks.to_vec(),
            blocked_hosts: self.blocked_hosts(),
            allowed_hosts: self.allowed_hosts(),
            credential,
        };
        let prior = self.tracker.show(&task.key).await?.run.spend.unwrap_or_default();
        let run =
            self.run_metered(&sandbox, Begin::Start(Box::new(req)), &task.key, &profile, prior, Duration::ZERO).await;
        // A sandbox that won't go doesn't undo the review: the verdict and spend are still recorded, and reconcile
        // destroys the orphan.
        if let Err(e) = self.compute.destroy(&sandbox.id).await {
            tracing::warn!(task = %task.key.0, error = %e, "couldn't destroy the reviewer's sandbox; reconcile will");
        }
        let run = match run {
            Ok(run) => run,
            Err(e) => return Err(self.review_infra_failed(task, &name, e, before, retries, infra_prior).await),
        };
        // A request for changes is marked reviewed by `request_rework`, in the same write as the review it records, so
        // a review can't be counted as done without its rework being requested or the task parked.
        let reviewed_head_at_request = head.clone();
        let (spend, reviewed, reviewed_head) = match &run {
            Run::Report(RunReport { result: RunResult::Reviewed(v), .. }, s) => {
                (*s, (v.kind != VerdictKind::RequestChanges).then_some(attempt), head)
            }
            Run::Report(_, s) | Run::Breach(_, s) => (*s, None, None),
        };
        // A reviewer that finished (a verdict, or a breach that is the task's doing) ends the run of infrastructure failures.
        let finished =
            matches!(run, Run::Breach(..) | Run::Report(RunReport { result: RunResult::Reviewed(_), .. }, _));
        let infra_failures = if finished { infra_prior.filter(|n| *n > 0).map(|_| 0) } else { None };
        sub.report(
            &task.key,
            RunUpdate { spend: Some(spend), reviewed, reviewed_head, infra_failures, ..RunUpdate::default() },
        )
        .await?;
        self.tracker.revoke_sub_agent(&name).await?;
        match run {
            Run::Report(RunReport { result: RunResult::Reviewed(verdict), .. }, _) => {
                self.setbacks.lock().unwrap().remove(&task.key);
                if let (Some(raise), Some(mut grade)) = (&verdict.raise_grade, grade)
                    && grade.raise(raise.grade, &raise.reason)
                {
                    self.record_grade(&shown, &grade).await?;
                }
                if verdict.kind == VerdictKind::RequestChanges {
                    let requested = ChangesRequested {
                        reviewer: verdict.reviewer.clone(),
                        body: verdict.body.clone(),
                        attempt,
                        conflict: false,
                        head: reviewed_head_at_request,
                    };
                    let round = before + 1;
                    self.tracker
                        .report(&task.key, RunUpdate { review_rounds: Some(round), ..RunUpdate::default() })
                        .await?;
                    self.request_rework(task, requested, round).await?;
                } else if let Err(e) = self.evaluate_gates(&task.key).await {
                    tracing::warn!(task = %task.key.0, error = %e, "couldn't evaluate the gates; the next reconcile retries");
                }
                Ok(Outcome::Reviewed { pr: pr.clone(), verdict })
            }
            Run::Report(report, _) => {
                tracing::warn!(task = %task.key.0, result = ?report.result, "review didn't produce a verdict");
                // No verdict, no round: handled like an infrastructure failure, so it backs off and is capped
                // rather than being reviewed again on every pass (TRAIL-267).
                let error = Error::Other(format!("the reviewer ended without a verdict: {:?}", report.result));
                self.review_infra_failed(task, &name, error, before, retries, infra_prior).await;
                Ok(Outcome::AwaitingReview { pr: pr.clone() })
            }
            Run::Breach(reason, _) => {
                // A breach is the task's doing, like a worker's: the round counts, and the task parks for a human
                // rather than being reviewed again on the next reconcile (TRAIL-319). The reviewer's sandbox is
                // already destroyed, so there's nothing to suspend.
                self.setbacks.lock().unwrap().remove(&task.key);
                tracing::warn!(task = %task.key.0, %reason, "reviewer breached its budget; parking");
                self.tracker.reopen(&task.key).await?;
                self.tracker.claim(&task.key).await?;
                let question = format!(
                    "The reviewer breached its budget in review round {round}: {reason}. The PR may be too big to \
                     review within the reviewer's budget; split it or raise the reviewer budget, then reopen the task."
                );
                self.tracker.ask(&task.key, &question).await?;
                // A fresh count for when a human reopens the task, last like `request_rework`'s.
                let fresh = RunUpdate { review_rounds: Some(0), ..RunUpdate::default() };
                if let Err(e) = self.tracker.report(&task.key, fresh).await {
                    tracing::warn!(task = %task.key.0, error = %e, "couldn't reset the review count");
                }
                Ok(Outcome::Parked { reason })
            }
        }
    }

    /// The merger (`docs/factory/review.md#the-merger`): evaluates the grade's gates on the PR's current head and
    /// records the decision in the task's metadata. When they pass it moves a task in review to `approved`, unless the
    /// grade needs Lane, whose own approval is what passes that gate; a push after an approval sends the task back to
    /// review. In shadow mode it says what it would do on the PR whenever that changes. In `on` mode it also squash-merges
    /// an approved task whose grade auto-merges, or that Lane approved, and closes it as done.
    pub async fn evaluate_gates(&self, key: &TaskKey) -> Result<()> {
        let Some(codehost) = &self.codehost else { return Ok(()) };
        let task = self.tracker.show(key).await?;
        let Some(pr) = &task.run.pr else { return Ok(()) };
        if !matches!(task.status, Status::InReview | Status::Approved) {
            return Ok(());
        }
        let Some(grade) = self.grade_pr(&task, pr).await? else { return Ok(()) };
        self.record_grade(&task, &grade).await?;
        let ci = codehost.ci(pr).await?;
        let head = ci.head.clone();
        if let CiState::Failed { checks } = &ci.state {
            return self.rework_ci_failure(&task, pr, &head, checks).await;
        }
        let prev = task.run.merge.clone();
        let mut status = task.status;
        // The head each approval was given on: the control plane's own is marked before it moves the task, so an
        // approval without a mark is Lane's.
        let mut approved_head = prev.as_ref().and_then(|d| d.approved_head.clone());
        let mut human_head = prev.as_ref().and_then(|d| d.human_head.clone());
        if status == Status::InReview {
            // However it got back to review (a push, Lane, a rework), no earlier approval applies.
            (approved_head, human_head) = (None, None);
        } else {
            if approved_head.is_none() && human_head.is_none() {
                human_head = Some(head.clone());
            }
            if approved_head.as_ref().or(human_head.as_ref()) != Some(&head) {
                tracing::info!(task = %key.0, %head, "the PR has a new head; sending the task back to review");
                self.tracker.unapprove(key).await?;
                (status, approved_head, human_head) = (Status::InReview, None, None);
            }
        }
        let gate = self.config.gate(grade.grade);
        let merge_mode = self.merge_mode();
        let verdict_approves = match &task.run.reviewed_head {
            // A clean merge of the base branch the host made since the review keeps the verdict (TRAIL-260), but only
            // the verdict of this attempt: a rework (a conflict-resolving one included) takes a new attempt, and its
            // merge commit is work no one reviewed, whatever its parents.
            Some(reviewed)
                if *reviewed == head
                    || (task.run.reviewed.is_some()
                        && task.run.reviewed == task.run.attempt
                        && codehost.carries_verdict(pr, reviewed).await?) =>
            {
                self.tracker.last_review(key).await?.is_some_and(|v| v.kind == VerdictKind::Approve)
            }
            _ => false,
        };
        let (mut passed, mut waiting, mut failing) = (Vec::new(), Vec::new(), Vec::new());
        for required in &gate.require {
            let (name, ok) = match required.as_str() {
                "ci" => match &ci.state {
                    CiState::Passed => ("ci", true),
                    CiState::Pending => ("ci", false),
                    CiState::Failed { checks } => {
                        failing = checks.clone();
                        ("ci", false)
                    }
                },
                "agent_review" => ("agent_review", verdict_approves),
                "human" => ("human", status == Status::Approved && human_head.as_ref() == Some(&head)),
                "quorum" => ("quorum", false),
                // The PR exists because the worker's checks passed before it was opened.
                _ => ("checks", true),
            };
            let list = if ok { &mut passed } else { &mut waiting };
            if !list.iter().any(|n: &String| n == name) {
                list.push(name.to_string());
            }
        }
        let gates_pass = waiting.is_empty();
        let mut decision = MergeDecision {
            grade: grade.grade,
            head: head.clone(),
            passed,
            waiting,
            failing,
            would_merge: gates_pass && gate.auto_merge && merge_mode == MergeMode::Shadow,
            approved_head,
            human_head,
            problem: None,
            refused: None,
            message: String::new(),
        };
        // Whether a task can be approved by the control plane: the gates pass without Lane, who approves by moving it.
        if gates_pass && status == Status::InReview {
            decision.approved_head = Some(head.clone());
            // The mark goes down before the approval so it can't pass for Lane's. Not again while approving is known
            // to fail, which would write the task every pass.
            if prev.as_ref().is_none_or(|d| d.problem.is_none()) {
                self.tracker.report(key, RunUpdate { merge: Some(decision.clone()), ..RunUpdate::default() }).await?;
            }
            match self.tracker.approve(key).await {
                Ok(()) => {
                    tracing::info!(task = %key.0, %head, "gates passed; approved");
                    status = Status::Approved;
                }
                Err(Error::Forbidden(why)) => {
                    let problem = format!(
                        "the factory can't move the task to approved ({why}); a person has to list `{}` as a \
                         reviewer of project {}",
                        self.tracker.agent(),
                        self.project()
                    );
                    tracing::warn!(task = %key.0, %problem, "gates passed");
                    decision.approved_head = None;
                    decision.problem = Some(problem);
                }
                Err(e) => return Err(e),
            }
        }
        // Without the right to approve, the merger can't act, so it only says what it would do, as in shadow mode.
        let merging = merge_mode == MergeMode::On
            && gates_pass
            && decision.problem.is_none()
            && status == Status::Approved
            && (gate.auto_merge || decision.human_head.as_ref() == Some(&head));
        decision.message = decision_message(&grade, &decision, gate.auto_merge, merging);
        // A refusal stands for the head it was given on, until a merge succeeds or the head moves.
        if merging {
            decision.refused = prev.as_ref().filter(|d| d.head == head).and_then(|d| d.refused.clone());
        }
        // A merge that may not happen (the PR is behind, its head moved, the host refuses) isn't announced until it
        // has: the PR keeps saying what it said, and the next pass decides again.
        let merged_message = merging.then(|| {
            let said = prev.as_ref().map(|d| d.message.clone()).unwrap_or_default();
            std::mem::replace(&mut decision.message, said)
        });
        if prev.as_ref() != Some(&decision) {
            if !merging && merge_mode != MergeMode::Off && prev.as_ref().is_none_or(|d| d.message != decision.message) {
                codehost.comment(pr, &decision.message).await?;
            }
            if decision.problem.is_some() && prev.as_ref().is_none_or(|d| d.problem != decision.problem) {
                self.tracker.note(key, decision.problem.as_deref().unwrap_or_default()).await?;
            }
            self.tracker.report(key, RunUpdate { merge: Some(decision.clone()), ..RunUpdate::default() }).await?;
        }
        if let Some(message) = merged_message {
            self.merge(&task, pr, &decision, &message).await?;
        }
        Ok(())
    }

    /// Merges an approved PR whose decision says to: brings it up to date with the base branch first, and sends a
    /// conflict, or a host that refuses the merge, to a rework instead of forcing it. A merge closes the task the way
    /// a merge by Lane does. `merged_message` goes on the PR once the host has merged it.
    async fn merge(&self, task: &Task, pr: &PrRef, decision: &MergeDecision, merged_message: &str) -> Result<()> {
        let Some(codehost) = &self.codehost else { return Ok(()) };
        let attempt = task.run.attempt.unwrap_or(1);
        // Gates passed on this head; a push since then is looked at on the next pass.
        if codehost.ci(pr).await?.head != decision.head {
            return Ok(());
        }
        if codehost.behind(pr).await? {
            match codehost.update_branch(pr).await? {
                BranchUpdate::Conflict => self.rework_conflict(task, pr, attempt).await?,
                // CI starts over on the new head; the next pass looks at it again.
                BranchUpdate::Updated | BranchUpdate::UpToDate => {}
            }
            return Ok(());
        }
        // Recorded first: if closing the task fails after the merge, reconcile still knows the merger did it.
        self.tracker
            .report(&task.key, RunUpdate { merging: Some(decision.head.clone()), ..RunUpdate::default() })
            .await?;
        let outcome = codehost.merge(pr, &decision.head).await;
        // An error may follow a merge the host accepted, so it keeps the intent and reconcile's `pr_state` decides.
        if matches!(outcome, Ok(MergeOutcome::Conflict | MergeOutcome::HeadMoved | MergeOutcome::Refused(_))) {
            self.tracker.report(&task.key, RunUpdate { merging: Some(String::new()), ..RunUpdate::default() }).await?;
        }
        match outcome? {
            MergeOutcome::Merged => {
                if let Err(e) = codehost.comment(pr, merged_message).await {
                    tracing::warn!(task = %task.key.0, pr = %pr.url, error = %e, "couldn't comment that the PR merged");
                }
                let commit = match codehost.pr_state(pr).await? {
                    PrState::Merged { commit } => commit,
                    _ => None,
                };
                self.close_merged(task, Merged { by: MergedBy::Merger, pr: pr.clone(), commit }).await
            }
            MergeOutcome::Conflict => self.rework_conflict(task, pr, attempt).await,
            // Nothing to rework: the next pass looks at the new head, or tries again once the host allows it.
            MergeOutcome::HeadMoved => {
                tracing::info!(task = %task.key.0, pr = %pr.url, "the PR moved before the merge; looking again next pass");
                Ok(())
            }
            MergeOutcome::Refused(why) => {
                // Said once per reason, on the PR and the task; the merge is still tried on each pass, quietly, since
                // the host lifts a refusal (a check that passes) without anything else changing.
                if decision.refused.as_ref() == Some(&why) {
                    tracing::debug!(task = %task.key.0, pr = %pr.url, %why, "the code host still refuses the merge");
                    return Ok(());
                }
                tracing::warn!(task = %task.key.0, pr = %pr.url, %why, "the code host refused the merge; trying again each pass");
                let note = format!("The code host refused to merge: {why}. The merger tries again each pass.");
                let codehost = self.codehost.as_ref().expect("checked above");
                codehost.comment(pr, &note).await?;
                self.tracker.note(&task.key, &note).await?;
                let decision = MergeDecision { refused: Some(why), ..decision.clone() };
                self.tracker.report(&task.key, RunUpdate { merge: Some(decision), ..RunUpdate::default() }).await
            }
        }
    }

    /// The PR's grade (`docs/factory/review.md#grades`): the task's, raised by what its files match and its size.
    /// Without a code host only the task's own grade is known, and a task with none has no grade.
    async fn grade_pr(&self, task: &Task, pr: &PrRef) -> Result<Option<PrGrade>> {
        let Some(codehost) = &self.codehost else {
            return Ok(task.grade.map(|g| PrGrade { grade: g, task: Some(g), reasons: Vec::new() }));
        };
        let files = codehost.files(pr).await?;
        // A task still at the grade an earlier review raised it to keeps the grade it started at, and that reasons.
        let recorded = task.run.grade.as_ref().filter(|r| task.grade == Some(r.grade));
        let started = recorded.map_or(task.grade, |r| r.task);
        let mut grade = self.config.grades.grader().grade(started, &files);
        if let Some(r) = recorded.filter(|r| r.grade >= grade.grade) {
            grade = r.clone();
        }
        // A person may have raised the task since.
        if let Some(g) = task.grade.filter(|g| *g > grade.grade) {
            grade.grade = g;
            grade.reasons.push(format!("the task was raised to {g}"));
        }
        Ok(Some(grade))
    }

    /// Writes a PR's grade to the task's metadata and, when it is above the task's own, raises the task to it with a
    /// note saying what did. Agents can only raise a grade, and only one a human has set.
    async fn record_grade(&self, task: &Task, grade: &PrGrade) -> Result<()> {
        let current = self.tracker.show(&task.key).await?;
        if let Some(have) = current.grade.filter(|have| grade.grade > *have) {
            self.tracker.raise_grade(&task.key, grade.grade).await?;
            let why = grade.reasons.join("; ");
            let note = format!("Raised to grade {} (task was {have}): {why}", grade.grade);
            tracing::info!(task = %task.key.0, "{note}");
            self.tracker.note(&task.key, &note).await?;
        }
        if current.run.grade.as_ref() != Some(grade) {
            self.tracker.report(&task.key, RunUpdate { grade: Some(grade.clone()), ..RunUpdate::default() }).await?;
        }
        Ok(())
    }

    /// A reviewer that failed on the infrastructure (TRAIL-213): the round doesn't count toward `max_review_rounds`,
    /// and the task waits out the same growing backoff as a worker's infrastructure failures before reconcile
    /// reviews it again. At `max_infra_failures` in a row the task is parked instead, saying so. Returns the error to
    /// surface.
    async fn review_infra_failed(
        &self,
        task: &Task,
        name: &str,
        error: Error,
        rounds_before: u32,
        retries: u32,
        prior: Option<u32>,
    ) -> Error {
        // A deliberate stop gives the round back like a failure, but isn't one: no count, no backoff, no parking.
        let cancelled = matches!(error, Error::Cancelled);
        // The persisted count survives a restart; the in-memory one covers a tracker that failed to write it.
        let held = self.setbacks.lock().unwrap().get(&task.key).map_or(0, |(n, _)| *n);
        let failures = prior.unwrap_or(0).max(held) + 1;
        let max = self.config.repo.max_infra_failures.max(1);
        let park = !cancelled && failures >= max;
        if cancelled {
            tracing::info!(task = %task.key.0, "review cancelled; the round doesn't count");
        } else {
            tracing::warn!(task = %task.key.0, %error, failures, max, "reviewer failed on the infrastructure; the round doesn't count");
            self.set_back(&task.key, failures);
        }
        // The retry's sub-agent and sandbox names carry the retry count: this one's sub-agent is revoked for good.
        let give_back = RunUpdate {
            review_rounds: Some(rounds_before),
            retries: Some(retries + 1),
            infra_failures: (!cancelled).then_some(if park { 0 } else { failures }),
            ..RunUpdate::default()
        };
        if let Err(e) = self.tracker.report(&task.key, give_back).await {
            tracing::warn!(task = %task.key.0, error = %e, "couldn't give the review round back");
        }
        if let Err(e) = self.tracker.revoke_sub_agent(name).await {
            tracing::warn!(task = %task.key.0, error = %e, "couldn't revoke the reviewer's sub-agent; reconcile will");
        }
        if park {
            let question = format!(
                "The infrastructure failed {failures} times in a row while reviewing the PR (the limit is {max}); \
                 it's not the task. The last error: {error}\n\nFix the cause, then reopen the task; raise \
                 `max_infra_failures` to allow more tries."
            );
            let parked = async {
                self.tracker.reopen(&task.key).await?;
                self.tracker.claim(&task.key).await?;
                self.tracker.ask(&task.key, &question).await
            };
            match parked.await {
                Ok(()) => {
                    self.setbacks.lock().unwrap().remove(&task.key);
                }
                Err(e) => {
                    tracing::warn!(task = %task.key.0, error = %e, "couldn't park the task; the next reconcile tries again");
                    // Not parked, so the count mustn't start again: the next failure tries to park once more.
                    let keep = RunUpdate { infra_failures: Some(failures), ..RunUpdate::default() };
                    if let Err(e) = self.tracker.report(&task.key, keep).await {
                        tracing::warn!(task = %task.key.0, error = %e, "couldn't keep the infrastructure failure count");
                    }
                }
            }
        }
        error
    }

    /// A review asked for changes: the task goes back to the queue with the review in its metadata, so the next run
    /// starts a fresh sandbox from the PR branch and is told what to fix. The task is parked instead once `round`
    /// reviews have asked for changes, which is when a human should look. Reconcile finishes this if it's cut short.
    async fn request_rework(&self, task: &Task, requested: ChangesRequested, round: u32) -> Result<()> {
        // `round` counts what the request is: review rounds, or conflict reworks for a conflict (TRAIL-221).
        let max =
            if requested.conflict { self.config.repo.max_conflict_reworks } else { self.config.repo.max_review_rounds };
        let (reviewer, body, conflict) = (requested.reviewer.clone(), requested.body.clone(), requested.conflict);
        // Recorded before the task is reopened, so the run that picks it up can't miss the review.
        let update = RunUpdate {
            reviewed: Some(requested.attempt),
            attempt_base: Some(requested.attempt),
            changes_requested: Some(requested),
            ..RunUpdate::default()
        };
        self.tracker.report(&task.key, update).await?;
        if round >= max {
            tracing::warn!(task = %task.key.0, round, "the reviewer still wants changes; parking");
            self.tracker.reopen(&task.key).await?;
            self.tracker.claim(&task.key).await?;
            let question = if conflict && reviewer == CI_REVIEWER {
                format!(
                    "CI keeps failing on the PR: {round} conflict reworks so far (the limit is {max}). The last \
                     failure:\n\n{body}\n\nFix the PR or settle what's landing, then reopen the task; raise \
                     `max_conflict_reworks` to allow more."
                )
            } else if conflict {
                format!(
                    "The PR keeps conflicting with the base branch: {round} conflict reworks so far \
                     (the limit is {max}). Merge the base branch into the PR yourself or settle what's landing, then \
                     reopen the task; raise `max_conflict_reworks` to allow more."
                )
            } else {
                format!(
                    "The reviewer still wants changes after {round} review rounds (the limit is {max}). The last \
                     review, by {reviewer}:\n\n{body}\n\nFix the PR or settle the disagreement, then reopen the \
                     task; raise `max_review_rounds` to allow more rounds."
                )
            };
            self.tracker.ask(&task.key, &question).await?;
            // A fresh count for when a human reopens the task after sorting it out. Last, so a reconcile that finishes
            // a park cut short still sees the round that hit the limit.
            let fresh = RunUpdate { review_rounds: Some(0), conflict_reworks: Some(0), ..RunUpdate::default() };
            if let Err(e) = self.tracker.report(&task.key, fresh).await {
                tracing::warn!(task = %task.key.0, error = %e, "couldn't reset the review and conflict counts");
            }
            return Ok(());
        }
        tracing::info!(task = %task.key.0, round, "changes requested; reopening for a rework");
        self.tracker.reopen(&task.key).await
    }

    /// A sandbox for `task`, its checkout prepared on `branch` when given (a reviewer's is the PR's head).
    async fn create_sandbox(&self, name: &str, task: &Task, role: Role, branch: Option<String>) -> Result<Sandbox> {
        let mut labels = self.base_labels();
        labels.insert(label::TASK.into(), task.key.0.clone());
        labels.insert(label::ROLE.into(), role.as_str().into());
        // Install egress only while the checkout is prepared, then narrowed before any agent runs.
        let spec = SandboxSpec { name: name.to_string(), labels, egress: self.install_egress() };
        let sandbox = self.compute.create(&spec).await?;
        let ready = async {
            self.daemon.prepare(&sandbox.id, self.config.prepare_request(role, branch)).await?;
            self.compute.set_egress(&sandbox.id, &self.agent_egress()).await
        };
        if let Err(e) = ready.await {
            // Not handed out, so nothing else would clean it up before reconcile.
            if let Err(d) = self.compute.destroy(&sandbox.id).await {
                tracing::warn!(sandbox = %sandbox.name, error = %d, "couldn't destroy a sandbox that failed to prepare; reconcile will");
            }
            return Err(e);
        }
        Ok(sandbox)
    }

    fn pool_labels(&self) -> Labels {
        let mut labels = self.base_labels();
        labels.insert(label::POOL.into(), Role::Worker.as_str().into());
        labels
    }

    fn pooled(&self, state: &str, warm: Option<&str>, task: Option<&Task>) -> Labels {
        let mut labels = self.pool_labels();
        labels.insert(label::STATE.into(), state.into());
        if let Some(warm) = warm {
            labels.insert(label::WARM.into(), warm.into());
        }
        if let Some(task) = task {
            labels.insert(label::TASK.into(), task.key.0.clone());
            labels.insert(label::ROLE.into(), Role::Worker.as_str().into());
        }
        labels
    }

    /// A worker sandbox for `task` from the repo's pool (`docs/factory/sandbox.md`): an idle one restored to its warm
    /// checkpoint, or a new one when none is idle. The tick's room check keeps the pool within `max_sandboxes`.
    /// The prepared checkout becomes the new warm checkpoint, so the next task starts from it.
    async fn acquire_worker(&self, task: &Task, branch: Option<String>, merge_base: bool) -> Result<Sandbox> {
        let pick = self.pool_pick.lock().await;
        let idle = self.compute.list(&self.pooled(label::IDLE, None, None)).await?;
        let (sandbox, warm) = match idle.into_iter().find(|s| !self.abandoned(&s.name)) {
            Some(s) => {
                let warm = s.labels.get(label::WARM).cloned();
                (s, warm)
            }
            None => {
                let name = self.next_pool_name().await?;
                let spec = SandboxSpec {
                    name,
                    labels: self.pooled(label::BUSY, None, Some(task)),
                    egress: self.install_egress(),
                };
                (self.compute.create(&spec).await?, None)
            }
        };
        self.compute.set_labels(&sandbox.id, &self.pooled(label::BUSY, warm.as_deref(), Some(task))).await?;
        drop(pick);
        self.compute.set_egress(&sandbox.id, &self.install_egress()).await?;
        if let Some(cp) = &warm {
            self.compute.restore(&sandbox.id, &CheckpointId(cp.clone())).await?;
            self.daemon.forget(&sandbox.id).await;
        }
        let prepare = PrepareRequest { merge_base, ..self.config.prepare_request(Role::Worker, branch) };
        self.daemon.prepare(&sandbox.id, prepare).await?;
        let cp = self.compute.checkpoint(&sandbox.id, "warm").await?;
        let labels = self.pooled(label::BUSY, Some(&cp.0), Some(task));
        self.compute.set_labels(&sandbox.id, &labels).await?;
        self.compute.set_egress(&sandbox.id, &self.agent_egress()).await?;
        Ok(Sandbox { labels, ..sandbox })
    }

    /// Returns a worker sandbox to the pool: restored to its warm checkpoint, which wipes everything the task did,
    /// with no egress. One that never got warm, or can't be restored, is destroyed instead: a sandbox that may still
    /// hold the last task's leftovers is never handed to another task.
    async fn release_worker(&self, sandbox: &Sandbox) -> Result<()> {
        self.compute.set_egress(&sandbox.id, &Egress::none()).await?;
        let Some(warm) = sandbox.labels.get(label::WARM).cloned() else {
            return self.compute.destroy(&sandbox.id).await;
        };
        if let Err(e) = self.compute.restore(&sandbox.id, &CheckpointId(warm.clone())).await {
            tracing::warn!(sandbox = %sandbox.name, error = %e, "couldn't restore the warm checkpoint; destroying");
            self.daemon.forget(&sandbox.id).await;
            return self.compute.destroy(&sandbox.id).await;
        }
        self.daemon.forget(&sandbox.id).await;
        self.compute.set_labels(&sandbox.id, &self.pooled(label::IDLE, Some(&warm), None)).await
    }

    /// The first free `<repo>-w<n>` name.
    async fn next_pool_name(&self) -> Result<String> {
        let taken: HashSet<String> =
            self.compute.list(&self.pool_labels()).await?.into_iter().map(|s| s.name).collect();
        let repo = self.config.repo.name.to_ascii_lowercase();
        Ok((1..).map(|n| format!("{repo}-w{n}")).find(|n| !taken.contains(n)).expect("a free name"))
    }

    /// The kill switch: without its role label a sandbox loses connector access, and without egress it reaches nothing.
    async fn cut_off(&self, sandbox: &Sandbox) -> Result<()> {
        // Both halves run, egress first, so one failing doesn't skip the other.
        let mut labels = sandbox.labels.clone();
        labels.remove(label::ROLE);
        let egress = self.compute.set_egress(&sandbox.id, &Egress::none()).await;
        let relabelled = self.compute.set_labels(&sandbox.id, &labels).await;
        egress.and(relabelled)
    }

    /// Runs one step in a sandbox, pricing usage as it streams in and cancelling on a breach.
    /// `elapsed` is how long the run has already gone on, for one picked up again after a restart.
    async fn run_metered(
        &self,
        sandbox: &Sandbox,
        begin: Begin,
        task: &TaskKey,
        profile: &Profile,
        prior: Usd,
        elapsed: Duration,
    ) -> Result<Run> {
        let limit = profile.budget.usd();
        let max_steps = profile.budget.steps;
        let deadline =
            tokio::time::sleep(Duration::from_secs(u64::from(profile.budget.minutes) * 60).saturating_sub(elapsed));
        tokio::pin!(deadline);
        let key = task.0.clone();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let run = async {
            match begin {
                Begin::Start(req) => self.daemon.run(&sandbox.id, *req, tx).await,
                Begin::Attach(run) => self.daemon.attach(&sandbox.id, &run, tx).await,
            }
        };
        tokio::pin!(run);

        let mut spent = Usd::ZERO;
        let mut steps = 0;
        let mut breach: Option<String> = None;
        let stall_after = profile.stall_timeout();
        let mut stalled = false;
        let mut grace_fired = false;
        let mut on_event = |ev: RunEvent, spent: &mut Usd| -> Option<String> {
            match ev {
                RunEvent::Usage { model, usage } => *spent += self.provider.price(&model, &usage),
                RunEvent::StepStarted { step } => {
                    steps += 1;
                    tracing::info!(task = %key, step, spent = %(prior + *spent), "step");
                }
                RunEvent::ToolCall { name, detail } => {
                    tracing::info!(task = %key, tool = %name, detail = detail.as_deref().unwrap_or(""), "tool");
                }
                RunEvent::NeedsInput { question } => tracing::info!(task = %key, %question, "asked"),
                RunEvent::Quota { window, used_percent, resets_at } => {
                    tracing::info!(%window, used_percent, resets_in = ?until(resets_at), "subscription usage");
                    self.quota.lock().unwrap().insert(window, (used_percent, resets_at));
                }
                _ => {}
            }
            if *spent > limit {
                Some(format!("spent {spent} of a {limit} budget"))
            } else if steps > max_steps {
                Some(format!("took more than {max_steps} steps"))
            } else {
                None
            }
        };
        // Armed on a breach: a daemon that doesn't stop by then is cut off without waiting for it.
        let grace = tokio::time::sleep(Duration::MAX / 4);
        tokio::pin!(grace);
        let result = loop {
            let new_breach = tokio::select! {
                biased;
                Some(ev) = rx.recv() => match ev {
                    // The daemon says how long the agent has been quiet, so a reattach doesn't restart the clock and
                    // time spent disconnected isn't counted (no heartbeat arrives then).
                    RunEvent::Heartbeat { quiet_secs } if breach.is_none() && Duration::from_secs(quiet_secs) >= stall_after => {
                        stalled = true;
                        Some(format!("stalled: no activity from the agent for {quiet_secs} s"))
                    }
                    ev if breach.is_none() => on_event(ev, &mut spent),
                    _ => None,
                },
                r = &mut run => break r,
                _ = &mut deadline, if breach.is_none() => Some(format!("ran past {} minutes", profile.budget.minutes)),
                _ = &mut grace, if breach.is_some() => {
                    tracing::warn!(sandbox = %sandbox.name, "daemon ignored cancel; cutting it off");
                    grace_fired = true;
                    break Err(Error::Cancelled);
                }
            };
            if let Some(reason) = new_breach {
                if let Err(e) = self.daemon.cancel(&sandbox.id).await {
                    tracing::warn!(sandbox = %sandbox.name, error = %e, "couldn't cancel; relying on the kill switch");
                }
                grace.as_mut().reset(tokio::time::Instant::now() + CANCEL_GRACE);
                breach = Some(reason);
            }
        };
        while let Ok(ev) = rx.try_recv() {
            on_event(ev, &mut spent);
        }
        let total = prior + spent;
        match (result, breach) {
            // The agent's silence says nothing about the task (TRAIL-173), so it's handled as an infrastructure
            // failure: the attempt is given back.
            (Err(Error::Cancelled), Some(reason)) if stalled => {
                // A daemon that ignored the cancel keeps running the agent, so it loses its access as a breach would.
                if grace_fired && let Err(e) = self.cut_off(sandbox).await {
                    tracing::warn!(sandbox = %sandbox.name, error = %e, "couldn't cut off a stalled sandbox");
                }
                Err(Error::Other(reason))
            }
            (Err(Error::Cancelled), Some(reason)) => Ok(Run::Breach(reason, total)),
            // The step finished before the cancel landed; its result stands.
            (Ok(report), _) => Ok(Run::Report(report, total)),
            (Err(e), _) => Err(e),
        }
    }

    /// Rebuilds the control plane's view after a restart: destroys sandboxes no
    /// task accounts for, requeues tasks whose run was interrupted, and retries
    /// reviews that didn't finish.
    pub async fn reconcile(&self) -> Result<Reconciled> {
        self.reconcile_except(&HashSet::new()).await
    }

    /// [`Factory::reconcile`], leaving alone the tasks this process is running right now.
    pub async fn reconcile_except(&self, running: &HashSet<TaskKey>) -> Result<Reconciled> {
        let (mut done, due) = self.reconcile_deferring(running).await?;
        for (task, sandbox) in due.runs {
            if let Err(e) = self.resume_run(&task, &sandbox).await {
                tracing::warn!(task = %task.key.0, error = %e, "couldn't finish a run picked up again");
            }
        }
        for task in due.reviews {
            match self.resume_review(&task).await {
                Ok(_) => done.reviewed.push(task.key.0.clone()),
                Err(e) => tracing::warn!(task = %task.key.0, error = %e, "couldn't review; the next reconcile retries"),
            }
        }
        Ok(done)
    }

    /// [`Factory::reconcile_except`] without running the reviews it finds due: they're returned for the caller to run,
    /// so the run loop can start workers alongside them instead of waiting (TRAIL-196).
    async fn reconcile_deferring(&self, running: &HashSet<TaskKey>) -> Result<(Reconciled, Due)> {
        let mut done = Reconciled::default();
        let mut due = Due::default();
        let live = self.compute.list(&self.base_labels()).await?;
        let tasks: Vec<Task> =
            self.tracker.in_flight(self.project()).await?.into_iter().filter(|t| !running.contains(&t.key)).collect();

        // Runs the daemon may still be working on, which are followed again rather than requeued (TRAIL-137).
        let live_runs: Vec<(&Task, &Sandbox)> =
            tasks.iter().filter_map(|t| Some((t, self.live_run(t, &live)?))).collect();

        for sandbox in &live {
            if sandbox.labels.get(label::TASK).is_some_and(|t| running.contains(&TaskKey(t.clone()))) {
                continue;
            }
            if live_runs.iter().any(|(_, s)| s.name == sandbox.name) {
                continue;
            }
            let parked = tasks.iter().any(|t| {
                t.status == Status::NeedsInput
                    && t.run.host.as_deref().or(t.run.sandbox.as_deref()) == Some(sandbox.name.as_str())
            });
            if parked {
                continue;
            }
            // One sandbox's failure is logged and skipped rather than stopping the control plane from starting.
            if sandbox.labels.contains_key(label::POOL) {
                self.reconcile_pooled(sandbox, &mut done).await;
                continue;
            }
            match self.compute.destroy(&sandbox.id).await {
                Ok(()) => done.destroyed.push(sandbox.name.clone()),
                Err(e) => tracing::warn!(sandbox = %sandbox.name, error = %e, "couldn't destroy an orphaned sandbox"),
            }
        }

        // Likewise one task's failure, a review that can't start for instance, mustn't stop the others or the boot.
        for (task, sandbox) in &live_runs {
            done.reattached.push(task.key.0.clone());
            due.runs.push(((*task).clone(), (*sandbox).clone()));
        }
        for task in tasks.iter().filter(|t| !live_runs.iter().any(|(r, _)| r.key == t.key)) {
            match self.reconcile_task(task, &mut done).await {
                Ok(true) => due.reviews.push(task.clone()),
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(task = %task.key.0, error = %e, "couldn't reconcile a task; the next reconcile retries")
                }
            }
        }
        Ok((done, due))
    }

    /// The pooled sandbox a task's worker run is on, if the run may still be going there: the task is in progress
    /// under its run's sub-agent, its report hasn't been handled, and the sandbox is busy with it.
    fn live_run<'a>(&self, task: &Task, live: &'a [Sandbox]) -> Option<&'a Sandbox> {
        let name = task.run.sandbox.as_ref()?;
        let host = task.run.host.as_ref()?;
        // A run that gave its attempt back (a stop on purpose, a lost daemon) is over, whatever the daemon still has.
        let current = worker_name(&task.key, task.run.attempt?, task.run.retries.unwrap_or(0));
        if *name != current
            || task.run.concluded.as_ref() == Some(name)
            || task.status != Status::InProgress
            || task.holder.as_deref() != Some(self.tracker.sub_agent(name).agent())
        {
            return None;
        }
        live.iter().find(|s| {
            s.name == *host
                && s.labels.get(label::STATE).map(String::as_str) == Some(label::BUSY)
                && s.labels.get(label::TASK).map(String::as_str) == Some(task.key.0.as_str())
        })
    }

    /// Reviews a task whose PR was left unreviewed, retiring the sub-agent of the round that didn't finish.
    async fn resume_review(&self, task: &Task) -> Result<Outcome> {
        let attempt = task.run.attempt.unwrap_or(1);
        let pr = task.run.pr.as_ref().ok_or_else(|| Error::Other(format!("{} has no PR to review", task.key.0)))?;
        // The round the interrupted review was named for: one past the rounds counted before it, whether or not it counted.
        let after_conflict = task.run.pending_changes().is_some_and(|c| c.conflict);
        let rounds = task.run.review_rounds.unwrap_or(0);
        if let Some(round) = Some(rounds + u32::from(after_conflict)).filter(|r| *r > 0) {
            let retries = task.run.retries.unwrap_or(0);
            self.tracker.revoke_sub_agent(&reviewer_name(&task.key, attempt, round, retries)).await?;
        }
        self.review(task, pr, attempt, &[]).await
    }

    /// Closes a task whose PR has merged: records who merged it, which PR and the commit in the task's metadata, then
    /// finishes it with a handoff naming them, which unblocks whatever is chained behind it. The one way a merge
    /// finishes a task: reconcile uses it for merges made outside the factory, and the merger uses it right after
    /// `CodeHost::merge` (`docs/factory/lifecycle.md`). Metadata first, so a handoff that failed to land is retried
    /// with the record already there.
    pub async fn close_merged(&self, task: &Task, merged: Merged) -> Result<()> {
        let who = match merged.by {
            MergedBy::Human => "Merged by a person on the code host",
            MergedBy::Merger => "Merged by the factory's merger",
        };
        let commit = merged.commit.as_deref().map_or_else(String::new, |c| format!(", as commit {c}"));
        let mut handoff = format!("{who}: {}{commit}.", merged.pr.url);
        if merged.by == MergedBy::Merger
            && let Some(d) = self.tracker.show(&task.key).await?.run.merge
        {
            handoff.push_str(&format!(" Grade {}; gates passed: {}.", d.grade, d.passed.join(", ")));
        }
        self.tracker.report(&task.key, RunUpdate { merged: Some(merged), ..RunUpdate::default() }).await?;
        // A parked task can't be finished from where it is: it goes back to open and is claimed first.
        if task.status == Status::NeedsInput {
            self.tracker.reopen(&task.key).await?;
            self.tracker.claim(&task.key).await?;
        }
        self.tracker.finish(&task.key, &handoff).await?;
        tracing::info!(task = %task.key.0, %handoff, "PR merged; task done");
        Ok(())
    }

    /// A PR closed without merging: the task is parked with a question for a human, rather than reopened, since a
    /// rework would push to a PR nobody is going to merge. Reopening it after reopening the PR resumes as a rework.
    async fn park_closed(&self, task: &Task, pr: &PrRef) -> Result<()> {
        tracing::warn!(task = %task.key.0, pr = %pr.url, "the PR was closed without merging; parking");
        // Already open when it's found before a run starts (TRAIL-304); there's nothing in review to reopen.
        if task.status != Status::Open {
            self.tracker.reopen(&task.key).await?;
        }
        self.tracker.claim(&task.key).await?;
        let question = format!(
            "The PR {} was closed without being merged. If the work is abandoned, cancel this task. To carry on, \
             reopen the PR on the code host and then reopen this task, and the next run continues on the PR's branch.",
            pr.url
        );
        self.tracker.ask(&task.key, &question).await
    }

    /// A PR that no longer merges cleanly with the base branch: a rework merges the base branch in and resolves the
    /// conflicts, then goes through the checks and review again. It counts toward `max_conflict_reworks`, not review
    /// rounds, so a PR that keeps conflicting is parked for a human without using up its reviews (TRAIL-199, TRAIL-221).
    async fn rework_conflict(&self, task: &Task, pr: &PrRef, attempt: u32) -> Result<()> {
        let round = task.run.conflict_reworks.unwrap_or(0) + 1;
        self.tracker.report(&task.key, RunUpdate { conflict_reworks: Some(round), ..RunUpdate::default() }).await?;
        let base = &self.config.repo.base_branch;
        let body = format!(
            "{} no longer merges cleanly with `{base}`, which has moved on since the branch was cut. The base \
             branch is already being merged into your checkout, uncommitted and with conflict markers where it \
             clashes. Resolve every conflict, keeping what both sides intended; don't discard either side's changes \
             or rebase. Then run the checks and finish.",
            pr.url
        );
        tracing::warn!(task = %task.key.0, pr = %pr.url, round, "the PR conflicts with the base branch; reworking");
        let head = match &self.codehost {
            Some(codehost) => codehost.head(pr).await.ok(),
            None => None,
        };
        let requested = ChangesRequested { reviewer: "factory".into(), body, attempt, conflict: true, head };
        self.request_rework(task, requested, round).await
    }

    /// CI finished and failed on the PR's current head, whether the worker's own commit or a merge of the base branch
    /// (a clean update keeps the verdict, so nothing else would look at it; TRAIL-390): the task, unapproved if it was,
    /// goes back for a rework told which checks failed and the end of their logs. It counts toward
    /// `max_conflict_reworks`, not review rounds, since CI breaking as main moves isn't a review finding.
    async fn rework_ci_failure(&self, task: &Task, pr: &PrRef, head: &str, checks: &[String]) -> Result<()> {
        let Some(codehost) = &self.codehost else { return Ok(()) };
        let attempt = task.run.attempt.unwrap_or(1);
        // Already asked for on this attempt: reconcile finishes reopening it, and the head isn't reworked twice.
        if task.run.pending_changes().is_some_and(|c| c.attempt == attempt) {
            return Ok(());
        }
        let after_update = match &task.run.reviewed_head {
            Some(reviewed) => reviewed != head && codehost.carries_verdict(pr, reviewed).await?,
            None => false,
        };
        let logs = codehost.ci_logs(pr, checks).await.unwrap_or_else(|e| {
            tracing::warn!(task = %task.key.0, error = %e, "couldn't read the logs of the failed checks");
            Vec::new()
        });
        let mut body = format!("CI failed on {} at {head}: {}.", pr.url, checks.join(", "));
        if after_update {
            body.push_str(
                " It passed before the base branch was merged into the PR, so the failure came in with that merge: \
                 something that landed on the base branch clashes with this change.",
            );
        }
        for (check, log) in &logs {
            body.push_str(&format!("\n\nEnd of the log of {check}:\n\n```\n{}\n```", log.trim_end()));
        }
        body.push_str("\n\nReproduce the failure, fix it, run the checks and finish.");
        if task.status == Status::Approved {
            self.tracker.unapprove(&task.key).await?;
        }
        let round = task.run.conflict_reworks.unwrap_or(0) + 1;
        self.tracker.report(&task.key, RunUpdate { conflict_reworks: Some(round), ..RunUpdate::default() }).await?;
        tracing::warn!(task = %task.key.0, pr = %pr.url, round, %head, "CI failed on the PR's head; reworking");
        let head = Some(head.to_string());
        let requested = ChangesRequested { reviewer: CI_REVIEWER.into(), body, attempt, conflict: true, head };
        self.request_rework(task, requested, round).await
    }

    /// Whether the task's PR was merged or closed, which is dealt with here: a merge, by whoever, finishes the task and
    /// a closed PR parks it.
    /// `None` when it did; when it's still open (or there's no PR to ask about), `Some` of whether it merges cleanly.
    async fn pr_settled(&self, task: &Task, done: &mut Reconciled) -> Result<Option<Option<bool>>> {
        let (Some(pr), Some(codehost)) = (&task.run.pr, &self.codehost) else { return Ok(Some(None)) };
        match codehost.pr_state(pr).await? {
            PrState::Merged { commit } => {
                // A merge the factory didn't make is a person's, outside the factory; one the merger started but
                // didn't get to close (TRAIL-289) is still the merger's.
                let by = merged_by(task);
                self.close_merged(task, Merged { by, pr: pr.clone(), commit }).await?;
                done.merged.push(task.key.0.clone());
                Ok(None)
            }
            PrState::Closed => {
                self.park_closed(task, pr).await?;
                done.closed.push(task.key.0.clone());
                Ok(None)
            }
            PrState::Open { mergeable } => {
                // GitHub works `mergeable` out after a push, so right after one a `false` may be the old head's. Read
                // it as unknown this once; the next reconcile asks again and gets the settled answer.
                let fresh = self.fresh_pushes.lock().unwrap().remove(&task.key);
                Ok(Some(if fresh && mergeable == Some(false) { None } else { mergeable }))
            }
        }
    }

    /// Puts one in-flight task right; true when its PR is due a review.
    async fn reconcile_task(&self, task: &Task, done: &mut Reconciled) -> Result<bool> {
        let Some(name) = &task.run.sandbox else { return Ok(false) };
        match task.status {
            Status::InProgress => {
                let sub = self.tracker.sub_agent(name);
                if task.holder.as_deref() == Some(sub.agent()) {
                    sub.release(&task.key).await?;
                }
                self.tracker.revoke_sub_agent(name).await?;
                done.requeued.push(task.key.0.clone());
            }
            Status::Open => {
                // A PR merged or closed while the task waited for its rework: settle it rather than rework a dead branch.
                if self.pr_settled(task, done).await?.is_none() {
                    return Ok(false);
                }
                self.tracker.revoke_sub_agent(name).await?;
            }
            // Parked for a human, so a PR still open or closed stays as it is (a closed one was the question). A merge
            // by anyone finishes the task all the same.
            Status::NeedsInput => {
                if let (Some(pr), Some(codehost)) = (&task.run.pr, &self.codehost)
                    && let PrState::Merged { commit } = codehost.pr_state(pr).await?
                {
                    self.close_merged(task, Merged { by: MergedBy::Human, pr: pr.clone(), commit }).await?;
                    done.merged.push(task.key.0.clone());
                }
            }
            Status::Approved => {
                let Some(mergeable) = self.pr_settled(task, done).await? else { return Ok(false) };
                // Main may have moved since the approval: a PR that now conflicts goes back for a rework, which
                // would otherwise wait for a merge attempt (and never come, in shadow mode or at a grade that
                // doesn't auto-merge).
                if let Some(pr) = &task.run.pr
                    && mergeable == Some(false)
                {
                    self.tracker.unapprove(&task.key).await?;
                    let attempt = task.run.attempt.unwrap_or(1);
                    self.rework_conflict(task, pr, attempt).await?;
                    done.conflicted.push(task.key.0.clone());
                    return Ok(false);
                }
                self.evaluate_gates(&task.key).await?;
            }
            Status::InReview => {
                let Some(mergeable) = self.pr_settled(task, done).await? else { return Ok(false) };
                let attempt = task.run.attempt.unwrap_or(1);
                // Changes were requested on this attempt but the task never left review: finish reopening it.
                // A park resets the counts to zero, so a request still recorded beside zero counts was parked on and
                // then settled by a person: it's stale, and the PR is looked at as it is now (TRAIL-304).
                let parked = |c: &ChangesRequested| {
                    let count = if c.conflict { task.run.conflict_reworks } else { task.run.review_rounds };
                    count == Some(0)
                };
                // The head recorded with the request is sturdier than the counts, whose reset can be lost: a PR that
                // has moved since was pushed to by a person (TRAIL-338).
                let moved =
                    match (&task.run.pr, &self.codehost, task.run.pending_changes().and_then(|c| c.head.as_ref())) {
                        (Some(pr), Some(codehost), Some(recorded)) => codehost.head(pr).await? != *recorded,
                        _ => false,
                    };
                if let Some(requested) =
                    task.run.pending_changes().filter(|c| c.attempt == attempt && !parked(c) && !moved)
                {
                    let round = if requested.conflict {
                        task.run.conflict_reworks.unwrap_or(1)
                    } else {
                        task.run.review_rounds.unwrap_or(1)
                    };
                    self.request_rework(task, requested.clone(), round).await?;
                    done.reworked.push(task.key.0.clone());
                    return Ok(false);
                }
                if let Some(pr) = &task.run.pr
                    // Not yet worked out by the host (`None`): the next reconcile asks again.
                    && mergeable == Some(false)
                {
                    self.rework_conflict(task, pr, attempt).await?;
                    done.conflicted.push(task.key.0.clone());
                    return Ok(false);
                }
                // A push from outside the factory leaves the head differing from the one last reviewed; that wants a
                // review like a rework does, or the merger's agent_review gate would wait on it forever (TRAIL-298).
                let mut stale = task.run.reviewed != Some(attempt);
                if let (Some(pr), Some(codehost), Some(reviewed)) =
                    (&task.run.pr, &self.codehost, &task.run.reviewed_head)
                    && !stale
                {
                    stale = !codehost.carries_verdict(pr, reviewed).await?;
                }
                let due = task.run.pr.is_some() && stale && self.setback_over(&task.key);
                if !due {
                    // CI may have finished, or Lane approved, since the gates were last looked at.
                    self.evaluate_gates(&task.key).await?;
                }
                return Ok(due);
            }
            _ => {}
        }
        Ok(false)
    }
}

impl Factory {
    /// The fullest usage window at or past its `pause_at`, if any, as (window, percent used, resets at). A reading
    /// whose window has since reset no longer counts.
    fn paused_until(&self) -> Option<(String, u32, i64)> {
        let now = unix_now();
        let quota = self.quota.lock().unwrap();
        self.config
            .usage
            .pause_at
            .iter()
            .filter_map(|(window, limit)| {
                let (used, resets_at) = *quota.get(window)?;
                (used >= *limit && resets_at > now).then(|| (window.clone(), used, resets_at))
            })
            .max_by_key(|(_, _, resets_at)| *resets_at)
    }

    /// Whether to hold off starting runs because a usage window is nearly full. Running ones carry on.
    fn usage_paused(&self) -> bool {
        let paused = self.paused_until();
        let mut logged = self.pause_logged.lock().unwrap();
        match &paused {
            Some((window, used, resets_at)) if *logged != Some(*resets_at) => {
                tracing::warn!(
                    %window,
                    used_percent = used,
                    resumes_in = ?until(*resets_at),
                    "subscription usage window nearly full; starting no new runs until it resets"
                );
                *logged = Some(*resets_at);
            }
            None if logged.take().is_some() => tracing::info!("usage window reset; starting runs again"),
            _ => {}
        }
        paused.is_some()
    }

    /// A pooled sandbox stays; one left busy by an interrupted run goes back to idle. One that keeps failing to
    /// release is given up on, so it can't hold the pool's only place while every retry fails (TRAIL-135).
    async fn reconcile_pooled(&self, sandbox: &Sandbox, done: &mut Reconciled) {
        let name = &sandbox.name;
        let state = sandbox.labels.get(label::STATE).map(String::as_str);
        if !self.due(name) {
            return;
        }
        if state == Some(label::BROKEN) || self.abandoned(name) {
            match self.compute.destroy(&sandbox.id).await {
                Ok(()) => {
                    tracing::info!(sandbox = %name, "destroyed a broken pooled sandbox");
                    self.troubled.lock().unwrap().remove(name);
                    done.destroyed.push(name.clone());
                }
                Err(e) => {
                    tracing::debug!(sandbox = %name, error = %e, "a broken pooled sandbox still can't be destroyed");
                    self.failed(name);
                }
            }
            return;
        }
        if state == Some(label::IDLE) {
            return;
        }
        tracing::info!(sandbox = %name, "releasing a pooled sandbox back to idle");
        let started = Instant::now();
        match self.release_worker(sandbox).await {
            Ok(()) => {
                tracing::info!(sandbox = %name, took = ?started.elapsed(), "released a pooled sandbox");
                self.troubled.lock().unwrap().remove(name);
                done.released.push(name.clone());
            }
            Err(e) if self.failed(name) => {
                self.abandon(sandbox, &e).await;
                done.abandoned.push(name.clone());
            }
            Err(e) => tracing::warn!(sandbox = %name, error = %e, "couldn't release a pooled sandbox; will retry"),
        }
    }

    /// Takes a sandbox out of the pool for good: destroyed if the compute host allows it, otherwise labelled
    /// [`label::BROKEN`] without a role (so it loses connector access) and destroyed by a later reconcile.
    async fn abandon(&self, sandbox: &Sandbox, error: &Error) {
        let name = &sandbox.name;
        tracing::error!(
            sandbox = %name,
            %error,
            "giving up on a pooled sandbox that won't release; leaving it out of the pool and starting a new one"
        );
        if let Some(t) = self.troubled.lock().unwrap().get_mut(name) {
            t.abandoned = true;
        }
        if self.compute.destroy(&sandbox.id).await.is_ok() {
            self.troubled.lock().unwrap().remove(name);
            return;
        }
        let mut labels = self.pool_labels();
        labels.insert(label::STATE.into(), label::BROKEN.into());
        if let Err(e) = self.compute.set_labels(&sandbox.id, &labels).await {
            tracing::error!(
                sandbox = %name,
                error = %e,
                "couldn't label it broken either; it's out of the pool until this control plane restarts"
            );
        }
    }

    fn abandoned(&self, name: &str) -> bool {
        self.troubled.lock().unwrap().get(name).is_some_and(|t| t.abandoned)
    }

    /// Whether a troubled sandbox's backoff has run out, or it isn't troubled.
    fn due(&self, name: &str) -> bool {
        self.troubled.lock().unwrap().get(name).is_none_or(|t| Instant::now() >= t.next_try)
    }

    /// Records a failure and backs off. True once the sandbox has used up its tries or its patience.
    fn failed(&self, name: &str) -> bool {
        let now = Instant::now();
        let mut troubled = self.troubled.lock().unwrap();
        let t = troubled.entry(name.to_string()).or_insert(Trouble {
            failures: 0,
            since: now,
            next_try: now,
            abandoned: false,
        });
        t.failures += 1;
        t.next_try = now + backoff(t.failures, FIRST_BACKOFF, MAX_BACKOFF);
        !t.abandoned && (t.failures >= RELEASE_ATTEMPTS || now - t.since >= RELEASE_PATIENCE)
    }
}

/// The wait after the `failures`th failure in a row: `first`, doubling up to `max`.
fn backoff(failures: u32, first: Duration, max: Duration) -> Duration {
    first.saturating_mul(1 << failures.saturating_sub(1).min(8)).min(max)
}

fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// How long until a unix time, plus a second so a timer set for a reset fires after it.
fn until(unix: i64) -> Duration {
    Duration::from_secs(u64::try_from(unix - unix_now()).unwrap_or(0) + 1)
}

/// A sandbox's token outlives its wall-clock budget by a margin, so a missed revoke cleans itself up.
fn token_ttl(profile: &Profile) -> Duration {
    Duration::from_secs((u64::from(profile.budget.minutes) + 30) * 60)
}

fn spec(task: &Task, context: HumanContext) -> TaskSpec {
    TaskSpec {
        key: task.key.clone(),
        title: task.title.clone(),
        description: task.description.clone(),
        previous_failure: task.run.failure.clone().filter(|f| !f.is_empty()),
        context,
    }
}
