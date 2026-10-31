use std::sync::Arc;

use trailhead_control::Outcome;
use trailhead_factory::{CiState, FollowUp, Role, RunOutcome, Status, TaskKey, Tracker, VerdictKind, run_name};
use trailhead_testkit::Script;

use crate::common::*;

#[tokio::test(start_paused = true)]
async fn a_reviewer_that_breaches_parks_the_task_and_isnt_run_again() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Big diff", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    let ran = r.factory.run_task(&r.tracker.task(&key)).await.unwrap();
    assert!(matches!(ran, Outcome::AwaitingReview { .. }), "{ran:?}");
    tokio::time::advance(std::time::Duration::from_secs(61)).await; // out of the no-verdict backoff

    // Every reviewer would breach on steps; only the first should ever run.
    for _ in 0..3 {
        r.harness.push(Role::Reviewer, Script::new(approve()).steps(50, 1));
    }
    for _ in 0..3 {
        r.factory.reconcile().await.unwrap();
    }
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput);
    let questions: Vec<_> = r.tracker.comments(&key).into_iter().filter(|c| c.kind == "question").collect();
    assert_eq!(questions.len(), 1);
    assert!(questions[0].body.contains("steps"), "{}", questions[0].body);
    assert_eq!(task.run.review_rounds, Some(0), "a fresh count once a human reopens it");
}

#[tokio::test]
async fn a_review_that_cant_start_doesnt_stop_reconcile() {
    let r = rig();
    let tracker: Arc<dyn Tracker> = Arc::new(r.tracker.clone());

    // TRAIL-1: its PR is open but unreviewed, and the reviewer's sandbox can't be set up.
    let unreviewed = r.tracker.add_task("TRAIL", "Unreviewed", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(RunOutcome::Failed { reason: "control plane died".into() }));
    let ran = r.factory.run_task(&r.tracker.task(&unreviewed)).await.unwrap();
    assert!(matches!(ran, Outcome::AwaitingReview { .. }), "{ran:?}");
    r.compute.fail(&format!("{}-1", trailhead_factory::review_name(&unreviewed, 1, 1)), "set_egress");

    // TRAIL-2: crashed mid-run, after it in the list.
    let crashed = r.tracker.add_task("TRAIL", "Crashed", "");
    let name = run_name(&crashed, 1);
    tracker.create_sub_agent(&name, "TRAIL", std::time::Duration::from_secs(3600)).await.unwrap();
    let sub = tracker.sub_agent(&name);
    sub.claim(&crashed).await.unwrap();
    sub.report(&crashed, update(1, &name)).await.unwrap();

    let done = r.factory.reconcile().await.unwrap();
    assert!(done.reviewed.is_empty(), "{done:?}");
    assert_eq!(done.requeued, [crashed.0], "the rest is still reconciled");
    assert_eq!(r.tracker.task(&unreviewed).status, Status::InReview, "left for the next reconcile");
}

#[tokio::test]
async fn a_review_whose_context_cant_be_read_creates_no_sandbox() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Unreviewed", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(RunOutcome::Failed { reason: "control plane died".into() }));
    let ran = r.factory.run_task(&r.tracker.task(&key)).await.unwrap();
    assert!(matches!(ran, Outcome::AwaitingReview { .. }), "{ran:?}");
    let before = r.compute.sandboxes().len();

    r.tracker.fail_human_context(true);
    let done = r.factory.reconcile().await.unwrap();
    assert!(done.reviewed.is_empty(), "{done:?}");
    assert_eq!(r.compute.sandboxes().len(), before, "no sandbox is held for a review that couldn't start");
    assert_eq!(r.tracker.task(&key).status, Status::InReview);
}

#[tokio::test(start_paused = true)]
async fn a_review_left_from_before_a_restart_doesnt_hold_up_workers() {
    let r = rig();
    let unreviewed = r.tracker.add_task("TRAIL", "Unreviewed", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(RunOutcome::Failed { reason: "control plane died".into() }));
    r.factory.run_task(&r.tracker.task(&unreviewed)).await.unwrap();
    tokio::time::advance(std::time::Duration::from_secs(61)).await; // out of the no-verdict backoff

    // The boot review is slow; a ready task starts anyway.
    let gate = Arc::new(tokio::sync::Notify::new());
    r.harness.push(Role::Reviewer, Script::new(approve()).wait_for(gate.clone()));
    let ready = r.tracker.add_task("TRAIL", "Ready", "");
    r.harness.push(Role::Worker, Script::new(RunOutcome::NeedsInput { question: "?".into() }));

    let factory = Arc::new(r.factory);
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let handle =
        tokio::spawn(factory.run(Arc::new(tokio::sync::Notify::new()), std::time::Duration::from_millis(5), async {
            let _ = stopped.await;
        }));
    let tracker = r.tracker.clone();
    until("the ready task to run", || tracker.task(&ready).status == Status::NeedsInput).await;
    assert_eq!(tracker.task(&unreviewed).run.reviewed, None, "the review is still going");

    gate.notify_one();
    until("the review to land", || tracker.task(&unreviewed).run.reviewed == Some(1)).await;
    stop.send(()).unwrap();
    handle.await.unwrap().unwrap();
}

fn approve_with(follow_ups: Vec<FollowUp>) -> RunOutcome {
    RunOutcome::Verdict { kind: VerdictKind::Approve, body: "Fine.".into(), follow_ups, raise_grade: None }
}

fn follow_up(title: &str, priority: Option<i64>) -> FollowUp {
    FollowUp { title: title.into(), description: format!("Details of {title}."), priority }
}

#[tokio::test]
async fn an_approval_files_its_follow_ups_as_unqueued_tasks_linked_to_the_source() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Add a greeting", "Acceptance: `hi()` exists.");
    r.harness.push(Role::Worker, Script::new(changes()));
    let two = vec![follow_up("Cover the edge", Some(2)), follow_up("Rename it", None)];
    r.harness.push(Role::Reviewer, Script::new(approve_with(two)));

    let ran = r.factory.tick().await.unwrap();
    let Outcome::Reviewed { pr, .. } = &ran[0].1 else { panic!("{:?}", ran[0].1) };

    let filed: Vec<_> = r.tracker.tasks_in("TRAIL").into_iter().filter(|t| t.key != key).collect();
    assert_eq!(filed.len(), 2);
    assert_eq!(filed[0].title, "Cover the edge");
    assert_eq!(r.tracker.priority(&filed[0].key), Some(2));
    assert_eq!(filed[1].title, "Rename it");
    for t in &filed {
        assert_eq!(t.status, Status::Open);
        assert!(t.description.contains(&key.0) && t.description.contains(&pr.url), "{}", t.description);
        assert!(
            r.tracker
                .ready("TRAIL", Some(&TaskKey("TRAIL-102".into())), 10)
                .await
                .unwrap()
                .iter()
                .all(|q| q.key != t.key)
        );
    }
    let comment = r.codehost.prs()[0].comments.last().unwrap().clone();
    for t in &filed {
        assert!(comment.contains(&format!("`{}` {}", t.key.0, t.title)), "{comment}");
    }
}

#[tokio::test]
async fn follow_ups_reuse_an_open_task_with_the_same_title() {
    let r = rig();
    r.tracker.add_task("TRAIL", "Add a greeting", "Acceptance: `hi()` exists.");
    let existing = r.tracker.add_task("TRAIL", "Cover the edge", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    let list = vec![follow_up("cover the edge ", None), follow_up("Rename it", None), follow_up("RENAME IT", None)];
    r.harness.push(Role::Reviewer, Script::new(approve_with(list)));

    r.factory.tick().await.unwrap();

    assert_eq!(r.tracker.tasks_in("TRAIL").len(), 3, "one new task: the duplicate and the repeat aren't filed");
    let comment = r.codehost.prs()[0].comments.last().unwrap().clone();
    assert!(comment.contains(&format!("`{}` cover the edge (already open)", existing.0)), "{comment}");
}

#[tokio::test]
async fn no_tasks_are_filed_for_an_empty_list_or_a_request_for_changes() {
    let r = rig();
    r.tracker.add_task("TRAIL", "Add a greeting", "Acceptance: `hi()` exists.");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve_with(vec![])));
    r.factory.tick().await.unwrap();
    assert_eq!(r.tracker.tasks_in("TRAIL").len(), 1);
    assert!(!r.codehost.prs()[0].comments.last().unwrap().contains("Follow-ups"));

    let r = rig();
    r.tracker.add_task("TRAIL", "Add a greeting", "Acceptance: `hi()` exists.");
    r.harness.push(Role::Worker, Script::new(changes()));
    let outcome = RunOutcome::Verdict {
        kind: VerdictKind::RequestChanges,
        body: "Add a test.".into(),
        follow_ups: vec![follow_up("Rename it", None)],
        raise_grade: None,
    };
    r.harness.push(Role::Reviewer, Script::new(outcome));
    r.factory.tick().await.unwrap();
    assert_eq!(r.tracker.tasks_in("TRAIL").len(), 1);
}

#[tokio::test]
async fn requested_changes_start_a_rework_on_the_pr_branch_with_the_review() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Add a greeting", "Acceptance: `hi()` exists.");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(request_changes("Add a test for `hi()`.")));

    let ran = r.factory.tick().await.unwrap();
    let Outcome::Reviewed { pr, verdict } = &ran[0].1 else { panic!("{:?}", ran[0].1) };
    assert_eq!(verdict.kind, VerdictKind::RequestChanges);
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::Open, "back in the queue, not stranded in review");
    assert_eq!(task.holder, None);
    let wanted = task.run.changes_requested.expect("the review is kept for the next run");
    assert_eq!(wanted.body, "Add a test for `hi()`.");
    assert_eq!(wanted.reviewer, "factory/trail-1-a1-r1");

    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    assert_eq!(ran.len(), 1, "ready returns the reopened task");
    let Outcome::Reviewed { pr: again, verdict } = &ran[0].1 else { panic!("{:?}", ran[0].1) };
    assert_eq!(verdict.kind, VerdictKind::Approve);
    assert_eq!(again, pr, "the rework pushes to the same PR");

    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::InReview, "approved: Lane merges");
    assert_eq!(task.run.attempt, Some(2));
    assert_eq!(task.run.review_rounds, Some(2), "rounds count across reworks");
    assert_eq!(r.codehost.prs().len(), 1);
    assert_eq!(r.codehost.commits(&pr.repo, "factory/trail-1").len(), 2);

    let feedback = r.harness.feedbacks();
    assert_eq!(feedback.len(), 1, "only the rework is resumed with feedback");
    assert!(feedback[0].body.contains("Add a test for `hi()`."), "{}", feedback[0].body);
    let reqs = r.daemon.requests();
    let worker_reqs: Vec<_> = reqs.iter().filter(|q| q.role == Role::Worker).collect();
    assert_eq!(worker_reqs[0].feedback, None);
    assert!(worker_reqs[1].feedback.is_some());
    let prepared = r.daemon.prepared();
    assert!(prepared.iter().any(|(_, p)| p.branch.as_deref() == Some("factory/trail-1")));
    let on_pr = |worker: bool| {
        prepared.iter().filter(|(id, p)| p.branch.is_some() && id.0.starts_with("trailhead-w") == worker).count()
    };
    assert_eq!(on_pr(true), 1, "only the rework checks out the PR among workers");
    assert_eq!(on_pr(false), 2, "and both reviewers check it out");
}

#[tokio::test]
async fn a_request_for_changes_is_stale_once_the_pr_head_has_moved() {
    // The counts say a rework is wanted, as when the reset after a park was lost.
    async fn left_in_review(push: bool) -> (Rig, TaskKey) {
        let mut r = rig();
        r.factory = r.factory.with_codehost(Arc::new(r.codehost.clone()));
        let key = r.tracker.add_task("TRAIL", "Add a greeting", "Acceptance: `hi()` exists.");
        r.harness.push(Role::Worker, Script::new(changes()));
        r.harness.push(Role::Reviewer, Script::new(request_changes("Add a test for `hi()`.")));
        let ran = r.factory.tick().await.unwrap();
        let Outcome::Reviewed { pr, .. } = &ran[0].1 else { panic!("{:?}", ran[0].1) };
        assert!(r.tracker.task(&key).run.changes_requested.unwrap().head.is_some());
        if push {
            r.codehost.push_to(pr);
        }
        r.tracker.set_status(&key, Status::InReview);
        (r, key)
    }

    let (r, key) = left_in_review(false).await;
    assert_eq!(r.factory.reconcile().await.unwrap().reworked, vec![key.0.clone()], "the head is where it was");

    let (r, key) = left_in_review(true).await;
    assert!(r.factory.reconcile().await.unwrap().reworked.is_empty(), "someone pushed since the request");
    assert_eq!(r.tracker.task(&key).status, Status::InReview);
}

#[tokio::test]
async fn a_rework_started_by_hand_gets_the_prs_last_review_as_feedback() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Add a greeting", "Acceptance: `hi()` exists.");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::InReview);

    // A review that never went through request_rework, then a person reopens the task.
    let verdict = trailhead_factory::Verdict {
        kind: VerdictKind::RequestChanges,
        body: "Add a test for `hi()`.".into(),
        reviewer: "factory/trail-1-a1-r1".into(),
        model: None,
        raise_grade: None,
    };
    r.tracker.record_verdict(&key, &verdict).await.unwrap();
    assert_eq!(r.tracker.task(&key).run.changes_requested, None);
    r.tracker.set_status(&key, Status::Open);

    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();

    let feedback = r.harness.feedbacks();
    assert_eq!(feedback.len(), 1);
    assert!(feedback[0].body.contains("Add a test for `hi()`."), "{}", feedback[0].body);
}

#[tokio::test]
async fn requested_changes_past_the_cap_park_the_task_with_the_last_review() {
    let r = rig_with(&CONFIG.replace("name = \"trailhead\"", "name = \"trailhead\"\nmax_review_rounds = 2"));
    let key = r.tracker.add_task("TRAIL", "Add a greeting", "");
    for body in ["first complaint", "second complaint"] {
        r.harness.push(Role::Worker, Script::new(changes()));
        r.harness.push(Role::Reviewer, Script::new(request_changes(body)));
    }
    r.factory.tick().await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::Open);
    r.factory.tick().await.unwrap();

    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput);
    let questions: Vec<_> = r.tracker.comments(&key).into_iter().filter(|c| c.kind == "question").collect();
    assert_eq!(questions.len(), 1);
    assert!(questions[0].body.contains("after 2 review rounds"), "{}", questions[0].body);
    assert!(questions[0].body.contains("second complaint"), "{}", questions[0].body);
    assert!(r.factory.tick().await.unwrap().is_empty(), "a parked task isn't reworked");
}

#[tokio::test]
async fn a_rework_that_fails_its_check_is_retried_not_parked() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Add a greeting", "");
    for body in ["first complaint", "second complaint"] {
        r.harness.push(Role::Worker, Script::new(changes()));
        r.harness.push(Role::Reviewer, Script::new(request_changes(body)));
        r.factory.tick().await.unwrap();
    }

    // Attempt 3 is the third run, but only the first failure since the last review asked for changes.
    r.daemon.fail_check("lint", "warning: unused variable");
    r.harness.push(Role::Worker, Script::new(changes()));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Failed { .. }), "{:?}", ran[0].1);
    assert_eq!(r.tracker.task(&key).status, Status::Open);

    r.daemon.pass_check("lint");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Reviewed { verdict, .. } if verdict.kind == VerdictKind::Approve));
    assert_eq!(r.tracker.task(&key).run.attempt, Some(4));

    let feedback = r.harness.feedbacks();
    assert_eq!(feedback.len(), 3, "every rework is told the review");
    assert!(feedback[0].body.contains("first complaint"));
    assert!(feedback[1..].iter().all(|f| f.body.contains("second complaint")), "{feedback:?}");
}

#[tokio::test]
async fn a_parked_request_a_person_settled_is_not_reworked_by_reconcile() {
    let r = rig_with(&CONFIG.replace("name = \"trailhead\"", "name = \"trailhead\"\nmax_review_rounds = 1"));
    let key = r.tracker.add_task("TRAIL", "Add a greeting", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(request_changes("Add a test.")));
    r.factory.tick().await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::NeedsInput);

    // A person fixes the PR by hand and moves the task back to review.
    r.tracker.set_status(&key, Status::InReview);
    let done = r.factory.reconcile().await.unwrap();
    assert!(done.reworked.is_empty(), "{done:?}");
    assert_eq!(r.tracker.task(&key).status, Status::InReview);
}

#[tokio::test]
async fn an_addressed_review_isnt_passed_to_later_runs() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Add a greeting", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(request_changes("Add a test.")));
    r.factory.tick().await.unwrap();
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::InReview);

    // Lane sends the approved task back.
    r.tracker.set_status(&key, Status::Open);
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();
    assert_eq!(r.harness.feedbacks().len(), 1, "only the first rework was told the review");
    assert!(r.factory.reconcile().await.unwrap().reworked.is_empty());
}

#[tokio::test]
async fn a_rework_posts_a_flaky_check_as_a_pr_comment() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Add a greeting", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(request_changes("Tidy up.")));
    r.factory.tick().await.unwrap();

    r.daemon.fail_check_times("test", "flaked", 1);
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();

    let pr = &r.codehost.prs()[0];
    assert_eq!(r.tracker.task(&key).status, Status::InReview);
    assert!(
        pr.comments.iter().any(|c| c.contains("⚠️ Check `test` failed, then passed when rerun.")),
        "{:?}",
        pr.comments
    );
    assert!(!pr.spec.body.contains("when rerun"), "the body is only written when the PR opens");
}

#[tokio::test(start_paused = true)]
async fn reviews_that_fail_on_the_infrastructure_back_off_and_park_at_the_cap() {
    let r = rig_with(&CONFIG.replace("name = \"trailhead\"", "name = \"trailhead\"\nmax_infra_failures = 3"));
    let key = r.tracker.add_task("TRAIL", "Unreviewable", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(RunOutcome::Failed { reason: "control plane died".into() }));
    let ran = r.factory.run_task(&r.tracker.task(&key)).await.unwrap();
    assert!(matches!(ran, Outcome::AwaitingReview { .. }), "{ran:?}");
    let rounds = r.tracker.task(&key).run.review_rounds;

    r.daemon.fail_runs(3, "connection lost");
    let secs = std::time::Duration::from_secs;
    for (n, wait) in [60, 120].into_iter().enumerate() {
        let done = r.factory.reconcile().await.unwrap();
        assert!(done.reviewed.is_empty(), "failure {} isn't a review", n + 1);
        assert_eq!(r.tracker.task(&key).status, Status::InReview, "failure {} is retried", n + 1);
        assert_eq!(r.tracker.task(&key).run.review_rounds, rounds, "the failed round is given back");
        // Backing off: the reviewer isn't started again, so the failure queued for it stays unused.
        r.factory.reconcile().await.unwrap();
        tokio::time::advance(secs(wait - 1)).await;
        r.factory.reconcile().await.unwrap();
        assert_eq!(r.tracker.task(&key).status, Status::InReview, "still a second short of {wait}s");
        tokio::time::advance(secs(2)).await;
    }
    r.factory.reconcile().await.unwrap();
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput);
    assert_eq!(task.run.review_rounds, rounds);
    let questions: Vec<_> = r.tracker.comments(&key).into_iter().filter(|c| c.kind == "question").collect();
    assert_eq!(questions.len(), 1);
    assert!(questions[0].body.contains("it's not the task") && questions[0].body.contains("connection lost"));
}

#[tokio::test(start_paused = true)]
async fn the_reviewers_infrastructure_failures_are_persisted_and_reset_by_a_finished_review() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Flaky review", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(RunOutcome::Failed { reason: "control plane died".into() }));
    let ran = r.factory.run_task(&r.tracker.task(&key)).await.unwrap();
    assert!(matches!(ran, Outcome::AwaitingReview { .. }), "{ran:?}");
    assert_eq!(r.tracker.task(&key).run.infra_failures, Some(1), "kept in the metadata, not only in memory");

    tokio::time::advance(std::time::Duration::from_secs(61)).await;
    r.daemon.fail_runs(1, "connection lost");
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.tracker.task(&key).run.infra_failures, Some(2));

    tokio::time::advance(std::time::Duration::from_secs(121)).await;
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.reviewed.len(), 1, "{done:?}");
    assert_eq!(r.tracker.task(&key).run.infra_failures.unwrap_or(0), 0, "a finished review ends the streak");
}

#[tokio::test(start_paused = true)]
async fn a_review_cancelled_on_purpose_isnt_an_infrastructure_failure() {
    let r = rig_with(&CONFIG.replace("name = \"trailhead\"", "name = \"trailhead\"\nmax_infra_failures = 2"));
    let key = r.tracker.add_task("TRAIL", "Stopped mid-review", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    let ran = r.factory.run_task(&r.tracker.task(&key)).await.unwrap();
    assert!(matches!(ran, Outcome::AwaitingReview { .. }), "{ran:?}");
    tokio::time::advance(std::time::Duration::from_secs(61)).await; // out of the no-verdict backoff
    let rounds = r.tracker.task(&key).run.review_rounds.unwrap_or(0);

    r.daemon.cancel_runs(1);
    r.factory.reconcile().await.unwrap();
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::InReview, "not parked: a cancel isn't a second failure");
    assert_eq!(task.run.review_rounds.unwrap_or(0), rounds, "the round is given back");

    // No backoff either: the next pass reviews it at once.
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.reviewed.len(), 1, "{done:?}");
}

#[tokio::test(start_paused = true)]
async fn a_reviewer_sandbox_that_wont_destroy_keeps_the_verdict() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Stubborn sandbox", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    let ran = r.factory.run_task(&r.tracker.task(&key)).await.unwrap();
    assert!(matches!(ran, Outcome::AwaitingReview { .. }), "{ran:?}");
    tokio::time::advance(std::time::Duration::from_secs(61)).await; // out of the no-verdict backoff

    r.compute.fail(&format!("{}-1", trailhead_factory::review_name(&key, 1, 1)), "destroy");
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.reviewed.len(), 1, "the finished verdict isn't thrown away: {done:?}");
    let task = r.tracker.task(&key);
    assert_eq!(task.run.review_rounds, Some(1), "the round counts");
    assert_eq!(task.run.reviewed, task.run.attempt);
}

#[tokio::test(start_paused = true)]
async fn a_reviewer_that_never_starts_doesnt_use_up_a_review_round() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Slow to review", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    let ran = r.factory.run_task(&r.tracker.task(&key)).await.unwrap();
    assert!(matches!(ran, Outcome::AwaitingReview { .. }), "{ran:?}");
    tokio::time::advance(std::time::Duration::from_secs(61)).await; // out of the no-verdict backoff

    r.daemon.fail_runs(1, "connection lost");
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.tracker.task(&key).run.review_rounds.unwrap_or(0), 0);

    tokio::time::advance(std::time::Duration::from_secs(121)).await;
    r.harness.push(Role::Reviewer, Script::new(request_changes("Add a test.")));
    r.factory.reconcile().await.unwrap();
    let task = r.tracker.task(&key);
    assert_eq!(task.run.review_rounds, Some(1));
    assert_eq!(task.status, Status::Open, "reworks instead of parking at max_review_rounds");
}

#[tokio::test(start_paused = true)]
async fn reviews_that_end_without_a_verdict_back_off_and_park_at_the_cap() {
    let r = rig_with(&CONFIG.replace("name = \"trailhead\"", "name = \"trailhead\"\nmax_infra_failures = 3"));
    let key = r.tracker.add_task("TRAIL", "Unreadable", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    for _ in 0..3 {
        r.harness.push(Role::Reviewer, Script::new(RunOutcome::Failed { reason: "prompt too big".into() }));
    }
    let ran = r.factory.run_task(&r.tracker.task(&key)).await.unwrap();
    assert!(matches!(ran, Outcome::AwaitingReview { .. }), "{ran:?}");
    let rounds = r.tracker.task(&key).run.review_rounds;

    // Backing off: the second failing reviewer isn't started until the wait is over.
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::InReview);
    for wait in [61, 121] {
        tokio::time::advance(std::time::Duration::from_secs(wait)).await;
        r.factory.reconcile().await.unwrap();
    }
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput);
    assert_eq!(task.run.review_rounds, rounds, "the failed rounds are given back");
    let questions: Vec<_> = r.tracker.comments(&key).into_iter().filter(|c| c.kind == "question").collect();
    assert_eq!(questions.len(), 1);
    assert!(questions[0].body.contains("without a verdict"), "{}", questions[0].body);
}

const NO_REVIEW_GATE: &str = r#"
[gates.1]
require = ["checks", "ci"]
auto_merge = true

[merge]
mode = "shadow"
"#;

#[tokio::test(start_paused = true)]
async fn a_review_backed_off_after_an_infrastructure_failure_is_not_skipped_by_approval() {
    let (r, queue) = queued_rig_with(NO_REVIEW_GATE);
    let key = r.tracker.add_subtask(&queue, "Do it", "");
    r.tracker.set_grade(&key, 1);
    r.harness.push(Role::Worker, Script::new(changes_to("docs/a.md")));
    r.harness.push(Role::Reviewer, Script::new(RunOutcome::Failed { reason: "control plane died".into() }));
    r.factory.tick().await.unwrap();
    let pr = r.tracker.task(&key).run.pr.expect("a PR");
    assert_eq!(r.tracker.task(&key).run.reviewed, None);

    // CI passes while the review is backed off: the gates (which don't ask for the review) must not approve yet.
    r.codehost.set_ci(&pr, CiState::Passed);
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::InReview);

    tokio::time::advance(std::time::Duration::from_secs(61)).await;
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.tracker.task(&key).run.reviewed, Some(1), "the review ran once the backoff was over");
}
