use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Instant;
use trailhead_factory::harness::is_transient_api_error;
use trailhead_factory::messages;
use trailhead_factory::{
    Error, Feedback, HumanContext, Merged, PrState, Profile, Result, Role, RunEvent, RunReport, RunRequest, RunResult,
    RunUpdate, Sandbox, Task, TaskKey, TaskSpec, Tracker, Usd, VerdictKind, run_name,
};

use crate::reconcile::merged_by;
use crate::timing::{backoff, unix_now, until};
use crate::{CANCEL_GRACE, Factory, Outcome};

impl Factory {
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
                    return Ok(Outcome::Settled { reason: messages::settled_merged(&pr.url) });
                }
                PrState::Closed => {
                    self.park_closed(task, pr).await?;
                    return Ok(Outcome::Settled { reason: messages::settled_closed(&pr.url) });
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
                Some(c) => Some(Feedback { body: messages::review_feedback(&c.reviewer, &c.body) }),
                None if rework => match self.tracker.last_review(&task.key).await? {
                    Some(v) if v.kind == VerdictKind::RequestChanges => {
                        Some(Feedback { body: messages::review_feedback(&v.reviewer, &v.body) })
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
                let cut = self.cut_off(sandbox).await;
                let suspended = cut.is_ok();
                if let Err(e) = &cut {
                    // Reconcile leaves a parked task's sandbox alone, so a sandbox that couldn't be cut off is
                    // destroyed: that loses the suspended state, which beats leaving it reachable.
                    tracing::warn!(task = %task.key.0, sandbox = %sandbox.name, error = %e, "couldn't cut off the breached sandbox; destroying it");
                    self.daemon.forget(&sandbox.id).await;
                    if let Err(e) = self.compute.destroy(&sandbox.id).await {
                        tracing::warn!(task = %task.key.0, sandbox = %sandbox.name, error = %e, "couldn't destroy the breached sandbox; reconcile will");
                    }
                }
                let question = messages::budget_breach_question(&reason, &sandbox.name, suspended);
                let asked = sub.ask(&task.key, &question).await;
                recorded?;
                asked?;
                return Ok(Outcome::Parked { reason });
            }
            Run::Report(report, spend) => (report, spend),
            Run::Lost(e, spend) => {
                // What the run cost before it failed counts toward the budget across retries (TRAIL-447).
                if let Err(re) = sub.report(&task.key, RunUpdate { spend: Some(spend), ..RunUpdate::default() }).await {
                    tracing::warn!(task = %task.key.0, error = %re, "couldn't record the failed run's spend");
                }
                return Err(self.infra_failed(task, sub, name, e, infra_prior, None).await);
            }
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
                    sub.ask(&task.key, &messages::gave_up_question(failed, max, &reason, &last)).await?;
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
        let held = self.setbacks.lock().unwrap().get(&task.key).map_or(0, |(n, _, _)| *n);
        let failures = prior.unwrap_or(0).max(held) + 1;
        let max = self.config.repo.max_infra_failures.max(1);
        // A host short of a resource (its disk) is waited out, not failed: the backoff grows but the task is never
        // parked for it, and the count that parks isn't raised.
        let waiting = matches!(error, Error::Unavailable(_));
        let park = failures >= max && !waiting;
        tracing::warn!(task = %task.key.0, %error, failures, max, waiting, "infrastructure failure; the attempt doesn't count");
        if waiting {
            self.set_back_waiting(&task.key, failures - 1);
        } else {
            self.set_back(&task.key, failures);
        }
        // Before the release: the run's sub-agent holds the claim, so only it can report. A parked task starts its
        // count again when Lane reopens it.
        let update = RunUpdate {
            attempt: Some(task.run.attempt.unwrap_or(0)),
            retries: Some(task.run.retries.unwrap_or(0) + 1),
            infra_failures: Some(if park {
                0
            } else if waiting {
                prior.unwrap_or(0)
            } else {
                failures
            }),
            // Set only when this run's agent wrote text that isn't about the task (its API trouble): the failure the
            // task had before the run is put back, so the next attempt is still told what failed.
            failure,
            ..RunUpdate::default()
        };
        if let Err(e) = sub.report(&task.key, update).await {
            tracing::warn!(task = %task.key.0, error = %e, "couldn't give the attempt back");
        }
        if park {
            let question = messages::infra_question(failures, max, &error.to_string());
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
    pub(crate) fn set_back(&self, key: &TaskKey, failures: u32) {
        let wait = backoff(failures, FIRST_INFRA_BACKOFF, MAX_INFRA_BACKOFF);
        self.setbacks.lock().unwrap().insert(key.clone(), (failures, 0, Instant::now() + wait));
    }

    /// Holds a task back while its host lacks a resource: the backoff grows with the waits as well as the failures,
    /// but the stored count, the one that parks, stays at `failures`.
    fn set_back_waiting(&self, key: &TaskKey, failures: u32) {
        let mut setbacks = self.setbacks.lock().unwrap();
        let waits = setbacks.get(key).map_or(0, |(_, w, _)| *w) + 1;
        let wait = backoff(failures.saturating_add(waits), FIRST_INFRA_BACKOFF, MAX_INFRA_BACKOFF);
        setbacks.insert(key.clone(), (failures, waits, Instant::now() + wait));
    }

    /// Whether a task that failed on the infrastructure has waited out its backoff.
    pub(crate) fn setback_over(&self, key: &TaskKey) -> bool {
        self.setbacks.lock().unwrap().get(key).is_none_or(|(_, _, next_try)| Instant::now() >= *next_try)
    }

    /// Runs one step in a sandbox, pricing usage as it streams in and cancelling on a breach.
    /// `elapsed` is how long the run has already gone on, for one picked up again after a restart.
    pub(crate) async fn run_metered(
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
                    self.host.quota.lock().unwrap().insert(window, (used_percent, resets_at));
                }
                _ => {}
            }
            if *spent > limit {
                Some(messages::spend_breach(*spent, limit))
            } else if steps > max_steps {
                Some(messages::steps_breach(max_steps))
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
                        Some(messages::stall_breach(quiet_secs))
                    }
                    ev if breach.is_none() => on_event(ev, &mut spent),
                    _ => None,
                },
                r = &mut run => break r,
                _ = &mut deadline, if breach.is_none() => Some(messages::time_breach(profile.budget.minutes.into())),
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
                Ok(Run::Lost(Error::Other(reason), total))
            }
            (Err(Error::Cancelled), Some(reason)) => Ok(Run::Breach(reason, total)),
            // The step finished before the cancel landed; its result stands.
            (Ok(report), _) => Ok(Run::Report(report, total)),
            (Err(Error::Cancelled), None) => Err(Error::Cancelled),
            (Err(e), _) => Ok(Run::Lost(e, total)),
        }
    }
}

/// The first wait before retrying a task after an infrastructure failure, doubling up to [`MAX_INFRA_BACKOFF`].
const FIRST_INFRA_BACKOFF: Duration = Duration::from_secs(60);

const MAX_INFRA_BACKOFF: Duration = Duration::from_secs(15 * 60);

/// How a metered run gets going: a new one, or one the daemon already has under way.
pub(crate) enum Begin {
    Start(Box<RunRequest>),
    Attach(String),
}

/// What one sandbox run came to, before the lifecycle acts on it.
pub(crate) enum Run {
    Report(RunReport, Usd),
    Breach(String, Usd),
    /// The run failed on the infrastructure (a stall, a lost daemon), having cost this much by then.
    Lost(Error, Usd),
}

/// A worker run's name: the attempt's, with the count of retries that gave their attempt back.
pub(crate) fn worker_name(task: &TaskKey, attempt: u32, retries: u32) -> String {
    match retries {
        0 => run_name(task, attempt),
        n => format!("{}-{n}", run_name(task, attempt)),
    }
}

/// A sandbox's token outlives its wall-clock budget by a margin, so a missed revoke cleans itself up.
pub(crate) fn token_ttl(profile: &Profile) -> Duration {
    Duration::from_secs((u64::from(profile.budget.minutes) + 30) * 60)
}

pub(crate) fn spec(task: &Task, context: HumanContext) -> TaskSpec {
    TaskSpec {
        key: task.key.clone(),
        title: task.title.clone(),
        description: task.description.clone(),
        previous_failure: task.run.failure.clone().filter(|f| !f.is_empty()),
        context,
    }
}
