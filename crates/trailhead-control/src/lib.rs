//! The factory's control plane: starts sandboxes for ready tasks, enforces
//! budgets, dispatches reviews and reconciles on boot. It keeps no state of its
//! own; everything it needs is in the tracker and the compute host
//! (`docs/factory/state.md`).

mod core_tracker;

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
    ChangesRequested, CheckResult, CheckpointId, Compute, Daemon, Egress, Error, FactoryConfig, Feedback, HumanContext,
    IROH_DOMAINS, Labels, ModelProvider, PrRef, Profile, Result, Role, RunEvent, RunReport, RunRequest, RunResult,
    RunUpdate, Sandbox, SandboxSpec, Status, Task, TaskKey, TaskSpec, Tracker, Usd, Verdict, VerdictKind, label,
    review_name, run_name,
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

pub struct Factory {
    pub config: FactoryConfig,
    pub compute: Arc<dyn Compute>,
    pub tracker: Arc<dyn Tracker>,
    pub provider: Arc<dyn ModelProvider>,
    pub daemon: Arc<dyn Daemon>,
    troubled: Mutex<HashMap<String, Trouble>>,
    /// Tasks whose last runs failed on the infrastructure: how many in a row, and when to try again.
    setbacks: Mutex<HashMap<TaskKey, (u32, Instant)>>,
    /// The latest reading of each subscription usage window: percent used, and when it resets (unix seconds).
    quota: Mutex<HashMap<String, (u32, i64)>>,
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
    pub requeued: Vec<String>,
    pub reviewed: Vec<String>,
    /// Tasks whose review asked for changes but stayed in review, now reopened for a rework or parked.
    pub reworked: Vec<String>,
    /// Pooled sandboxes that kept failing to release, now out of the pool.
    pub abandoned: Vec<String>,
}

impl Factory {
    pub fn new(
        config: FactoryConfig,
        compute: Arc<dyn Compute>,
        tracker: Arc<dyn Tracker>,
        provider: Arc<dyn ModelProvider>,
        daemon: Arc<dyn Daemon>,
    ) -> Factory {
        Factory {
            config,
            compute,
            tracker,
            provider,
            daemon,
            troubled: Mutex::default(),
            setbacks: Mutex::default(),
            quota: Mutex::default(),
            pause_logged: Mutex::default(),
            pool_pick: tokio::sync::Mutex::default(),
        }
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
        tokio::pin!(shutdown);
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
            if dirty {
                dirty = false;
                match self.reconcile_deferring(&keys.values().cloned().collect()).await {
                    Ok((done, due)) => {
                        if done != Reconciled::default() || !due.is_empty() {
                            let reviewing: Vec<&str> = due.iter().map(|t| t.key.0.as_str()).collect();
                            tracing::info!(?done, ?reviewing, "reconciled");
                        }
                        for task in due {
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
            if let Err(e) = self.start_ready(&mut running, &mut keys, reviews.len()).await {
                tracing::warn!(error = %e, "couldn't start ready tasks");
            }
            let resume = self.paused_until().map(|(_, _, resets_at)| until(resets_at));
            tokio::select! {
                _ = &mut shutdown => break,
                _ = wake.notified() => {}
                _ = tokio::time::sleep(resume.unwrap_or_default()), if resume.is_some() => {}
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
        if self.usage_paused() {
            return Ok(());
        }
        let busy: HashSet<TaskKey> = keys.values().cloned().collect();
        let others = self
            .occupied()
            .await?
            .into_iter()
            .filter(|s| s.labels.get(label::TASK).is_none_or(|t| !busy.contains(&TaskKey(t.clone()))))
            .count();
        let room = self.config.repo.max_sandboxes.saturating_sub(keys.len() - reviewing + others);
        if room == 0 {
            return Ok(());
        }
        let ready = self.tracker.ready(self.project(), self.queue().as_ref(), room).await?;
        for task in ready.into_iter().filter(|t| !busy.contains(&t.key) && self.setback_over(&t.key)) {
            let key = task.key.clone();
            let factory = self.clone();
            let handle = running.spawn(async move { factory.run_task(&task).await });
            keys.insert(handle.id(), key);
        }
        Ok(())
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

    /// Starts runs for ready tasks, up to the repo's sandbox cap, one after another. For tests and one-shot use.
    pub async fn tick(&self) -> Result<Vec<(Task, Outcome)>> {
        if self.usage_paused() {
            return Ok(Vec::new());
        }
        let room = self.config.repo.max_sandboxes.saturating_sub(self.occupied().await?.len());
        if room == 0 {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let ready = self.tracker.ready(self.project(), self.queue().as_ref(), room).await?;
        for task in ready.into_iter().filter(|t| self.setback_over(&t.key)) {
            let outcome = self.run_task(&task).await?;
            out.push((task, outcome));
        }
        Ok(out)
    }

    /// One attempt at a task: a fresh sandbox, the worker, then the reviewer.
    pub async fn run_task(&self, task: &Task) -> Result<Outcome> {
        let attempt = task.run.attempt.unwrap_or(0) + 1;
        let profile = self.config.worker().clone();
        // A run that failed before its agent started gave its attempt back but revoked its sub-agent, which can't be
        // undone, so a retry's name carries the count of those to stay unique.
        let retries = task.run.retries.unwrap_or(0);
        let name = match retries {
            0 => run_name(&task.key, attempt),
            n => format!("{}-{n}", run_name(&task.key, attempt)),
        };
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
            // No sub-agent, so nothing holds the claim: back off and count it, as the factory's own identity.
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
            sandbox: Some(name.clone()),
            profile: Some(self.config.repo.worker.clone()),
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
            let sandbox = self.acquire_worker(task, rework.then(|| Self::branch(task))).await?;
            sub.report(&task.key, RunUpdate { host: Some(sandbox.name.clone()), ..RunUpdate::default() }).await?;
            Ok::<_, Error>((sandbox, context))
        };
        let (sandbox, context) = match prepared.await {
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
            pr: task.run.pr.clone(),
            feedback: task
                .run
                .pending_changes()
                .filter(|_| rework)
                .map(|c| Feedback { body: format!("Review by {}:\n\n{}", c.reviewer, c.body) }),
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
        let run = match self.run_metered(&sandbox, req, &profile, prior).await {
            Ok(run) => run,
            Err(Error::Cancelled) => {
                // A stop on purpose cancelled the run; the next boot's reconcile requeues it without a strike.
                self.give_back_attempt(task, &sub).await;
                return Err(Error::Cancelled);
            }
            // The daemon was lost without reattaching, or the like: the agent didn't give up, so the attempt is
            // given back (TRAIL-172).
            Err(e) => return Err(self.infra_failed(task, &sub, &name, e, infra_prior, None).await),
        };

        let (report, spend) = match run {
            Run::Breach(reason, spend) => {
                tracing::warn!(task = %task.key.0, %reason, "budget breached; parking");
                let parked = async {
                    self.compute.checkpoint(&sandbox.id, &format!("parked: {reason}")).await?;
                    self.cut_off(&sandbox).await
                };
                if let Err(e) = parked.await {
                    return Err(self.infra_failed(task, &sub, &name, e, infra_prior, None).await);
                }
                sub.report(&task.key, RunUpdate { spend: Some(spend), ..RunUpdate::default() }).await?;
                sub.ask(
                    &task.key,
                    &format!(
                        "Budget breached: {reason}. The sandbox {} is suspended; raise the budget or cancel.",
                        sandbox.name
                    ),
                )
                .await?;
                return Ok(Outcome::Parked { reason });
            }
            Run::Report(report, spend) => (report, spend),
        };
        // An agent that ends the run itself shows the infrastructure working again, unless what it ended with is its
        // API trouble.
        let api_trouble = matches!(&report.result, RunResult::Failed(reason) if is_transient_api_error(reason));
        let infra_prior = if api_trouble { infra_prior } else { Some(0) };
        if !api_trouble {
            self.setbacks.lock().unwrap().remove(&task.key);
        }
        let infra_failures = if api_trouble { None } else { task.run.infra_failures.filter(|n| *n > 0).map(|_| 0) };
        let update = RunUpdate { spend: Some(spend), infra_failures, ..RunUpdate::default() };
        if let Err(e) = sub.report(&task.key, update).await {
            return Err(self.infra_failed(task, &sub, &name, e, infra_prior, None).await);
        }

        match report.result {
            RunResult::PrOpened(pr) => {
                // MVP: no suspend during review; requested changes reopen the task (`request_rework`) and a new run
                // starts from the PR branch.
                if let Err(e) = self.release_worker(&sandbox).await {
                    return Err(self.infra_failed(task, &sub, &name, e, infra_prior, None).await);
                }
                self.tracker.revoke_sub_agent(&name).await?;
                self.review(task, &pr, attempt, &report.checks).await
            }
            RunResult::NeedsInput(question) => Ok(Outcome::Parked { reason: question }),
            RunResult::Failed(reason) => {
                tracing::warn!(task = %task.key.0, %reason, "attempt failed");
                if let Err(e) = self.release_worker(&sandbox).await {
                    return Err(self.infra_failed(task, &sub, &name, e, infra_prior, None).await);
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
                    self.infra_failed(task, &sub, &name, error, infra_prior, restore).await;
                    return Ok(Outcome::Failed { reason });
                }
                let max = self.config.repo.max_attempts;
                // Counted from the last review that asked for changes, so reworks get attempts of their own.
                let failed = attempt - task.run.attempt_base.unwrap_or(0);
                if failed >= max {
                    // The daemon released the task; claiming it again as the sub-agent is how it gets parked.
                    let last = self
                        .tracker
                        .show(&task.key)
                        .await?
                        .run
                        .failure
                        .filter(|f| !f.is_empty())
                        .unwrap_or_else(|| reason.clone());
                    sub.claim(&task.key).await?;
                    sub.ask(
                        &task.key,
                        &format!(
                            "Gave up after {failed} failed attempts (the limit is {max}). The last failure: \
                             {reason}\n\n{last}\n\nFix the cause, then reopen the task; raise `max_attempts` to \
                             allow more tries."
                        ),
                    )
                    .await?;
                    self.tracker.revoke_sub_agent(&name).await?;
                    return Ok(Outcome::Parked { reason });
                }
                self.tracker.revoke_sub_agent(&name).await?;
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
        let failures = prior.unwrap_or(0) + 1;
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
            if let Err(e) = sub.ask(&task.key, &question).await {
                tracing::warn!(task = %task.key.0, error = %e, "couldn't park the task; reconcile will requeue it");
                // Not parked, so the count mustn't start again: the next failure tries to park once more.
                let keep = RunUpdate { infra_failures: Some(failures), ..RunUpdate::default() };
                if let Err(e) = sub.report(&task.key, keep).await {
                    tracing::warn!(task = %task.key.0, error = %e, "couldn't keep the infrastructure failure count");
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
        let round = self.tracker.show(&task.key).await?.run.review_rounds.unwrap_or(0) + 1;
        let name = review_name(&task.key, attempt, round);
        self.tracker.report(&task.key, RunUpdate { review_rounds: Some(round), ..RunUpdate::default() }).await?;
        let profile = self.config.reviewer().clone();
        let credential = self.tracker.create_sub_agent(&name, self.project(), token_ttl(&profile)).await?;
        let sub = self.tracker.sub_agent(&name);
        let sandbox = self.create_sandbox(&name, task, Role::Reviewer).await?;
        tracing::info!(task = %task.key.0, sandbox = %name, "starting reviewer");
        let context = self.tracker.human_context(&task.key).await?;
        let req = RunRequest {
            run: name.clone(),
            role: Role::Reviewer,
            task: spec(task, context),
            repo: self.config.repo.repo(),
            base_branch: self.config.repo.base_branch.clone(),
            branch: Self::branch(task),
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
        let run = self.run_metered(&sandbox, req, &profile, prior).await;
        self.compute.destroy(&sandbox.id).await?;
        let run = run?;
        // A request for changes is marked reviewed by `request_rework`, in the same write as the review it records, so
        // a review can't be counted as done without its rework being requested or the task parked.
        let (spend, reviewed) = match &run {
            Run::Report(RunReport { result: RunResult::Reviewed(v), .. }, s) => {
                (*s, (v.kind != VerdictKind::RequestChanges).then_some(attempt))
            }
            Run::Report(_, s) | Run::Breach(_, s) => (*s, None),
        };
        sub.report(&task.key, RunUpdate { spend: Some(spend), reviewed, ..RunUpdate::default() }).await?;
        self.tracker.revoke_sub_agent(&name).await?;
        match run {
            Run::Report(RunReport { result: RunResult::Reviewed(verdict), .. }, _) => {
                if verdict.kind == VerdictKind::RequestChanges {
                    let requested =
                        ChangesRequested { reviewer: verdict.reviewer.clone(), body: verdict.body.clone(), attempt };
                    self.request_rework(task, requested, round).await?;
                }
                Ok(Outcome::Reviewed { pr: pr.clone(), verdict })
            }
            Run::Report(report, _) => {
                tracing::warn!(task = %task.key.0, result = ?report.result, "review didn't produce a verdict");
                Ok(Outcome::AwaitingReview { pr: pr.clone() })
            }
            Run::Breach(reason, _) => {
                tracing::warn!(task = %task.key.0, %reason, "reviewer breached its budget");
                Ok(Outcome::AwaitingReview { pr: pr.clone() })
            }
        }
    }

    /// A review asked for changes: the task goes back to the queue with the review in its metadata, so the next run
    /// starts a fresh sandbox from the PR branch and is told what to fix. The task is parked instead once `round`
    /// reviews have asked for changes, which is when a human should look. Reconcile finishes this if it's cut short.
    async fn request_rework(&self, task: &Task, requested: ChangesRequested, round: u32) -> Result<()> {
        let max = self.config.repo.max_review_rounds;
        let (reviewer, body) = (requested.reviewer.clone(), requested.body.clone());
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
            let question = format!(
                "The reviewer still wants changes after {round} review rounds (the limit is {max}). The last review, \
                 by {reviewer}:\n\n{body}\n\nFix the PR or settle the disagreement, then reopen the task; raise \
                 `max_review_rounds` to allow more rounds."
            );
            self.tracker.ask(&task.key, &question).await?;
            // A fresh count for when a human reopens the task after sorting it out. Last, so a reconcile that finishes
            // a park cut short still sees the round that hit the limit.
            let fresh = RunUpdate { review_rounds: Some(0), ..RunUpdate::default() };
            if let Err(e) = self.tracker.report(&task.key, fresh).await {
                tracing::warn!(task = %task.key.0, error = %e, "couldn't reset the review rounds");
            }
            return Ok(());
        }
        tracing::info!(task = %task.key.0, round, "changes requested; reopening for a rework");
        self.tracker.reopen(&task.key).await
    }

    async fn create_sandbox(&self, name: &str, task: &Task, role: Role) -> Result<Sandbox> {
        let mut labels = self.base_labels();
        labels.insert(label::TASK.into(), task.key.0.clone());
        labels.insert(label::ROLE.into(), role.as_str().into());
        // Install egress only while the checkout is prepared, then narrowed before any agent runs.
        let spec = SandboxSpec { name: name.to_string(), labels, egress: self.install_egress() };
        let sandbox = self.compute.create(&spec).await?;
        self.daemon.prepare(&sandbox.id, self.config.prepare_request(role, None)).await?;
        self.compute.set_egress(&sandbox.id, &self.agent_egress()).await?;
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
    async fn acquire_worker(&self, task: &Task, branch: Option<String>) -> Result<Sandbox> {
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
        self.daemon.prepare(&sandbox.id, self.config.prepare_request(Role::Worker, branch)).await?;
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
        let mut labels = sandbox.labels.clone();
        labels.remove(label::ROLE);
        self.compute.set_labels(&sandbox.id, &labels).await?;
        self.compute.set_egress(&sandbox.id, &Egress::none()).await
    }

    /// Runs one step in a sandbox, pricing usage as it streams in and cancelling on a breach.
    async fn run_metered(&self, sandbox: &Sandbox, req: RunRequest, profile: &Profile, prior: Usd) -> Result<Run> {
        let limit = profile.budget.usd();
        let max_steps = profile.budget.steps;
        let deadline = tokio::time::sleep(Duration::from_secs(u64::from(profile.budget.minutes) * 60));
        tokio::pin!(deadline);
        let key = req.task.key.0.clone();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let run = self.daemon.run(&sandbox.id, req, tx);
        tokio::pin!(run);

        let mut spent = Usd::ZERO;
        let mut steps = 0;
        let mut breach: Option<String> = None;
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
                Some(ev) = rx.recv() => if breach.is_none() { on_event(ev, &mut spent) } else { None },
                r = &mut run => break r,
                _ = &mut deadline, if breach.is_none() => Some(format!("ran past {} minutes", profile.budget.minutes)),
                _ = &mut grace, if breach.is_some() => {
                    tracing::warn!(sandbox = %sandbox.name, "daemon ignored cancel; cutting it off");
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
        for task in due {
            match self.resume_review(&task).await {
                Ok(_) => done.reviewed.push(task.key.0.clone()),
                Err(e) => tracing::warn!(task = %task.key.0, error = %e, "couldn't review; the next reconcile retries"),
            }
        }
        Ok(done)
    }

    /// [`Factory::reconcile_except`] without running the reviews it finds due: they're returned for the caller to run,
    /// so the run loop can start workers alongside them instead of waiting (TRAIL-196).
    async fn reconcile_deferring(&self, running: &HashSet<TaskKey>) -> Result<(Reconciled, Vec<Task>)> {
        let mut done = Reconciled::default();
        let mut due = Vec::new();
        let live = self.compute.list(&self.base_labels()).await?;
        let tasks: Vec<Task> =
            self.tracker.in_flight(self.project()).await?.into_iter().filter(|t| !running.contains(&t.key)).collect();

        for sandbox in &live {
            if sandbox.labels.get(label::TASK).is_some_and(|t| running.contains(&TaskKey(t.clone()))) {
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
        for task in &tasks {
            match self.reconcile_task(task, &mut done).await {
                Ok(true) => due.push(task.clone()),
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(task = %task.key.0, error = %e, "couldn't reconcile a task; the next reconcile retries")
                }
            }
        }
        Ok((done, due))
    }

    /// Reviews a task whose PR was left unreviewed, retiring the sub-agent of the round that didn't finish.
    async fn resume_review(&self, task: &Task) -> Result<Outcome> {
        let attempt = task.run.attempt.unwrap_or(1);
        let pr = task.run.pr.as_ref().ok_or_else(|| Error::Other(format!("{} has no PR to review", task.key.0)))?;
        if let Some(round) = task.run.review_rounds.filter(|r| *r > 0) {
            self.tracker.revoke_sub_agent(&review_name(&task.key, attempt, round)).await?;
        }
        self.review(task, pr, attempt, &[]).await
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
            Status::Open => self.tracker.revoke_sub_agent(name).await?,
            Status::InReview => {
                let attempt = task.run.attempt.unwrap_or(1);
                // Changes were requested on this attempt but the task never left review: finish reopening it.
                if let Some(requested) = task.run.pending_changes().filter(|c| c.attempt == attempt) {
                    let round = task.run.review_rounds.unwrap_or(1);
                    self.request_rework(task, requested.clone(), round).await?;
                    done.reworked.push(task.key.0.clone());
                    return Ok(false);
                }
                return Ok(task.run.pr.is_some() && task.run.reviewed != Some(attempt));
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
        match self.release_worker(sandbox).await {
            Ok(()) => {
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
