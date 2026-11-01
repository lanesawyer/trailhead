use trailhead_factory::{
    BranchUpdate, CiState, Error, MergeDecision, MergeMode, MergeOutcome, Merged, MergedBy, PrGrade, PrRef, PrState,
    Result, RunUpdate, Status, Task, TaskKey, VerdictKind,
};

use crate::Factory;
use crate::decide::{Facts, decide};
use trailhead_factory::messages::{self, Rework, Wording, decision_message};

impl Factory {
    /// The merger (`docs/factory/review.md#the-merger`): evaluates the grade's gates on the PR's current head and
    /// records the decision in the task's metadata. When they pass it moves a task in review to `approved`, unless the
    /// grade needs Lane, whose own approval is what passes that gate; a push after an approval sends the task back to
    /// review. In shadow mode it says what it would do on the PR whenever that changes. In `on` mode it also squash-merges
    /// an approved task whose grade auto-merges, or that Lane approved, and closes it as done.
    pub async fn evaluate_gates(&self, key: &TaskKey) -> Result<()> {
        self.evaluate(key, true, false).await
    }

    /// Like `evaluate_gates`, but only says on the PR where things stand: it neither approves nor merges. For when the
    /// task must stay as it is, a review owed (TRAIL-263) or changes just asked for (TRAIL-388).
    /// `parks` is for changes asked for that park the task for Lane instead of starting a rework.
    pub(crate) async fn say_gates(&self, key: &TaskKey, parks: bool) -> Result<()> {
        self.evaluate(key, false, parks).await
    }

    async fn evaluate(&self, key: &TaskKey, act: bool, parks: bool) -> Result<()> {
        let Some(codehost) = &self.codehost else { return Ok(()) };
        let task = self.tracker.show(key).await?;
        let Some(pr) = &task.run.pr else { return Ok(()) };
        if !matches!(task.status, Status::InReview | Status::Approved) {
            return Ok(());
        }
        // The head comes first and is shared with grading, which lists the files again only when it has moved.
        let ci = codehost.ci(pr).await?;
        let head = ci.head.clone();
        let Some(grade) = self.grade_pr(&task, pr, Some(&head)).await? else { return Ok(()) };
        self.record_grade(&task, &grade).await?;
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
            let given_on = approved_head.as_ref().or(human_head.as_ref()).cloned();
            // A clean merge of the base branch the host made since (the merger's own update) keeps the approval, Lane's
            // included, as it keeps the verdict; CI on the new head is the gate.
            let carried = match &given_on {
                Some(given) if *given != head => codehost.carries_verdict(pr, given).await?,
                _ => false,
            };
            if carried {
                tracing::info!(task = %key.0, %head, "the PR was only updated from its base; keeping the approval");
                approved_head = approved_head.map(|_| head.clone());
                human_head = human_head.map(|_| head.clone());
            } else if given_on.as_ref() != Some(&head) {
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
        let facts = Facts {
            act,
            merge_mode,
            require: &gate.require,
            auto_merge: gate.auto_merge,
            ci: &ci.state,
            verdict_approves,
            status,
            head: &head,
            approved_head: approved_head.as_deref(),
            human_head: human_head.as_deref(),
        };
        let verdict = decide(&facts);
        if verdict.withdraw {
            tracing::info!(task = %key.0, %head, "the gates no longer pass; withdrawing the approval");
            self.tracker.unapprove(key).await?;
            (approved_head, human_head) = (None, None);
        }
        let mut decision = MergeDecision {
            grade: grade.grade,
            head: head.clone(),
            passed: verdict.passed,
            waiting: verdict.waiting,
            failing: verdict.failing,
            would_merge: verdict.would_merge,
            approved_head,
            human_head,
            problem: None,
            refused: None,
            comment: prev.as_ref().and_then(|d| d.comment),
            message: String::new(),
        };
        // Whether a task can be approved by the control plane: the gates pass without Lane, who approves by moving it.
        if verdict.approve {
            decision.approved_head = Some(head.clone());
            // The mark goes down before the approval so it can't pass for Lane's. Not again while approving is known
            // to fail, which would write the task every pass.
            if prev.as_ref().is_none_or(|d| d.problem.is_none()) {
                self.tracker.report(key, RunUpdate { merge: Some(decision.clone()), ..RunUpdate::default() }).await?;
            }
            match self.tracker.approve(key).await {
                Ok(()) => {
                    tracing::info!(task = %key.0, %head, "gates passed; approved");
                }
                Err(Error::Forbidden(why)) => {
                    let problem = messages::cannot_approve(&why);
                    tracing::warn!(task = %key.0, %problem, "gates passed");
                    decision.approved_head = None;
                    decision.problem = Some(problem);
                }
                Err(e) => return Err(e),
            }
        }
        // Without the right to approve, the merger can't act, so it only says what it would do, as in shadow mode.
        let merging = verdict.merge && decision.problem.is_none();
        // Only the shadow merger's "would merge" says whether the PR is behind: it is the one that never acts on it.
        let behind = decision.would_merge && codehost.behind(pr).await?;
        let changes = match self.rework_state(&task, &head).await? {
            Some(Rework::Running) if parks => Some(Rework::Parked),
            changes => changes,
        };
        decision.message =
            decision_message(&grade, &decision, Wording { auto_merge: gate.auto_merge, merging, behind, changes });
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
                decision.comment = Some(self.say(pr, decision.comment, &decision.message).await?);
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

    /// Says `body` on the PR in the merger's one status comment: edits comment `id` when there is one, and posts a
    /// new one (returning its id) when there isn't or someone deleted it.
    async fn say(&self, pr: &PrRef, id: Option<u64>, body: &str) -> Result<u64> {
        let codehost = self.codehost.as_ref().expect("only called with a code host");
        if let Some(id) = id {
            match codehost.edit_comment(pr, id, body).await {
                Ok(()) => return Ok(id),
                Err(Error::NotFound(_)) => {
                    tracing::info!(pr = %pr.url, id, "the status comment is gone; posting a new one");
                }
                Err(e) => return Err(e),
            }
        }
        codehost.comment(pr, body).await
    }

    /// Whether the latest review asked for changes that the PR's `head` still hasn't answered.
    async fn rework_state(&self, task: &Task, head: &str) -> Result<Option<Rework>> {
        if task.run.reviewed_head.as_deref() == Some(head) {
            let asked =
                self.tracker.last_review(&task.key).await?.is_some_and(|v| v.kind == VerdictKind::RequestChanges);
            return Ok(asked.then_some(Rework::Running));
        }
        let pushed =
            task.run.pending_changes().is_some_and(|c| !c.conflict && c.head.as_deref().is_some_and(|h| h != head));
        Ok(pushed.then_some(Rework::Pushed))
    }

    /// Merges an approved PR whose decision says to: brings it up to date with the base branch first, and sends a
    /// conflict, or a host that refuses the merge, to a rework instead of forcing it. A merge closes the task the way
    /// a merge by Lane does. `merged_message` goes on the PR once the host has merged it.
    pub(crate) async fn merge(
        &self,
        task: &Task,
        pr: &PrRef,
        decision: &MergeDecision,
        merged_message: &str,
    ) -> Result<()> {
        let Some(codehost) = &self.codehost else { return Ok(()) };
        let attempt = task.run.attempt.unwrap_or(1);
        // Gates passed on this head; a push since then is looked at on the next pass.
        if codehost.ci(pr).await?.head != decision.head {
            return Ok(());
        }
        if codehost.behind(pr).await? {
            // An update asked for on this head is still queued: the head moves when it lands.
            if self.updating.lock().unwrap().get(&task.key) == Some(&decision.head) {
                tracing::debug!(task = %task.key.0, pr = %pr.url, "the branch update is still queued; waiting");
                return Ok(());
            }
            match codehost.update_branch(pr).await? {
                BranchUpdate::Conflict => {
                    self.updating.lock().unwrap().remove(&task.key);
                    self.rework_conflict(task, pr, attempt).await?
                }
                // CI starts over on the new head; the next pass looks at it again.
                BranchUpdate::Updated => {
                    self.updating.lock().unwrap().insert(task.key.clone(), decision.head.clone());
                }
                BranchUpdate::UpToDate => {}
            }
            return Ok(());
        }
        self.updating.lock().unwrap().remove(&task.key);
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
                if let Err(e) = self.say(pr, decision.comment, merged_message).await {
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
                let note = messages::merge_refused(&why);
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
    /// Given the PR's `head`, the files listed for that same head last time are reused.
    pub(crate) async fn grade_pr(&self, task: &Task, pr: &PrRef, head: Option<&str>) -> Result<Option<PrGrade>> {
        let Some(codehost) = &self.codehost else {
            return Ok(task.grade.map(|g| PrGrade { grade: g, task: Some(g), reasons: Vec::new() }));
        };
        let cached = head.and_then(|h| {
            self.listed.lock().unwrap().get(&task.key).filter(|(at, _)| at == h).map(|(_, files)| files.clone())
        });
        let files = match cached {
            Some(files) => files,
            None => {
                let files = codehost.files(pr).await?;
                if let Some(h) = head {
                    self.listed.lock().unwrap().insert(task.key.clone(), (h.to_string(), files.clone()));
                }
                files
            }
        };
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
            grade.reasons.push(messages::task_raised_reason(g));
        }
        Ok(Some(grade))
    }

    /// Writes a PR's grade to the task's metadata and, when it is above the task's own, raises the task to it with a
    /// note saying what did. Agents can only raise a grade, and only one a human has set.
    pub(crate) async fn record_grade(&self, task: &Task, grade: &PrGrade) -> Result<()> {
        let current = self.tracker.show(&task.key).await?;
        if let Some(have) = current.grade.filter(|have| grade.grade > *have) {
            self.tracker.raise_grade(&task.key, grade.grade).await?;
            let why = grade.reasons.join("; ");
            let note = messages::grade_raised_note(grade.grade, have, &why);
            tracing::info!(task = %task.key.0, "{note}");
            self.tracker.note(&task.key, &note).await?;
        }
        if current.run.grade.as_ref() != Some(grade) {
            self.tracker.report(&task.key, RunUpdate { grade: Some(grade.clone()), ..RunUpdate::default() }).await?;
        }
        Ok(())
    }
}
