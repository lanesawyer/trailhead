use std::collections::HashSet;

use trailhead_factory::messages::{self, AfterUpdate, BreachedSandbox};
use trailhead_factory::{
    CI_REVIEWER, ChangesRequested, CiState, Error, Merged, MergedBy, Patch, PrRef, PrState, RequestKind, Result,
    RunPatch, Sandbox, Status, Task, TaskKey, WorkerBreach, label,
};

use crate::phase::{Parked, Phase, Review, may_be_running, phase, unreviewed};
use crate::review::reviewer_name;
use crate::run_loop::Due;
use crate::timing::until;
use crate::{Factory, Outcome, Reconciled};

/// What `pr_settled` found of a task's PR.
enum PrSettle {
    /// Merged or closed, and dealt with.
    Settled,
    /// Still open (or no PR to ask about); whether it merges cleanly, if known, and the head the host showed, so a
    /// caller that needs it needn't ask again (TRAIL-351).
    Open { mergeable: Option<bool>, head: Option<String> },
}

impl Factory {
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
                tracing::warn!(task = %task.key, error = %e, "couldn't finish a run picked up again");
            }
        }
        for task in due.reviews {
            match self.resume_review(&task).await {
                Ok(_) => done.reviewed.push(task.key.0.clone()),
                Err(e) => tracing::warn!(task = %task.key, error = %e, "couldn't review; the next reconcile retries"),
            }
        }
        Ok(done)
    }

    /// Warns, once per stretch outside the queues, about factory tasks sitting outside every repo's queue:
    /// re-parented while their run was live, they're invisible to reconcile, so their claim lapses and the sandbox
    /// stays (TRAIL-531). Which repo owns one is unknowable, so nothing is torn down.
    async fn warn_strays(&self) {
        // A repo without a queue works the whole project, so nothing in it can sit outside a queue, whichever repo
        // asks: the other repo's in-flight runs are that repo's own.
        if self.queue().is_empty() || self.host.queueless_projects.lock().unwrap().contains(self.project()) {
            return;
        }
        let queues: Vec<TaskKey> = self.host.queues.lock().unwrap().iter().cloned().map(TaskKey).collect();
        match self.tracker.strays(self.project(), &queues).await {
            Ok(strays) => {
                let mut all = self.host.strays_logged.lock().unwrap();
                // Repos on one host can be in different projects; each pass only tracks its own project's strays.
                let logged = all.entry(self.project().to_string()).or_default();
                // A task that stops being a stray is forgotten, so it's warned about again if it strays again.
                logged.retain(|k| strays.iter().any(|t| &t.key.0 == k));
                for t in strays.iter().filter(|t| logged.insert(t.key.0.clone())) {
                    tracing::warn!(
                        task = %t.key,
                        "a factory task sits outside every configured queue, so reconcile can't see it; move it back under its queue or close it"
                    );
                }
            }
            Err(e) => tracing::warn!(error = %e, "couldn't look for factory tasks outside the queues"),
        }
    }

    /// [`Factory::reconcile_except`] without running the reviews it finds due: they're returned for the caller to run,
    /// so the run loop can start workers alongside them instead of waiting (TRAIL-196).
    pub(crate) async fn reconcile_deferring(&self, running: &HashSet<TaskKey>) -> Result<(Reconciled, Due)> {
        let mut done = Reconciled::default();
        let mut due = Due::default();
        // A target that can't be reached is unknown, not empty: what ran there is left as it is until it answers.
        let (live, down) = self.list_all(&self.base_labels()).await?;
        let in_flight = self.tracker.in_flight(self.project(), &self.queue()).await;
        // The tracker says which pooled sandboxes a parked task holds; a board rebuilt at boot can't know.
        let parked: HashSet<String> = in_flight
            .iter()
            .flatten()
            .filter(|t| t.status == Status::NeedsInput && !running.contains(&t.key))
            .filter_map(|t| t.run.host.clone().or_else(|| t.run.sandbox.clone()))
            .collect();
        self.status.sync_sandboxes(&live, &down, &parked);
        self.warn_strays().await;
        let in_flight = in_flight?;
        // Only after a listing that succeeded: a tracker error must not wipe what the merger is tracking.
        let in_review: HashSet<&TaskKey> = in_flight
            .iter()
            .filter(|t| matches!(t.status, Status::InReview | Status::Approved))
            .map(|t| &t.key)
            .collect();
        self.retain_merge_path(&in_review);
        let factory_prs = in_flight.iter().filter_map(|t| t.run.pr.as_ref().map(|p| p.number)).collect();
        self.person_pass(&factory_prs).await;
        let tasks: Vec<Task> = in_flight.into_iter().filter(|t| !running.contains(&t.key)).collect();

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
            // A run parked for its breach, or one whose park is yet to land, keeps its suspended sandbox, unless the
            // breach left it live (stuck): that one is destroyed, not released for reuse, until it is gone.
            let (mut parked, mut stuck) = (false, false);
            for t in tasks.iter().filter(|t| {
                (t.status == Status::NeedsInput
                    || matches!(phase(t, self.tracker.agent(), false), Phase::WorkerBreachUnparked(_)))
                    && t.run.released.is_none()
                    && t.run.host.as_deref().or(t.run.sandbox.as_deref()) == Some(sandbox.name.as_str())
            }) {
                // An earlier run's stuck flag outlives it in the run record, so only the current run's breach counts.
                if t.run.worker_breach.as_ref().is_some_and(|b| b.stuck && t.run.sandbox.as_ref() == Some(&b.run)) {
                    stuck = true;
                } else {
                    parked = true;
                }
            }
            if parked {
                continue;
            }
            // One sandbox's failure is logged and skipped rather than stopping the control plane from starting.
            if sandbox.labels.contains_key(label::POOL) && !stuck {
                self.reconcile_pooled(sandbox, &mut done).await;
                continue;
            }
            match self.target_of(sandbox).compute.destroy(&sandbox.id).await {
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
            if task.status == Status::InProgress && Self::on_unreachable(task, &down) {
                tracing::warn!(task = %task.key, target = ?task.run.target, "its target can't be reached; leaving the run as it is");
                done.unknown.push(task.key.0.clone());
                continue;
            }
            // Its sandbox isn't busy with it: a run still going was followed above.
            let phase = phase(task, self.tracker.agent(), false);
            if let Phase::WorkerBreachUnparked(breach) = phase {
                self.finish_worker_breach(task, breach, &live, &mut done).await;
                continue;
            }
            // A task that is only waiting on its PR costs the code host requests; those wait out a nearly spent limit.
            // Finishing a reviewer-breach park only needs the tracker, so a pause doesn't hold it back.
            if task.status != Status::InProgress
                && !matches!(phase, Phase::ReviewBreachUnparked { .. })
                && let Some(resets_at) = self.code_host_pause()
            {
                tracing::warn!(task = %task.key, resumes_in = ?until(resets_at), "code host rate limit nearly spent; leaving its PR alone");
                continue;
            }
            match self.reconcile_task(task, &mut done).await {
                Ok(true) => due.reviews.push(task.clone()),
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(task = %task.key, error = %e, "couldn't reconcile a task; the next reconcile retries")
                }
            }
        }
        Ok((done, due))
    }

    /// Finishes the park of a run that breached its budget, cut short by a restart: its sandbox is cut off (again, if
    /// that was done) and the question is asked, rather than the task being requeued on a fresh budget. The spend
    /// was recorded with the breach, so it isn't counted again. A failure is left for the next reconcile.
    async fn finish_worker_breach(&self, task: &Task, breach: &WorkerBreach, live: &[Sandbox], done: &mut Reconciled) {
        let reason = breach.reason.as_str();
        let (Some(name), Some(host)) = (&task.run.sandbox, task.run.host.as_ref()) else {
            tracing::warn!(task = %task.key, %reason, sandbox = ?task.run.sandbox, host = ?task.run.host, "a worker's budget breach has no sandbox or host to finish the park with; leaving the task as it is");
            return;
        };
        tracing::warn!(task = %task.key, %reason, "finishing the park for a worker's budget breach");
        // A sandbox that's gone was destroyed after the breach, so there's nothing suspended to point at.
        let state = match live.iter().find(|s| s.name == *host) {
            Some(sandbox) => self.cut_off_breached(task, sandbox).await,
            None => BreachedSandbox::Destroyed,
        };
        let question = messages::budget_breach_question(reason, host, state);
        match self.park_worker_breach(&task.key, &self.tracker.sub_agent(name), breach, state, &question).await {
            Ok(()) => done.breached.push(task.key.0.clone()),
            Err(e) => {
                tracing::warn!(task = %task.key, error = %e, "couldn't park the breached task; the next reconcile retries")
            }
        }
    }

    /// Whether a task's run may be on a target that didn't answer: the target it recorded, or, for a task that
    /// recorded none, any of them.
    fn on_unreachable(task: &Task, down: &HashSet<String>) -> bool {
        match &task.run.target {
            Some(target) => down.contains(target),
            None => !down.is_empty(),
        }
    }

    /// The pooled sandbox a task's worker run is on, if the run may still be going there: the record says it may
    /// be, and the sandbox is busy with it.
    fn live_run<'a>(&self, task: &Task, live: &'a [Sandbox]) -> Option<&'a Sandbox> {
        if !may_be_running(task, self.tracker.agent()) {
            return None;
        }
        let host = task.run.host.as_ref()?;
        live.iter().find(|s| {
            s.name == *host
                && s.labels.get(label::STATE).map(String::as_str) == Some(label::BUSY)
                && s.labels.get(label::TASK).map(String::as_str) == Some(task.key.as_str())
        })
    }

    /// Reviews a task whose PR was left unreviewed, retiring the sub-agent of the round that didn't finish.
    pub(crate) async fn resume_review(&self, task: &Task) -> Result<Outcome> {
        let attempt = task.run.attempt.unwrap_or(1);
        let pr = task.run.pr.as_ref().ok_or_else(|| Error::Other(format!("{} has no PR to review", task.key)))?;
        // The round the interrupted review was named for: one past the rounds counted before it, whether or not it counted.
        let after_conflict = task.run.last_changes().is_some_and(|c| !c.kind.is_review());
        let rounds = task.run.review_rounds.unwrap_or(0);
        if let Some(round) = Some(rounds + u32::from(after_conflict)).filter(|r| *r > 0) {
            let retries = task.run.retries.unwrap_or(0);
            let name = reviewer_name(&task.key, attempt, round, retries);
            if let Err(e) = self.tracker.revoke_sub_agent(&name).await {
                tracing::warn!(task = %task.key, error = %e, "couldn't revoke the interrupted reviewer's sub-agent; its token will expire");
            }
        }
        self.review(task, pr, attempt, &[]).await
    }

    /// Closes a task whose PR has merged: records who merged it, which PR and the commit in the task's metadata, then
    /// finishes it with a handoff naming them, which unblocks whatever is chained behind it. The one way a merge
    /// finishes a task: reconcile uses it for merges made outside the factory, and the merger uses it right after
    /// `CodeHost::merge` (`docs/factory/lifecycle.md`). Metadata first, so a handoff that failed to land is retried
    /// with the record already there.
    pub async fn close_merged(&self, task: &Task, merged: Merged) -> Result<()> {
        // The task has left the merge path, so a queued branch update is no longer waited on (TRAIL-491).
        self.forget_merge_path(&task.key);
        let decision = match merged.by {
            MergedBy::Merger => self.tracker.show(&task.key).await?.run.merge,
            MergedBy::Human => None,
        };
        // A park made by the worker's sub-agent leaves the claim with it, and the factory's writes are refused until the
        // lease runs out (TRAIL-403): give it up first. A failure is logged, and the writes below say if it mattered.
        // The sub-agent is revoked by then, so the factory releases the claim as the agent above it.
        if task.holder.as_deref().is_some_and(|h| h.starts_with(&format!("{}/", self.tracker.agent())))
            && let Err(e) = self.tracker.release(&task.key).await
        {
            tracing::warn!(task = %task.key.0, error = %e, "couldn't release the sub-agent's claim before closing");
        }
        let merged_by_human = merged.by == MergedBy::Human;
        let pr_url = merged.pr.url.clone();
        let handoff = messages::merged_handoff(merged.by, &merged.pr.url, merged.commit.as_deref(), decision.as_ref());
        self.tracker.report(&task.key, RunPatch { merged: Patch::Set(merged), ..RunPatch::default() }).await?;
        // A parked task can't be finished without a claim: it is claimed where it is, never reopened first, so no other
        // agent can take it from open in between (TRAIL-402).
        // A park cut short after the unapproved merge was found: finish it, don't try to finish the task (TRAIL-729).
        if merged_by_human && task.run.unapproved_merge == Some(true) && task.status != Status::NeedsInput {
            return self.park_unapproved_merge(task, &pr_url).await;
        }
        if task.status == Status::NeedsInput {
            self.tracker.claim(&task.key).await?;
        }
        match self.tracker.finish(&task.key, &handoff).await {
            // A person merged work nobody approved, and core won't let the factory call it done: it isn't one of the
            // project's reviewers (TRAIL-1439), or its own identity sent the work to review. The merge is recorded, so
            // ask once for a person to mark it done rather than be refused again every pass (TRAIL-706).
            Err(Error::Forbidden(why)) if merged_by_human => {
                tracing::warn!(task = %task.key, %why, "the PR was merged unapproved; parking for a person to finish it");
                self.tracker
                    .report(&task.key, RunPatch { unapproved_merge: Patch::Set(true), ..RunPatch::default() })
                    .await?;
                return self.park_unapproved_merge(task, &pr_url).await;
            }
            finished => finished?,
        }
        tracing::info!(task = %task.key, %handoff, "PR merged; task done");
        Ok(())
    }

    /// Parks a task whose PR a person merged unapproved, from wherever a cut-short park left it: an approved task is
    /// unapproved, one in review or open is claimed as it stands (never reopened first: a failed claim then leaves it
    /// as it was, not open and unheld; TRAIL-826), then asked. A parked one is already claimed.
    async fn park_unapproved_merge(&self, task: &Task, pr_url: &str) -> Result<()> {
        if task.status == Status::Approved {
            self.tracker.unapprove(&task.key).await?;
        }
        if matches!(task.status, Status::InReview | Status::Approved | Status::Open) {
            self.tracker.claim(&task.key).await?;
        }
        self.tracker.ask(&task.key, &messages::merged_unapproved_question(pr_url)).await
    }

    /// A PR closed without merging: the task is parked with a question for a human, rather than reopened, since a
    /// rework would push to a PR nobody is going to merge. Reopening it after reopening the PR resumes as a rework.
    pub(crate) async fn park_closed(&self, task: &Task, pr: &PrRef) -> Result<()> {
        tracing::warn!(task = %task.key, pr = %pr.url, "the PR was closed without merging; parking");
        self.forget_merge_path(&task.key);
        // Claimed as it stands, whether open, in review or parked, never reopened first: if the claim then failed, an
        // open, unheld task could start a rework against this PR (TRAIL-216). An approved task can't be claimed, so it
        // is unapproved back to in review, still held, rather than passed through open.
        if task.status == Status::Approved {
            self.tracker.unapprove(&task.key).await?;
        }
        self.tracker.claim(&task.key).await?;
        let question = messages::pr_closed_question(&pr.url);
        self.tracker.ask(&task.key, &question).await
    }

    /// A PR that no longer merges cleanly with the base branch: a rework merges the base branch in and resolves the
    /// conflicts, then goes through the checks and review again. It counts toward `max_conflict_reworks`, not review
    /// rounds, so a PR that keeps conflicting is parked for a human without using up its reviews (TRAIL-199, TRAIL-221).
    pub(crate) async fn rework_conflict(&self, task: &Task, pr: &PrRef, attempt: u32) -> Result<()> {
        let round = task.run.conflict_reworks.unwrap_or(0) + 1;
        self.tracker.report(&task.key, RunPatch { conflict_reworks: Patch::Set(round), ..RunPatch::default() }).await?;
        let base = &self.config.repo.base_branch;
        let body = messages::conflict_feedback(&pr.url, base);
        tracing::warn!(task = %task.key, pr = %pr.url, round, "the PR conflicts with the base branch; reworking");
        let head = match &self.codehost {
            Some(codehost) => codehost.head(pr).await.ok(),
            None => None,
        };
        let requested = ChangesRequested {
            reviewer: "factory".into(),
            body,
            attempt,
            kind: RequestKind::Conflict,
            head,
            settled: false,
        };
        self.request_rework(task, requested, round, task.run.retries.unwrap_or(0)).await
    }

    /// CI finished and failed on the PR's current head, whether the worker's own commit or a merge of the base branch
    /// (a clean update keeps the verdict, so nothing else would look at it; TRAIL-390): the task, unapproved if it was,
    /// goes back for a rework told which checks failed and the end of their logs. It counts toward
    /// `max_conflict_reworks`, not review rounds, since CI breaking as main moves isn't a review finding.
    /// True when a rework was requested, false when there was nothing to do (no code host, or one already asked for).
    pub(crate) async fn rework_ci_failure(
        &self,
        task: &Task,
        pr: &PrRef,
        head: &str,
        checks: &[String],
    ) -> Result<bool> {
        let Some(codehost) = &self.codehost else { return Ok(false) };
        let attempt = task.run.attempt.unwrap_or(1);
        // Already asked for on this attempt: reconcile finishes reopening it, and the head isn't reworked twice.
        if task.run.pending_changes().is_some_and(|c| c.attempt == attempt) {
            return Ok(false);
        }
        let after_update = match &task.run.reviewed_head {
            Some(reviewed) if reviewed != head && codehost.carries_verdict(pr, reviewed).await? => {
                // Only CI seen passing on the reviewed head says the merge brought the failure in. A host that
                // can't be asked gets the softer wording.
                match codehost.ci_at(pr, reviewed).await {
                    Ok(CiState::Passed) => AfterUpdate::CiPassedBefore,
                    Ok(_) => AfterUpdate::CiUnconfirmed,
                    Err(e) => {
                        tracing::warn!(task = %task.key, error = %e, "couldn't read CI on the reviewed head");
                        AfterUpdate::CiUnconfirmed
                    }
                }
            }
            _ => AfterUpdate::No,
        };
        let logs = codehost.ci_logs(pr, checks).await.unwrap_or_else(|e| {
            tracing::warn!(task = %task.key, error = %e, "couldn't read the logs of the failed checks");
            Vec::new()
        });
        let base = &self.config.repo.base_branch;
        let body = messages::ci_failure_feedback(&pr.url, head, checks, after_update, &logs, base);
        if task.status == Status::Approved {
            self.tracker.unapprove(&task.key).await?;
        }
        let round = task.run.conflict_reworks.unwrap_or(0) + 1;
        self.tracker.report(&task.key, RunPatch { conflict_reworks: Patch::Set(round), ..RunPatch::default() }).await?;
        tracing::warn!(task = %task.key, pr = %pr.url, round, %head, "CI failed on the PR's head; reworking");
        let head = Some(head.to_string());
        let requested = ChangesRequested {
            reviewer: CI_REVIEWER.into(),
            body,
            attempt,
            kind: RequestKind::Ci,
            head,
            settled: false,
        };
        self.request_rework(task, requested, round, task.run.retries.unwrap_or(0)).await?;
        Ok(true)
    }

    /// Whether the task's PR was merged or closed, which is dealt with here: a merge, by whoever, finishes the task and
    /// a closed PR parks it.
    /// [`PrSettle::Settled`] when it did; when it's still open (or there's no PR to ask about), [`PrSettle::Open`]
    /// with whether it merges cleanly.
    async fn pr_settled(&self, task: &Task, done: &mut Reconciled) -> Result<PrSettle> {
        let (Some(pr), Some(codehost)) = (&task.run.pr, &self.codehost) else {
            return Ok(PrSettle::Open { mergeable: None, head: None });
        };
        match codehost.pr_state(pr).await? {
            PrState::Merged { commit } => {
                // A merge the factory didn't make is a person's, outside the factory; one the merger started but
                // didn't get to close (TRAIL-289) is still the merger's.
                let by = self.merged_by(task, pr).await?;
                self.close_merged(task, Merged { by, pr: pr.clone(), commit }).await?;
                done.merged.push(task.key.0.clone());
                Ok(PrSettle::Settled)
            }
            PrState::Closed => {
                self.park_closed(task, pr).await?;
                done.closed.push(task.key.0.clone());
                Ok(PrSettle::Settled)
            }
            PrState::Open { mergeable, head } => {
                // GitHub works `mergeable` out after a push, so right after a conflict rework a reading may still be
                // the old head's. While the PR shows the head from before the push, or the host hasn't answered, the
                // reading is unknown. An answer for any other head (the rework's, or one pushed since) settles the
                // mark, so a head that moves on can't leave it stuck (TRAIL-446).
                let Some(stale) = task.run.stale_head.as_deref() else {
                    return Ok(PrSettle::Open { mergeable, head });
                };
                if mergeable.is_none() || head.as_deref() == Some(stale) {
                    return Ok(PrSettle::Open { mergeable: None, head });
                }
                let settled = RunPatch { stale_head: Patch::Clear, ..RunPatch::default() };
                if let Err(e) = self.tracker.report(&task.key, settled).await {
                    tracing::warn!(task = %task.key, error = %e, "couldn't clear the stale head; reading it again");
                }
                Ok(PrSettle::Open { mergeable, head })
            }
        }
    }

    /// Puts one in-flight task right, by its phase; true when its PR is due a review. For a task whose run isn't
    /// going: one that is, the caller follows.
    pub(crate) async fn reconcile_task(&self, task: &Task, done: &mut Reconciled) -> Result<bool> {
        match phase(task, self.tracker.agent(), false) {
            Phase::Untouched | Phase::Closed => Ok(false),
            // A park for the reviewer's breach that was cut short: finish it rather than let a worker rerun the task.
            Phase::ReviewBreachUnparked { breach, claim } => {
                tracing::warn!(task = %task.key, "finishing the park for the reviewer's breach");
                self.park_review_breach(&task.key, breach.clone(), claim, task.run.retries.unwrap_or(0)).await?;
                done.breached.push(task.key.0.clone());
                Ok(false)
            }
            // The worker's breach is finished by the caller, which has the sandboxes; from anywhere else the task is
            // put back like any run that stopped.
            Phase::Running | Phase::WorkerBreachUnparked(_) | Phase::Interrupted => {
                self.requeue_interrupted(task, done).await?;
                Ok(false)
            }
            Phase::Requeued => {
                self.settle_requeued(task, done).await?;
                Ok(false)
            }
            Phase::Parked(parked) => {
                self.reconcile_parked(task, parked, done).await?;
                Ok(false)
            }
            Phase::Approved => {
                self.reconcile_approved(task, done).await?;
                Ok(false)
            }
            Phase::InReview(review) => self.reconcile_in_review(task, review, done).await,
        }
    }

    async fn requeue_interrupted(&self, task: &Task, done: &mut Reconciled) -> Result<()> {
        let Some(name) = &task.run.sandbox else { return Ok(()) };
        let sub = self.tracker.sub_agent(name);
        if task.holder.as_deref() == Some(sub.agent()) {
            sub.release(&task.key).await?;
        }
        if let Err(e) = self.tracker.revoke_sub_agent(name).await {
            tracing::warn!(task = %task.key, error = %e, "couldn't revoke the sub-agent; its token will expire");
        }
        done.requeued.push(task.key.0.clone());
        Ok(())
    }

    /// A PR merged or closed while the task waited for its rework is settled rather than reworked on a dead branch.
    async fn settle_requeued(&self, task: &Task, done: &mut Reconciled) -> Result<()> {
        if matches!(self.pr_settled(task, done).await?, PrSettle::Settled) {
            return Ok(());
        }
        if let Some(name) = &task.run.sandbox
            && let Err(e) = self.tracker.revoke_sub_agent(name).await
        {
            tracing::warn!(task = %task.key, error = %e, "couldn't revoke the sub-agent; its token will expire");
        }
        Ok(())
    }

    /// Parked for a human, so a PR still open or closed stays as it is (a closed one was the question). A merge by
    /// anyone finishes the task all the same, unless it is already recorded (merged by hand before approval, parked
    /// for a person to mark it done).
    async fn reconcile_parked(&self, task: &Task, parked: Parked<'_>, done: &mut Reconciled) -> Result<()> {
        // One already parked for a merge by hand waits for a person to finish it: don't ask again. A `merged`
        // record without the park is a close that failed before it, so it is retried (TRAIL-729).
        if !parked.awaiting_done
            && let (Some(pr), Some(codehost)) = (&task.run.pr, &self.codehost)
            && let PrState::Merged { commit } = codehost.pr_state(pr).await?
        {
            self.close_merged(task, Merged { by: MergedBy::Human, pr: pr.clone(), commit }).await?;
            done.merged.push(task.key.0.clone());
        }
        // A park whose settling write was lost: the request still stands at the limit it parked on. Finished
        // here, before a person can answer, so that answering without a push doesn't have it reworked again.
        if let Some(c) = parked.unsettled {
            let (count, max) = if c.kind.is_review() {
                (task.run.review_rounds, self.config.repo.max_review_rounds)
            } else {
                (task.run.conflict_reworks, self.config.repo.max_conflict_reworks)
            };
            if count.is_some_and(|n| n >= max) {
                tracing::warn!(task = %task.key, "finishing the park's settling of its changes request");
                self.settle_parked_request(task, c, task.run.retries.unwrap_or(0)).await;
            }
        }
        Ok(())
    }

    async fn reconcile_approved(&self, task: &Task, done: &mut Reconciled) -> Result<()> {
        let PrSettle::Open { mergeable, .. } = self.pr_settled(task, done).await? else { return Ok(()) };
        // Main may have moved since the approval: a PR that now conflicts goes back for a rework, which would
        // otherwise wait for a merge attempt (and never come, in shadow mode or at a grade that doesn't auto-merge).
        if let Some(pr) = &task.run.pr
            && mergeable == Some(false)
        {
            self.tracker.unapprove(&task.key).await?;
            let attempt = task.run.attempt.unwrap_or(1);
            self.rework_conflict(task, pr, attempt).await?;
            done.conflicted.push(task.key.0.clone());
            return Ok(());
        }
        self.evaluate_gates(&task.key).await
    }

    /// True when the PR is due a review.
    async fn reconcile_in_review(&self, task: &Task, review: Review<'_>, done: &mut Reconciled) -> Result<bool> {
        let PrSettle::Open { mergeable, head } = self.pr_settled(task, done).await? else { return Ok(false) };
        let attempt = task.run.attempt.unwrap_or(1);
        let mut standing = match review {
            Review::ReworkWaiting { request } => request,
            Review::RequestCutShort(request) => Some(request),
            Review::Unreviewed | Review::AwaitingGates => None,
        };
        // The head recorded with the request is sturdier than the counts, whose reset can be lost: a PR that has
        // moved since was pushed to by a person (TRAIL-338). It's only fetched for a request that stands: after a
        // normal rework the old one lingers until the next review, and asking the host about it each reconcile would
        // be wasted.
        if let (Some(c), Some(pr), Some(codehost)) = (standing, &task.run.pr, &self.codehost)
            && let Some(recorded) = &c.head
        {
            let current = match &head {
                Some(head) => head.clone(),
                None => codehost.head(pr).await?,
            };
            if current != *recorded {
                standing = None;
            }
        }
        if matches!(review, Review::ReworkWaiting { .. }) {
            // Waiting for a worker: the task is listed as ready, and the worker's handoff answers the request. One
            // that no longer stands (the PR was pushed to, or a person settled the park) is answered here, and the
            // PR is looked at as it is.
            let recorded = task.run.changes_requested.as_ref().filter(|c| c.attempt == attempt);
            let stale =
                recorded.is_some_and(|c| c.settled || task.run.pending_changes().is_some()) && standing.is_none();
            let (Some(pr), true) = (&task.run.pr, stale) else { return Ok(false) };
            self.tracker.handoff(&task.key, &messages::stale_request_handoff(&pr.url)).await?;
        }
        // Changes were requested on this attempt but never recorded as a review (the request was cut short): finish it.
        if let Some(requested) = standing {
            let round = if requested.kind.is_review() {
                task.run.review_rounds.unwrap_or(1)
            } else {
                task.run.conflict_reworks.unwrap_or(1)
            };
            self.request_rework(task, requested.clone(), round, task.run.retries.unwrap_or(0)).await?;
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
        // A push from outside the factory leaves the head differing from the one last reviewed; that wants a review
        // like a rework does, or the merger's agent_review gate would wait on it forever (TRAIL-298).
        let mut owed = unreviewed(task);
        if let (Some(pr), Some(codehost), Some(reviewed)) = (&task.run.pr, &self.codehost, &task.run.reviewed_head)
            && !owed
            // The head the host showed above is the one reviewed: nothing to ask.
            && head.as_ref() != Some(reviewed)
        {
            owed = !codehost.carries_verdict(pr, reviewed).await?;
        }
        let due = owed && self.setback_over(&task.key);
        // A review backed off after an infrastructure failure is still owed: approving now could pass a grade whose
        // gate doesn't ask for agent_review, and nothing would schedule the review afterwards (TRAIL-263).
        if !owed {
            // CI may have finished, or Lane approved, since the gates were last looked at.
            self.evaluate_gates(&task.key).await?;
        } else if self.rework_if_ci_failed(&task.key).await? {
            // A failed CI is reworked without waiting for the review, which would only look at a head that is about
            // to change.
            return Ok(false);
        } else if task.run.pending_changes().is_some_and(|c| c.kind.is_review()) {
            // The rework has pushed and its head awaits review: say so, without approving.
            self.say_gates(&task.key, false).await?;
        }
        Ok(due)
    }
}

impl Factory {
    /// Who merged a PR that is found merged: the merger when it recorded the intent to merge this very head
    /// (TRAIL-289), else a person. An intent for another head is stale: the host didn't merge it, and the PR was
    /// pushed to and merged by someone since (TRAIL-456).
    pub(crate) async fn merged_by(&self, task: &Task, pr: &PrRef) -> Result<MergedBy> {
        let Some(intent) = task.run.merging.as_deref() else { return Ok(MergedBy::Human) };
        let Some(codehost) = &self.codehost else { return Ok(MergedBy::Human) };
        Ok(if codehost.head(pr).await? == intent { MergedBy::Merger } else { MergedBy::Human })
    }
}
