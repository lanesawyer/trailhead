use std::collections::HashSet;

use trailhead_factory::{
    ChangesRequested, Error, Merged, MergedBy, PrRef, PrState, Result, RunUpdate, Sandbox, Status, Task, TaskKey, label,
};

use crate::lifecycle::worker_name;
use crate::review::{CI_REVIEWER, reviewer_name};
use crate::run_loop::Due;
use crate::{Factory, Outcome, Reconciled};

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
    pub(crate) async fn reconcile_deferring(&self, running: &HashSet<TaskKey>) -> Result<(Reconciled, Due)> {
        let mut done = Reconciled::default();
        let mut due = Due::default();
        let live = self.compute.list(&self.base_labels()).await?;
        let tasks: Vec<Task> = self
            .tracker
            .in_flight(self.project(), &self.queue())
            .await?
            .into_iter()
            .filter(|t| !running.contains(&t.key))
            .collect();

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
    pub(crate) async fn resume_review(&self, task: &Task) -> Result<Outcome> {
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
            handoff.push_str(&format!(
                " Grade {}; gates passed: {}.",
                d.grade,
                d.passed.iter().map(|g| g.as_str()).collect::<Vec<_>>().join(", ")
            ));
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
    pub(crate) async fn park_closed(&self, task: &Task, pr: &PrRef) -> Result<()> {
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
    pub(crate) async fn rework_conflict(&self, task: &Task, pr: &PrRef, attempt: u32) -> Result<()> {
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
    pub(crate) async fn rework_ci_failure(&self, task: &Task, pr: &PrRef, head: &str, checks: &[String]) -> Result<()> {
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
        body.push_str(&format!(
            "\n\nThe base branch `{}` is already being merged into your checkout, uncommitted, so the changes you see \
             staged beyond your own are that merge: keep them, and don't discard or rebase them. Reproduce the \
             failure, fix it, run the checks and finish.",
            self.config.repo.base_branch
        ));
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
    pub(crate) async fn reconcile_task(&self, task: &Task, done: &mut Reconciled) -> Result<bool> {
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
                let owed = task.run.pr.is_some() && stale;
                let due = owed && self.setback_over(&task.key);
                // A review backed off after an infrastructure failure is still owed: approving now could pass a grade
                // whose gate doesn't ask for agent_review, and nothing would schedule the review afterwards (TRAIL-263).
                if !owed {
                    // CI may have finished, or Lane approved, since the gates were last looked at.
                    self.evaluate_gates(&task.key).await?;
                } else if task.run.pending_changes().is_some_and(|c| !c.conflict) {
                    // The rework has pushed and its head awaits review: say so, without approving.
                    self.say_gates(&task.key, false).await?;
                }
                return Ok(due);
            }
            _ => {}
        }
        Ok(false)
    }
}

/// Who merged a PR that is found merged: the merger when it recorded the intent before merging (TRAIL-289), else a person.
pub(crate) fn merged_by(task: &Task) -> MergedBy {
    if task.run.merging.as_deref().is_some_and(|h| !h.is_empty()) { MergedBy::Merger } else { MergedBy::Human }
}
