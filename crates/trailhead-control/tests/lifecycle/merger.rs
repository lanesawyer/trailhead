use std::sync::Arc;

use trailhead_control::Outcome;
use trailhead_factory::{
    CiState, CodeHost, Commit, MergedBy, NewPr, PrRef, RateLimit, Role, RunOutcome, Status, Tracker,
};
use trailhead_testkit::Script;

use crate::common::*;

const SHADOW: &str = r#"
[gates.1]
require = ["checks", "ci", "agent_review"]
auto_merge = true

[gates.3]
require = ["checks", "ci", "agent_review", "human"]
auto_merge = false

[merge]
mode = "shadow"
"#;

/// What was said on the PR after the review's own comment.
fn said_count(r: &Rig) -> usize {
    said(r).len()
}

#[tokio::test]
async fn shadow_mode_waits_for_ci_then_approves_and_says_each_change_once() {
    let (r, queue) = queued_rig_with(SHADOW);
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    assert_eq!(r.tracker.task(&key).status, Status::InReview);
    assert_eq!(said(&r), ["Grade 1: waiting for CI."]);

    r.factory.reconcile().await.unwrap();
    r.factory.reconcile().await.unwrap();
    assert_eq!(said(&r).len(), 1, "an unchanged decision isn't said again");

    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    r.factory.reconcile().await.unwrap();
    assert_eq!(said(&r), ["Grade 1: checks, CI and review passed; would merge (shadow)."], "edited in place");
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::Approved);
    let decision = task.run.merge.expect("recorded");
    assert_eq!((decision.grade, decision.would_merge), (gr(1), true));
    assert_eq!(
        decision.passed,
        [
            trailhead_factory::GateKind::Checks,
            trailhead_factory::GateKind::Ci,
            trailhead_factory::GateKind::AgentReview
        ]
    );
    assert!(decision.waiting.is_empty());
    assert!(!r.codehost.prs()[0].merged, "shadow mode never merges");

    // Lane merges on GitHub and the task is closed as done.
    let head = CodeHost::head(&r.codehost, &pr).await.unwrap();
    r.codehost.merge(&pr, &head).await.unwrap();
    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.merged, std::slice::from_ref(&key.0));
    assert_eq!(r.tracker.task(&key).status, Status::Done);
}

#[tokio::test]
async fn a_merge_the_merger_started_but_didnt_close_is_still_the_mergers() {
    let (r, queue) = queued_rig_with(SHADOW);
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::Approved);

    // The merger recorded its intent and the host merged, but closing the task failed.
    let head = CodeHost::head(&r.codehost, &pr).await.unwrap();
    let intent = trailhead_factory::RunUpdate { merging: Some(head.clone()), ..Default::default() };
    Tracker::report(&r.tracker, &key, intent).await.unwrap();
    r.codehost.merge(&pr, &head).await.unwrap();
    r.factory.reconcile().await.unwrap();
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::Done);
    assert_eq!(task.run.merged.expect("recorded").by, MergedBy::Merger);
    let handoff = r.tracker.comments(&key).into_iter().rev().find(|c| c.kind == "handoff").expect("a handoff").body;
    assert!(handoff.contains(" Grade 1; gates passed: "), "{handoff}");
}

#[tokio::test]
async fn the_pr_says_the_review_asked_for_changes_even_where_agent_review_is_advisory() {
    let (r, queue) = queued_rig_with(SHADOW);
    let key = r.tracker.add_subtask(&queue, "Do it", "");
    r.tracker.set_grade(&key, 4);
    r.harness.push(Role::Worker, Script::new(changes_to("docs/a.md")));
    r.harness.push(Role::Reviewer, Script::new(request_changes("Add a test.")));
    r.factory.tick().await.unwrap();
    let said = said(&r);
    assert!(
        said.iter()
            .any(|m| m.starts_with("Grade 4:")
                && m.ends_with("The latest review asked for changes; a rework is running.")),
        "{said:?}"
    );
}

#[tokio::test]
async fn the_pr_says_the_rework_has_pushed_before_the_new_head_is_reviewed() {
    let (r, queue) = queued_rig_with(SHADOW);
    let key = r.tracker.add_subtask(&queue, "Do it", "");
    r.tracker.set_grade(&key, 4);
    r.harness.push(Role::Worker, Script::new(changes_to("docs/a.md")));
    r.harness.push(Role::Reviewer, Script::new(request_changes("Add a test.")));
    let ran = r.factory.tick().await.unwrap();
    let Outcome::Reviewed { pr, .. } = &ran[0].1 else { panic!("{:?}", ran[0].1) };
    // The rework pushes and the task is back in review, its new head not yet reviewed.
    Tracker::report(&r.tracker, &key, trailhead_factory::RunUpdate { attempt: Some(2), ..Default::default() })
        .await
        .unwrap();
    r.codehost.push_to(pr);
    r.tracker.set_status(&key, Status::InReview);
    Tracker::handoff(&r.tracker, &key, "Pushed the rework.").await.unwrap();
    r.factory.reconcile().await.unwrap();
    let said = said(&r);
    assert!(said.last().unwrap().ends_with("The latest review asked for changes; the rework has pushed."), "{said:?}");
    assert_eq!(r.tracker.task(&key).status, Status::InReview, "saying it doesn't approve");
    r.factory.reconcile().await.unwrap();
    assert_eq!(said_count(&r), said.len(), "and is said once");
}

#[tokio::test]
async fn a_push_by_a_person_is_not_credited_to_the_rework() {
    let (r, queue) = queued_rig_with(SHADOW);
    let key = r.tracker.add_subtask(&queue, "Do it", "");
    r.tracker.set_grade(&key, 4);
    r.harness.push(Role::Worker, Script::new(changes_to("docs/a.md")));
    r.harness.push(Role::Reviewer, Script::new(request_changes("Add a test.")));
    let ran = r.factory.tick().await.unwrap();
    let Outcome::Reviewed { pr, .. } = &ran[0].1 else { panic!("{:?}", ran[0].1) };
    // A person pushes before any rework ran: the attempt is still the one that was reviewed.
    r.codehost.push_to(pr);
    r.tracker.set_status(&key, Status::InReview);
    Tracker::handoff(&r.tracker, &key, "Pushed the rework.").await.unwrap();
    r.factory.reconcile().await.unwrap();
    let said = said(&r);
    assert_eq!(said, ["Grade 4: waiting for CI and a person."], "the push moved the PR, so no rework is mentioned");
}

#[tokio::test]
async fn a_grade_three_pr_waits_for_lane_who_approves_it() {
    let (r, queue) = queued_rig_with(SHADOW);
    let (key, pr) = reviewed(&r, &queue, 1, "Cargo.toml").await;
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    assert_eq!(
        said(&r),
        ["Grade 3 (task was 1; Cargo.toml changed): waiting for a person."],
        "one comment, edited as the decision changed"
    );
    assert_eq!(r.tracker.task(&key).status, Status::InReview, "the factory doesn't approve for Lane");

    r.tracker.approve_as_human(&key);
    r.factory.reconcile().await.unwrap();
    r.factory.reconcile().await.unwrap();
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::Approved);
    assert_eq!(
        said(&r),
        [
            "Grade 3 (task was 1; Cargo.toml changed): checks, CI, review and a person's approval passed; not auto-merged at this grade, so a person merges."
        ]
    );
    let decision = task.run.merge.unwrap();
    assert!(!decision.would_merge);
    assert_eq!(decision.approved_head, None, "it was Lane's approval, not the factory's");
    assert_eq!(decision.human_head.as_deref(), Some(decision.head.as_str()));
}

#[tokio::test]
async fn an_approval_on_an_old_head_doesnt_count() {
    let (r, queue) = queued_rig_with(SHADOW);
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::Approved);

    // Someone pushes: the approval is withdrawn, and the review of the old head doesn't cover the new one.
    r.codehost.push_to(&pr);
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::InReview);
    assert_eq!(said(&r).last().unwrap(), "Grade 1: waiting for CI and review.");
    r.codehost.set_ci(&pr, CiState::Passed);

    // The next reconcile sees a head nobody reviewed, and reviews it (TRAIL-298).
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.reviewed, std::slice::from_ref(&key.0));
    assert_eq!(r.tracker.task(&key).status, Status::Approved);
    assert!(
        said(&r).iter().any(|m| m == "Grade 1: checks, CI and review passed; would merge (shadow)."),
        "{:?}",
        said(&r)
    );
}

#[tokio::test]
async fn the_factory_withdraws_its_approval_when_gates_stop_passing_on_the_same_head() {
    let (r, queue) = queued_rig_with(SHADOW);
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::Approved);

    // CI running again on the same head is waited for, not a reason to withdraw.
    r.codehost.set_ci(&pr, CiState::Pending);
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::Approved);

    // Lane raises the grade to one that needs a person.
    r.tracker.set_grade(&key, 3);
    r.factory.reconcile().await.unwrap();
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::InReview);
    assert_eq!(task.run.merge.unwrap().approved_head, None);
}

#[tokio::test]
async fn failed_ci_sends_the_task_back_for_a_rework_told_what_failed() {
    let (r, queue) = queued_rig_with(SHADOW);
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.set_ci(&pr, CiState::Failed { checks: vec!["lint".into(), "test".into()] });
    r.codehost.set_ci_logs(&pr, vec![("test".into(), "assertion failed: left == right".into())]);
    r.factory.reconcile().await.unwrap();
    let task = r.tracker.task(&key);
    assert_eq!((task.status, task.changes_requested), (Status::InReview, true));
    let wanted = task.run.changes_requested.expect("the rework is told why");
    assert!(wanted.conflict, "counted as a conflict rework, not a review round");
    assert!(
        wanted.body.contains("lint, test") && wanted.body.contains("assertion failed: left == right"),
        "{wanted:?}"
    );
    assert!(!wanted.body.contains("base branch was merged"), "{wanted:?}");
    assert!(wanted.body.contains("is already being merged into your checkout, uncommitted"), "{wanted:?}");
    assert_eq!(task.run.conflict_reworks, Some(1));
    assert_eq!(task.run.review_rounds, Some(1), "review rounds are left alone");
}

#[tokio::test]
async fn a_ci_failure_rework_merges_the_base_though_the_pr_merges_cleanly() {
    let (r, queue) = queued_rig_with(SHADOW);
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.set_ci(&pr, CiState::Failed { checks: vec!["test".into()] });
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::InReview);

    // The host says the PR merges cleanly (TRAIL-243), but the feedback promises the merge, so the checkout has it.
    r.harness.push(Role::Worker, Script::new(changes_to("docs/a.md")));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();
    let prepared = r.daemon.prepared();
    let rework: Vec<_> =
        prepared.iter().filter(|(id, p)| p.branch.is_some() && id.0.starts_with("trailhead-w")).collect();
    assert_eq!(rework.len(), 1, "{prepared:?}");
    assert!(rework[0].1.merge_base, "{:?}", rework[0].1);
}

#[tokio::test]
async fn red_ci_after_a_branch_update_unapproves_and_reworks_saying_the_merge_brought_it() {
    let (r, queue) = queued_rig_with(SHADOW);
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::Approved);

    // Lane clicks Update branch: a clean merge keeps the verdict, then CI on the merge commit fails.
    r.codehost.set_behind(&pr, true);
    r.codehost.update_branch(&pr).await.unwrap();
    r.codehost.set_ci(&pr, CiState::Pending);
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::Approved, "the approval survives the update; CI is waited for");
    assert!(r.tracker.task(&key).run.changes_requested.is_none());

    r.codehost.set_ci(&pr, CiState::Failed { checks: vec!["test".into()] });
    r.factory.reconcile().await.unwrap();
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::InReview);
    let wanted = task.run.changes_requested.expect("the rework is told why");
    assert!(wanted.body.contains("test") && wanted.body.contains("base branch was merged"), "{wanted:?}");
}

#[tokio::test]
async fn red_ci_after_a_branch_update_says_so_definitely_only_when_ci_passed_on_the_reviewed_head() {
    for (passed, definite) in [(true, true), (false, false)] {
        let (r, queue) = queued_rig_with(SHADOW);
        let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
        r.codehost.set_ci(&pr, CiState::Passed);
        r.factory.reconcile().await.unwrap();
        let reviewed_head = r.tracker.task(&key).run.reviewed_head.expect("reviewed");
        if passed {
            r.codehost.set_ci_at(&pr, &reviewed_head, CiState::Passed);
        }
        r.codehost.set_behind(&pr, true);
        r.codehost.update_branch(&pr).await.unwrap();
        r.codehost.set_ci(&pr, CiState::Failed { checks: vec!["test".into()] });
        r.factory.reconcile().await.unwrap();
        let wanted = r.tracker.task(&key).run.changes_requested.expect("the rework is told why");
        assert_eq!(wanted.body.contains("CI passed on the PR before"), definite, "{wanted:?}");
        assert_eq!(wanted.body.contains("may have come in"), !definite, "{wanted:?}");
    }
}

#[tokio::test]
async fn pending_ci_is_not_reworked() {
    let (r, queue) = queued_rig_with(SHADOW);
    let (key, _) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.factory.reconcile().await.unwrap();
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::InReview);
    assert!(task.run.changes_requested.is_none());
}

#[tokio::test]
async fn failed_ci_parks_the_task_at_the_conflict_rework_cap() {
    let (r, queue) = queued_rig_with(SHADOW);
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.tracker
        .report(&key, trailhead_factory::RunUpdate { conflict_reworks: Some(4), ..Default::default() })
        .await
        .unwrap();
    r.codehost.set_ci(&pr, CiState::Failed { checks: vec!["test".into()] });
    r.factory.reconcile().await.unwrap();
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput);
    assert_eq!(task.run.conflict_reworks, Some(0), "a fresh count once a human reopens it");
    let question = r.tracker.comments(&key).into_iter().find(|c| c.kind == "question").expect("a question");
    assert!(
        question.body.contains("CI keeps failing") && question.body.contains("max_conflict_reworks"),
        "{}",
        question.body
    );
}

#[tokio::test]
async fn an_answered_ci_park_is_reworked_with_the_base_merged_in() {
    let (r, queue) = queued_rig_with(SHADOW);
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.tracker
        .report(&key, trailhead_factory::RunUpdate { conflict_reworks: Some(4), ..Default::default() })
        .await
        .unwrap();
    r.codehost.set_ci(&pr, CiState::Failed { checks: vec!["test".into()] });
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::NeedsInput);
    assert_eq!(r.tracker.task(&key).run.pending_changes(), None, "a person's answer settles the request");

    // They answer without pushing, and the task goes back to a worker, which still needs to know what it was for.
    r.tracker.set_status(&key, Status::Open);
    r.harness.push(Role::Worker, Script::new(changes_to("docs/a.md")));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();
    let prepared = r.daemon.prepared();
    let rework: Vec<_> =
        prepared.iter().filter(|(id, p)| p.branch.is_some() && id.0.starts_with("trailhead-w")).collect();
    assert_eq!(rework.len(), 1, "{prepared:?}");
    assert!(rework[0].1.merge_base, "{:?}", rework[0].1);
}

#[tokio::test]
async fn without_delegated_reviewer_rights_the_factory_says_so_and_only_records() {
    let (r, queue) = queued_rig_with(SHADOW);
    r.tracker.set_delegated("factory", false);
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    r.factory.reconcile().await.unwrap();
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::InReview);
    let decision = task.run.merge.unwrap();
    assert_eq!(decision.approved_head, None);
    assert!(decision.problem.as_deref().unwrap().contains("isn't a delegated reviewer"), "{decision:?}");
    assert!(said(&r).last().unwrap().contains("passed, but the factory can't move the task to approved"));
    assert_eq!(notes(&r, &key).len(), 1, "said once");
}

#[tokio::test]
async fn with_merge_mode_off_gates_are_recorded_and_tasks_approved_but_the_pr_is_left_alone() {
    let (r, queue) = queued_rig();
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::Approved);
    assert!(!r.tracker.task(&key).run.merge.unwrap().would_merge);
    assert!(said(&r).is_empty());
}

fn merging() -> String {
    SHADOW.replace("shadow", "on")
}

#[tokio::test]
async fn a_merge_that_does_not_happen_is_not_announced_until_it_does() {
    let (r, queue) = queued_rig_with(&merging());
    let (_key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.set_behind(&pr, true);
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    // Only brought up to date: nothing merged, so nothing says "merged".
    assert!(!r.codehost.prs()[0].merged);
    assert!(said(&r).iter().all(|m| !m.contains("merged")), "{:?}", said(&r));
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    r.factory.reconcile().await.unwrap();
    assert!(r.codehost.prs()[0].merged);
    let merged: Vec<_> = said(&r).into_iter().filter(|m| m.contains("merged")).collect();
    assert_eq!(merged, ["Grade 1: checks, CI and review passed; merged."]);
}

#[tokio::test]
async fn merge_mode_on_merges_an_approved_grade_one_pr_and_closes_the_task() {
    let (r, queue) = queued_rig_with(&merging());
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    assert_eq!(said(&r), ["Grade 1: checks, CI and review passed; merged."]);
    assert!(r.codehost.prs()[0].merged);
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::Done);
    let merged = task.run.merged.expect("recorded");
    assert_eq!((merged.by, merged.pr), (MergedBy::Merger, pr.clone()));
    assert_eq!(task.run.merge.expect("decision").message, "Grade 1: checks, CI and review passed; merged.");
    let handoff = r.tracker.comments(&key).into_iter().rev().find(|c| c.kind == "handoff").expect("a handoff").body;
    assert!(handoff.contains(&pr.url) && handoff.contains("gates passed: checks, ci, agent_review"), "{handoff}");
}

#[tokio::test]
async fn a_merge_whose_close_failed_is_closed_by_the_next_pass_as_the_mergers() {
    let (r, queue) = queued_rig_with(&merging());
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.set_ci(&pr, CiState::Passed);
    r.tracker.fail_next("factory", "finish", 1);
    // The merger writes its intent and the host merges, but closing the task fails.
    let _ = r.factory.reconcile().await;
    assert!(r.codehost.prs()[0].merged);
    assert_ne!(r.tracker.task(&key).status, Status::Done);
    assert!(r.tracker.task(&key).run.merging.as_deref().is_some_and(|h| !h.is_empty()), "the intent is kept");

    r.factory.reconcile().await.unwrap();
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::Done);
    assert_eq!(task.run.merged.expect("recorded").by, MergedBy::Merger);
}

#[tokio::test]
async fn switching_merge_mode_off_in_the_source_stops_the_merger_without_a_restart() {
    let (mut r, queue) = queued_rig_with(&merging());
    let mode = Arc::new(std::sync::Mutex::new(Ok(trailhead_factory::MergeMode::On)));
    let source = mode.clone();
    r.factory = r.factory.with_merge_mode_source(Box::new(move || source.lock().unwrap().clone()));
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.set_ci(&pr, CiState::Passed);
    *mode.lock().unwrap() = Ok(trailhead_factory::MergeMode::Off);
    r.factory.reconcile().await.unwrap();
    assert!(!r.codehost.prs()[0].merged);
    assert_eq!(r.tracker.task(&key).status, Status::Approved);
    // A source that can't be read keeps the last mode.
    *mode.lock().unwrap() = Err("gone".into());
    r.factory.reconcile().await.unwrap();
    assert!(!r.codehost.prs()[0].merged);
    *mode.lock().unwrap() = Ok(trailhead_factory::MergeMode::On);
    r.factory.reconcile().await.unwrap();
    assert!(r.codehost.prs()[0].merged);
}

#[tokio::test]
async fn a_conflict_at_merge_reworks_the_pr_instead_of_forcing_it() {
    let (r, queue) = queued_rig_with(&merging());
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.set_conflicts(true);
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    assert!(!r.codehost.prs()[0].merged);
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::InReview, "waiting in review for a rework");
    assert!(task.run.pending_changes().is_some_and(|c| c.conflict));
}

#[tokio::test]
async fn a_merge_the_host_refuses_is_tried_again_not_reworked() {
    let (r, queue) = queued_rig_with(&merging());
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.refuse_merge(Some("Required status check \"test\" is expected."));
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    assert!(!r.codehost.prs()[0].merged);
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::Approved, "still approved, not sent to a rework");
    assert!(task.run.pending_changes().is_none());
    let why = "Required status check \"test\" is expected.";
    assert_eq!(task.run.merge.as_ref().and_then(|d| d.refused.as_deref()), Some(why));
    let said = |r: &Rig| r.codehost.prs()[0].comments.iter().filter(|c| c.contains("refused to merge")).count();
    let noted = |r: &Rig| r.tracker.comments(&key).iter().filter(|c| c.body.contains("refused to merge")).count();
    assert_eq!((said(&r), noted(&r)), (1, 1));

    // Still refused for the same reason: tried again, but not said again.
    r.factory.reconcile().await.unwrap();
    assert_eq!((said(&r), noted(&r)), (1, 1));

    // A different reason is said again, and is the one the decision remembers.
    let other = "Changes must be made through a merge queue.";
    r.codehost.refuse_merge(Some(other));
    r.factory.reconcile().await.unwrap();
    assert_eq!((said(&r), noted(&r)), (2, 2));
    assert_eq!(r.tracker.task(&key).run.merge.as_ref().and_then(|d| d.refused.as_deref()), Some(other));

    r.codehost.refuse_merge(None);
    r.factory.reconcile().await.unwrap();
    assert!(r.codehost.prs()[0].merged);
    assert_eq!(r.tracker.task(&key).status, Status::Done);
}

#[tokio::test]
async fn a_refusal_stands_for_the_head_it_was_given_on() {
    let (r, queue) = queued_rig_with(&merging());
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    let why = "Required status check \"test\" is expected.";
    r.codehost.refuse_merge(Some(why));
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    let decision = |r: &Rig| r.tracker.task(&key).run.merge.expect("a merge decision");
    let said = |r: &Rig| r.codehost.prs()[0].comments.iter().filter(|c| c.contains("refused to merge")).count();
    let noted = |r: &Rig| r.tracker.comments(&key).iter().filter(|c| c.body.contains("refused to merge")).count();
    let first = decision(&r);
    assert_eq!(first.refused.as_deref(), Some(why));
    assert_eq!((said(&r), noted(&r)), (1, 1));

    // The merger's own branch update moves the head but keeps the approval, so it is still merging on the new
    // head. The refusal was for the old one, so the same reason is said again once the host refuses again.
    r.codehost.set_behind(&pr, true);
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::Approved);
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    let second = decision(&r);
    assert_ne!(second.head, first.head, "the update moved the head");
    assert_eq!(second.refused.as_deref(), Some(why));
    assert_eq!((said(&r), noted(&r)), (2, 2));
    assert!(!r.codehost.prs()[0].merged);

    // A push isn't the merger's update: it doesn't carry the verdict, so the task goes back to review and
    // there is no refusal left to remember.
    r.codehost.push_to(&pr);
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.tracker.task(&key).run.merge.and_then(|d| d.refused), None);
    assert!(!r.codehost.prs()[0].merged);
}

#[tokio::test]
async fn a_merge_the_host_refused_is_not_the_mergers_when_a_person_merges_it_later() {
    let (r, queue) = queued_rig_with(&merging());
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.refuse_merge(Some("Required status check \"test\" is expected."));
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    assert!(!r.codehost.prs()[0].merged);
    assert_eq!(r.tracker.task(&key).run.merging.as_deref(), Some(""), "the intent is cleared");

    r.codehost.refuse_merge(None);
    let head = CodeHost::head(&r.codehost, &pr).await.unwrap();
    r.codehost.merge(&pr, &head).await.unwrap();
    r.factory.reconcile().await.unwrap();
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::Done);
    assert_eq!(task.run.merged.expect("recorded").by, MergedBy::Human);
}

#[tokio::test]
async fn a_pr_behind_its_base_is_updated_before_it_is_merged() {
    let (r, queue) = queued_rig_with(&merging());
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.set_behind(&pr, true);
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    assert!(!r.codehost.prs()[0].merged, "not merged while behind");
    assert_ne!(r.tracker.task(&key).status, Status::Done);
}

#[tokio::test]
async fn the_mergers_own_update_keeps_the_verdict_and_no_second_review_runs() {
    let (r, queue) = queued_rig_with(&merging());
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    let reviewed_head = r.tracker.task(&key).run.reviewed_head.expect("reviewed");
    r.codehost.set_behind(&pr, true);
    r.codehost.set_ci(&pr, CiState::Passed);
    // Nothing is scripted for a reviewer, so a second review would fail.
    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(r.codehost.update_requests(), 1, "the merger updated the branch");
    assert!(done.reviewed.is_empty(), "{done:?}");

    r.codehost.set_ci(&pr, CiState::Passed);
    let done = r.factory.reconcile().await.unwrap();
    assert!(done.reviewed.is_empty(), "the update didn't send the PR back to review: {done:?}");
    assert_eq!(r.tracker.task(&key).run.reviewed_head.as_deref(), Some(reviewed_head.as_str()));
    assert!(r.codehost.prs()[0].merged, "merged on the new head with the old verdict");
    assert_eq!(r.tracker.task(&key).status, Status::Done);
}

#[tokio::test]
async fn a_pr_whose_head_hasnt_moved_is_not_listed_again_for_grading() {
    let (r, queue) = queued_rig_with(&merging());
    let (_key, _pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.factory.reconcile().await.unwrap();
    let listed = r.codehost.file_listings();
    for _ in 0..3 {
        r.factory.reconcile().await.unwrap();
    }
    assert_eq!(r.codehost.file_listings(), listed, "same head, same files");
}

#[tokio::test]
async fn an_update_the_host_has_queued_is_not_asked_for_again() {
    let (r, queue) = queued_rig_with(&merging());
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.queue_updates(true);
    r.codehost.set_behind(&pr, true);
    r.codehost.set_ci(&pr, CiState::Passed);
    for _ in 0..3 {
        r.factory.reconcile().await.unwrap();
    }
    assert_eq!(r.codehost.update_requests(), 1, "asked once while the head hasn't moved");
    assert!(!r.codehost.prs()[0].merged);

    r.codehost.land_updates(&pr).await;
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.codehost.update_requests(), 2);
    assert!(r.codehost.prs()[0].merged);
    assert_eq!(r.tracker.task(&key).status, Status::Done);
}

#[tokio::test(start_paused = true)]
async fn an_update_that_never_lands_is_asked_for_again_after_the_patience() {
    let (r, queue) = queued_rig_with(&merging());
    let (_key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.queue_updates(true);
    r.codehost.set_behind(&pr, true);
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.codehost.update_requests(), 1);

    tokio::time::advance(trailhead_control::UPDATE_PATIENCE + std::time::Duration::from_secs(1)).await;
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.codehost.update_requests(), 2, "asked again once the wait outlasted the patience");
}

#[tokio::test(start_paused = true)]
async fn an_update_the_host_keeps_dropping_is_sent_to_rework() {
    let (r, queue) = queued_rig_with(&merging());
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.queue_updates(true);
    r.codehost.set_behind(&pr, true);
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    for _ in 1..trailhead_control::UPDATE_ASKS {
        tokio::time::advance(trailhead_control::UPDATE_PATIENCE + std::time::Duration::from_secs(1)).await;
        r.factory.reconcile().await.unwrap();
    }
    assert_eq!(r.codehost.update_requests(), trailhead_control::UPDATE_ASKS as usize);
    assert_eq!(r.tracker.task(&key).run.conflict_reworks, None);

    tokio::time::advance(trailhead_control::UPDATE_PATIENCE + std::time::Duration::from_secs(1)).await;
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.codehost.update_requests(), trailhead_control::UPDATE_ASKS as usize, "no further ask");
    assert_eq!(r.tracker.task(&key).run.conflict_reworks, Some(1), "sent to a conflict rework");
}

#[tokio::test]
async fn shadow_mode_says_it_would_bring_a_behind_pr_up_to_date_first() {
    let (r, queue) = queued_rig_with(SHADOW);
    let (_key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.set_behind(&pr, true);
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    r.factory.reconcile().await.unwrap();
    assert_eq!(
        said(&r).last().unwrap(),
        "Grade 1: checks, CI and review passed; would bring the branch up to date, then merge (shadow)."
    );
    assert_eq!(r.codehost.update_requests(), 0, "shadow mode changes nothing");

    r.codehost.set_behind(&pr, false);
    r.factory.reconcile().await.unwrap();
    assert_eq!(said(&r).last().unwrap(), "Grade 1: checks, CI and review passed; would merge (shadow).");
}

#[tokio::test(start_paused = true)]
async fn a_conflict_resolving_rework_doesnt_inherit_the_verdict() {
    let (r, queue) = queued_rig_with(&merging());
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.set_conflicts(true);
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::InReview, "waiting in review for a rework");

    // The rework pushes a single commit that merges the base: a merge the host didn't make, holding the resolution.
    let resolution = Commit { message: "fix: resolve".into(), changes: vec![], merges: vec!["base".into()] };
    r.harness
        .push(Role::Worker, Script::new(RunOutcome::Changes { commits: vec![resolution], summary: "Done.".into() }));
    r.harness.push(Role::Reviewer, Script::new(RunOutcome::Failed { reason: "control plane died".into() }));
    r.factory.tick().await.unwrap();
    r.codehost.set_ci(&pr, CiState::Passed);

    // Reviews of the resolution then fail on the infrastructure, so the passes wait out a backoff and evaluate the
    // gates meanwhile.
    r.daemon.fail_runs(3, "connection lost");
    for _ in 0..3 {
        let done = r.factory.reconcile().await.unwrap();
        assert!(done.reviewed.is_empty() && done.merged.is_empty(), "{done:?}");
    }
    assert!(!r.codehost.prs()[0].merged, "the old approval doesn't cover the resolution");
    assert_ne!(r.tracker.task(&key).status, Status::Done);
}

#[tokio::test]
async fn a_push_on_top_of_a_host_made_update_needs_a_new_review() {
    let (r, queue) = queued_rig_with(&merging());
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.set_ci(&pr, CiState::Passed);
    r.codehost.set_behind(&pr, true);
    r.factory.reconcile().await.unwrap();
    r.codehost.push_to(&pr);
    r.codehost.set_ci(&pr, CiState::Passed);
    // The first pass withdraws the approval of the old head; the next sees a head nobody reviewed.
    r.factory.reconcile().await.unwrap();
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.reviewed, std::slice::from_ref(&key.0));
}

#[tokio::test]
async fn lane_approving_a_grade_three_task_gets_it_merged() {
    let (r, queue) = queued_rig_with(&merging());
    let (key, pr) = reviewed(&r, &queue, 1, "Cargo.toml").await;
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    assert!(!r.codehost.prs()[0].merged, "waits for Lane");
    r.tracker.approve_as_human(&key);
    r.factory.reconcile().await.unwrap();
    assert!(r.codehost.prs()[0].merged);
    assert_eq!(r.tracker.task(&key).status, Status::Done);
}

#[tokio::test]
async fn a_host_made_update_keeps_lanes_approval_of_a_grade_three_task() {
    let (r, queue) = queued_rig_with(&merging());
    let (key, pr) = reviewed(&r, &queue, 1, "Cargo.toml").await;
    r.codehost.set_ci(&pr, CiState::Passed);
    r.tracker.approve_as_human(&key);
    r.codehost.set_behind(&pr, true);
    r.factory.reconcile().await.unwrap();
    assert!(!r.codehost.prs()[0].merged, "updated, not merged, while CI starts over");

    r.codehost.set_ci(&pr, CiState::Passed);
    for _ in 0..3 {
        r.factory.reconcile().await.unwrap();
    }
    assert!(r.codehost.prs()[0].merged);
    assert_eq!(r.tracker.task(&key).status, Status::Done);
}

#[tokio::test]
async fn a_push_on_top_of_a_host_made_update_unapproves_lanes_approval_of_a_grade_three_task() {
    let (r, queue) = queued_rig_with(&merging());
    let (key, pr) = reviewed(&r, &queue, 1, "Cargo.toml").await;
    r.codehost.set_ci(&pr, CiState::Passed);
    r.tracker.approve_as_human(&key);
    r.codehost.set_behind(&pr, true);
    r.factory.reconcile().await.unwrap();
    assert!(!r.codehost.prs()[0].merged, "updated, not merged, while CI starts over");

    r.codehost.push_to(&pr);
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();

    assert!(!r.codehost.prs()[0].merged);
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::InReview, "Lane's approval doesn't cover a push on top of the update");
    assert_eq!(task.run.merge.unwrap().human_head, None);
}

#[tokio::test]
async fn a_push_after_lanes_approval_of_a_grade_three_task_unapproves_it() {
    let (r, queue) = queued_rig_with(&merging());
    let (key, pr) = reviewed(&r, &queue, 1, "Cargo.toml").await;
    r.tracker.approve_as_human(&key);
    // CI is still running, so the first pass only marks the approval as Lane's, given on this head.
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::Approved);
    let old = r.tracker.task(&key).run.merge.unwrap();
    assert_eq!(old.human_head.as_deref(), Some(old.head.as_str()), "Lane's approval is marked on the old head");
    r.codehost.push_to(&pr);
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();

    assert!(!r.codehost.prs()[0].merged);
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::InReview, "Lane's approval was of the old head");
    assert_eq!(task.run.merge.unwrap().human_head, None);
}

#[tokio::test]
async fn merge_mode_on_without_reviewer_rights_falls_back_to_saying_what_it_would_do() {
    let (r, queue) = queued_rig_with(&merging());
    r.tracker.set_delegated("factory", false);
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    r.factory.reconcile().await.unwrap();
    assert!(!r.codehost.prs()[0].merged);
    assert_eq!(r.tracker.task(&key).status, Status::InReview);
    assert!(said(&r).last().unwrap().contains("passed, but the factory can't move the task to approved"));
}

#[tokio::test]
async fn the_merger_keeps_one_status_comment_and_edits_it() {
    let (r, queue) = queued_rig_with(SHADOW);
    let (key, pr) = reviewed(&r, &queue, 3, "docs/a.md").await;
    assert_eq!(said(&r), ["Grade 3: waiting for CI and a person."]);
    let id = r.tracker.task(&key).run.merge.unwrap().comment.expect("the comment id is recorded");

    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    assert_eq!(said(&r), ["Grade 3: waiting for a person."], "two changes, one comment, edited");
    assert_eq!(r.tracker.task(&key).run.merge.unwrap().comment, Some(id));
}

#[tokio::test]
async fn the_status_comment_lists_when_the_status_changed() {
    let (r, queue) = queued_rig_with(SHADOW);
    let (_key, pr) = reviewed(&r, &queue, 3, "docs/a.md").await;
    let status = || r.codehost.prs()[0].comments[1].clone();
    assert_eq!(status(), "Grade 3: waiting for CI and a person.", "no History before a change");

    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    let body = status();
    let (now, history) = body.split_once("\n\nHistory:\n").expect("a History list");
    assert_eq!(now, "Grade 3: waiting for a person.");
    let lines: Vec<&str> = history.lines().collect();
    assert_eq!(lines.len(), 2, "{history}");
    assert!(
        lines[0].starts_with("- 20") && lines[0].ends_with(" UTC: Grade 3: waiting for CI and a person."),
        "{history}"
    );
    assert!(lines[1].ends_with(" UTC: Grade 3: waiting for a person."), "{history}");
}
#[tokio::test]
async fn a_deleted_status_comment_is_posted_again() {
    let (r, queue) = queued_rig_with(SHADOW);
    let (key, pr) = reviewed(&r, &queue, 3, "docs/a.md").await;
    let id = r.tracker.task(&key).run.merge.unwrap().comment.unwrap();
    r.codehost.delete_comment(&pr, id);

    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    let new = r.tracker.task(&key).run.merge.unwrap().comment.unwrap();
    assert_ne!(new, id, "the new comment's id is recorded");
    assert_eq!(said(&r).last().unwrap(), "Grade 3: waiting for a person.");
    assert_eq!(said(&r)[0], "Grade 3: waiting for CI and a person.", "the deleted one is untouched");
}

#[tokio::test]
async fn the_pr_says_the_task_is_parked_when_the_request_for_changes_hits_the_round_limit() {
    let (r, queue) = queued_rig_rounds(SHADOW, Some(1));
    let key = r.tracker.add_subtask(&queue, "Do it", "");
    r.tracker.set_grade(&key, 4);
    r.harness.push(Role::Worker, Script::new(changes_to("docs/a.md")));
    r.harness.push(Role::Reviewer, Script::new(request_changes("Add a test.")));
    r.factory.tick().await.unwrap();
    let said = said(&r);
    assert!(
        said.iter().any(|m| m.ends_with("The latest review asked for changes; the task is parked for Lane.")),
        "{said:?}"
    );
    assert!(!said.iter().any(|m| m.contains("a rework is running")), "{said:?}");
}

#[tokio::test]
async fn a_grade_raise_leaves_lanes_own_approval_in_place() {
    // Grade 3 here needs a gate that can't pass yet, so the raised grade fails a non-CI gate.
    let config = merging().replace(
        "require = [\"checks\", \"ci\", \"agent_review\", \"human\"]\nauto_merge = false",
        "require = [\"checks\", \"ci\", \"quorum\"]\nauto_merge = false\nquorum = { reviewers = 3, agree = 3 }",
    );
    let (r, queue) = queued_rig_with(&config);
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.set_ci(&pr, CiState::Passed);
    // Lane approves with no mark of the control plane's, then raises the grade.
    r.tracker.approve_as_human(&key);
    r.tracker.set_grade(&key, 3);
    r.factory.reconcile().await.unwrap();
    r.factory.reconcile().await.unwrap();

    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::Approved, "only the control plane's own approval is withdrawn");
    assert!(!r.codehost.prs()[0].merged);
    let decision = task.run.merge.unwrap();
    assert_eq!(decision.grade, gr(3));
    assert_eq!(decision.approved_head, None);
    assert!(!decision.waiting.is_empty(), "the raised grade's gates don't pass");
}

#[tokio::test]
async fn a_stale_merge_intent_does_not_make_a_persons_merge_the_mergers() {
    let (r, queue) = queued_rig_with(SHADOW);
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    r.factory.reconcile().await.unwrap();

    // The merger recorded its intent, `merge` errored without merging, then the PR was pushed to.
    let old = CodeHost::head(&r.codehost, &pr).await.unwrap();
    let intent = trailhead_factory::RunUpdate { merging: Some(old), ..Default::default() };
    Tracker::report(&r.tracker, &key, intent).await.unwrap();
    r.codehost.push_to(&pr);
    // A person merges the new head.
    let head = CodeHost::head(&r.codehost, &pr).await.unwrap();
    r.codehost.merge(&pr, &head).await.unwrap();
    r.factory.reconcile().await.unwrap();
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::Done);
    assert_eq!(task.run.merged.expect("recorded").by, MergedBy::Human);
    let handoff = r.tracker.comments(&key).into_iter().rev().find(|c| c.kind == "handoff").expect("a handoff").body;
    assert!(!handoff.contains("gates passed"), "{handoff}");
}

#[tokio::test]
async fn a_queued_update_is_forgotten_when_a_person_merges_the_pr() {
    let (r, queue) = queued_rig_with(&merging());
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.queue_updates(true);
    r.codehost.set_behind(&pr, true);
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    assert!(r.factory.awaiting_branch_update(&key));

    let head = CodeHost::head(&r.codehost, &pr).await.unwrap();
    r.codehost.merge(&pr, &head).await.unwrap();
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::Done);
    assert!(!r.factory.awaiting_branch_update(&key));
}

#[tokio::test]
async fn a_queued_update_is_forgotten_when_a_person_closes_the_pr() {
    let (r, queue) = queued_rig_with(&merging());
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.queue_updates(true);
    r.codehost.set_behind(&pr, true);
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    assert!(r.factory.awaiting_branch_update(&key));

    r.codehost.close(&pr);
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::NeedsInput);
    assert!(!r.factory.awaiting_branch_update(&key));
}

#[tokio::test]
async fn a_pr_merged_by_hand_before_approval_is_parked_once_not_retried() {
    let (r, queue) = queued_rig_with(SHADOW);
    let (key, pr) = reviewed(&r, &queue, 3, "docs/a.md").await;
    assert_eq!(r.tracker.task(&key).status, Status::InReview);
    // Core refuses the factory's `done` on work it did that nobody approved.
    r.tracker.forbid_finishing_unapproved(true);
    let head = CodeHost::head(&r.codehost, &pr).await.unwrap();
    r.codehost.merge(&pr, &head).await.unwrap();

    r.factory.reconcile().await.unwrap();
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput, "a person is asked to mark it done");
    assert_eq!(task.run.merged.expect("the merge is recorded").by, MergedBy::Human);
    let asked = |r: &Rig| r.tracker.comments(&key).iter().filter(|c| c.kind == "question").count();
    assert_eq!(asked(&r), 1);

    let again = r.factory.reconcile().await.unwrap();
    assert!(again.merged.is_empty(), "a parked task isn't finished again");
    assert_eq!(r.tracker.task(&key).status, Status::NeedsInput);
    assert_eq!(asked(&r), 1, "and not asked again");
}

#[tokio::test]
async fn a_nearly_spent_code_host_rate_limit_holds_the_merger_until_it_resets() {
    let (r, queue) = queued_rig_with(&merging());
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    r.codehost.set_ci(&pr, CiState::Passed);
    let resets_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 + 3600;

    r.codehost.set_rate_limit(Some(RateLimit { limit: 5000, remaining: 400, resets_at }));
    r.factory.reconcile().await.unwrap();
    assert!(!r.codehost.prs()[0].merged, "the PR is left alone while the limit is nearly spent");
    assert_eq!(r.tracker.task(&key).status, Status::InReview);

    // Plenty left, or the window already reset: the merger carries on.
    r.codehost.set_rate_limit(Some(RateLimit { limit: 5000, remaining: 400, resets_at: resets_at - 7200 }));
    r.factory.reconcile().await.unwrap();
    assert!(r.codehost.prs()[0].merged);
}

#[tokio::test]
async fn the_poll_stretches_as_the_rate_limit_runs_down() {
    let (r, _queue) = queued_rig_with(&merging());
    let poll = std::time::Duration::from_secs(10);
    let resets_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 + 3600;
    let at = |remaining| {
        r.codehost.set_rate_limit(Some(RateLimit { limit: 5000, remaining, resets_at }));
        r.factory.polled_every(poll).as_secs()
    };
    assert_eq!(r.factory.polled_every(poll).as_secs(), 10, "no reading, no change");
    assert_eq!((at(4000), at(2000), at(1000)), (10, 20, 40));
}

/// A person's PR on the same repo and base as the factory's, opened by hand.
async fn person_pr(r: &Rig) -> PrRef {
    let first = r.codehost.prs()[0].clone();
    let repo = first.pr.repo.clone();
    let commit = Commit { message: "mine".into(), changes: Vec::new(), merges: Vec::new() };
    r.codehost.publish(&repo, "start", "person/branch", &[commit]).await.unwrap();
    let spec = NewPr {
        branch: "person/branch".into(),
        base: first.spec.base.clone(),
        title: "A person's change".into(),
        body: String::new(),
    };
    r.codehost.open_pr(&repo, spec).await.unwrap()
}

#[tokio::test]
async fn a_persons_pr_with_auto_merge_on_holds_a_ready_merge_and_is_kept_up_to_date() {
    let (r, queue) = queued_rig_with(&merging());
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    let mine = person_pr(&r).await;
    r.codehost.set_auto_merge(&mine, true);
    r.codehost.set_behind(&mine, true);
    r.codehost.set_ci(&mine, CiState::Passed);
    r.codehost.set_ci(&pr, CiState::Passed);
    for _ in 0..3 {
        r.factory.reconcile().await.unwrap();
    }
    assert!(!r.codehost.prs()[0].merged, "the factory's merge waits");
    assert_ne!(r.tracker.task(&key).status, Status::Done);
    assert_eq!(r.codehost.update_requests(), 1, "the person's branch was brought up to date");
    assert!(!r.codehost.prs()[1].behind);
    assert_eq!(r.factory.status().snapshot().holding.as_deref(), Some("#2 has auto-merge on"));

    // GitHub merges it once its CI is green; the factory's merge goes ahead.
    r.codehost.merge_by_host(&mine);
    r.factory.reconcile().await.unwrap();
    r.factory.reconcile().await.unwrap();
    assert!(r.codehost.prs()[0].merged);
    assert_eq!(r.tracker.task(&key).status, Status::Done);
    assert_eq!(r.factory.status().snapshot().holding, None);
}

#[tokio::test]
async fn a_persons_pr_without_auto_merge_changes_nothing() {
    let (r, queue) = queued_rig_with(&merging());
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    let mine = person_pr(&r).await;
    r.codehost.set_behind(&mine, true);
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    r.factory.reconcile().await.unwrap();
    assert!(r.codehost.prs()[0].merged);
    assert_eq!(r.tracker.task(&key).status, Status::Done);
    assert_eq!(r.codehost.update_requests(), 0);
}

#[tokio::test]
async fn a_failing_persons_pr_still_holds_and_is_left_alone() {
    let (r, queue) = queued_rig_with(&merging());
    let (_key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    let mine = person_pr(&r).await;
    r.codehost.set_auto_merge(&mine, true);
    r.codehost.set_behind(&mine, true);
    r.codehost.set_ci(&mine, CiState::Failed { checks: vec!["build".into()] });
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    r.factory.reconcile().await.unwrap();
    assert!(!r.codehost.prs()[0].merged, "held while auto-merge stays on");
    assert_eq!(r.codehost.update_requests(), 0, "its code is not touched");
    let held = r.factory.status().snapshot().holding.unwrap();
    assert!(held.contains("#2") && held.contains("CI failed: build"), "{held}");

    // A person turns auto-merge off: the factory carries on.
    r.codehost.set_auto_merge(&mine, false);
    r.factory.reconcile().await.unwrap();
    r.factory.reconcile().await.unwrap();
    assert!(r.codehost.prs()[0].merged);
}

#[tokio::test]
async fn a_host_error_while_looking_after_a_persons_pr_keeps_the_hold() {
    let (r, queue) = queued_rig_with(&merging());
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    let mine = person_pr(&r).await;
    r.codehost.set_auto_merge(&mine, true);
    r.codehost.set_ci(&mine, CiState::Passed);
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    assert!(!r.codehost.prs()[0].merged);

    // The host fails to say whether it's behind: the hold stays, so the factory's merge still waits.
    r.codehost.fail_behind(true);
    for _ in 0..3 {
        r.factory.reconcile().await.unwrap();
    }
    assert!(!r.codehost.prs()[0].merged, "the factory's merge waits");
    assert_ne!(r.tracker.task(&key).status, Status::Done);
    assert!(r.factory.status().snapshot().holding.is_some());
}
