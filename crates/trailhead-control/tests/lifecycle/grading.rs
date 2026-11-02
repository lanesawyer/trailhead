use trailhead_factory::{RaiseGrade, Role, RunOutcome, Status, VerdictKind};
use trailhead_testkit::Script;

use crate::common::*;

#[tokio::test]
async fn an_ungraded_task_is_skipped_and_reported_once_until_a_human_grades_it() {
    let (r, queue) = queued_rig();
    let key = r.tracker.add_subtask(&queue, "Document it", "");
    r.harness.push(Role::Worker, Script::new(changes_to("docs/a.md")));
    r.harness.push(Role::Reviewer, Script::new(approve()));

    for _ in 0..3 {
        assert!(r.factory.tick().await.unwrap().is_empty());
    }
    assert_eq!(r.tracker.task(&key).status, Status::Open);
    assert_eq!(notes(&r, &key), ["no grade; set one to queue it"], "said once, not on every poll");
    assert_eq!(r.tracker.task(&key).run.ungraded, Some(true), "recorded, so a restart doesn't say it again");

    r.tracker.set_grade(&key, 1);
    let ran = r.factory.tick().await.unwrap();
    assert_eq!(ran.len(), 1);
    assert_eq!(r.tracker.task(&key).status, Status::InReview);
    assert_ne!(r.tracker.task(&key).run.ungraded, Some(true), "graded, so the flag is cleared");
}

#[tokio::test]
async fn a_task_whose_grade_is_cleared_is_reported_ungraded_again() {
    let (r, queue) = queued_rig();
    let key = r.tracker.add_subtask(&queue, "Document it", "");
    r.harness.push(Role::Worker, Script::new(changes_to("docs/a.md")));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    assert!(r.factory.tick().await.unwrap().is_empty());
    assert_eq!(r.tracker.task(&key).run.ungraded, Some(true));

    // Graded and picked up, the flag is cleared.
    r.tracker.set_grade(&key, 1);
    assert_eq!(r.factory.tick().await.unwrap().len(), 1);
    assert_ne!(r.tracker.task(&key).run.ungraded, Some(true));

    // Reopened with its grade taken away, it's said again.
    r.tracker.set_status(&key, Status::Open);
    r.tracker.clear_grade(&key);
    assert!(r.factory.tick().await.unwrap().is_empty());
    assert_eq!(notes(&r, &key), ["no grade; set one to queue it", "no grade; set one to queue it"]);
    assert_eq!(r.tracker.task(&key).run.ungraded, Some(true));
}

#[tokio::test]
async fn ungraded_tasks_dont_crowd_graded_ones_out_of_the_cap() {
    let (r, queue) = queued_rig();
    let first = r.tracker.add_subtask(&queue, "Ungraded", "");
    let second = r.tracker.add_subtask(&queue, "Graded", "");
    r.tracker.set_grade(&second, 1);
    r.harness.push(Role::Worker, Script::new(changes_to("docs/a.md")));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    assert_eq!(ran.len(), 1);
    assert_eq!(ran[0].0.key, second);
    assert_eq!(r.tracker.task(&first).status, Status::Open);
}

#[tokio::test]
async fn a_grade_one_task_that_changes_cargo_lock_is_raised_to_three() {
    let (r, queue) = queued_rig();
    let key = r.tracker.add_subtask(&queue, "Bump a dependency", "");
    r.tracker.set_grade(&key, 1);
    r.harness.push(Role::Worker, Script::new(changes_to("Cargo.lock")));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();

    let task = r.tracker.task(&key);
    assert_eq!(task.grade, Some(3), "the task is raised to match the PR");
    let grade = task.run.grade.expect("recorded in the factory metadata");
    assert_eq!((grade.grade, grade.task), (3, Some(1)));
    assert_eq!(grade.reasons, ["Cargo.lock changed"]);
    assert_eq!(notes(&r, &key), ["Raised to grade 3 (task was 1): Cargo.lock changed"]);
    let comment = r.codehost.prs()[0].comments[0].clone();
    assert!(comment.ends_with("\n\nGrade 3 (task was 1; Cargo.lock changed)"), "{comment}");

    // A rework's review sees the same PR and neither raises nor notes again.
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let shown = r.tracker.task(&key);
    r.factory.review(&shown, shown.run.pr.as_ref().unwrap(), 1, &[]).await.unwrap();
    assert_eq!(notes(&r, &key).len(), 1);
    let comment = r.codehost.prs()[0].comments[1].clone();
    assert!(comment.ends_with("\n\nGrade 3 (task was 1; Cargo.lock changed)"), "{comment}");
}

#[tokio::test]
async fn a_docs_only_change_keeps_the_tasks_grade() {
    let (r, queue) = queued_rig();
    let key = r.tracker.add_subtask(&queue, "Document it", "");
    r.tracker.set_grade(&key, 1);
    r.harness.push(Role::Worker, Script::new(changes_to("docs/a.md")));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();
    assert_eq!(r.tracker.task(&key).grade, Some(1));
    assert!(notes(&r, &key).is_empty());
    assert!(r.codehost.prs()[0].comments[0].ends_with("\n\nGrade 1"));
}

#[tokio::test]
async fn a_reviewer_can_raise_the_grade() {
    let (r, queue) = queued_rig();
    let key = r.tracker.add_subtask(&queue, "Document it", "");
    r.tracker.set_grade(&key, 1);
    r.harness.push(Role::Worker, Script::new(changes_to("docs/a.md")));
    let raise = RaiseGrade { grade: 4, reason: "the docs change the token policy".into() };
    let verdict = RunOutcome::Verdict {
        kind: VerdictKind::Approve,
        body: "Fine.".into(),
        follow_ups: vec![],
        raise_grade: Some(raise),
    };
    r.harness.push(Role::Reviewer, Script::new(verdict));
    r.factory.tick().await.unwrap();

    let task = r.tracker.task(&key);
    assert_eq!(task.grade, Some(4));
    assert_eq!(task.run.grade.map(|g| (g.grade, g.task)), Some((4, Some(1))));
    assert_eq!(
        notes(&r, &key),
        ["Raised to grade 4 (task was 1): the reviewer raised it: the docs change the token policy"]
    );
    let comment = r.codehost.prs()[0].comments[0].clone();
    assert!(
        comment.ends_with("\n\nGrade 4 (task was 1; the reviewer raised it: the docs change the token policy)"),
        "{comment}"
    );
}

#[tokio::test]
async fn a_reviewer_cant_lower_the_grade() {
    let (r, queue) = queued_rig();
    let key = r.tracker.add_subtask(&queue, "Bump a dependency", "");
    r.tracker.set_grade(&key, 3);
    r.harness.push(Role::Worker, Script::new(changes_to("Cargo.lock")));
    let lower = RaiseGrade { grade: 1, reason: "trivial".into() };
    let verdict = RunOutcome::Verdict {
        kind: VerdictKind::Approve,
        body: "Fine.".into(),
        follow_ups: vec![],
        raise_grade: Some(lower),
    };
    r.harness.push(Role::Reviewer, Script::new(verdict));
    r.factory.tick().await.unwrap();
    assert_eq!(r.tracker.task(&key).grade, Some(3));
    assert!(r.codehost.prs()[0].comments[0].ends_with("\n\nGrade 3"));
}
