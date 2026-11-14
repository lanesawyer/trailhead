//! Running on several computes at once (`docs/factory/targets.md`): placement by priority and limit, pools per
//! target, and a target that can't be reached being unknown rather than empty.

use std::sync::Arc;

use trailhead_control::{Factory, Host, Placed};
use trailhead_factory::HostConfig;
use trailhead_factory::{
    Compute, FactoryConfig, Labels, Role, SandboxSpec, Status, Target, TaskKey, Tracker, Usd, label, run_name,
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
    two_with(TARGETS, None)
}

/// `two` with the given target tables, and the file its factory keeps the day's spend in.
fn two_with(targets: &str, spend_file: Option<std::path::PathBuf>) -> Two {
    two_capped_at(3, targets, spend_file)
}

/// `two_with` with the repo's `max_sandboxes` set to `repo_max`.
fn two_capped_at(repo_max: usize, targets: &str, spend_file: Option<std::path::PathBuf>) -> Two {
    let config =
        CONFIG.replace("name = \"trailhead\"\n", &format!("name = \"trailhead\"\nmax_sandboxes = {repo_max}\n"));
    let config = FactoryConfig::parse_table(&format!("{config}{targets}"), "TRAIL", None).unwrap();
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
    let mut factory =
        Factory::with_targets(config, placed, Arc::new(tracker.clone()), Host::new(&HostConfig::default()));
    if let Some(path) = spend_file {
        factory = factory.with_spend_file(path);
    }
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
    let report = trailhead_factory::RunPatch {
        host: trailhead_factory::Patch::Set(host.into()),
        target: trailhead_factory::Patch::Set(target.into()),
        started: trailhead_factory::Patch::Set(
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64,
        ),
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
    sub.report(
        &lost,
        trailhead_factory::RunPatch { target: trailhead_factory::Patch::Set("cloud".into()), ..update(1, &name) },
    )
    .await
    .unwrap();
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
    let factory = Factory::with_targets(config, placed, Arc::new(tracker.clone()), Host::new(&HostConfig::default()));
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

const CAPPED: &str = r#"
[targets.local]
kind = "docker"
priority = 1
max_sandboxes = 1

[targets.cloud]
kind = "sprites"
priority = 2
daily_usd = 5
"#;

fn spend_file(dir: &tempfile::TempDir, cloud_micros: u64, day: &str) -> std::path::PathBuf {
    spend_file_for(dir, "cloud", cloud_micros, day)
}

fn spend_file_for(dir: &tempfile::TempDir, target: &str, micros: u64, day: &str) -> std::path::PathBuf {
    let path = dir.path().join("spend.json");
    std::fs::write(&path, format!(r#"{{"day":"{day}","spent":{{"{target}":{micros}}}}}"#)).unwrap();
    path
}

fn today() -> String {
    jiff::Zoned::now().date().to_string()
}

#[tokio::test]
async fn a_target_at_its_daily_cap_takes_no_new_runs() {
    let dir = tempfile::tempdir().unwrap();
    // The cap was reached before this process started.
    let t = two_with(CAPPED, Some(spend_file(&dir, 5_000_000, &today())));
    let (first, second) = (t.tracker.add_task("TRAIL", "First", ""), t.tracker.add_task("TRAIL", "Second", ""));
    t.local.harness.push(Role::Worker, Script::new(changes()));
    t.local.harness.push(Role::Reviewer, Script::new(approve()));
    t.factory.tick().await.unwrap();
    assert_eq!(ran(&t.local, Role::Worker), vec![first.0.clone()]);
    assert!(ran(&t.cloud, Role::Worker).is_empty(), "the cap kept the second task off the cloud");
    assert_eq!(t.tracker.task(&second).status, Status::Open);
    let board = t.factory.status().snapshot().targets;
    assert_eq!(board.len(), 1, "{board:?}");
    assert_eq!((board[0].target.as_str(), board[0].capped), ("cloud", true));
    assert_eq!((board[0].spent, board[0].cap), (Usd(5_000_000), Usd(5_000_000)));
}

#[tokio::test]
async fn a_target_under_its_cap_or_with_yesterdays_spend_still_takes_runs() {
    let dir = tempfile::tempdir().unwrap();
    for (micros, day) in [(4_000_000, today()), (9_000_000, "2000-01-01".to_string())] {
        let t = two_with(CAPPED, Some(spend_file(&dir, micros, &day)));
        t.tracker.add_task("TRAIL", "First", "");
        let second = t.tracker.add_task("TRAIL", "Second", "");
        for side in [&t.local, &t.cloud] {
            side.harness.push(Role::Worker, Script::new(changes()));
            side.harness.push(Role::Reviewer, Script::new(approve()));
        }
        t.factory.tick().await.unwrap();
        assert_eq!(ran(&t.cloud, Role::Worker), vec![second.0.clone()]);
    }
}

#[tokio::test]
async fn a_reworked_task_stays_on_the_target_that_holds_its_sandbox() {
    let t = two();
    // Cloud holds the task's sandbox; local, first by priority, has room.
    let key = going_on_target(&t, &t.cloud.compute, "cloud", "trailhead-cloud-w1").await;
    t.tracker.set_status(&key, Status::Open);
    for side in [&t.local, &t.cloud] {
        side.harness.push(Role::Worker, Script::new(changes()));
        side.harness.push(Role::Reviewer, Script::new(approve()));
    }
    t.factory.run_task(&t.tracker.task(&key)).await.unwrap();
    assert_eq!(ran(&t.cloud, Role::Worker), vec![key.0.clone()]);
    assert!(ran(&t.local, Role::Worker).is_empty());
}

#[tokio::test]
async fn a_task_whose_sandbox_is_gone_is_placed_by_priority_again() {
    let t = two();
    let key = going_on_target(&t, &t.cloud.compute, "cloud", "trailhead-cloud-w1").await;
    t.cloud.compute.destroy(&t.cloud.compute.get("trailhead-cloud-w1").unwrap().id).await.unwrap();
    t.tracker.set_status(&key, Status::Open);
    t.local.harness.push(Role::Worker, Script::new(changes()));
    t.local.harness.push(Role::Reviewer, Script::new(approve()));
    t.factory.tick().await.unwrap();
    assert_eq!(ran(&t.local, Role::Worker), vec![key.0.clone()]);
}

#[tokio::test]
async fn the_spend_board_shows_while_the_factory_is_full() {
    let dir = tempfile::tempdir().unwrap();
    let t = two_with(CAPPED, Some(spend_file(&dir, 5_000_000, &today())));
    // Three sandboxes held for no running task fill the repo's cap of 3, so `tick` has no room and returns before
    // any pass reaches placement.
    for n in 0..3 {
        let labels = Labels::from([
            (label::FACTORY.into(), "trailhead".into()),
            (label::REPO.into(), "lanesawyer/trailhead".into()),
            (label::POOL.into(), "worker".into()),
            (label::STATE.into(), label::BUSY.into()),
            (label::TARGET.into(), "local".into()),
        ]);
        let spec = SandboxSpec { name: format!("trailhead-held-{n}"), labels, egress: Default::default() };
        t.local.compute.create(&spec).await.unwrap();
    }
    t.tracker.add_task("TRAIL", "Waiting", "");
    t.factory.tick().await.unwrap();
    assert!(ran(&t.local, Role::Worker).is_empty() && ran(&t.cloud, Role::Worker).is_empty());
    let board = t.factory.status().snapshot().targets;
    assert_eq!(board.len(), 1, "{board:?}");
    assert_eq!((board[0].target.as_str(), board[0].capped), ("cloud", true));
    assert_eq!((board[0].spent, board[0].cap), (Usd(5_000_000), Usd(5_000_000)));
}

/// Local first by priority, with `local_extra` in its table, then the cloud.
fn local_holds(local_extra: &str) -> String {
    format!(
        "[targets.local]\nkind = \"docker\"\npriority = 1\n{local_extra}\n[targets.cloud]\nkind = \"sprites\"\npriority = 2\n"
    )
}

/// With the repo's `max_sandboxes` at 1, a task that overran its budget and parked on local, then answered: it is ready
/// again and still holds the repo's one sandbox, which hasn't been released yet. Local being down or capped must not
/// send it to the cloud, which would put the repo at two sandboxes.
async fn answered_holder_on_local(t: &Two) -> TaskKey {
    let key = t.tracker.add_task("TRAIL", "Parked", "");
    t.local.harness.push(Role::Worker, Script::new(changes()).steps(10, 2_000_000));
    t.factory.tick().await.unwrap();
    assert_eq!(t.tracker.task(&key).status, Status::NeedsInput);
    assert_eq!(t.tracker.task(&key).run.target.as_deref(), Some("local"));
    t.tracker.answer(&key, "Go on.");
    for side in [&t.local, &t.cloud] {
        side.harness.push(Role::Worker, Script::new(changes()));
        side.harness.push(Role::Reviewer, Script::new(approve()));
    }
    key
}

#[tokio::test]
async fn a_holder_on_a_down_target_does_not_start_elsewhere_over_the_repo_cap() {
    let t = two_capped_at(1, &local_holds(""), None);
    let key = answered_holder_on_local(&t).await;
    t.local.compute.set_unreachable(true);
    // Whether the tick errors or finds nothing to do, the repo mustn't go to two sandboxes.
    let _ = t.factory.tick().await;
    assert!(ran(&t.cloud, Role::Worker).is_empty(), "starting on the cloud would put the repo at two sandboxes");
    assert_eq!(ran(&t.local, Role::Worker), vec![key.0.clone()], "only the parked run ever ran there");

    // Local answers again: the task resumes in the sandbox it holds.
    t.local.compute.set_unreachable(false);
    t.factory.tick().await.unwrap();
    assert_eq!(ran(&t.local, Role::Worker), vec![key.0.clone(); 2]);
    assert!(ran(&t.cloud, Role::Worker).is_empty());
}

#[tokio::test]
async fn a_holder_on_a_capped_target_waits_for_room_then_starts_elsewhere() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = two_capped_at(1, &local_holds("daily_usd = 5\n"), None);
    let key = answered_holder_on_local(&t).await;
    // Local reaches its daily cap while the task waits, and its sandbox can't be released just now.
    t.factory = t.factory.with_spend_file(spend_file_for(&dir, "local", 5_000_000, &today()));
    let held = t.local.compute.sandboxes().into_iter().next().unwrap();
    t.local.compute.fail(&held.name, "set_egress");

    assert!(t.factory.tick().await.unwrap().is_empty());
    let board = t.factory.status().snapshot().targets;
    assert!(board.iter().any(|b| b.target == "local" && b.capped), "{board:?}");
    assert!(ran(&t.cloud, Role::Worker).is_empty(), "starting on the cloud would put the repo at two sandboxes");
    assert_eq!(ran(&t.local, Role::Worker), vec![key.0.clone()], "local is at its daily cap");

    // The held sandbox goes: the repo has room, and the task starts on the one target that takes runs.
    t.local.compute.heal(&held.name);
    t.local.compute.destroy(&held.id).await.unwrap();
    t.factory.tick().await.unwrap();
    assert_eq!(ran(&t.cloud, Role::Worker), vec![key.0.clone()]);
}
