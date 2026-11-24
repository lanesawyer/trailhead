use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use trailhead_factory::{
    BranchUpdate, ChangedFile, CiState, Error, HISTORY_LIMIT, MergeDecision, MergeMode, MergeOutcome, Merged, MergedBy,
    Patch, PrGrade, PrRef, PrState, Result, RunPatch, Said, Scope, Status, Task, TaskKey, VerdictKind,
};

use tokio::time::Instant;

use crate::Factory;
use crate::decide::{Facts, decide};
use trailhead_factory::messages::{self, Rework, Wording, decision_message};

/// What the host last said of whether a PR's head only carries the base into the head that was reviewed.
struct Carried {
    reviewed: String,
    head: String,
    carried: bool,
}

/// A branch update the merger asked the host for and is waiting on.
pub(crate) struct Update {
    /// The head it was asked on: the update has landed once the head moves.
    pub head: String,
    pub since: Instant,
    /// How many times it has been asked on this head.
    pub asks: u32,
}

/// What the merge path keeps between passes. In memory: a restart reads and asks again.
#[derive(Default)]
pub(crate) struct MergerState {
    /// The merge mode last read from the repo's file, kept when a later read fails.
    pub(crate) merge_mode: Mutex<MergeMode>,
    /// Why the merge mode last couldn't be re-read, while it still can't: said once, not on every merge decision
    /// (TRAIL-1691: a file an older server couldn't parse logged the same line 450 times in a few minutes).
    pub(crate) merge_mode_unread: Mutex<Option<String>>,
    /// The files last listed for each task's PR and the head they were listed at, so a pass that finds the head
    /// unchanged grades from them instead of asking the host again (TRAIL-266).
    listed: Mutex<HashMap<TaskKey, (String, Vec<ChangedFile>)>>,
    /// What the host last said about whether a task's PR head is only a clean merge of the base into the reviewed
    /// head: the answer for a (reviewed, head) pair doesn't change, so a pass's several questions about it (the
    /// stale check, the gates) cost one round trip (TRAIL-1220).
    carried: Mutex<HashMap<TaskKey, Carried>>,
    /// Tasks whose PR the merger asked the host to bring up to date. The host queues the merge (a 202), so until the
    /// head moves the PR is still behind and asking again would only queue another, unless the update never lands:
    /// after `UPDATE_PATIENCE` the merger asks again, up to `UPDATE_ASKS` times on one head, then reworks
    /// (TRAIL-490, TRAIL-621).
    updating: Mutex<HashMap<TaskKey, Update>>,
    /// The person's PR with auto-merge on that the merger is holding its merges for (TRAIL-804).
    pub(crate) held: Mutex<Option<crate::person::Held>>,
}

impl MergerState {
    /// The state a factory starts with: nothing kept, and the merge mode its config was read with.
    pub(crate) fn starting(mode: MergeMode) -> MergerState {
        MergerState { merge_mode: Mutex::new(mode), ..MergerState::default() }
    }

    /// Drops everything kept for a task that has left the merge path. A new per-task map is cleared here too.
    pub(crate) fn forget(&self, task: &TaskKey) {
        self.updating.lock().unwrap().remove(task);
        self.listed.lock().unwrap().remove(task);
        self.carried.lock().unwrap().remove(task);
    }

    /// Drops everything kept for the tasks not in `keep`.
    pub(crate) fn retain(&self, keep: &HashSet<&TaskKey>) {
        self.updating.lock().unwrap().retain(|k, _| keep.contains(k));
        self.listed.lock().unwrap().retain(|k, _| keep.contains(k));
        self.carried.lock().unwrap().retain(|k, _| keep.contains(k));
    }

    /// The files of a task's PR as last listed, if that was at `head`.
    fn listed_at(&self, task: &TaskKey, head: &str) -> Option<Vec<ChangedFile>> {
        self.listed.lock().unwrap().get(task).filter(|(at, _)| at == head).map(|(_, files)| files.clone())
    }

    /// Keeps the files a task's PR was listed with at `head`.
    fn list(&self, task: &TaskKey, head: &str, files: Vec<ChangedFile>) {
        self.listed.lock().unwrap().insert(task.clone(), (head.to_string(), files));
    }

    /// Whether any listing is kept for a task.
    pub(crate) fn lists(&self, task: &TaskKey) -> bool {
        self.listed.lock().unwrap().contains_key(task)
    }

    /// Whether anything is kept of what the host said of a task's verdict.
    pub(crate) fn carries(&self, task: &TaskKey) -> bool {
        self.carried.lock().unwrap().contains_key(task)
    }

    /// What the host said of this (reviewed, head) pair, if it was asked.
    fn carried(&self, task: &TaskKey, reviewed: &str, head: &str) -> Option<bool> {
        let kept = self.carried.lock().unwrap();
        kept.get(task).filter(|c| c.reviewed == reviewed && c.head == head).map(|c| c.carried)
    }

    /// Keeps what the host said of this (reviewed, head) pair.
    fn carry(&self, task: &TaskKey, reviewed: &str, head: &str, carried: bool) {
        let kept = Carried { reviewed: reviewed.to_string(), head: head.to_string(), carried };
        self.carried.lock().unwrap().insert(task.clone(), kept);
    }

    /// The branch update asked for on a task's PR and not yet seen to land.
    pub(crate) fn update_asked(&self, task: &TaskKey) -> Option<Update> {
        let kept = self.updating.lock().unwrap();
        kept.get(task).map(|u| Update { head: u.head.clone(), since: u.since, asks: u.asks })
    }

    /// Notes a branch update asked for on `head` just now, the `asks`th on it.
    fn ask_update(&self, task: &TaskKey, head: &str, asks: u32) {
        let asked = Update { head: head.to_string(), since: Instant::now(), asks };
        self.updating.lock().unwrap().insert(task.clone(), asked);
    }

    /// The task's PR is no longer waiting on an update: it landed, conflicted, or was given up on.
    fn update_over(&self, task: &TaskKey) {
        self.updating.lock().unwrap().remove(task);
    }
}

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

    /// Sends a PR whose CI failed on its current head back for a rework, and does nothing else: no approving, no
    /// merging, no word on the PR. For a task whose review is owed, whether backed off after an infrastructure failure
    /// (TRAIL-263) or due now, where the gates aren't evaluated but a failed CI needn't wait for the review. True only
    /// when a rework was requested (the caller's `task` is used as it is, not read again, TRAIL-1241); one already
    /// asked for on this attempt leaves the owed review to its usual handling.
    pub(crate) async fn rework_if_ci_failed(&self, task: &Task) -> Result<bool> {
        let Some(codehost) = &self.codehost else { return Ok(false) };
        let Some(pr) = &task.run.pr else { return Ok(false) };
        if !matches!(task.status, Status::InReview | Status::Approved) {
            return Ok(false);
        }
        let ci = codehost.ci(pr).await?;
        let CiState::Failed { checks } = &ci.state else { return Ok(false) };
        self.rework_ci_failure(task, pr, &ci.head, checks).await
    }

    /// Whether the PR's `head` is only a clean merge of the base branch into the `reviewed` head, so the verdict on
    /// `reviewed` stands. The answer for a (reviewed, head) pair is remembered per task, so asking again in the same
    /// pass, or in the next while the head hasn't moved, needn't go to the host (TRAIL-1220). Without a known `head`
    /// the host is asked every time. A failed ask isn't remembered.
    pub(crate) async fn carries_verdict(
        &self,
        key: &TaskKey,
        pr: &PrRef,
        reviewed: &str,
        head: Option<&str>,
    ) -> Result<bool> {
        let Some(codehost) = &self.codehost else { return Ok(false) };
        if let Some(head) = head
            && let Some(carried) = self.merger.carried(key, reviewed, head)
        {
            return Ok(carried);
        }
        let carried = codehost.carries_verdict(pr, reviewed).await?;
        if let Some(head) = head {
            self.merger.carry(key, reviewed, head, carried);
        }
        Ok(carried)
    }

    async fn evaluate(&self, key: &TaskKey, act: bool, parks: bool) -> Result<()> {
        let Some(codehost) = &self.codehost else { return Ok(()) };
        let task = self.tracker.show(key).await?;
        let Some(pr) = &task.run.pr else { return Ok(()) };
        if !matches!(task.status, Status::InReview | Status::Approved) {
            self.forget_merge_path(key);
            return Ok(());
        }
        // A task waiting on a rework is neither approved nor merged, whatever the gates say; nor is one a person took
        // the grade off where grading is what hands a task to the factory (TRAIL-1617).
        let act = act && !task.changes_requested && !self.lost_its_grade(&task).await;
        // The head comes first and is shared with grading, which lists the files again only when it has moved.
        let ci = codehost.ci(pr).await?;
        let head = ci.head.clone();
        let Some(grade) = self.grade_pr(&task, pr, Some(&head)).await? else { return Ok(()) };
        self.record_grade(&task, &grade).await?;
        if let CiState::Failed { checks } = &ci.state {
            return self.rework_ci_failure(&task, pr, &head, checks).await.map(|_| ());
        }
        let prev = task.run.merge.clone();
        let mut status = task.status;
        // The head each approval was given on: the control plane's own is marked before it moves the task, so an
        // approval without a mark is Lane's.
        let mut approved_head = prev.as_ref().and_then(|d| d.approved_head.clone());
        let mut human_head = prev.as_ref().and_then(|d| d.human_head.clone());
        let mut unapproved = false;
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
                Some(given) if *given != head => self.carries_verdict(&task.key, pr, given, Some(&head)).await?,
                _ => false,
            };
            if carried {
                tracing::info!(task = %key, %head, "the PR was only updated from its base; keeping the approval");
                approved_head = approved_head.map(|_| head.clone());
                human_head = human_head.map(|_| head.clone());
            } else if given_on.as_ref() != Some(&head) {
                tracing::info!(task = %key, %head, "the PR has a new head; sending the task back to review");
                self.tracker.unapprove(key).await?;
                unapproved = true;
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
                        && self.carries_verdict(key, pr, reviewed, Some(&head)).await?) =>
            {
                // The reviewer's verdict, not a rework the control plane asked for since: a conflict request that
                // was settled without a push leaves the review of this head standing (TRAIL-1468).
                self.tracker.last_review_by_others(key).await?.is_some_and(|v| v.kind == VerdictKind::Approve)
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
        let waits = crate::notify::waits_on_person(act, &verdict.waiting);
        if verdict.withdraw {
            tracing::info!(task = %key, %head, "the gates no longer pass; withdrawing the approval");
            self.tracker.unapprove(key).await?;
            unapproved = true;
            (approved_head, human_head) = (None, None);
        }
        // An intent to merge another head is stale (`merged_by` ignores it), and so is any once the approval is gone;
        // clearing it keeps the record from passing for a live merge.
        if task.run.merging.as_deref().is_some_and(|m| !m.is_empty() && (unapproved || m != head)) {
            self.tracker.report(key, RunPatch { merging: Patch::Clear, ..RunPatch::default() }).await?;
        }
        let mut decision = MergeDecision {
            grade: grade.grade,
            head: head.clone(),
            passed: verdict.passed,
            waiting: verdict.waiting,
            would_merge: verdict.would_merge,
            approved_head,
            human_head,
            problem: None,
            refused: None,
            comment: prev.as_ref().and_then(|d| d.comment),
            message: String::new(),
            history: prev.as_ref().map(|d| d.history.clone()).unwrap_or_default(),
            notified: prev.as_ref().is_some_and(|d| d.notified),
        };
        // Whether a task can be approved by the control plane: the gates pass without Lane, who approves by moving it.
        if verdict.approve {
            decision.approved_head = Some(head.clone());
            // The mark goes down before the approval so it can't pass for Lane's. Not again while approving is known
            // to fail, which would write the task every pass.
            if prev.as_ref().is_none_or(|d| d.problem.is_none()) {
                self.tracker.report(key, RunPatch::merge(decision.clone())).await?;
            }
            match self.tracker.approve(key).await {
                Ok(()) => {
                    tracing::info!(task = %key, %head, "gates passed; approved");
                }
                Err(Error::Forbidden(why)) => {
                    let problem = messages::cannot_approve(&why);
                    tracing::warn!(task = %key, %problem, "gates passed");
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
        decision.notified = self.tell_review(&task, pr, &grade, waits, decision.notified).await;
        if prev.as_ref() != Some(&decision) {
            if !merging && merge_mode != MergeMode::Off && prev.as_ref().is_none_or(|d| d.message != decision.message) {
                push_said(&mut decision.history, &decision.message);
                let body = messages::status_comment(&decision.message, &decision.history);
                decision.comment = Some(self.say(pr, decision.comment, &body).await?);
            }
            if decision.problem.is_some() && prev.as_ref().is_none_or(|d| d.problem != decision.problem) {
                self.tracker.note(key, decision.problem.as_deref().unwrap_or_default()).await?;
            }
            self.tracker.report(key, RunPatch::merge(decision.clone())).await?;
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
            // A request still open, whoever made it, or a reviewer's that was answered without a push.
            let asked = task.changes_requested
                || self
                    .tracker
                    .last_review_by_others(&task.key)
                    .await?
                    .is_some_and(|v| v.kind == VerdictKind::RequestChanges);
            return Ok(asked.then_some(Rework::Running));
        }
        // Only a later attempt than the reviewed one is the rework; a new head on the same attempt is a person's push.
        // And until the rework hands off, the request is still open, so a new head then is a person's push too.
        if task.changes_requested {
            return Ok(None);
        }
        let attempt = task.run.attempt.unwrap_or(0);
        let pushed = task
            .run
            .last_changes()
            .is_some_and(|c| c.kind.is_review() && attempt > c.attempt && c.head.as_deref().is_some_and(|h| h != head));
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
        // A person's PR with auto-merge on goes first; the hold is set by the pass over the open PRs (TRAIL-804).
        if self.merges_held() {
            tracing::debug!(task = %task.key, pr = %pr.url, "merges are held for a person's PR; waiting");
            return Ok(());
        }
        // Gates passed on this head; a push since then is looked at on the next pass.
        if codehost.ci(pr).await?.head != decision.head {
            return Ok(());
        }
        if codehost.behind(pr).await? {
            // An update asked for on this head is still queued: the head moves when it lands.
            let mut asked = 0;
            if let Some(Update { head, since, asks: times }) = self.merger.update_asked(&task.key)
                && head == decision.head
            {
                if since.elapsed() < crate::UPDATE_PATIENCE {
                    tracing::debug!(task = %task.key, pr = %pr.url, "the branch update is still queued; waiting");
                    return Ok(());
                }
                // A host that keeps dropping the update has likely found a conflict it only reports by dropping it.
                if times >= crate::UPDATE_ASKS {
                    tracing::warn!(task = %task.key, pr = %pr.url, times, "the branch update never landed; reworking");
                    self.merger.update_over(&task.key);
                    return self.rework_dropped_update(task, pr, attempt).await;
                }
                tracing::warn!(task = %task.key, pr = %pr.url, "the branch update never landed; asking again");
                asked = times;
            }
            match codehost.update_branch(pr).await? {
                BranchUpdate::Conflict => {
                    self.merger.update_over(&task.key);
                    self.rework_conflict(task, pr, attempt).await?;
                }
                // CI starts over on the new head; the next pass looks at it again.
                BranchUpdate::Updated => {
                    self.merger.ask_update(&task.key, &decision.head, asked + 1);
                }
                BranchUpdate::UpToDate => {}
            }
            return Ok(());
        }
        self.merger.update_over(&task.key);
        // A person may have set the task aside, or taken its grade off, since the gates were read: look once more
        // before merging what is no longer the factory's (TRAIL-1617).
        let current = self.tracker.show(&task.key).await?;
        if current.status != Status::Approved || self.lost_its_grade(&current).await {
            tracing::info!(task = %task.key, status = ?current.status, "the task changed under the merge; not merging");
            return Ok(());
        }
        // Recorded first: if closing the task fails after the merge, reconcile still knows the merger did it.
        self.tracker.report(&task.key, RunPatch::merging(decision.head.clone())).await?;
        let outcome = codehost.merge(pr, &decision.head).await;
        // An error may follow a merge the host accepted, so it keeps the intent and reconcile's `pr_state` decides.
        if matches!(outcome, Ok(MergeOutcome::Conflict | MergeOutcome::HeadMoved | MergeOutcome::Refused(_))) {
            self.tracker.report(&task.key, RunPatch { merging: Patch::Clear, ..RunPatch::default() }).await?;
        }
        match outcome? {
            MergeOutcome::Merged => {
                // The task's record says what the PR says; the decision was recorded with the earlier message.
                let mut merged = MergeDecision { message: merged_message.to_string(), ..decision.clone() };
                push_said(&mut merged.history, merged_message);
                let body = messages::status_comment(&merged.message, &merged.history);
                // Said before recording, so a comment `say` had to post anew is the one the record keeps.
                match self.say(pr, decision.comment, &body).await {
                    Ok(id) => merged.comment = Some(id),
                    Err(e) => {
                        tracing::warn!(task = %task.key, pr = %pr.url, error = %e, "couldn't comment that the PR merged");
                    }
                }
                if let Err(e) = self.tracker.report(&task.key, RunPatch::merge(merged)).await {
                    tracing::warn!(task = %task.key, error = %e, "couldn't record the merged message");
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
                tracing::info!(task = %task.key, pr = %pr.url, "the PR moved before the merge; looking again next pass");
                Ok(())
            }
            MergeOutcome::Refused(why) => {
                // Said once per reason, on the PR and the task; the merge is still tried on each pass, quietly, since
                // the host lifts a refusal (a check that passes) without anything else changing.
                if decision.refused.as_ref() == Some(&why) {
                    tracing::debug!(task = %task.key, pr = %pr.url, %why, "the code host still refuses the merge");
                    return Ok(());
                }
                tracing::warn!(task = %task.key, pr = %pr.url, %why, "the code host refused the merge; trying again each pass");
                let note = messages::merge_refused(&why);
                codehost.comment(pr, &note).await?;
                self.tracker.note(&task.key, &note).await?;
                let decision = MergeDecision { refused: Some(why), ..decision.clone() };
                self.tracker.report(&task.key, RunPatch::merge(decision)).await
            }
        }
    }

    /// Whether `task` has no grade where only graded tasks are the factory's (under a queue, or in a pointed project):
    /// a person took it off, so the task isn't the factory's to approve or merge. Unsure (the projects can't be
    /// listed) counts as lost.
    async fn lost_its_grade(&self, task: &Task) -> bool {
        if task.grade.is_some() {
            return false;
        }
        match self.scopes().await {
            Ok(scopes) => Scope::of(&scopes, &task.key).is_none_or(Scope::needs_grade),
            Err(e) => {
                tracing::warn!(task = %task.key, error = %e, "couldn't list the projects to see whether an ungraded task is the factory's");
                true
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
        let cached = head.and_then(|h| self.merger.listed_at(&task.key, h));
        let files = match cached {
            Some(files) => files,
            None => {
                let files = codehost.files(pr).await?;
                if let Some(h) = head {
                    self.merger.list(&task.key, h, files.clone());
                }
                files
            }
        };
        // A task still at the grade an earlier review raised it to keeps the grade it started at, and that reasons.
        let recorded = task.run.grade.as_ref().filter(|r| task.grade == Some(r.grade));
        let started = recorded.map_or(task.grade, |r| r.task);
        let path = self.path_of(&task.key);
        let mut grade = self.config.grades.grader().within(path.as_deref()).grade(started, &files);
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
            tracing::info!(task = %task.key, "{note}");
            self.tracker.note(&task.key, &note).await?;
        }
        if current.run.grade.as_ref() != Some(grade) {
            self.tracker.report(&task.key, RunPatch::grade(grade.clone())).await?;
        }
        Ok(())
    }
}

/// Adds `message`, said now, to a status comment's `history`, which keeps only the latest `HISTORY_LIMIT`.
fn push_said(history: &mut Vec<Said>, message: &str) {
    let at = jiff::Timestamp::now().strftime("%Y-%m-%d %H:%M UTC").to_string();
    history.push(Said { at, message: message.to_string() });
    let extra = history.len().saturating_sub(HISTORY_LIMIT);
    history.drain(..extra);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_keeps_only_the_latest_eight_oldest_first() {
        let mut history = Vec::new();
        for n in 1..=HISTORY_LIMIT + 3 {
            push_said(&mut history, &format!("status {n}"));
        }
        let messages: Vec<&str> = history.iter().map(|s| s.message.as_str()).collect();
        let expected: Vec<String> = (4..=HISTORY_LIMIT + 3).map(|n| format!("status {n}")).collect();
        assert_eq!(history.len(), HISTORY_LIMIT);
        assert_eq!(messages, expected);
    }
}
