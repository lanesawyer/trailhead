use std::sync::Arc;

use trailhead_factory::{Compute, Role, RunOutcome, Status, TaskKey, label};
use trailhead_testkit::Script;

use crate::common::*;

#[tokio::test]
async fn sandboxes_are_prepared_under_install_egress_then_narrowed() {
    let r = rig();
    r.tracker.add_task("TRAIL", "Add a greeting", "Acceptance: `hi()` exists.");
    // Park the worker so its sandbox survives to be inspected.
    r.harness.push(Role::Worker, Script::new(RunOutcome::NeedsInput { question: "Which greeting?".into() }));
    r.factory.tick().await.unwrap();

    let prepared = r.daemon.prepared();
    assert_eq!(prepared.len(), 1);
    assert_eq!(prepared[0].0.0, "trailhead-w1");
    assert_eq!(prepared[0].1.repo.slug(), "lanesawyer/trailhead");
    assert_eq!(prepared[0].1.base_branch, "main");

    let egress = r.compute.egress("trailhead-w1").unwrap().domains;
    assert!(egress.iter().any(|d| d == "*.iroh.link"), "{egress:?}");
    assert!(!egress.iter().any(|d| d == "github.com"), "clone egress is closed before the agent runs: {egress:?}");
    let blocked = r.daemon.requests().into_iter().next().unwrap().blocked_hosts;
    assert_eq!(blocked, ["github.com", "codeload.github.com"], "the daemon confirms the lockdown before starting");
    let allowed = r.daemon.requests().into_iter().next().unwrap().allowed_hosts;
    assert_eq!(allowed, ["iroh.link"], "named agent hosts are kept resolved; wildcards can't be");
}

#[tokio::test]
async fn workers_reuse_a_warm_pooled_sandbox_restored_between_tasks() {
    let r = rig_with(&CONFIG.replace("[checks]\n", "[checks]\nsetup = \"cargo build\"\n"));
    let first = r.tracker.add_task("TRAIL", "First", "");
    let second = r.tracker.add_task("TRAIL", "Second", "");
    for _ in 0..2 {
        r.harness.push(Role::Worker, Script::new(changes()));
        r.harness.push(Role::Reviewer, Script::new(approve()));
    }
    r.factory.tick().await.unwrap();
    r.factory.tick().await.unwrap();
    assert_eq!(r.tracker.task(&first).status, Status::InReview);
    assert_eq!(r.tracker.task(&second).status, Status::InReview);

    let names: Vec<_> = r.compute.sandboxes().into_iter().map(|s| s.name).collect();
    assert_eq!(names, ["trailhead-w1"], "one warm sandbox served both tasks");
    assert_eq!(r.tracker.task(&second).run.host.as_deref(), Some("trailhead-w1"));
    assert_eq!(r.tracker.task(&second).run.sandbox.as_deref(), Some("trail-2-a1"), "each run keeps its own name");
    // Released after the first task, restored again before the second, released after it.
    assert_eq!(r.compute.restores("trailhead-w1").len(), 3);
    assert_eq!(r.compute.checkpoints("trailhead-w1"), ["warm", "warm"], "each prepare refreshes the warm point");

    let prepared = r.daemon.prepared();
    let workers: Vec<_> = prepared.iter().filter(|(s, _)| s.0 == "trailhead-w1").collect();
    let reviewers: Vec<_> = prepared.iter().filter(|(s, _)| s.0.ends_with("-r1")).collect();
    assert_eq!(workers.len(), 2);
    assert!(workers.iter().all(|(_, p)| p.setup.is_some()), "workers build");
    assert_eq!(reviewers.len(), 2);
    assert!(reviewers.iter().all(|(_, p)| p.setup.is_none()), "reviewers only read the diff");
}

#[tokio::test]
async fn reconcile_returns_a_busy_pooled_sandbox_to_idle() {
    let r = rig();
    r.tracker.add_task("TRAIL", "First", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();
    // A control plane that died mid-task leaves its pooled sandbox busy.
    let mut labels = r.compute.get("trailhead-w1").unwrap().labels;
    labels.insert(label::STATE.into(), label::BUSY.into());
    labels.insert(label::TASK.into(), "TRAIL-9".into());
    let id = trailhead_factory::SandboxId("trailhead-w1".into());
    r.compute.set_labels(&id, &labels).await.unwrap();

    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.released, ["trailhead-w1"]);
    assert!(done.destroyed.is_empty(), "pooled sandboxes aren't destroyed");
    let after = r.compute.get("trailhead-w1").unwrap();
    assert_eq!(after.labels.get(label::STATE).map(String::as_str), Some(label::IDLE));
    assert!(!after.labels.contains_key(label::TASK));
}

#[tokio::test]
async fn a_pooled_sandbox_that_cant_be_restored_is_destroyed_without_stopping_reconcile() {
    let r = rig();
    r.tracker.add_task("TRAIL", "First", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();
    // Left busy by a crash, pointing at a warm checkpoint that's gone.
    let mut labels = r.compute.get("trailhead-w1").unwrap().labels;
    labels.insert(label::STATE.into(), label::BUSY.into());
    labels.insert(label::WARM.into(), "gone".into());
    r.compute.set_labels(&trailhead_factory::SandboxId("trailhead-w1".into()), &labels).await.unwrap();
    // And an orphan, to show reconcile carries on past the failure.
    live(&r.compute, "trail-99-a1", &TaskKey("TRAIL-99".into()), "worker").await;

    let done = r.factory.reconcile().await.unwrap();
    assert!(r.compute.get("trailhead-w1").is_none(), "never reused with leftovers; destroyed");
    assert_eq!(done.released, ["trailhead-w1"]);
    assert_eq!(done.destroyed, ["trail-99-a1"]);
}

/// Runs one task through the pool so `trailhead-w1` exists and is warm, then leaves it busy as a crash would, with
/// its storage gone bad for `ops`.
async fn wedged_pool(r: &Rig, ops: &[&'static str]) {
    r.tracker.add_task("TRAIL", "First", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();
    let mut labels = r.compute.get("trailhead-w1").unwrap().labels;
    labels.insert(label::STATE.into(), label::BUSY.into());
    labels.insert(label::TASK.into(), "TRAIL-9".into());
    labels.insert(label::ROLE.into(), "worker".into());
    r.compute.set_labels(&trailhead_factory::SandboxId("trailhead-w1".into()), &labels).await.unwrap();
    for op in ops {
        r.compute.fail("trailhead-w1", op);
    }
}

/// Reconciles until it gives up on a sandbox, stepping past each backoff.
async fn reconcile_until_abandoned(r: &Rig) {
    for _ in 0..20 {
        let done = r.factory.reconcile().await.unwrap();
        if !done.abandoned.is_empty() {
            assert_eq!(done.abandoned, ["trailhead-w1"]);
            return;
        }
        assert!(done.released.is_empty());
        tokio::time::advance(std::time::Duration::from_secs(5 * 60)).await;
    }
    panic!("never gave up on trailhead-w1");
}

#[tokio::test(start_paused = true)]
async fn a_pooled_sandbox_that_wont_release_is_abandoned_and_replaced() {
    let r = rig();
    wedged_pool(&r, &["set_egress", "destroy"]).await;
    // Retries back off: an immediate second reconcile doesn't try again.
    r.factory.reconcile().await.unwrap();
    let again = r.factory.reconcile().await.unwrap();
    assert!(again.abandoned.is_empty() && again.released.is_empty());

    // Five tries or ten minutes, whichever comes first; with these waits, ten minutes.
    reconcile_until_abandoned(&r).await;
    let w1 = r.compute.get("trailhead-w1").expect("destroy failed, so it's still there");
    assert_eq!(w1.labels.get(label::STATE).map(String::as_str), Some(label::BROKEN));
    assert!(!w1.labels.contains_key(label::ROLE), "no role, so no connector access");

    // The pool's one place is free again, so new work runs in a new sandbox.
    let key = r.tracker.add_task("TRAIL", "Second", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    assert_eq!(ran.len(), 1);
    assert_eq!(r.tracker.task(&key).run.host.as_deref(), Some("trailhead-w2"));

    // Once the compute host lets it, the broken one is destroyed.
    r.compute.heal("trailhead-w1");
    tokio::time::advance(std::time::Duration::from_secs(5 * 60)).await;
    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.destroyed, ["trailhead-w1"]);
    assert!(r.compute.get("trailhead-w1").is_none());
}

#[tokio::test(start_paused = true)]
async fn a_sandbox_that_cant_even_be_labelled_broken_is_still_left_out() {
    let r = rig();
    wedged_pool(&r, &["set_egress", "destroy", "set_labels"]).await;
    reconcile_until_abandoned(&r).await;
    let w1 = r.compute.get("trailhead-w1").unwrap();
    assert_eq!(w1.labels.get(label::STATE).map(String::as_str), Some(label::BUSY), "labels couldn't change");

    let key = r.tracker.add_task("TRAIL", "Second", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();
    assert_eq!(r.tracker.task(&key).run.host.as_deref(), Some("trailhead-w2"), "out of the cap in memory");
}

#[tokio::test(start_paused = true)]
async fn a_sandbox_that_recovers_is_released_as_usual() {
    let r = rig();
    wedged_pool(&r, &["set_egress"]).await;
    assert!(r.factory.reconcile().await.unwrap().released.is_empty());
    r.compute.heal("trailhead-w1");
    tokio::time::advance(std::time::Duration::from_secs(60)).await;
    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.released, ["trailhead-w1"]);
    assert!(done.abandoned.is_empty());
}

#[tokio::test]
async fn two_runs_starting_together_never_take_the_same_idle_sandbox() {
    let r = rig_with(&CONFIG.replace("name = \"trailhead\"", "name = \"trailhead\"\nmax_sandboxes = 4"));
    let first = r.tracker.add_task("TRAIL", "First", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();
    assert_eq!(r.tracker.task(&first).status, Status::InReview);

    let a = r.tracker.add_task("TRAIL", "A", "");
    let b = r.tracker.add_task("TRAIL", "B", "");
    // Both workers wait on a gate, so the two runs are in flight together.
    let gate = Arc::new(tokio::sync::Notify::new());
    for _ in 0..2 {
        r.harness.push(Role::Worker, Script::new(changes()).wait_for(gate.clone()));
        r.harness.push(Role::Reviewer, Script::new(approve()));
    }
    let (ta, tb) = (r.tracker.task(&a), r.tracker.task(&b));
    let release = async {
        until("both workers to start", || r.harness.contexts().len() >= 3).await;
        gate.notify_one();
        gate.notify_one();
    };
    let (ra, rb, ()) = tokio::join!(r.factory.run_task(&ta), r.factory.run_task(&tb), release);
    ra.unwrap();
    rb.unwrap();
    assert_eq!(r.tracker.task(&a).status, Status::InReview);
    assert_eq!(r.tracker.task(&b).status, Status::InReview);

    let hosts = [r.tracker.task(&a).run.host.clone().unwrap(), r.tracker.task(&b).run.host.clone().unwrap()];
    assert_ne!(hosts[0], hosts[1], "each run gets its own sandbox");
    let mut names: Vec<_> = r.compute.sandboxes().into_iter().map(|s| s.name).filter(|n| n.contains("-w")).collect();
    names.sort();
    assert_eq!(names, ["trailhead-w1", "trailhead-w2"], "one reused the idle sandbox, the other got a new one");
}

#[tokio::test]
async fn a_sandbox_out_of_disk_is_reported_before_the_run_starts() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Needs disk", "");
    r.compute.fail("trailhead-w1", "preflight");
    r.harness.push(Role::Worker, Script::new(changes()));

    let _ = r.factory.tick().await;
    assert!(r.codehost.prs().is_empty());
    assert!(r.compute.checkpoints("trailhead-w1").is_empty(), "never prepared, so never warmed");
    assert_ne!(r.tracker.task(&key).status, Status::Done);
}

#[tokio::test]
async fn a_host_out_of_disk_is_waited_out_never_parked() {
    let r = rig_with(&CONFIG.replace("name = \"trailhead\"", "name = \"trailhead\"\nmax_infra_failures = 2"));
    let key = r.tracker.add_task("TRAIL", "Waits for disk", "");
    for n in 1..=4 {
        r.compute.fail(&format!("trailhead-w{n}"), "preflight");
        let err = r.factory.run_task(&r.tracker.task(&key)).await.unwrap_err();
        assert!(matches!(err, trailhead_factory::Error::Unavailable(_)), "{err}");
    }
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::Open, "still waiting, not parked for a person");
    assert_eq!(task.run.infra_failures.unwrap_or(0), 0, "waiting for disk isn't counted as a failure");
    assert_eq!(task.run.attempt, Some(0), "no attempt was used");
}

#[tokio::test]
async fn disk_waits_dont_count_toward_parking_a_later_real_failure() {
    let r = rig_with(&CONFIG.replace("name = \"trailhead\"", "name = \"trailhead\"\nmax_infra_failures = 2"));
    let key = r.tracker.add_task("TRAIL", "Waits then fails", "");
    for n in 1..=2 {
        r.compute.fail(&format!("trailhead-w{n}"), "preflight");
        let err = r.factory.run_task(&r.tracker.task(&key)).await.unwrap_err();
        assert!(matches!(err, trailhead_factory::Error::Unavailable(_)), "{err}");
    }
    r.compute.fail("trailhead-w3", "set_labels");
    r.factory.run_task(&r.tracker.task(&key)).await.unwrap_err();
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::Open, "one real failure after the waits isn't enough to park");
    assert_eq!(task.run.infra_failures, Some(1));
}
