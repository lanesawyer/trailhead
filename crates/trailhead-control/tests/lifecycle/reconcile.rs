use std::sync::Arc;

use trailhead_control::{Outcome, Reconciled};
use trailhead_factory::{CodeHost, Role, RunOutcome, Status, TaskKey, Tracker, Usd, label, run_name};
use trailhead_testkit::Script;

use crate::common::*;

#[tokio::test(start_paused = true)]
async fn reconcile_after_a_crash_requeues_and_cleans_up() {
    let r = rig();
    let factory = &r.factory;
    let tracker: Arc<dyn Tracker> = Arc::new(r.tracker.clone());

    // TRAIL-1: crashed mid-run. Its sub-agent holds the claim and its sandbox is live.
    let crashed = r.tracker.add_task("TRAIL", "Crashed", "");
    let name = run_name(&crashed, 1);
    tracker.create_sub_agent(&name, "TRAIL", std::time::Duration::from_secs(3600)).await.unwrap();
    let sub = tracker.sub_agent(&name);
    sub.claim(&crashed).await.unwrap();
    sub.report(&crashed, update(1, &name)).await.unwrap();
    live(&r.compute, &name, &crashed, "worker").await;

    // TRAIL-2: crashed between opening the PR and finishing review.
    let unreviewed = r.tracker.add_task("TRAIL", "Unreviewed", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(RunOutcome::Failed { reason: "control plane died".into() }));
    let ran = factory.run_task(&r.tracker.task(&unreviewed)).await.unwrap();
    assert!(matches!(ran, Outcome::AwaitingReview { .. }), "{ran:?}");

    // TRAIL-3: parked, with its suspended sandbox. Kept.
    let parked = r.tracker.add_task("TRAIL", "Parked", "");
    r.harness.push(Role::Worker, Script::new(RunOutcome::NeedsInput { question: "?".into() }));
    factory.run_task(&r.tracker.task(&parked)).await.unwrap();

    // An orphan nobody accounts for.
    live(&r.compute, "trail-99-a1", &TaskKey("TRAIL-99".into()), "worker").await;

    r.harness.push(Role::Reviewer, Script::new(approve()));
    tokio::time::advance(std::time::Duration::from_secs(61)).await; // out of the no-verdict backoff
    let done = factory.reconcile().await.unwrap();
    assert_eq!(
        done,
        Reconciled {
            destroyed: vec!["trail-1-a1".into(), "trail-99-a1".into()],
            released: vec![],
            reattached: vec![],
            requeued: vec![crashed.0.clone()],
            reviewed: vec![unreviewed.0.clone()],
            reworked: vec![],
            conflicted: vec![],
            merged: vec![],
            closed: vec![],
            abandoned: vec![],
        }
    );
    let names: Vec<String> = r.compute.sandboxes().into_iter().map(|s| s.name).collect();
    assert_eq!(names, ["trailhead-w1"], "the pooled sandbox stays, holding the parked task");
    assert_eq!(r.tracker.task(&crashed).status, Status::Open);
    assert!(r.tracker.is_revoked("factory/trail-1-a1"));
    assert_eq!(r.tracker.task(&unreviewed).run.reviewed, Some(1));
    assert_eq!(r.tracker.task(&unreviewed).run.review_rounds, Some(1));
    assert!(r.tracker.is_revoked("factory/trail-2-a1-r1"));
    assert!(r.tracker.is_revoked("factory/trail-2-a1-r1-1"));
    assert_eq!(r.tracker.task(&parked).status, Status::NeedsInput);

    // A second pass finds nothing to do.
    assert_eq!(factory.reconcile().await.unwrap(), Reconciled::default());
}

#[tokio::test]
async fn reconcile_reattaches_to_a_run_still_going_and_counts_all_its_spend() {
    let r = rig();
    let key = going(&r, Usd::dollars(0.5), 120).await;
    let report = trailhead_factory::RunReport {
        result: trailhead_factory::RunResult::NeedsInput("Which greeting?".into()),
        checks: vec![],
        usage: Default::default(),
    };
    r.daemon.ongoing("trail-1-a1", vec![spent(400_000), spent(600_000)], report);

    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.reattached, std::slice::from_ref(&key.0));
    assert!(done.requeued.is_empty() && done.released.is_empty(), "{done:?}");
    assert_eq!(r.daemon.attached(), ["trail-1-a1"]);
    let task = r.tracker.task(&key);
    assert_eq!(task.run.spend, Some(Usd::dollars(1.5)), "the earlier runs' spend plus the replayed usage");
    assert_eq!(task.run.attempt, Some(1), "the attempt counts, as it would have had the run not been interrupted");
    assert_eq!(task.status, Status::InProgress, "nothing requeued it");
    let sandbox = r.compute.get("trailhead-w1").unwrap();
    assert_eq!(sandbox.labels.get(label::STATE).map(String::as_str), Some(label::BUSY));
}

#[tokio::test]
async fn reconcile_does_not_reattach_to_a_run_whose_report_was_already_counted() {
    let r = rig();
    let key = going(&r, Usd::dollars(0.5), 120).await;
    // The run ended and `conclude` counted its spend, then died before the sandbox was released.
    let sub = Arc::new(r.tracker.clone()).sub_agent("trail-1-a1");
    let counted = trailhead_factory::RunUpdate {
        spend: Some(Usd::dollars(1.5)),
        concluded: Some("trail-1-a1".into()),
        ..Default::default()
    };
    sub.report(&key, counted).await.unwrap();
    let report = trailhead_factory::RunReport {
        result: trailhead_factory::RunResult::NeedsInput("Which greeting?".into()),
        checks: vec![],
        usage: Default::default(),
    };
    r.daemon.ongoing("trail-1-a1", vec![spent(400_000), spent(600_000)], report);

    let done = r.factory.reconcile().await.unwrap();
    assert!(done.reattached.is_empty(), "{done:?}");
    assert_eq!(done.requeued, std::slice::from_ref(&key.0));
    assert!(r.daemon.attached().is_empty());
    let task = r.tracker.task(&key);
    assert_eq!(task.run.spend, Some(Usd::dollars(1.5)), "not counted a second time");
    assert_eq!(task.status, Status::Open);
}

#[tokio::test]
async fn a_reattached_run_counts_its_wall_clock_from_when_it_started() {
    let r = rig();
    // Started 61 minutes back against a 60 minute worker budget: nothing is left of it.
    let key = going(&r, Usd::ZERO, 61 * 60).await;
    r.daemon.ongoing_hung("trail-1-a1", vec![spent(1_000)]);

    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.reattached, std::slice::from_ref(&key.0));
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput, "parked, not given another full budget");
    assert!(
        r.tracker.comments(&key).iter().any(|c| c.kind == "question" && c.body.contains("60 minutes")),
        "{:?}",
        r.tracker.comments(&key)
    );
    let sandbox = r.compute.get("trailhead-w1").expect("sandbox kept for resuming");
    assert!(!sandbox.labels.contains_key(label::ROLE), "connector access cut");
    assert!(r.compute.checkpoints("trailhead-w1").last().unwrap().starts_with("parked:"));
}

#[tokio::test]
async fn a_run_the_daemon_has_lost_goes_back_to_the_queue_without_using_an_attempt() {
    let r = rig();
    let key = going(&r, Usd::ZERO, 60).await;

    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.reattached, std::slice::from_ref(&key.0));
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::Open);
    assert_eq!(task.holder, None);
    assert_eq!(task.run.attempt, Some(0), "given back");
    assert!(r.tracker.is_revoked("factory/trail-1-a1"));

    // The sandbox it left busy is dealt with by the next pass.
    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.released, ["trailhead-w1"]);
}

#[tokio::test]
async fn no_worker_starts_on_a_task_whose_pr_merged_meanwhile() {
    let mut r = rig();
    r.factory = r.factory.with_codehost(Arc::new(r.codehost.clone()));
    let key = r.tracker.add_task("TRAIL", "Add a greeting", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(request_changes("Add a test.")));
    r.factory.tick().await.unwrap();
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::Open);
    let pr = task.run.pr.clone().expect("a PR");

    let head = CodeHost::head(&r.codehost, &pr).await.unwrap();
    r.codehost.merge(&pr, &head).await.unwrap();
    let workers = |r: &Rig| r.daemon.requests().iter().filter(|q| q.role == Role::Worker).count();
    let before = workers(&r);
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Settled { .. }), "{:?}", ran[0].1);
    assert_eq!(workers(&r), before, "no worker run started");
    assert_eq!(r.tracker.task(&key).status, Status::Done);
}

#[tokio::test]
async fn no_worker_starts_on_a_task_whose_pr_closed_meanwhile() {
    let mut r = rig();
    r.factory = r.factory.with_codehost(Arc::new(r.codehost.clone()));
    let key = r.tracker.add_task("TRAIL", "Add a greeting", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(request_changes("Add a test.")));
    r.factory.tick().await.unwrap();
    let pr = r.tracker.task(&key).run.pr.clone().expect("a PR");

    r.codehost.close(&pr);
    let workers = |r: &Rig| r.daemon.requests().iter().filter(|q| q.role == Role::Worker).count();
    let before = workers(&r);
    r.factory.tick().await.unwrap();
    assert_eq!(workers(&r), before, "no worker run started");
    assert_eq!(r.tracker.task(&key).status, Status::NeedsInput);
}

#[tokio::test]
async fn a_rework_request_cut_short_is_finished_by_reconcile() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Add a greeting", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(request_changes("Add a test.")));
    r.factory.tick().await.unwrap();
    // The review was recorded, but the control plane stopped before the task left review.
    r.tracker.set_status(&key, Status::InReview);

    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.reworked, std::slice::from_ref(&key.0));
    assert!(done.reviewed.is_empty(), "the review isn't run again: {done:?}");
    assert_eq!(r.tracker.task(&key).status, Status::Open);
    assert!(r.factory.reconcile().await.unwrap().reworked.is_empty());
}

#[tokio::test]
async fn a_parked_task_whose_pr_is_merged_is_finished() {
    let mut r = rig();
    r.factory = r.factory.with_codehost(Arc::new(r.codehost.clone()));
    let key = r.tracker.add_task("TRAIL", "Add a greeting", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(request_changes("Add a test.")));
    r.factory.tick().await.unwrap();
    let pr = r.tracker.task(&key).run.pr.clone().expect("a PR");
    r.codehost.close(&pr);
    r.factory.tick().await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::NeedsInput);

    let head = CodeHost::head(&r.codehost, &pr).await.unwrap();
    r.codehost.merge(&pr, &head).await.unwrap();
    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.merged, std::slice::from_ref(&key.0));
    assert_eq!(r.tracker.task(&key).status, Status::Done);
}
