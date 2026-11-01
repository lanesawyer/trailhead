use std::time::Duration;

use trailhead_factory::messages;
use trailhead_factory::{
    ChangesRequested, CheckResult, Error, PrRef, Result, Role, RunReport, RunRequest, RunResult, RunUpdate, Task,
    TaskKey, VerdictKind, review_name,
};

use crate::lifecycle::{Begin, Run, spec, token_ttl};
use crate::{Factory, Outcome};

impl Factory {
    /// Runs the reviewer on an open PR in its own fresh sandbox.
    pub async fn review(&self, task: &Task, pr: &PrRef, attempt: u32, checks: &[CheckResult]) -> Result<Outcome> {
        let shown = self.tracker.show(&task.key).await?;
        // Graded before anything starts, so a code host that fails here leaves the review to be tried again whole.
        let grade = self.grade_pr(&shown, pr, None).await?;
        if let Some(grade) = &grade {
            self.record_grade(&shown, grade).await?;
        }
        // Read before anything is created, so a tracker that fails here holds no sub-agent or sandbox.
        let mut context = self.tracker.human_context(&task.key).await?;
        // The follow-ups earlier rounds filed, so this one doesn't file a reworded copy. Not worth holding up the review.
        match self.tracker.follow_ups(&task.key).await {
            Ok(follow_ups) => context.follow_ups = follow_ups,
            Err(e) => {
                tracing::warn!(task = %task.key.0, error = %e, "couldn't list the task's follow-ups; reviewing without them")
            }
        }
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
                    // While the task is still in review, so the PR says the review asked for changes at any grade.
                    // Message only: a verdict that asks for changes mustn't approve, or merge on Lane's earlier approval.
                    let parks = round >= self.config.repo.max_review_rounds;
                    if let Err(e) = self.say_gates(&task.key, parks).await {
                        tracing::warn!(task = %task.key.0, error = %e, "couldn't evaluate the gates; the next reconcile retries");
                    }
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
                let error = Error::Other(messages::review_without_verdict(&report.result));
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
                let question = messages::review_breach_question(round, &reason);
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
        let held = self.setbacks.lock().unwrap().get(&task.key).map_or(0, |(n, _, _)| *n);
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
            let question = messages::review_infra_question(failures, max, &error.to_string());
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
    pub(crate) async fn request_rework(&self, task: &Task, requested: ChangesRequested, round: u32) -> Result<()> {
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
                messages::ci_rework_limit_question(round, max, &body)
            } else if conflict {
                messages::conflict_rework_limit_question(round, max)
            } else {
                messages::review_rework_limit_question(round, max, &reviewer, &body)
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
}

/// Who a rework asked for by failing CI is attributed to, in the worker's feedback and the park question.
pub(crate) const CI_REVIEWER: &str = "CI";

/// A reviewer's name: the round's, with the count of infrastructure retries so a retried round gets a fresh one.
pub(crate) fn reviewer_name(task: &TaskKey, attempt: u32, round: u32, retries: u32) -> String {
    match retries {
        0 => review_name(task, attempt, round),
        n => format!("{}-{n}", review_name(task, attempt, round)),
    }
}
