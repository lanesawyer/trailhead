use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Instant;
use trailhead_factory::harness::is_transient_api_error;
use trailhead_factory::messages::{self, BreachedSandbox};
use trailhead_factory::{
    ChangesRequested, Error, Feedback, HumanContext, Merged, Patch, PrRef, PrState, Profile, RequestKind, Result, Role,
    RunEvent, RunPatch, RunPhase, RunReport, RunRequest, RunResult, Sandbox, Status, Task, TaskKey, TaskSpec, Tracker,
    Usd, VerdictKind, WorkerBreach, run_name,
};

use crate::infra::{Park, Setback};
use crate::place::Placement;
use crate::timing::{backoff, unix_now, until};
use crate::{CANCEL_GRACE, Factory, Outcome, Placed};

impl Factory {
    /// One attempt at a task on the first target with room, or `Error::Unavailable` when none has any.
    pub async fn run_task(&self, task: &Task) -> Result<Outcome> {
        let mut survey = self.survey().await;
        let Some(target) = self.place_task(&mut survey, task) else {
            return Err(Error::Unavailable(messages::no_target_has_room(&task.key)));
        };
        let place = self.hold(&task.key, Role::Worker, target);
        self.run_task_on(task, target, place).await
    }

    /// One attempt at a task on `target`: a fresh sandbox, the worker, then the reviewer. `place` is the worker's
    /// place on `target`, given up when its sandbox is released, before the review is placed on its own.
    pub(crate) async fn run_task_on(&self, task: &Task, target: &Placed, place: Placement) -> Result<Outcome> {
        // A rework on a PR that has merged or closed since the task was queued would push to a branch nobody wants
        // (TRAIL-304). The sub-agent isn't made yet, so the factory's own identity holds the task while it's settled.
        if let (Some(pr), Some(codehost)) = (&task.run.pr, &self.codehost) {
            let conflict_rework = task.run.pending_changes().is_some_and(|c| c.kind == RequestKind::Conflict);
            let mut state = codehost.pr_state(pr).await?;
            // Right after the base moves the host may not have worked `mergeable` out yet. Ask again a few times
            // before a worker is spent on a conflict that a clean local merge would leave nothing to fix (TRAIL-953).
            if conflict_rework {
                for wait in MERGEABLE_WAITS {
                    if !matches!(state, PrState::Open { mergeable: None, .. }) {
                        break;
                    }
                    tokio::time::sleep(wait).await;
                    state = codehost.pr_state(pr).await?;
                }
            }
            match state {
                PrState::Open { mergeable: Some(true), .. } if conflict_rework => {
                    tracing::info!(task = %task.key, pr = %pr.url, "the host no longer reports a conflict; back to review");
                    if !self.settle_gone_conflict(task, pr).await? {
                        return Ok(Outcome::Settled { reason: messages::settled_conflict_superseded(&pr.url) });
                    }
                    return Ok(Outcome::Settled { reason: messages::settled_conflict_gone(&pr.url) });
                }
                PrState::Merged { commit } => {
                    self.tracker.claim(&task.key).await?;
                    let by = self.merged_by(task, pr).await?;
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
                // The give-back below may be lost with the tracker down, so the wait grows with the skips held in
                // memory instead of resetting to a minute. It's a wait only: the count that parks isn't raised.
                let held = self.backoff.setbacks.lock().unwrap().get(&task.key).map_or(0, |(n, _, _)| *n);
                self.set_back_waiting(&task.key, held);
                let update = RunPatch { retries: Patch::Set(retries + 1), ..RunPatch::default() };
                if let Err(e) = self.tracker.report(&task.key, update).await {
                    tracing::warn!(task = %task.key, error = %e, "couldn't skip a used run name");
                }
                return Err(e);
            }
            // No sub-agent, so nothing holds the claim: back off and count it, as the factory's own identity. Revoking
            // the sub-agent that was never created fails, and that warning is expected.
            Err(e) => {
                // Parking and releasing need the claim, so take it as the factory's own identity first.
                if let Err(claim) = self.tracker.claim(&task.key).await {
                    tracing::warn!(task = %task.key, error = %claim, "couldn't claim the task to back it off");
                }
                return Err(self
                    .infra_failed(task, &self.tracker, &name, e, task.run.infra_failures, Patch::Keep)
                    .await);
            }
        };
        let sub = self.tracker.sub_agent(&name);
        let infra_prior = task.run.infra_failures;
        // Claiming as the sub-agent before creating the sandbox keeps two control planes from racing for one task.
        if let Err(e) = sub.claim(&task.key).await {
            return Err(self.infra_failed(task, &sub, &name, e, infra_prior, Patch::Keep).await);
        }
        let started = RunPatch {
            attempt: Patch::Set(attempt),
            // A task parked for giving up and reopened starts its count again.
            attempt_base: Patch::from(task.run.gave_up.filter(|g| *g > task.run.attempt_base.unwrap_or(0))),
            sandbox: Patch::Set(name.clone()),
            profile: Patch::Set(self.config.repo.worker.clone()),
            started: Patch::Set(unix_now()),
            // A sandbox given up while parked is no longer the task's to look for (TRAIL-601).
            released: if task.run.released.is_some() { Patch::Clear } else { Patch::Keep },
            ..RunPatch::default()
        };
        if let Err(e) = sub.report(&task.key, started).await {
            return Err(self.infra_failed(task, &sub, &name, e, infra_prior, Patch::Keep).await);
        }
        let _run = self.status.begin(&task.key, RunPhase::Preparing { cold: false });
        tracing::info!(task = %task.key, run = %name, "preparing a worker sandbox");
        // A task with a PR is a rework: the checkout starts from the PR branch's head, and the worker is told what the
        // review asked for.
        let rework = task.run.pr.is_some();
        let prepared = async {
            let context = self.tracker.human_context(&task.key).await?;
            // A review that never went through request_rework (an older one, or a task reopened by hand) left no
            // changes_requested; the review comment is the feedback then (TRAIL-218).
            let feedback = match task.run.last_changes().filter(|_| rework) {
                Some(c) => Some(Feedback { body: messages::review_feedback(&c.reviewer, &c.body) }),
                None if rework => match self.tracker.last_review(&task.key).await? {
                    Some(v) if v.kind == VerdictKind::RequestChanges => {
                        Some(Feedback { body: messages::review_feedback(&v.reviewer, &v.body) })
                    }
                    _ => None,
                },
                None => None,
            };
            let merge_base = task.run.last_changes().is_some_and(|c| !c.kind.is_review());
            // A task whose sandbox was given up while parked, with no PR, may have pushed its branch before the PR failed
            // to open: it resumes from it when the remote has it (TRAIL-751).
            let resume = !rework && task.run.released.is_some();
            let (sandbox, setup_failure) = self
                .acquire_worker(target, task, (rework || resume).then(|| Self::branch(task)), resume, merge_base)
                .await?;
            // The agent starts with the output so it can fix what broke, after whatever the review asked for.
            let feedback = match (feedback, setup_failure) {
                (feedback, Some(output)) => {
                    let setup = messages::setup_failed_feedback(&output);
                    let body = feedback.map_or(setup.clone(), |f| format!("{}\n\n{setup}", f.body));
                    Some(Feedback { body })
                }
                (feedback, None) => feedback,
            };
            sub.report(
                &task.key,
                RunPatch {
                    host: Patch::Set(sandbox.name.clone()),
                    target: Patch::Set(target.target.name.clone()),
                    ..RunPatch::default()
                },
            )
            .await?;
            Ok::<_, Error>((sandbox, context, feedback))
        };
        let (sandbox, context, feedback) = match prepared.await {
            Ok(prepared) => prepared,
            Err(e) => return Err(self.infra_failed(task, &sub, &name, e, infra_prior, Patch::Keep).await),
        };
        tracing::info!(task = %task.key, run = %name, sandbox = %sandbox.name, "starting worker");
        self.status.phase(&task.key, RunPhase::Agent, Some(&sandbox.name));
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
            model: target.provider.resolve(profile.model),
            endpoint: target.provider.endpoint(),
            max_steps: profile.budget.steps,
            required_checks: self.config.required_checks(),
            worker_checks: Vec::new(),
            blocked_hosts: self.blocked_hosts(&sandbox),
            allowed_hosts: self.allowed_hosts(),
            env: self.config.stack.env.clone(),
            credential,
        };
        let prior = task.run.spend.unwrap_or_default();
        let run =
            self.run_metered(&sandbox, Begin::Start(Box::new(req)), &task.key, &profile, prior, Duration::ZERO).await;
        self.conclude(task, &sub, &sandbox, &name, attempt, run, Some(place)).await
    }

    /// Follows a worker run that was going when the last control plane stopped (TRAIL-137): the daemon replays the
    /// run's events, which are priced again on top of the spend of the runs before it, and the deadline counts from
    /// when the run started. What the run comes to is handled as if this process had started it.
    pub async fn resume_run(&self, task: &Task, sandbox: &Sandbox) -> Result<Outcome> {
        let name =
            task.run.sandbox.clone().ok_or_else(|| Error::Other(format!("{} has no run to resume", task.key)))?;
        let attempt = task.run.attempt.unwrap_or(1);
        let sub = self.tracker.sub_agent(&name);
        let _run = self.status.begin(&task.key, RunPhase::Agent);
        self.status.phase(&task.key, RunPhase::Agent, Some(&sandbox.name));
        self.status.reattaching(&task.key);
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
        tracing::info!(task = %task.key, run = %name, sandbox = %sandbox.name, ?elapsed, "reattaching to a run in flight");
        let run = self.run_metered(sandbox, Begin::Attach(name.clone()), &task.key, &profile, prior, elapsed).await;
        self.conclude(task, &sub, sandbox, &name, attempt, run, None).await
    }

    /// Cuts a breached sandbox off, saying what became of it. One that can't be cut off is destroyed: reconcile
    /// leaves a parked task's sandbox alone, so that loses the suspended state, which beats leaving it reachable.
    pub(crate) async fn cut_off_breached(&self, task: &Task, sandbox: &Sandbox) -> BreachedSandbox {
        let Err(e) = self.cut_off(sandbox).await else { return BreachedSandbox::Suspended };
        tracing::warn!(task = %task.key, sandbox = %sandbox.name, error = %e, "couldn't cut off the breached sandbox; destroying it");
        self.daemon_of(sandbox).forget(&sandbox.id).await;
        if let Err(e) = self.target_of(sandbox).compute.destroy(&sandbox.id).await {
            tracing::warn!(task = %task.key, sandbox = %sandbox.name, error = %e, "couldn't destroy the breached sandbox; reconcile retries");
            return BreachedSandbox::Stuck;
        }
        BreachedSandbox::Destroyed
    }

    /// Asks a worker breach's question, marking the breach parked first so that a question that was asked can't be
    /// asked again once it's answered. The mark is taken back if the ask fails, so reconcile finishes the park. A mark
    /// that can't be written doesn't stop the ask.
    pub(crate) async fn park_worker_breach(
        &self,
        key: &TaskKey,
        sub: &Arc<dyn Tracker>,
        breach: &WorkerBreach,
        state: BreachedSandbox,
        question: &str,
    ) -> Result<()> {
        let stuck = state == BreachedSandbox::Stuck;
        let mark = |parked| RunPatch {
            worker_breach: Patch::Set(WorkerBreach { parked, stuck, ..breach.clone() }),
            ..RunPatch::default()
        };
        if let Err(e) = sub.report(key, mark(true)).await {
            tracing::warn!(task = %key, error = %e, "couldn't mark the breach parked; asking anyway");
        }
        if let Err(e) = sub.ask(key, question).await {
            if let Err(re) = sub.report(key, mark(false)).await {
                tracing::warn!(task = %key, error = %re, "couldn't take back the breach's park mark");
            }
            return Err(e);
        }
        Ok(())
    }

    /// A run that overran its budget: its spend is recorded, its sandbox cut off and the task parked.
    async fn conclude_breach(
        &self,
        task: &Task,
        sub: &Arc<dyn Tracker>,
        sandbox: &Sandbox,
        name: &str,
        reason: String,
        spend: Usd,
    ) -> Result<Outcome> {
        tracing::warn!(task = %task.key, %reason, "budget breached; parking");
        // The spend is recorded before any teardown. A record that fails doesn't skip the kill switch or the
        // park; its error is returned after them. The report marks the run concluded and records the breach, so
        // a restart before the park lands neither reattaches to the run and prices its replayed events a second
        // time, nor requeues the task on a fresh budget: reconcile finishes the park (TRAIL-451).
        let update = RunPatch {
            spend: Patch::Set(spend),
            concluded: Patch::Set(name.to_string()),
            worker_breach: Patch::Set(WorkerBreach {
                run: name.to_string(),
                reason: reason.clone(),
                parked: false,
                stuck: false,
            }),
            ..RunPatch::default()
        };
        let recorded = sub.report(&task.key, update).await;
        if let Err(e) = &recorded {
            tracing::warn!(task = %task.key, error = %e, "couldn't record the breach's spend; cutting off anyway");
        }
        if let Err(e) = self.target_of(sandbox).compute.checkpoint(&sandbox.id, &format!("parked: {reason}")).await {
            tracing::warn!(task = %task.key, sandbox = %sandbox.name, error = %e, "couldn't checkpoint the breached sandbox");
        }
        let state = self.cut_off_breached(task, sandbox).await;
        let breach = WorkerBreach { run: name.to_string(), reason: reason.clone(), parked: false, stuck: false };
        let question = messages::budget_breach_question(&reason, &sandbox.name, state);
        let asked = self.park_worker_breach(&task.key, sub, &breach, state, &question).await;
        recorded?;
        asked?;
        self.status.paused(&sandbox.name);
        Ok(Outcome::Parked { reason })
    }

    /// What a worker's run came to: the report acted on, or the failure given back to the queue.
    #[allow(clippy::too_many_arguments)]
    async fn conclude(
        &self,
        task: &Task,
        sub: &Arc<dyn Tracker>,
        sandbox: &Sandbox,
        name: &str,
        attempt: u32,
        run: Result<Run>,
        place: Option<Placement>,
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
            Err(e) => return Err(self.infra_failed(task, sub, name, e, infra_prior, Patch::Keep).await),
        };

        // From here the agent has ended the run, so its attempt counts. Errors go to reconcile as before, except after
        // the agent's own API trouble, which gives the attempt back (TRAIL-172). A breach is the agent's doing too.
        let (report, spend) = match run {
            Run::Breach(reason, spend) => return self.conclude_breach(task, sub, sandbox, name, reason, spend).await,
            Run::Report(report, spend) => (report, spend),
            Run::Lost(e, spend) => {
                // What the run cost before it failed counts toward the budget across retries (TRAIL-447).
                if let Err(re) =
                    sub.report(&task.key, RunPatch { spend: Patch::Set(spend), ..RunPatch::default() }).await
                {
                    tracing::warn!(task = %task.key, error = %re, "couldn't record the failed run's spend");
                }
                return Err(self.infra_failed(task, sub, name, e, infra_prior, Patch::Keep).await);
            }
        };
        // An agent that ends the run itself shows the infrastructure working again, unless what it ended with is its
        // API trouble.
        let api_trouble = matches!(&report.result, RunResult::Failed(reason) if is_transient_api_error(reason));
        if !api_trouble {
            self.backoff.setbacks.lock().unwrap().remove(&task.key);
            self.backoff.setup_failures.lock().unwrap().remove(&task.key);
        }
        let infra_failures = if api_trouble { None } else { task.run.infra_failures.filter(|n| *n > 0).map(|_| 0) };
        let update = RunPatch {
            spend: Patch::Set(spend),
            concluded: Patch::Set(name.to_string()),
            infra_failures: infra_failures.into(),
            ..RunPatch::default()
        };
        match sub.report(&task.key, update).await {
            Err(e) if api_trouble => return Err(self.infra_failed(task, sub, name, e, infra_prior, Patch::Keep).await),
            reported => reported?,
        }

        match report.result {
            RunResult::PrOpened(pr) => {
                // The daemon may have opened the PR without being able to write it down (TRAIL-441). Whatever is
                // missing is recorded here; a failure is logged, since the PR is open either way.
                self.record_opened_pr(task, sub, &pr).await;
                if let Some(stale) =
                    task.run.last_changes().filter(|c| !c.kind.is_review()).and_then(|c| c.head.clone())
                {
                    // Recorded so a `mergeable` reading for the head from before the push is ignored, across a restart
                    // too. A request whose head the host couldn't say leaves the mark off; readings count as they come.
                    let update = RunPatch { stale_head: Patch::Set(stale), ..RunPatch::default() };
                    if let Err(e) = sub.report(&task.key, update).await {
                        tracing::warn!(task = %task.key, error = %e, "couldn't record the head from before the push");
                    }
                }
                // MVP: no suspend during review; requested changes leave the task in review as changes requested
                // (`request_rework`) and a new run starts from the PR branch.
                if let Err(e) = self.release_worker(sandbox).await {
                    tracing::warn!(task = %task.key, sandbox = %sandbox.name, error = %e, "couldn't release the worker's sandbox; reconcile retries");
                }
                // The worker is done with its target; the review takes its own place.
                drop(place);
                if let Err(e) = self.tracker.revoke_sub_agent(name).await {
                    tracing::warn!(task = %task.key, error = %e, "couldn't revoke the worker's sub-agent; its token will expire");
                }
                if self.conflicts_on_publish(task, &pr).await {
                    return match self.rework_conflict(task, &pr, attempt).await {
                        Ok(()) => Ok(Outcome::Conflicted { pr }),
                        // Still in review and conflicting, so reconcile reworks it.
                        Err(e) => {
                            tracing::warn!(task = %task.key, error = %e, "couldn't request a rework of the conflicting PR; reconcile retries");
                            Ok(Outcome::AwaitingReview { pr })
                        }
                    };
                }
                self.review(task, &pr, attempt, &report.checks).await
            }
            RunResult::NeedsInput(question) => {
                // An attempt that asked a question didn't give up, so a give-up from before it isn't "the attempt
                // before" the next one (TRAIL-727). The failure stays, so the next attempt is still told of it;
                // `asked` is what the backstop below goes by (TRAIL-805).
                if task.run.failure.is_some() {
                    let mark = RunPatch { asked: Patch::Set(attempt), ..RunPatch::default() };
                    if let Err(e) = sub.report(&task.key, mark).await {
                        tracing::warn!(task = %task.key, error = %e, "couldn't record the attempt that asked");
                    }
                }
                self.status.paused(&sandbox.name);
                Ok(Outcome::Parked { reason: question })
            }
            RunResult::Failed(reason) => {
                tracing::warn!(task = %task.key, %reason, "attempt failed");
                match self.release_worker(sandbox).await {
                    Err(e) if api_trouble => {
                        return Err(self.infra_failed(task, sub, name, e, infra_prior, Patch::Keep).await);
                    }
                    // Any other failure to release leaves the sandbox to reconcile; the park below still happens.
                    Err(e) => {
                        tracing::warn!(task = %task.key, sandbox = %sandbox.name, error = %e, "couldn't release the worker's sandbox; reconcile retries");
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
                    let restore = Patch::restore(task.run.failure.clone());
                    self.infra_failed(task, sub, name, error, infra_prior, restore).await;
                    return Ok(Outcome::Failed { reason });
                }
                let max = self.config.repo.max_attempts;
                // Counted from the last review that asked for changes, so reworks get attempts of their own.
                let failed = attempt.saturating_sub(task.run.attempt_floor());
                // The failure the daemon just recorded for this attempt.
                let last = match self.tracker.show(&task.key).await {
                    Ok(shown) => shown.run.failure,
                    Err(e) => {
                        tracing::warn!(task = %task.key, error = %e, "couldn't read the last failure; parking without it");
                        None
                    }
                };
                // Two attempts in a row that gave up say the cause is in the environment, not in a try that can
                // improve: a give-up publishes nothing, so each starts from the same checkout. Failed checks, a lost
                // merge and conflict markers come from runs that changed it, so those are left to try again. A human
                // is asked now instead of after `max_attempts` (TRAIL-705).
                let repeated = task
                    .run
                    .failure
                    .clone()
                    .filter(|before| messages::is_agent_gave_up(before))
                    // The failure is from before a question when the attempt just before this one asked it.
                    .filter(|_| task.run.asked != Some(attempt.saturating_sub(1)))
                    .filter(|_| failed >= 2 && last.as_deref().is_some_and(messages::is_agent_gave_up));
                if failed >= max || repeated.is_some() {
                    // The daemon released the task; claiming it again as the sub-agent is how it gets parked.
                    // A task that can't be read still parks, with this attempt's reason alone.
                    let last = last.unwrap_or_else(|| reason.clone());
                    let question = match &repeated {
                        Some(before) if failed < max => messages::repeated_failure_question(before, &reason),
                        _ => messages::gave_up_question(failed, max, &reason, &last),
                    };
                    let parked = async {
                        sub.claim(&task.key).await?;
                        // Recorded before the question, so a task that is parked has the marker; reopening it is all
                        // a human has to do.
                        sub.report(&task.key, RunPatch { gave_up: Patch::Set(attempt), ..RunPatch::default() }).await?;
                        if let Err(e) = sub.ask(&task.key, &question).await {
                            // Not parked, so the marker mustn't stand: when the claim lapses and the task runs again,
                            // its count would restart from it. Put back what was there before.
                            let restore = RunPatch { gave_up: Patch::restore(task.run.gave_up), ..RunPatch::default() };
                            if let Err(re) = sub.report(&task.key, restore).await {
                                tracing::warn!(task = %task.key, error = %re, "couldn't undo the gave-up marker after the park failed");
                            }
                            return Err(e);
                        }
                        Ok(())
                    };
                    if let Err(e) = parked.await {
                        // The sub-agent exists, so the failure goes through `infra_failed`: it backs the task off,
                        // parks it after repeated failures, releases it and revokes the sub-agent.
                        return Err(self.infra_failed(task, sub, name, e, infra_prior, Patch::Keep).await);
                    }
                    if let Err(e) = self.tracker.revoke_sub_agent(name).await {
                        tracing::warn!(task = %task.key, error = %e, "couldn't revoke the worker's sub-agent; its token will expire");
                    }
                    self.status.paused(&sandbox.name);
                    return Ok(Outcome::Parked { reason });
                }
                if let Err(e) = self.tracker.revoke_sub_agent(name).await {
                    tracing::warn!(task = %task.key, error = %e, "couldn't revoke the worker's sub-agent; its token will expire");
                }
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
        failure: Patch<String>,
    ) -> Error {
        let max = self.config.repo.max_infra_failures.max(1);
        // A `setup` that fails on a fresh run fails on the base branch itself, so a second failure is proof enough:
        // more retries would only spend `max_infra_failures` on the same compile error (TRAIL-652).
        // Counted apart from the other infrastructure failures, so it takes two `setup` failures in a row, not one
        // after some unrelated failure (TRAIL-876).
        let setup_failures = {
            let mut counts = self.backoff.setup_failures.lock().unwrap();
            if matches!(error, Error::SetupFailed(_)) {
                let n = counts.entry(task.key.clone()).or_insert(0);
                *n += 1;
                *n
            } else {
                counts.remove(&task.key);
                0
            }
        };
        let broken_base = setup_failures >= SETUP_FAILURES_TO_PARK;
        let setback = Setback::decide(&error, prior, self.held_setbacks(&task.key), max, broken_base);
        let Setback { failures, waits, waiting, .. } = setback;
        tracing::warn!(task = %task.key, %error, failures, max, waiting, waits, "infrastructure failure; the attempt doesn't count");
        if setback.park.is_none() {
            self.back_off(&task.key, &setback);
        }
        // Before the release: the run's sub-agent holds the claim, so only it can report.
        let update = RunPatch {
            attempt: Patch::Set(task.run.attempt.unwrap_or(0)),
            retries: Patch::Set(task.run.retries.unwrap_or(0) + 1),
            infra_failures: Patch::Set(setback.recorded()),
            // Set only when this run's agent wrote text that isn't about the task (its API trouble): the failure the
            // task had before the run is put back, so the next attempt is still told what failed.
            failure,
            ..RunPatch::default()
        };
        if let Err(e) = sub.report(&task.key, update).await {
            tracing::warn!(task = %task.key, error = %e, "couldn't give the attempt back");
        }
        if let Some(why) = setback.park {
            let question = match why {
                Park::BrokenBase => messages::setup_failed_question(setup_failures, &error.to_string()),
                Park::Rejected => messages::rejected_question(&error.to_string()),
                Park::Waited => messages::resource_wait_question(waits, &error.to_string()),
                Park::Limit => messages::infra_question(failures, max, &error.to_string()),
            };
            match sub.ask(&task.key, &question).await {
                // Reopened by Lane, it starts again with no wait, as its count does.
                Ok(()) => {
                    self.backoff.setbacks.lock().unwrap().remove(&task.key);
                    self.backoff.setup_failures.lock().unwrap().remove(&task.key);
                }
                Err(e) => {
                    tracing::warn!(task = %task.key, error = %e, "couldn't park the task; reconcile will requeue it");
                    // The task needs its backoff, or reconcile requeues it to retry at once.
                    self.back_off(&task.key, &setback);
                    let keep = RunPatch { infra_failures: Patch::Set(setback.unparked()), ..RunPatch::default() };
                    if let Err(e) = sub.report(&task.key, keep).await {
                        tracing::warn!(task = %task.key, error = %e, "couldn't keep the infrastructure failure count");
                    }
                }
            }
        } else if let Err(e) = sub.release(&task.key).await {
            tracing::warn!(task = %task.key, error = %e, "couldn't release the task; reconcile retries");
        }
        if let Err(e) = self.tracker.revoke_sub_agent(name).await {
            tracing::warn!(task = %task.key, error = %e, "couldn't revoke the run's sub-agent; reconcile retries");
        }
        error
    }

    /// Restores the attempt count the task had before this run took the next number, reporting as the run's
    /// sub-agent, which holds the claim.
    async fn give_back_attempt(&self, task: &Task, sub: &Arc<dyn Tracker>) {
        let update = RunPatch {
            attempt: Patch::Set(task.run.attempt.unwrap_or(0)),
            retries: Patch::Set(task.run.retries.unwrap_or(0) + 1),
            ..RunPatch::default()
        };
        if let Err(e) = sub.report(&task.key, update).await {
            tracing::warn!(task = %task.key, error = %e, "couldn't give the attempt back");
        }
    }

    /// Holds a task back for a backoff that grows with each infrastructure failure in a row.
    pub(crate) fn set_back(&self, key: &TaskKey, failures: u32) {
        let wait = backoff(failures, FIRST_INFRA_BACKOFF, MAX_INFRA_BACKOFF);
        self.backoff.setbacks.lock().unwrap().insert(key.clone(), (failures, 0, Instant::now() + wait));
    }

    /// Holds a task back while its host lacks a resource: the backoff grows with the waits as well as the failures,
    /// but the stored count, the one that parks, stays at `failures`.
    pub(crate) fn set_back_waiting(&self, key: &TaskKey, failures: u32) {
        let mut setbacks = self.backoff.setbacks.lock().unwrap();
        let waits = setbacks.get(key).map_or(0, |(_, w, _)| *w) + 1;
        let wait = backoff(failures.saturating_add(waits), FIRST_INFRA_BACKOFF, MAX_INFRA_BACKOFF);
        setbacks.insert(key.clone(), (failures, waits, Instant::now() + wait));
    }

    /// Whether a task that failed on the infrastructure has waited out its backoff.
    pub(crate) fn setback_over(&self, key: &TaskKey) -> bool {
        self.backoff.setbacks.lock().unwrap().get(key).is_none_or(|(_, _, next_try)| Instant::now() >= *next_try)
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
        let mut reattaching = matches!(begin, Begin::Attach(_));
        let run = async {
            match begin {
                Begin::Start(req) => self.daemon_of(sandbox).run(&sandbox.id, *req, tx).await,
                Begin::Attach(run) => self.daemon_of(sandbox).attach(&sandbox.id, &run, tx).await,
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
            if std::mem::take(&mut reattaching) {
                self.status.reattached(task);
            }
            match ev {
                RunEvent::Usage { model, usage } => {
                    *spent += self.provider_of(sandbox).price(&model, &usage);
                    self.status.spend(task, prior + *spent);
                }
                RunEvent::StepStarted { step } => {
                    steps += 1;
                    self.status.step(task, steps, prior + *spent);
                    tracing::info!(task = %key, step, spent = %(prior + *spent), "step");
                }
                RunEvent::ToolCall { name, detail } => {
                    self.status.tool_call(task, &name, detail.as_deref());
                    tracing::info!(task = %key, tool = %name, detail = detail.as_deref().unwrap_or(""), "tool");
                }
                RunEvent::Checking => self.status.phase(task, RunPhase::Checks, None),
                RunEvent::Publishing => self.status.phase(task, RunPhase::Publishing, None),
                RunEvent::NeedsInput { question } => tracing::info!(task = %key, %question, "asked"),
                RunEvent::Quota { window, used_percent, resets_at } => {
                    tracing::info!(%window, used_percent, resets_in = ?until(resets_at), "subscription usage");
                    self.host.record_usage(window, used_percent, resets_at);
                    self.status.usage(self.host.usage());
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
                if let Err(e) = self.daemon_of(sandbox).cancel(&sandbox.id).await {
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

    /// A rework the factory asked for over a conflict that the host no longer reports: the base moved back or the
    /// conflict was fixed meanwhile (TRAIL-243). The merge would leave the agent nothing to resolve and the run would
    /// fail as 'changed nothing', using up an attempt, so no worker runs: the request is dropped and the task goes
    /// back to review, where the gates look at the PR as it is. A CI failure's rework isn't one of these, since it
    /// wants the base merged in whether or not the PR conflicts.
    ///
    /// The task is read again first, and so is its newest review: a review may have asked for changes since `task`
    /// was read, and its request, not the conflict's, is the one standing then. A handoff would count as answering
    /// it and the request would be lost (TRAIL-1418), so nothing is written and the next tick's rework (or
    /// reconcile, for a verdict whose request was never recorded) starts it. The verdict is the check because the
    /// reviewer's request is recorded on the task only after the verdict, the sandbox's teardown and the gates.
    /// False when the settle was skipped.
    async fn settle_gone_conflict(&self, task: &Task, pr: &PrRef) -> Result<bool> {
        let task = &self.tracker.show(&task.key).await?;
        let Some(conflict) = task.run.pending_changes().filter(|c| c.kind == RequestKind::Conflict) else {
            return Ok(false);
        };
        // The conflict request's own verdict is the factory's, with the request's body.
        let superseded = self
            .tracker
            .last_review(&task.key)
            .await?
            .is_some_and(|v| v.kind == VerdictKind::RequestChanges && v.body != conflict.body);
        if superseded {
            return Ok(false);
        }
        // The request is marked settled, which is how reconcile tells it from one still to act on. The conflict count
        // stays as it was, so reworks that really ran still count toward `max_conflict_reworks`.
        if let Some(requested) = task.run.last_changes() {
            let settled = RunPatch {
                changes_requested: Patch::Set(ChangesRequested { settled: true, ..requested.clone() }),
                ..RunPatch::default()
            };
            self.tracker.report(&task.key, settled).await?;
        }
        self.tracker.claim(&task.key).await?;
        // The handoff answers the conflict request, so the task is no longer waiting on a rework.
        let handoff = messages::conflict_gone_note(&pr.url, &self.config.repo.base_branch);
        self.tracker.request_review(&task.key, &handoff).await?;
        Ok(true)
    }

    /// Whether the PR a worker just pushed already conflicts with the base branch, which moved while the run worked
    /// on an older one (TRAIL-844), so a review now would be wasted. The host works `mergeable` out after a push, so
    /// it's asked a few times. A reading counts only once the PR's head has moved past the one from before the push
    /// (TRAIL-446); a rework that doesn't know that head, an answer that doesn't come, or a host error reads as no
    /// conflict, and the review goes ahead as before.
    async fn conflicts_on_publish(&self, task: &Task, pr: &PrRef) -> bool {
        let Some(codehost) = &self.codehost else { return false };
        let before = match &task.run.pr {
            None => None,
            Some(_) => match task.run.last_changes().and_then(|c| c.head.clone()) {
                Some(head) => Some(head),
                None => return false,
            },
        };
        for wait in MERGEABLE_WAITS {
            tokio::time::sleep(wait).await;
            match codehost.pr_state(pr).await {
                Ok(PrState::Open { mergeable: Some(mergeable), head: Some(head) })
                    if Some(&head) != before.as_ref() =>
                {
                    return !mergeable;
                }
                Ok(PrState::Open { .. }) => {}
                Ok(_) => return false,
                Err(e) => {
                    tracing::warn!(task = %task.key, error = %e, "couldn't ask whether the PR merges cleanly; reviewing it");
                    return false;
                }
            }
        }
        false
    }
}

/// The waits before each reading of a just-pushed PR's `mergeable`: GitHub usually has it within a few seconds.
const MERGEABLE_WAITS: [Duration; 4] =
    [Duration::ZERO, Duration::from_secs(2), Duration::from_secs(3), Duration::from_secs(5)];

/// The first wait before retrying a task after an infrastructure failure, doubling up to [`MAX_INFRA_BACKOFF`].
const FIRST_INFRA_BACKOFF: Duration = Duration::from_secs(60);

/// How many `setup` failures in a row on a fresh run park the task: the base branch is broken, not one sandbox.
const SETUP_FAILURES_TO_PARK: u32 = 2;

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
        previous_failure: task.run.failure.clone(),
        context,
    }
}

impl Factory {
    /// Makes sure the tracker has the PR a worker opened and the review request, writing what the daemon couldn't.
    async fn record_opened_pr(&self, task: &Task, sub: &Arc<dyn Tracker>, pr: &PrRef) {
        let shown = match self.tracker.show(&task.key).await {
            Ok(shown) => shown,
            Err(e) => {
                tracing::warn!(task = %task.key, error = %e, "couldn't read the task to check the opened PR was recorded");
                return;
            }
        };
        if shown.run.pr.as_ref() != Some(pr) {
            let update = RunPatch { pr: Patch::Set(pr.clone()), ..RunPatch::default() };
            if let Err(e) = sub.report(&task.key, update).await {
                tracing::warn!(task = %task.key, error = %e, "couldn't record the opened PR");
            }
        }
        if (shown.status != Status::InReview || shown.changes_requested)
            && let Err(e) = sub.request_review(&task.key, &messages::pushed_for_review_handoff(&pr.url)).await
        {
            tracing::warn!(task = %task.key, error = %e, "couldn't move the task to review");
        }
    }
}
