//! Everything the factory says to people: PR comments, task notes, the questions it parks a task with, and the text
//! of a rework. Pure functions from typed inputs to strings, shared by the control plane and the daemon, so the
//! wording lives in one place (and could be translated from one place, if it ever is). Out of scope: `tracing` logs,
//! harness prompts, and the CLI's and MCP's text.
//!
//! The functions are grouped by who reads the text: the PR, the task's notes, the questions a parked task waits on,
//! and the feedback a rework's worker is given.

use crate::{
    CheckResult, GateKind, Grade, MergeDecision, MergedBy, ModelId, PrGrade, TaskKey, Usd, Verdict, VerdictKind,
    raised_by,
};

// ---- On the PR ----

/// Where the changes the latest review asked for stand, said on the PR at every grade, since a grade whose gate
/// doesn't need the agent review would otherwise read as if nothing were wrong (TRAIL-388).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rework {
    /// The review of the PR's current head asked for changes.
    Running,
    /// Like `Running`, but the round limit is reached: the task is parked for Lane and no rework follows.
    Parked,
    /// The PR's head has moved since the review that asked for changes, and no review has judged the new head.
    Pushed,
}

/// What the merger knows beyond the decision itself when it words the PR comment.
#[derive(Clone, Copy)]
pub struct Wording {
    pub auto_merge: bool,
    /// The merger is on and will try to merge now.
    pub merging: bool,
    /// The PR is behind its base branch.
    pub behind: bool,
    pub changes: Option<Rework>,
}

/// What the merger says on the PR about its decision, e.g. `Grade 1: checks, CI and review passed; would merge
/// (shadow).` or `Grade 3 (Cargo.toml changed): waiting for a person.` `merging` is whether the merger is on and will
/// try to merge now; the message then says the merge happened, so it is only posted once it has.
pub fn decision_message(grade: &PrGrade, d: &MergeDecision, said: Wording) -> String {
    let message = gate_message(grade, d, &said);
    match said.changes {
        Some(Rework::Running) => format!("{message} The latest review asked for changes; a rework is running."),
        Some(Rework::Parked) => format!("{message} The latest review asked for changes; the task is parked for Lane."),
        Some(Rework::Pushed) => format!("{message} The latest review asked for changes; the rework has pushed."),
        None => message,
    }
}

fn gate_message(grade: &PrGrade, d: &MergeDecision, said: &Wording) -> String {
    let Wording { auto_merge, merging, behind, .. } = *said;
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
            match gate {
                GateKind::Ci => waiting.push("CI".to_string()),
                GateKind::AgentReview => waiting.push("review".into()),
                GateKind::Human => waiting.push("a person".into()),
                GateKind::Checks | GateKind::Quorum | GateKind::Named(_) => waiting.push(gate.to_string()),
            }
        }
        if !waiting.is_empty() {
            parts.push(format!("waiting for {}", join(waiting)));
        }
        return format!("{head}: {}.", parts.join("; "));
    }
    let passed = d.passed.iter().map(|g| match g {
        GateKind::Ci => "CI".to_string(),
        GateKind::AgentReview => "review".into(),
        GateKind::Human => "a person's approval".into(),
        GateKind::Checks | GateKind::Quorum | GateKind::Named(_) => g.to_string(),
    });
    let passed = join(passed.collect());
    match (&d.problem, d.would_merge, auto_merge) {
        (Some(problem), ..) => format!("{head}: {passed} passed, but {problem}."),
        _ if merging => format!("{head}: {passed} passed; merged."),
        (None, true, _) if behind => {
            format!("{head}: {passed} passed; would bring the branch up to date, then merge (shadow).")
        }
        (None, true, _) => format!("{head}: {passed} passed; would merge (shadow)."),
        (None, false, true) => format!("{head}: {passed} passed; a person merges."),
        (None, false, false) => format!("{head}: {passed} passed; not auto-merged at this grade, so a person merges."),
    }
}

/// A follow-up that is now a task: newly filed, or already open under that title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filed {
    pub key: TaskKey,
    pub title: String,
    pub existing: bool,
}

/// The reviewer's comment on the PR: its verdict and body, the grade as it will settle, and the follow-ups it filed.
pub fn review_comment(v: &Verdict, grade: Option<&PrGrade>, filed: &[Filed]) -> String {
    let verdict = match v.kind {
        VerdictKind::Approve => "Approve",
        VerdictKind::RequestChanges => "Request changes",
    };
    let model = v.model.as_ref().map(|m| format!(" (`{}`)", m.0)).unwrap_or_default();
    let mut comment = format!("🔍 **Review by `{}`{model} · Verdict: {verdict}**\n\n{}", v.reviewer, v.body);
    if let Some(grade) = grade {
        comment.push_str(&format!("\n\n{}", grade.describe()));
    }
    if !filed.is_empty() {
        comment.push_str("\n\nFollow-ups filed in Trailhead:");
        for f in filed {
            let already = if f.existing { " (already open)" } else { "" };
            comment.push_str(&format!("\n- `{}` {}{already}", f.key.0, f.title));
        }
    }
    comment
}

/// Everything the factory posts goes out under the connector's account (Lane's, until TRAIL-95), so posts say which
/// agent and model wrote them.
pub fn pr_footer(agent: &str, model: &ModelId, task: &TaskKey) -> String {
    format!("\n\n---\n🤖 Opened by the Trailhead factory · worker `{agent}` · `{}` · {}", model.0, task.0)
}

/// The warning a PR carries for each check that failed and then passed on a rerun. Empty when there are none;
/// otherwise it starts with a blank line, to follow the PR's summary.
pub fn flaky_checks(retried: &[String]) -> String {
    retried.iter().map(|name| format!("\n\n⚠️ Check `{name}` failed, then passed when rerun.")).collect()
}

/// The note a PR carries for the checks that were skipped because the change touches none of their paths, so a reader
/// doesn't take them for passes. Empty when none were; otherwise it starts with a blank line, like [`flaky_checks`].
pub fn skipped_checks(checks: &[CheckResult]) -> String {
    let names: Vec<String> = checks.iter().filter(|c| c.skipped).map(|c| format!("`{}`", c.name)).collect();
    if names.is_empty() {
        return String::new();
    }
    format!("\n\nℹ️ Skipped, not run: {}. The change touches none of the paths they are limited to.", names.join(", "))
}

/// What the merger says, on the PR and the task, when the code host refuses to merge.
pub fn merge_refused(why: &str) -> String {
    format!("The code host refused to merge: {why}. The merger tries again each pass.")
}

/// Why the merger can't act on a decision: it isn't allowed to move the task to approved.
pub fn cannot_approve(why: &str) -> String {
    format!("the factory can't move the task to approved ({why})")
}

/// Why an agent that isn't a delegated reviewer can't approve a task; it ends up in the merger's PR comment.
pub fn not_a_reviewer(agent: &str, task: &str, project: &str) -> String {
    format!(
        "{agent} can't approve {task}: it isn't a delegated reviewer, so a person has to list it as a reviewer of project {project}"
    )
}

/// What the run reports when the daemon couldn't finish it after the agent's work was over; it can reach a park question.
pub fn finishing_failed(error: &dyn std::fmt::Display) -> String {
    format!("finishing the run failed: {error}")
}

// ---- On the task ----

/// The reason a PR's grade carries when a person raised the task above what its files matched.
pub fn task_raised_reason(grade: Grade) -> String {
    format!("the task was raised to {grade}")
}

/// The note that says what raised the task's grade, and why.
pub fn grade_raised_note(grade: Grade, was: Grade, why: &str) -> String {
    format!("Raised to grade {grade} (task was {was}): {why}")
}

/// Why a task waits: every target is full, over its limit or unreachable (`docs/factory/targets.md`).
pub fn no_target_has_room(task: &str) -> String {
    format!("no target has room for {task}; it waits")
}

/// The note on a task the factory skips until someone grades it.
pub fn ungraded_note() -> String {
    "no grade; set one to queue it".to_string()
}

/// The note on a task the factory skips because its metadata doesn't parse. `fields` are the names of the offending
/// fields, if they are known.
pub fn bad_metadata_note(metadata_key: &str, fields: &[String], error: &str) -> String {
    let (in_fields, fix) = if fields.is_empty() {
        (String::new(), format!("fix or remove the `{metadata_key}` metadata"))
    } else {
        (format!(" in {}", fields.join(", ")), "fix or remove that field".to_string())
    };
    format!(
        "The factory is skipping this task: its `{metadata_key}` metadata is malformed{in_fields} ({error}). \
         To have the factory pick the task up again, {fix}."
    )
}

/// The handoff that closes a task whose PR merged: who merged it, which PR and commit, and for the merger's merges
/// the grade and the gates that passed.
pub fn merged_handoff(by: MergedBy, pr_url: &str, commit: Option<&str>, decision: Option<&MergeDecision>) -> String {
    let who = match by {
        MergedBy::Human => "Merged by a person on the code host",
        MergedBy::Merger => "Merged by the factory's merger",
    };
    let commit = commit.map_or_else(String::new, |c| format!(", as commit {c}"));
    let mut handoff = format!("{who}: {pr_url}{commit}.");
    if by == MergedBy::Merger
        && let Some(d) = decision
    {
        handoff.push_str(&format!(
            " Grade {}; gates passed: {}.",
            d.grade,
            d.passed.iter().map(|g| g.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    handoff
}

/// Why a run settled without work: its PR had already merged.
pub fn settled_merged(pr_url: &str) -> String {
    format!("{pr_url} was merged")
}

/// Why a run settled without work: its PR had been closed.
pub fn settled_closed(pr_url: &str) -> String {
    format!("{pr_url} was closed")
}

/// Why a run settled without work: the conflict its rework was for is gone.
pub fn settled_conflict_gone(pr_url: &str) -> String {
    format!("{pr_url} no longer conflicts with its base branch")
}

/// The handoff on a task whose worker pushed its PR for review, which answers the changes a review asked for.
pub fn pushed_for_review_handoff(pr_url: &str) -> String {
    format!("Pushed {pr_url} for review.")
}

/// The handoff on a task whose rework request no longer applies, because the PR was pushed to from outside the
/// factory since: the PR is looked at as it stands.
pub fn stale_request_handoff(pr_url: &str) -> String {
    format!(
        "The changes requested on {pr_url} no longer apply: it was pushed to since (or the request was settled by a \
         person), so it is reviewed as it stands."
    )
}

/// The handoff on a task sent back to review instead of reworked, since the host stopped reporting its conflict.
pub fn conflict_gone_note(pr_url: &str, base: &str) -> String {
    format!(
        "{pr_url} was sent for a conflict rework, but the host now says it merges cleanly with `{base}` (the \
         base moved back or the conflict was fixed), so there was nothing to resolve. No worker ran: the rework \
         request was dropped and the task went back to review."
    )
}

/// The note on a task whose worker gave up, kept as the failure the next attempt is told.
pub fn agent_gave_up(reason: &str) -> String {
    format!("The agent gave up: {reason}")
}

/// Whether a recorded failure is [`agent_gave_up`]'s, as opposed to failed checks, a lost merge or conflict markers,
/// which all come from an attempt that changed the checkout.
pub fn is_agent_gave_up(failure: &str) -> bool {
    failure.starts_with(&agent_gave_up(""))
}

/// How much of a failed check's output goes on the task.
const NOTE_TAIL: usize = 4000;

/// Why an attempt's checks failed, for the task, so a person or the next attempt can see it.
pub fn checks_failed_note(checks: &[CheckResult]) -> String {
    let mut note = String::from("The factory's checks failed, so nothing was published.\n");
    for c in checks.iter().filter(|c| !c.passed) {
        let out = c.output.trim_end();
        let mut start = out.len().saturating_sub(NOTE_TAIL);
        while !out.is_char_boundary(start) {
            start += 1;
        }
        note.push_str(&format!("\n`{}` failed; the end of its output:\n```\n{}\n```\n", c.name, &out[start..]));
    }
    note
}

/// The reason a run failed because a check did.
pub fn check_failed_reason(name: &str) -> String {
    format!("check {name} failed")
}

/// The reason, and the note, for a run whose merge of the base branch was abandoned.
pub fn merge_abandoned(base: &str) -> (String, String) {
    (
        format!("the merge of the base branch ({base}) was abandoned"),
        format!(
            "The merge of the base branch ({base}) was abandoned (aborted or rebased away), so the changes \
             don't include what landed there. Nothing was published; finish the merge and commit it.\n"
        ),
    )
}

/// The reason, and the note, for a run whose branch no longer builds on where it started.
pub fn branch_rewritten(start: &str) -> (String, String) {
    (
        format!("the branch was rewritten ({start} is no longer in its history)"),
        format!(
            "The branch was rewritten (rebased or reset), so it no longer builds on {start}, where the run \
             started. Nothing was published; merge the base branch instead of rebasing.\n"
        ),
    )
}

/// The reason, and the note, for a run that left conflict markers in `files`.
pub fn conflict_markers_left(files: &[String]) -> (String, String) {
    (
        format!("conflict markers left in {}", files.join(", ")),
        format!(
            "The merge of the base branch still has conflict markers (`<<<<<<<`/`>>>>>>>` lines) in: {}. \
             Nothing was published.\n",
            files.join(", ")
        ),
    )
}

/// The description of a task a reviewer filed as a follow-up: what it wrote, then where it came from.
pub fn follow_up_description(written: &str, source: &TaskKey, pr_url: Option<&str>) -> String {
    let mut description = written.trim().to_string();
    if !description.is_empty() {
        description.push_str("\n\n");
    }
    description.push_str(&raised_by(source));
    if let Some(url) = pr_url {
        description.push_str(&format!(" ({url})"));
    }
    description.push('.');
    description
}

// ---- Questions that park a task ----

/// Why a task is parked when a run overran its budget. `reason` is one of the `*_breach` texts.
pub fn budget_breach_question(reason: &str, sandbox: &str, state: BreachedSandbox) -> String {
    match state {
        BreachedSandbox::Suspended => {
            format!("Budget breached: {reason}. The sandbox {sandbox} is suspended; raise the budget or cancel.")
        }
        BreachedSandbox::Destroyed => format!(
            "Budget breached: {reason}. The sandbox {sandbox} couldn't be suspended and was destroyed, so the next run starts from a fresh checkout; raise the budget or cancel."
        ),
        BreachedSandbox::Stuck => format!(
            "Budget breached: {reason}. The sandbox {sandbox} couldn't be suspended or destroyed and is being torn down, so the next run starts from a fresh checkout; raise the budget or cancel."
        ),
    }
}

/// What became of a breached run's sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreachedSandbox {
    /// Cut off and kept suspended.
    Suspended,
    /// Couldn't be cut off, and was destroyed.
    Destroyed,
    /// Couldn't be cut off or destroyed; reconcile will tear it down.
    Stuck,
}

/// A run that spent more than its budget.
pub fn spend_breach(spent: Usd, limit: Usd) -> String {
    format!("spent {spent} of a {limit} budget")
}

/// A run that took more steps than its budget.
pub fn steps_breach(max_steps: u32) -> String {
    format!("took more than {max_steps} steps")
}

/// A run whose agent went quiet for too long.
pub fn stall_breach(quiet_secs: u64) -> String {
    format!("stalled: no activity from the agent for {quiet_secs} s")
}

/// A run that outlasted its time budget.
pub fn time_breach(minutes: u64) -> String {
    format!("ran past {minutes} minutes")
}

/// Why a task is parked when its attempts ran out. `last` is the last failure's note.
pub fn gave_up_question(failed: u32, max: u32, reason: &str, last: &str) -> String {
    format!(
        "Gave up after {failed} failed attempts (the limit is {max}). The last failure: \
         {reason}\n\n{last}\n\nFix the cause, then reopen the task; the attempt count starts again from \
         here. Or raise `max_attempts` to allow more tries."
    )
}

/// Why a task is parked when two attempts in a row gave up, which points at the environment, not the task.
pub fn repeated_failure_question(before: &str, reason: &str) -> String {
    format!(
        "Two attempts in a row gave up, so trying again is unlikely to help. The attempt before: {before}\n\n\
         The last attempt: {reason}\n\nIf the cause is in the environment (network, a missing tool, credentials), \
         fix it; otherwise say what to change. Then reopen the task; the attempt count starts again from here."
    )
}

/// Why a task is parked when the infrastructure kept failing under a worker.
pub fn infra_question(failures: u32, max: u32, error: &str) -> String {
    format!(
        "The infrastructure failed {failures} times in a row (the limit is {max}); it's not the task. The \
         last error: {error}\n\nFix the cause, then reopen the task; raise `max_infra_failures` to allow \
         more tries."
    )
}

/// Why a task is parked when its `setup` failed on a fresh run again and again: the base branch is broken.
pub fn setup_failed_question(failures: u32, error: &str) -> String {
    format!(
        "The repo's `setup` failed {failures} times in a row on a fresh checkout, so the base branch is probably \
         broken and another sandbox would fail the same way; it's not the infrastructure. The last error: \
         {error}\n\nFix the base branch, then reopen the task."
    )
}

/// Why a task is parked at once when a service refused a request for good, so retrying is no use.
pub fn rejected_question(error: &str) -> String {
    format!(
        "The infrastructure refused a request and won't answer differently on a retry, so the task is parked \
         at once; it's not the task. The error: {error}\n\nFix the cause (the compute settings, the sandbox or \
         the credentials), then reopen the task."
    )
}

/// Why a task is parked when its host stayed short of a resource (its disk) through every wait.
pub fn resource_wait_question(waits: u32, error: &str) -> String {
    format!(
        "The task has waited {waits} times in a row for a host with room, and none has freed up. The last \
         message: {error}\n\nFree the space (on the Docker host, prune images and volumes), then reopen the task."
    )
}

/// Why a task is parked when the infrastructure kept failing under its reviewer.
pub fn review_infra_question(failures: u32, max: u32, error: &str) -> String {
    format!(
        "The infrastructure failed {failures} times in a row while reviewing the PR (the limit is {max}); \
         it's not the task. The last error: {error}\n\nFix the cause, then reopen the task; raise \
         `max_infra_failures` to allow more tries."
    )
}

/// Why a task is parked when its reviewer kept ending without a verdict. The infrastructure ran; the reviewer
/// didn't call for one, which points at the model, the review prompt or the PR.
pub fn review_no_verdict_question(failures: u32, max: u32, error: &str) -> String {
    format!(
        "The reviewer ended without a verdict {failures} times in a row (the limit is {max}). The last \
         result: {error}\n\nLook at the reviewer's transcript and the review prompt: the model may not be \
         calling for a verdict, or the PR may be something the reviewer can't handle (too large, or \
         unreadable). Fix the cause, then reopen the task; raise `max_infra_failures` to allow more tries."
    )
}

/// The error recorded when a reviewer's run ended without a verdict. `result` is how it ended.
pub fn review_without_verdict(result: &dyn std::fmt::Debug) -> String {
    format!("the reviewer ended without a verdict: {result:?}")
}

/// Why a task is parked when its reviewer overran its budget.
pub fn review_breach_question(round: u32, reason: &str) -> String {
    format!(
        "The reviewer breached its budget in review round {round}: {reason}. The PR may be too big to \
         review within the reviewer's budget; split it or raise the reviewer budget, then reopen the task."
    )
}

/// Why a task is parked when CI keeps failing on its PR.
pub fn ci_rework_limit_question(round: u32, max: u32, last_failure: &str) -> String {
    format!(
        "CI keeps failing on the PR: {round} conflict reworks so far (the limit is {max}). The last \
         failure:\n\n{last_failure}\n\nFix the PR or settle what's landing, then reopen the task; raise \
         `max_conflict_reworks` to allow more."
    )
}

/// Why a task is parked when its PR keeps conflicting with the base branch.
pub fn conflict_rework_limit_question(round: u32, max: u32) -> String {
    format!(
        "The PR keeps conflicting with the base branch: {round} conflict reworks so far \
         (the limit is {max}). Merge the base branch into the PR yourself or settle what's landing, then \
         reopen the task; raise `max_conflict_reworks` to allow more."
    )
}

/// Why a task is parked when the reviewer keeps asking for changes.
pub fn review_rework_limit_question(round: u32, max: u32, reviewer: &str, last_review: &str) -> String {
    format!(
        "The reviewer still wants changes after {round} review rounds (the limit is {max}). The last \
         review, by {reviewer}:\n\n{last_review}\n\nFix the PR or settle the disagreement, then reopen the \
         task; raise `max_review_rounds` to allow more rounds."
    )
}

/// Why a task is parked when its PR was closed without merging.
pub fn pr_closed_question(pr_url: &str) -> String {
    format!(
        "The PR {pr_url} was closed without being merged. If the work is abandoned, cancel this task. To carry on, \
         reopen the PR on the code host and then reopen this task, and the next run continues on the PR's branch."
    )
}

/// Why a task is parked when its PR was merged by hand before anyone approved the work.
pub fn merged_unapproved_question(pr_url: &str) -> String {
    format!(
        "The PR {pr_url} was merged by hand before the work was approved, so the factory can't finish this task. Mark it done."
    )
}

/// Why a task is parked when the agent's work is done and checked but publishing it failed.
pub fn publish_failed_question(error: &dyn std::fmt::Display) -> String {
    format!(
        "The agent finished and the checks passed, but publishing to GitHub failed: {error}. Fix the \
         cause, then reopen the task."
    )
}

// ---- What a rework's worker is told ----

/// A review's feedback to the worker that reworks the PR.
pub fn review_feedback(reviewer: &str, body: &str) -> String {
    format!("Review by {reviewer}:\n\n{body}")
}

/// What a worker is told when the repo's `setup` failed on its checkout of the PR branch, so it can fix what broke.
pub fn setup_failed_feedback(output: &str) -> String {
    format!(
        "The repo's `setup` check failed on your checkout before you started, so the build is broken as it \
         stands. Fixing it is part of this task. The output:\n\n{output}"
    )
}

/// What a worker is told when its PR no longer merges cleanly with the base branch.
pub fn conflict_feedback(pr_url: &str, base: &str) -> String {
    format!(
        "{pr_url} no longer merges cleanly with `{base}`, which has moved on since the branch was cut. The base \
         branch is already being merged into your checkout, uncommitted and with conflict markers where it \
         clashes. Resolve every conflict, keeping what both sides intended; don't discard either side's changes \
         or rebase. Then run the checks and finish."
    )
}

/// What a worker is told when CI failed on its PR: the checks, the end of their logs, and `after_update` when the
/// base branch was merged in after the review (CI is not known to have passed before).
pub fn ci_failure_feedback(
    pr_url: &str,
    head: &str,
    checks: &[String],
    after_update: bool,
    logs: &[(String, String)],
    base: &str,
) -> String {
    let mut body = format!("CI failed on {pr_url} at {head}: {}.", checks.join(", "));
    if after_update {
        body.push_str(
            " The base branch was merged into the PR after it was reviewed, so the failure may have come in with \
             that merge: something that landed on the base branch may clash with this change. CI wasn't seen to \
             pass before the merge, so check whether the failure is your change's own too.",
        );
    }
    for (check, log) in logs {
        body.push_str(&format!("\n\nEnd of the log of {check}:\n\n```\n{}\n```", log.trim_end()));
    }
    body.push_str(&format!(
        "\n\nThe base branch `{base}` is already being merged into your checkout, uncommitted, so the changes you see \
         staged beyond your own are that merge: keep them, and don't discard or rebase them. If `{base}` has \
         moved on, the merge may have stopped with conflicts, leaving conflict markers in the files that clash: \
         resolve every one, keeping what both sides intended. Reproduce the failure, fix it, run the checks and \
         finish."
    ));
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_agent_task_and_project_when_it_cannot_approve() {
        let m = not_a_reviewer("factory/merger", "TRAIL-7", "TRAIL");
        assert_eq!(
            m,
            "factory/merger can't approve TRAIL-7: it isn't a delegated reviewer, so a person has to list it as a reviewer of project TRAIL"
        );
        assert_eq!(finishing_failed(&"boom"), "finishing the run failed: boom");
    }

    #[test]
    fn ci_failure_feedback_does_not_claim_ci_passed_before_an_update() {
        let plain = ci_failure_feedback("url", "abc", &["test".to_string()], false, &[], "main");
        assert!(!plain.contains("merged into the PR after"), "{plain}");
        let updated = ci_failure_feedback("url", "abc", &["test".to_string()], true, &[], "main");
        assert!(updated.contains("merged into the PR after it was reviewed"), "{updated}");
        assert!(!updated.contains("It passed"), "{updated}");
        assert!(updated.contains("conflict markers"), "{updated}");
    }

    #[test]
    fn bad_metadata_note_names_the_fix_for_the_fields_or_the_whole_value() {
        let fields = bad_metadata_note("factory", &["`attempt_base`".to_string()], "oops");
        assert!(fields.contains("malformed in `attempt_base` (oops)"), "{fields}");
        assert!(fields.ends_with("fix or remove that field."), "{fields}");
        let whole = bad_metadata_note("factory", &[], "oops");
        assert!(whole.ends_with("fix or remove the `factory` metadata."), "{whole}");
        assert!(!whole.contains("that field"), "{whole}");
    }

    fn decision() -> MergeDecision {
        MergeDecision {
            grade: Grade::ALL[0],
            head: "abc".into(),
            passed: vec![GateKind::Checks, GateKind::Ci, GateKind::AgentReview],
            waiting: vec![],
            would_merge: true,
            approved_head: None,
            human_head: None,
            problem: None,
            refused: None,
            comment: None,
            message: String::new(),
        }
    }

    fn grade(g: u8) -> PrGrade {
        let g = Grade::new(g).unwrap();
        PrGrade { grade: g, task: Some(g), reasons: vec![] }
    }

    fn said() -> Wording {
        Wording { auto_merge: true, merging: false, behind: false, changes: None }
    }

    #[test]
    fn decisions_read_as_sentences() {
        let d = decision();
        assert_eq!(
            decision_message(&grade(1), &d, said()),
            "Grade 1: checks, CI and review passed; would merge (shadow)."
        );
        assert_eq!(
            decision_message(&grade(1), &d, Wording { merging: true, ..said() }),
            "Grade 1: checks, CI and review passed; merged."
        );
        assert_eq!(
            decision_message(&grade(1), &d, Wording { behind: true, ..said() }),
            "Grade 1: checks, CI and review passed; would bring the branch up to date, then merge (shadow)."
        );
        assert!(
            decision_message(&grade(1), &d, Wording { changes: Some(Rework::Parked), ..said() })
                .ends_with("the task is parked for Lane.")
        );
        let waiting = MergeDecision { passed: vec![], waiting: vec![GateKind::Ci, GateKind::Human], ..d.clone() };
        assert_eq!(decision_message(&grade(3), &waiting, said()), "Grade 3: waiting for CI and a person.");
        let rework = Wording { changes: Some(Rework::Running), ..said() };
        assert!(
            decision_message(&grade(1), &d, rework)
                .ends_with(" The latest review asked for changes; a rework is running.")
        );
    }

    #[test]
    fn reviews_name_the_reviewer_and_the_follow_ups() {
        let v = Verdict {
            kind: VerdictKind::RequestChanges,
            body: "Fix it.".into(),
            reviewer: "factory/trail-1-a1-r1".into(),
            model: Some(ModelId("m".into())),
            raise_grade: None,
        };
        let filed = [Filed { key: TaskKey("TRAIL-9".into()), title: "Tidy".into(), existing: true }];
        assert_eq!(
            review_comment(&v, Some(&grade(2)), &filed),
            "🔍 **Review by `factory/trail-1-a1-r1` (`m`) · Verdict: Request changes**\n\nFix it.\n\nGrade 2\n\n\
             Follow-ups filed in Trailhead:\n- `TRAIL-9` Tidy (already open)"
        );
    }

    #[test]
    fn handoffs_and_notes() {
        assert_eq!(
            merged_handoff(MergedBy::Human, "https://x/pr/1", Some("abc"), None),
            "Merged by a person on the code host: https://x/pr/1, as commit abc."
        );
        assert_eq!(
            merged_handoff(MergedBy::Merger, "https://x/pr/1", None, Some(&decision())),
            "Merged by the factory's merger: https://x/pr/1. Grade 1; gates passed: checks, ci, agent_review."
        );
        assert_eq!(
            follow_up_description(" Why. ", &TaskKey("TRAIL-1".into()), Some("https://x/pr/1")),
            "Why.\n\nRaised by the reviewer of TRAIL-1 (https://x/pr/1)."
        );
        let failed = CheckResult { name: "fmt".into(), passed: false, output: "bad\n".into(), skipped: false };
        let passed = CheckResult { name: "ok".into(), passed: true, output: String::new(), skipped: false };
        assert_eq!(
            checks_failed_note(&[passed, failed]),
            "The factory's checks failed, so nothing was published.\n\n`fmt` failed; the end of its output:\n```\nbad\n```\n"
        );
        assert_eq!(flaky_checks(&["a".into()]), "\n\n⚠️ Check `a` failed, then passed when rerun.");
        assert!(no_target_has_room("TRAIL-1").contains("TRAIL-1"));
    }

    #[test]
    fn questions_say_what_to_raise() {
        assert!(budget_breach_question("r", "sb", BreachedSandbox::Destroyed).contains("was destroyed"));
        let stuck = budget_breach_question("r", "sb", BreachedSandbox::Stuck);
        assert!(stuck.contains("being torn down") && !stuck.contains("was destroyed"), "{stuck}");
        assert!(infra_question(3, 3, "boom").contains("`max_infra_failures`"));
        assert!(review_infra_question(3, 3, "boom").contains("while reviewing the PR"));
        let q = review_no_verdict_question(3, 3, "boom");
        assert!(q.contains("ended without a verdict") && !q.contains("infrastructure failed"));
        assert!(conflict_rework_limit_question(2, 2).contains("`max_conflict_reworks`"));
        assert!(ci_rework_limit_question(2, 2, "red").contains("CI keeps failing"));
        assert!(review_rework_limit_question(2, 2, "r", "b").contains("`max_review_rounds`"));
        assert!(gave_up_question(3, 3, "why", "last").contains("`max_attempts`"));
        assert_eq!(
            budget_breach_question("took more than 5 steps", "trail-1-a1", BreachedSandbox::Suspended),
            "Budget breached: took more than 5 steps. The sandbox trail-1-a1 is suspended; raise the budget or cancel."
        );
    }
}
