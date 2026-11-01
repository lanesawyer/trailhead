//! Running on several computes at once (`docs/factory/targets.md`): placement by priority and limit, pools per
//! target, and a target that can't be reached being unknown rather than empty.

use std::sync::Arc;

use trailhead_control::{Factory, Host, Placed};
use trailhead_factory::{
    Compute, FactoryConfig, Labels, Role, SandboxSpec, Status, Target, TaskKey, Tracker, label, run_name,
};
use trailhead_testkit::{FakeCodeHost, FakeCompute, FakeProvider, FakeTracker, LocalDaemon, Script, ScriptedHarness};

use crate::common::*;

const TARGETS: &str = r#"
[targets.local]
kind = "docker"
priority = 1
max_sandboxes = 1

[targets.cloud]
kind = "sprites"
priority = 2
"#;

/// One target's fakes: its compute and the harness its daemon runs, which says what ran there.
struct Side {
    compute: FakeCompute,
    harness: Arc<ScriptedHarness>,
}

struct Two {
    factory: Factory,
    tracker: FakeTracker,
    local: Side,
    cloud: Side,
}

fn two() -> Two {
    let config = CONFIG.replace("name = \"trailhead\"\n", "name = \"trailhead\"\nmax_sandboxes = 3\n");
    let config = FactoryConfig::parse_table(&format!("{config}{TARGETS}"), "TRAIL", None).unwrap();
    let tracker = FakeTracker::new("factory");
    let codehost = FakeCodeHost::new();
    let mut placed = Vec::new();
    let mut sides = Vec::new();
    for spec in config.target_specs() {
        let compute = FakeCompute::new();
        let harness = Arc::new(ScriptedHarness::new());
        let heads = codehost.clone();
        let daemon = Arc::new(
            LocalDaemon::new(harness.clone(), Arc::new(codehost.clone()), tracker.clone())
                .with_heads(move |repo, branch| heads.head(repo, branch)),
        );
        placed.push(Placed {
            target: Target::default().listed(&spec),
            compute: Arc::new(compute.clone()),
            daemon,
            provider: Arc::new(FakeProvider::new(1)),
        });
        sides.push(Side { compute, harness });
    }
    let factory = Factory::with_targets(config, placed, Arc::new(tracker.clone()), Host::new(None));
    let cloud = sides.pop().unwrap();
    let local = sides.pop().unwrap();
    Two { factory, tracker, local, cloud }
}

/// The tasks the harness ran a role for.
fn ran(side: &Side, role: Role) -> Vec<String> {
    side.harness.contexts().into_iter().filter(|c| c.role == role).map(|c| c.task.key.0).collect()
}

/// A worker run the last control plane left going on `compute`, recorded as on `target`.
async fn going_on_target(t: &Two, compute: &FakeCompute, target: &str, host: &str) -> TaskKey {
    let key = t.tracker.add_task("TRAIL", "Going", "");
    let name = run_name(&key, 1);
    let tracker: Arc<dyn Tracker> = Arc::new(t.tracker.clone());
    tracker.create_sub_agent(&name, "TRAIL", std::time::Duration::from_secs(3600)).await.unwrap();
    let sub = tracker.sub_agent(&name);
    sub.claim(&key).await.unwrap();
    let report = trailhead_factory::RunUpdate {
        host: Some(host.into()),
        target: Some(target.into()),
        started: Some(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64),
        ..update(1, &name)
    };
    sub.report(&key, report).await.unwrap();
    let labels = Labels::from([
        (label::FACTORY.into(), "trailhead".into()),
        (label::REPO.into(), "lanesawyer/trailhead".into()),
        (label::POOL.into(), "worker".into()),
        (label::STATE.into(), label::BUSY.into()),
        (label::TASK.into(), key.0.clone()),
        (label::ROLE.into(), "worker".into()),
        (label::TARGET.into(), target.into()),
    ]);
    compute.create(&SandboxSpec { name: host.into(), labels, egress: Default::default() }).await.unwrap();
    key
}

#[tokio::test]
async fn a_task_goes_to_the_first_target_with_room() {
    let t = two();
    let (first, second) = (t.tracker.add_task("TRAIL", "First", ""), t.tracker.add_task("TRAIL", "Second", ""));
    for side in [&t.local, &t.cloud] {
        side.harness.push(Role::Worker, Script::new(changes()));
        side.harness.push(Role::Reviewer, Script::new(approve()));
    }
    // Local holds one place, which the first task takes; the second spills to the cloud.
    let ran_now = t.factory.tick().await.unwrap();
    assert_eq!(ran_now.len(), 2);
    assert_eq!(ran(&t.local, Role::Worker), vec![first.0.clone()]);
    assert_eq!(ran(&t.cloud, Role::Worker), vec![second.0.clone()]);
    assert_eq!(t.tracker.task(&first).run.target.as_deref(), Some("local"));
    assert_eq!(t.tracker.task(&second).run.target.as_deref(), Some("cloud"));
}

#[tokio::test]
async fn pools_are_per_target_and_their_names_carry_it() {
    let t = two();
    t.tracker.add_task("TRAIL", "First", "");
    t.tracker.add_task("TRAIL", "Second", "");
    for side in [&t.local, &t.cloud] {
        side.harness.push(Role::Worker, Script::new(changes()));
        side.harness.push(Role::Reviewer, Script::new(approve()));
    }
    t.factory.tick().await.unwrap();
    let local = t.local.compute.sandboxes();
    assert_eq!(local.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["trailhead-local-w1"]);
    assert_eq!(local[0].labels.get(label::TARGET).map(String::as_str), Some("local"));
    let cloud = t.cloud.compute.sandboxes();
    assert_eq!(cloud.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["trailhead-cloud-w1"]);
    assert_eq!(cloud[0].labels.get(label::TARGET).map(String::as_str), Some("cloud"));
}

#[tokio::test]
async fn reviews_are_placed_on_their_own() {
    let t = two();
    // A parked task keeps local's one place, so the next worker and its review both go to the cloud.
    let parked = t.tracker.add_task("TRAIL", "Parked", "");
    t.local.harness.push(Role::Worker, Script::new(changes()).steps(10, 2_000_000));
    t.factory.tick().await.unwrap();
    assert_eq!(t.tracker.task(&parked).status, Status::NeedsInput);
    let next = t.tracker.add_task("TRAIL", "Next", "");
    t.cloud.harness.push(Role::Worker, Script::new(changes()));
    t.cloud.harness.push(Role::Reviewer, Script::new(approve()));
    t.factory.tick().await.unwrap();
    assert_eq!(ran(&t.cloud, Role::Worker), vec![next.0.clone()]);
    assert_eq!(ran(&t.cloud, Role::Reviewer), vec![next.0.clone()], "the review found local full too");
    assert!(ran(&t.local, Role::Reviewer).is_empty());
}

#[tokio::test]
async fn an_unreachable_target_gets_no_runs_and_the_next_one_takes_them() {
    let t = two();
    t.local.compute.set_unreachable(true);
    let key = t.tracker.add_task("TRAIL", "Only", "");
    t.cloud.harness.push(Role::Worker, Script::new(changes()));
    t.cloud.harness.push(Role::Reviewer, Script::new(approve()));
    t.factory.tick().await.unwrap();
    assert_eq!(ran(&t.cloud, Role::Worker), vec![key.0.clone()]);
    assert!(ran(&t.local, Role::Worker).is_empty());
}

#[tokio::test]
async fn with_every_target_down_nothing_starts_and_nothing_is_known() {
    let t = two();
    t.local.compute.set_unreachable(true);
    t.cloud.compute.set_unreachable(true);
    let key = t.tracker.add_task("TRAIL", "Waits", "");
    assert!(t.factory.tick().await.is_err(), "no target answered, so there is nothing to count places from");
    assert_eq!(t.tracker.task(&key).status, Status::Open);
    assert!(t.factory.reconcile().await.is_err(), "with no target answering, nothing is known");
}

#[tokio::test]
async fn reconcile_leaves_a_run_on_an_unreachable_target_alone() {
    let t = two();
    let key = going_on_target(&t, &t.local.compute, "local", "trailhead-local-w1").await;
    t.local.compute.set_unreachable(true);

    let done = t.factory.reconcile().await.unwrap();
    assert_eq!(done.unknown, vec![key.0.clone()]);
    assert!(done.requeued.is_empty() && done.destroyed.is_empty(), "{done:?}");
    assert_eq!(t.tracker.task(&key).status, Status::InProgress, "its sandbox isn't gone, only out of reach");
    assert!(t.local.compute.get("trailhead-local-w1").is_some());

    // Once the target answers, the run is found where it was and followed again.
    t.local.compute.set_unreachable(false);
    t.local.harness.push(Role::Worker, Script::new(changes()));
    let done = t.factory.reconcile().await.unwrap();
    assert!(done.unknown.is_empty());
    assert_eq!(done.reattached, vec![key.0.clone()]);
}

#[tokio::test]
async fn a_run_on_a_reachable_target_is_still_requeued_when_another_is_down() {
    let t = two();
    let lost = t.tracker.add_task("TRAIL", "Lost", "");
    let name = run_name(&lost, 1);
    let tracker: Arc<dyn Tracker> = Arc::new(t.tracker.clone());
    tracker.create_sub_agent(&name, "TRAIL", std::time::Duration::from_secs(3600)).await.unwrap();
    let sub = tracker.sub_agent(&name);
    sub.claim(&lost).await.unwrap();
    sub.report(&lost, trailhead_factory::RunUpdate { target: Some("cloud".into()), ..update(1, &name) }).await.unwrap();
    t.local.compute.set_unreachable(true);
    let done = t.factory.reconcile().await.unwrap();
    assert_eq!(done.requeued, vec![lost.0.clone()], "the cloud answered and has no sandbox for it");
    assert!(done.unknown.is_empty());
}

#[tokio::test]
async fn a_review_counts_on_its_own_target_and_the_worker_no_longer_on_its() {
    // Cloud holds one place. Its worker is released before the review, so the review fits where the worker ran; had
    // the worker's place been kept until the run ended, the review would find cloud full and wait.
    let config = CONFIG.replace("name = \"trailhead\"\n", "name = \"trailhead\"\nmax_sandboxes = 3\n");
    let targets = TARGETS.replace("priority = 2\n", "priority = 2\nmax_sandboxes = 1\n");
    let config = FactoryConfig::parse_table(&format!("{config}{targets}"), "TRAIL", None).unwrap();
    let tracker = FakeTracker::new("factory");
    let codehost = FakeCodeHost::new();
    let mut placed = Vec::new();
    let mut harnesses = Vec::new();
    for spec in config.target_specs() {
        let harness = Arc::new(ScriptedHarness::new());
        let heads = codehost.clone();
        let daemon = Arc::new(
            LocalDaemon::new(harness.clone(), Arc::new(codehost.clone()), tracker.clone())
                .with_heads(move |repo, branch| heads.head(repo, branch)),
        );
        placed.push(Placed {
            target: Target::default().listed(&spec),
            compute: Arc::new(FakeCompute::new()),
            daemon,
            provider: Arc::new(FakeProvider::new(1)),
        });
        harnesses.push(harness);
    }
    let factory = Factory::with_targets(config, placed, Arc::new(tracker.clone()), Host::new(None));
    let (local, cloud) = (&harnesses[0], &harnesses[1]);
    // A parked task keeps local's one place, so the next worker goes to the cloud.
    let parked = tracker.add_task("TRAIL", "Parked", "");
    local.push(Role::Worker, Script::new(changes()).steps(10, 2_000_000));
    factory.tick().await.unwrap();
    assert_eq!(tracker.task(&parked).status, Status::NeedsInput);
    let next = tracker.add_task("TRAIL", "Next", "");
    cloud.push(Role::Worker, Script::new(changes()));
    cloud.push(Role::Reviewer, Script::new(approve()));
    factory.tick().await.unwrap();
    let ran_on = |h: &Arc<ScriptedHarness>, role| {
        h.contexts().into_iter().filter(|c| c.role == role).map(|c| c.task.key.0).collect::<Vec<_>>()
    };
    assert_eq!(ran_on(cloud, Role::Worker), vec![next.0.clone()]);
    assert_eq!(ran_on(cloud, Role::Reviewer), vec![next.0.clone()], "the worker's place was given up for the review");
}
