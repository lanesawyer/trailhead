use std::sync::Arc;

use trailhead_control::{Host, Outcome};
use trailhead_factory::{Compute, Role, RunOutcome, SandboxSpec, Status, TaskKey, Usd, label, run_name};
use trailhead_testkit::{Script, TrackerOp, unix_now};

use crate::common::*;
use crate::logs::Logs;

#[tokio::test]
async fn a_parked_sandbox_keeps_its_place_in_the_cap() {
    let r = rig();
    r.tracker.add_task("TRAIL", "Expensive", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(10, 2_000_000));
    r.factory.tick().await.unwrap();
    // The kill switch took the role label away, but the parked task still holds the pool's one place.
    r.tracker.add_task("TRAIL", "Next", "");
    assert!(r.factory.tick().await.unwrap().is_empty());
    assert_eq!(r.compute.sandboxes().len(), 1, "no second sandbox");
}

#[tokio::test]
async fn reopening_a_parked_task_frees_its_place_without_a_reconcile() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Ambiguous", "");
    r.harness.push(Role::Worker, Script::new(RunOutcome::NeedsInput { question: "Which crate?".into() }));
    r.factory.tick().await.unwrap();
    assert_eq!(busy(&r.compute), ["trailhead-w1"]);

    // Still parked: the place stays taken.
    r.tracker.add_task("TRAIL", "Next", "");
    assert!(r.factory.tick().await.unwrap().is_empty());

    r.tracker.answer(&key, "The core crate.");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    assert!(!ran.is_empty(), "the reopened task started without a reconcile");
    let answered =
        r.harness.contexts().iter().any(|c| c.task.context.answers.iter().any(|a| a.answer == "The core crate."));
    assert!(answered, "the next attempt has the answer in its prompt");
}

#[tokio::test]
async fn cancelling_or_finishing_a_parked_task_frees_its_place_without_a_reconcile() {
    for status in [Status::Cancelled, Status::Done] {
        let r = rig();
        let key = r.tracker.add_task("TRAIL", "Expensive", "");
        r.harness.push(Role::Worker, Script::new(changes()).steps(10, 2_000_000));
        r.factory.tick().await.unwrap();
        assert_eq!(r.tracker.task(&key).status, Status::NeedsInput);

        let next = r.tracker.add_task("TRAIL", "Next", "");
        assert!(r.factory.tick().await.unwrap().is_empty(), "the parked task holds the one place");

        r.tracker.set_status(&key, status);
        r.harness.push(Role::Worker, Script::new(changes()));
        r.harness.push(Role::Reviewer, Script::new(approve()));
        let ran = r.factory.tick().await.unwrap();
        assert!(ran.iter().any(|(t, _)| t.key == next), "{status:?}: the next task started without a reconcile");
    }
}

#[tokio::test]
async fn the_run_loop_frees_a_parked_tasks_place_when_it_is_reopened() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Ambiguous", "");
    r.harness.push(Role::Worker, Script::new(RunOutcome::NeedsInput { question: "Which crate?".into() }));
    r.factory.tick().await.unwrap();
    let next = r.tracker.add_task("TRAIL", "Next", "");

    let factory = Arc::new(r.factory);
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let handle =
        tokio::spawn(factory.run(Arc::new(tokio::sync::Notify::new()), std::time::Duration::from_millis(5), async {
            let _ = stopped.await;
        }));
    // Parked: the loop polls many times and the one place stays taken.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(r.tracker.task(&next).status, Status::Open, "the parked task still holds the place");

    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.tracker.answer(&key, "The core crate.");
    let tracker = r.tracker.clone();
    until("the reopened task to run", || tracker.task(&key).status != Status::NeedsInput).await;
    let harness = r.harness.clone();
    until("the answer to reach a prompt", || {
        harness.contexts().iter().any(|c| c.task.context.answers.iter().any(|a| a.answer == "The core crate."))
    })
    .await;
    stop.send(()).unwrap();
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn sandbox_cap_limits_concurrent_runs() {
    let r = rig();
    r.tracker.add_task("TRAIL", "One", "");
    r.tracker.add_task("TRAIL", "Two", "");
    // A live worker sandbox from elsewhere fills the cap of one.
    let mut labels = trailhead_factory::Labels::new();
    labels.insert(label::FACTORY.into(), "trailhead".into());
    labels.insert(label::REPO.into(), "lanesawyer/trailhead".into());
    labels.insert(label::ROLE.into(), "worker".into());
    let spec = SandboxSpec { name: "busy".into(), labels, egress: Default::default() };
    r.compute.create(&spec).await.unwrap();
    assert!(r.factory.tick().await.unwrap().is_empty());
}

fn two_at_a_time() -> Rig {
    rig_with(&CONFIG.replace("name = \"trailhead\"", "name = \"trailhead\"\nmax_sandboxes = 2"))
}

#[tokio::test]
async fn run_loop_reconciles_then_works_the_queue_and_stops() {
    let r = two_at_a_time();
    live(&r.compute, "trail-99-a1", &TaskKey("TRAIL-99".into()), "worker").await;
    let keys: Vec<TaskKey> = (1..=3).map(|i| r.tracker.add_task("TRAIL", &format!("task {i}"), "")).collect();
    for _ in 0..3 {
        r.harness.push(Role::Worker, Script::new(changes()));
        r.harness.push(Role::Reviewer, Script::new(approve()));
    }

    let factory = Arc::new(r.factory);
    let wake = Arc::new(tokio::sync::Notify::new());
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let poll = std::time::Duration::from_millis(10);
    let handle = tokio::spawn(factory.run(wake.clone(), poll, async {
        let _ = stopped.await;
    }));

    let tracker = r.tracker.clone();
    until("every task in review", || keys.iter().all(|k| tracker.task(k).status == Status::InReview)).await;
    assert_eq!(busy(&r.compute), Vec::<String>::new(), "the orphan went at boot and runs cleaned up");

    // New work is picked up on wake.
    let late = r.tracker.add_task("TRAIL", "late", "");
    r.harness.push(Role::Worker, Script::new(RunOutcome::NeedsInput { question: "?".into() }));
    wake.notify_one();
    until("the late task to park", || tracker.task(&late).status == Status::NeedsInput).await;

    stop.send(()).unwrap();
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_wake_starts_new_work_without_waiting_for_a_long_poll() {
    let r = rig();
    let factory = Arc::new(r.factory);
    let wake = Arc::new(tokio::sync::Notify::new());
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let poll = std::time::Duration::from_secs(600);
    let handle = tokio::spawn(factory.run(wake.clone(), poll, async {
        let _ = stopped.await;
    }));

    // The loop is idle on its ten-minute poll; only the wake can start the new task.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let key = r.tracker.add_task("TRAIL", "late", "");
    r.harness.push(Role::Worker, Script::new(RunOutcome::NeedsInput { question: "?".into() }));
    wake.notify_one();
    let tracker = r.tracker.clone();
    until("the task to park", || tracker.task(&key).status == Status::NeedsInput).await;

    stop.send(()).unwrap();
    handle.await.unwrap().unwrap();
}

/// Starts the run loop on one hanging task, stops it once the task is running, and returns how long the stop took.
async fn stop_mid_run(r: Rig) -> std::time::Duration {
    let key = r.tracker.add_task("TRAIL", "Hangs", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 100).hang());
    let factory = Arc::new(r.factory);
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let handle =
        tokio::spawn(factory.run(Arc::new(tokio::sync::Notify::new()), std::time::Duration::from_secs(60), async {
            let _ = stopped.await;
        }));
    let tracker = r.tracker.clone();
    until("the task to start", || tracker.task(&key).status == Status::InProgress && !r.daemon.requests().is_empty())
        .await;
    let start = tokio::time::Instant::now();
    stop.send(()).unwrap();
    handle.await.unwrap().unwrap();
    start.elapsed()
}

#[tokio::test(start_paused = true)]
async fn stopping_the_run_loop_cancels_runs_in_flight() {
    let took = stop_mid_run(rig()).await;
    assert!(took < trailhead_control::SHUTDOWN_GRACE, "the cancel landed rather than waiting out the grace: {took:?}");
}

#[tokio::test(start_paused = true)]
async fn a_run_that_ignores_the_stop_is_abandoned_after_the_grace() {
    let r = rig();
    r.daemon.ignore_cancel();
    assert_eq!(stop_mid_run(r).await, trailhead_control::SHUTDOWN_GRACE);
}

/// The run loop on one task held at a gate, drained once it runs, with a second task queued during the drain.
struct Drain {
    r: Rig,
    first: TaskKey,
    second: TaskKey,
    gate: Arc<tokio::sync::Notify>,
    cancel: tokio::sync::oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<trailhead_factory::Result<()>>,
}

async fn drain_one_in_flight(r: Rig) -> Drain {
    drain_one_in_flight_polling(r, std::time::Duration::from_secs(60)).await
}

async fn drain_one_in_flight_polling(mut r: Rig, poll: std::time::Duration) -> Drain {
    let first = r.tracker.add_task("TRAIL", "First", "");
    let gate = Arc::new(tokio::sync::Notify::new());
    r.harness.push(Role::Worker, Script::new(changes()).wait_for(gate.clone()));
    let factory = Arc::new(std::mem::replace(&mut r.factory, rig().factory));
    let (drain, draining) = tokio::sync::oneshot::channel::<()>();
    let (cancel, cancelled) = tokio::sync::oneshot::channel::<()>();
    let wake = Arc::new(tokio::sync::Notify::new());
    let handle = tokio::spawn(factory.run_draining(
        wake.clone(),
        poll,
        async {
            let _ = draining.await;
        },
        async {
            let _ = cancelled.await;
        },
    ));
    let tracker = r.tracker.clone();
    until("the first task to start", || tracker.task(&first).status == Status::InProgress).await;
    drain.send(()).unwrap();
    let second = r.tracker.add_task("TRAIL", "Second", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    wake.notify_one();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(r.tracker.task(&second).status, Status::Open, "a drain starts nothing new");
    assert!(!handle.is_finished(), "the run in flight keeps the drain going");
    Drain { r, first, second, gate, cancel, handle }
}

#[tokio::test]
async fn a_drain_starts_nothing_new_and_returns_when_the_run_in_flight_finishes() {
    let r = rig();
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let d = drain_one_in_flight(r).await;

    d.gate.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(10), d.handle).await.unwrap().unwrap().unwrap();

    // The finished worker run went on to its review, and nothing else began.
    assert_eq!(d.r.tracker.task(&d.first).status, Status::InReview);
    assert_eq!(d.r.tracker.task(&d.first).run.reviewed, Some(1));
    assert_eq!(d.r.tracker.task(&d.second).status, Status::Open);
}

#[tokio::test]
async fn a_drain_keeps_following_a_live_run_reconcile_finds() {
    let d = drain_one_in_flight_polling(rig(), std::time::Duration::from_millis(20)).await;

    // A run the daemon still has turns up mid-drain, as when a run errors on the control plane's side.
    let key = going_on(&d.r, Usd::ZERO, 60, "trailhead-w9").await;
    d.r.daemon.ongoing_hung(&run_name(&key, 1), vec![spent(1_000)]);
    let daemon = d.r.daemon.clone();
    until("the live run to be followed", || daemon.attached() == [run_name(&key, 1)]).await;

    // Once the first run finishes the drain still waits on the one it picked up.
    d.gate.notify_one();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!d.handle.is_finished(), "the drain waits for the live run too");
    assert_eq!(d.r.tracker.task(&d.second).status, Status::Open, "and starts nothing new");

    d.cancel.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), d.handle).await.unwrap().unwrap().unwrap();
}

#[tokio::test]
async fn a_drain_follows_the_last_run_when_it_errors_but_the_daemon_keeps_it() {
    let mut r = rig();
    r.tracker.add_task("TRAIL", "First", "");
    let gate = Arc::new(tokio::sync::Notify::new());
    r.daemon.gate_prepare(gate.clone());
    let factory = Arc::new(std::mem::replace(&mut r.factory, rig().factory));
    let (drain, draining) = tokio::sync::oneshot::channel::<()>();
    let (cancel, cancelled) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(factory.run_draining(
        Arc::new(tokio::sync::Notify::new()),
        std::time::Duration::from_secs(60),
        async {
            let _ = draining.await;
        },
        async {
            let _ = cancelled.await;
        },
    ));
    let daemon = r.daemon.clone();
    until("the run to be preparing", || daemon.prepares_started() == 1).await;
    drain.send(()).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // The run's stream drops on the control plane's side while the daemon still has a run reconcile hasn't seen.
    r.daemon.fail_runs(1, "stream dropped");
    let live = going_on(&r, Usd::ZERO, 60, "trailhead-w9").await;
    r.daemon.ongoing_hung(&run_name(&live, 1), vec![spent(1_000)]);
    gate.notify_one();
    until("the live run to be followed", || daemon.attached() == [run_name(&live, 1)]).await;
    assert!(!handle.is_finished(), "the drain waits for the run reconcile found");

    cancel.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), handle).await.unwrap().unwrap().unwrap();
}

#[tokio::test]
async fn a_second_signal_during_a_drain_cancels_the_run() {
    let d = drain_one_in_flight(rig()).await;

    d.cancel.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), d.handle).await.unwrap().unwrap().unwrap();
    assert_eq!(d.r.tracker.task(&d.second).status, Status::Open);
}

#[tokio::test]
async fn run_loop_respects_the_sandbox_cap() {
    let r = two_at_a_time();
    for i in 1..=3 {
        r.tracker.add_task("TRAIL", &format!("slow {i}"), "");
        r.harness.push(Role::Worker, Script::new(changes()).hang());
    }
    let factory = Arc::new(r.factory);
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let handle =
        tokio::spawn(factory.run(Arc::new(tokio::sync::Notify::new()), std::time::Duration::from_millis(5), async {
            let _ = stopped.await;
        }));
    let compute = r.compute.clone();
    until("two sandboxes", || compute.sandboxes().len() == 2).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(r.compute.sandboxes().len(), 2, "never more than the cap");
    assert_eq!(r.tracker.task(&TaskKey("TRAIL-3".into())).status, Status::Open);

    // Shutdown abandons the runs; the next boot's reconcile requeues them.
    stop.send(()).unwrap();
    handle.await.unwrap().unwrap();
}

/// The test config on a host pausing at 80% of the five-hour window.
fn pausing_rig() -> Rig {
    let pause_at = [("five_hour".to_string(), 80)].into();
    rig_on(CONFIG, Host::new(&trailhead_factory::HostConfig { pause_at, ..Default::default() }))
}

/// Runs one task whose agent reports the five-hour window `used` percent full, resetting in `resets_in` seconds.
async fn report_usage(r: &Rig, used: u32, resets_in: i64) {
    r.tracker.add_task("TRAIL", "First", "");
    let script = Script::new(changes()).quota("five_hour", used, unix_now() + resets_in).quota("seven_day", 99, 0);
    r.harness.push(Role::Worker, script);
    r.harness.push(Role::Reviewer, Script::new(approve()));
    assert_eq!(r.factory.tick().await.unwrap().len(), 1, "a run already under way finishes");
}

#[tokio::test]
async fn a_nearly_full_usage_window_pauses_new_runs() {
    let r = pausing_rig();
    report_usage(&r, 85, 3600).await;
    r.tracker.add_task("TRAIL", "Second", "");
    assert!(r.factory.tick().await.unwrap().is_empty(), "paused until the window resets");
}

#[tokio::test(start_paused = true)]
async fn runs_start_again_once_the_window_resets() {
    let r = pausing_rig();
    report_usage(&r, 95, 1).await;
    r.tracker.add_task("TRAIL", "Second", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    assert!(r.factory.tick().await.unwrap().is_empty());
    tokio::time::advance(std::time::Duration::from_millis(2100)).await;
    assert_eq!(r.factory.tick().await.unwrap().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn the_run_loop_wakes_itself_when_the_window_resets() {
    let r = pausing_rig();
    report_usage(&r, 90, 1).await;
    let key = r.tracker.add_task("TRAIL", "Second", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let factory = Arc::new(r.factory);
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    // A poll far longer than the test: only the reset timer can start the second task in time.
    let poll = std::time::Duration::from_secs(600);
    let handle = tokio::spawn(factory.run(Arc::new(tokio::sync::Notify::new()), poll, async {
        let _ = stopped.await;
    }));
    let tracker = r.tracker.clone();
    for _ in 0..100 {
        if tracker.task(&key).status == Status::InReview {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(tracker.task(&key).status, Status::InReview, "started once the window reset");
    stop.send(()).unwrap();
    handle.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn the_run_loop_holds_due_reviews_while_the_window_is_paused() {
    held_reviews_land_after_the_window_resets(std::time::Duration::from_millis(5)).await;
}

/// With a poll far longer than the test and nothing waking the loop, only the reset timer can start the review.
#[tokio::test(start_paused = true)]
async fn the_window_reset_alone_starts_the_reviews_it_held() {
    held_reviews_land_after_the_window_resets(std::time::Duration::from_secs(60)).await;
}

async fn held_reviews_land_after_the_window_resets(poll: std::time::Duration) {
    let r = pausing_rig();
    let key = r.tracker.add_task("TRAIL", "Unreviewed", "");
    let script = Script::new(changes()).quota("five_hour", 90, unix_now() + 120);
    r.harness.push(Role::Worker, script);
    r.harness.push(Role::Reviewer, Script::new(RunOutcome::Failed { reason: "control plane died".into() }));
    let ran = r.factory.run_task(&r.tracker.task(&key)).await.unwrap();
    assert!(matches!(ran, Outcome::AwaitingReview { .. }), "{ran:?}");
    r.harness.push(Role::Reviewer, Script::new(approve()));
    tokio::time::advance(std::time::Duration::from_secs(61)).await; // out of the no-verdict backoff

    let (logs, _capture) = Logs::capture();

    let factory = Arc::new(r.factory);
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(factory.run(Arc::new(tokio::sync::Notify::new()), poll, async {
        let _ = stopped.await;
    }));
    let tracker = r.tracker.clone();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(tracker.task(&key).run.reviewed, None, "no reviewer starts while the window is paused");
    let held = || logs.text().matches("holding due reviews back").count();
    assert_eq!(held(), 1, "said once, naming the review: {}", logs.text());
    assert!(logs.text().contains(&key.0), "{}", logs.text());
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(held(), 1, "not repeated on every poll");
    tokio::time::advance(std::time::Duration::from_secs(60)).await; // past the window's reset
    until("the review to land once the window resets", || tracker.task(&key).run.reviewed == Some(1)).await;
    stop.send(()).unwrap();
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn usage_under_the_threshold_or_without_one_doesnt_pause() {
    // 70% is under the five-hour threshold, and the seven-day window at 99% has none configured.
    let r = pausing_rig();
    report_usage(&r, 70, 3600).await;
    r.tracker.add_task("TRAIL", "Second", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    assert_eq!(r.factory.tick().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_drain_ends_when_the_last_run_errors_and_the_daemon_no_longer_has_it() {
    let mut r = rig();
    r.tracker.add_task("TRAIL", "First", "");
    let gate = Arc::new(tokio::sync::Notify::new());
    r.daemon.gate_prepare(gate.clone());
    let factory = Arc::new(std::mem::replace(&mut r.factory, rig().factory));
    let (drain, draining) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(factory.run_draining(
        Arc::new(tokio::sync::Notify::new()),
        std::time::Duration::from_millis(20),
        async {
            let _ = draining.await;
        },
        std::future::pending::<()>(),
    ));
    let daemon = r.daemon.clone();
    until("the run to be preparing", || daemon.prepares_started() == 1).await;
    drain.send(()).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // The run's stream drops and the daemon has nothing left, so reconcile finds nothing and the drain is over.
    r.daemon.fail_runs(1, "stream dropped");
    gate.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(10), handle).await.unwrap().unwrap().unwrap();
}

#[tokio::test]
async fn a_drain_retries_the_reconcile_when_it_fails_after_the_last_run_errors() {
    let mut r = rig();
    r.tracker.add_task("TRAIL", "First", "");
    let gate = Arc::new(tokio::sync::Notify::new());
    r.daemon.gate_prepare(gate.clone());
    let factory = Arc::new(std::mem::replace(&mut r.factory, rig().factory));
    let (drain, draining) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(factory.run_draining(
        Arc::new(tokio::sync::Notify::new()),
        std::time::Duration::from_millis(20),
        async {
            let _ = draining.await;
        },
        std::future::pending::<()>(),
    ));
    let daemon = r.daemon.clone();
    until("the run to be preparing", || daemon.prepares_started() == 1).await;
    drain.send(()).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // The run errors while the compute API is down, so reconcile can't look: the drain mustn't conclude on that.
    r.daemon.fail_runs(1, "stream dropped");
    r.compute.set_unreachable(true);
    gate.notify_one();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!handle.is_finished(), "a failed reconcile doesn't end the drain");

    r.compute.set_unreachable(false);
    tokio::time::timeout(std::time::Duration::from_secs(10), handle).await.unwrap().unwrap().unwrap();
}

const AFTER: std::time::Duration = std::time::Duration::from_secs(61);

fn releasing_after(minutes: u32) -> Rig {
    rig_with(
        &CONFIG.replace("name = \"trailhead\"", &format!("name = \"trailhead\"\nrelease_parked_after = {minutes}")),
    )
}

/// A task parked with a question on the pool's one sandbox, and a second task waiting for a place.
async fn parked_with_one_waiting(r: &Rig) -> (TaskKey, TaskKey) {
    let parked = r.tracker.add_task("TRAIL", "Ambiguous", "");
    r.harness.push(Role::Worker, Script::new(RunOutcome::NeedsInput { question: "Already implemented?".into() }));
    r.factory.tick().await.unwrap();
    assert_eq!(r.tracker.task(&parked).status, Status::NeedsInput);
    let waiting = r.tracker.add_task("TRAIL", "Next", "");
    (parked, waiting)
}

#[tokio::test(start_paused = true)]
async fn a_parked_task_with_no_changes_gives_its_sandbox_to_a_waiting_task_after_the_delay() {
    let r = releasing_after(1);
    r.daemon.set_remote_branches(&["main"]);
    let (parked, waiting) = parked_with_one_waiting(&r).await;
    r.tracker.lapse_lease(&parked);

    assert!(r.factory.tick().await.unwrap().is_empty(), "not parked long enough yet");
    assert_eq!(r.tracker.task(&parked).run.released, None);

    tokio::time::advance(AFTER).await;
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    assert!(ran.iter().any(|(t, _)| t.key == waiting), "the waiting task took the place");
    let task = r.tracker.task(&parked);
    assert_eq!(task.status, Status::NeedsInput, "the question still stands");
    assert!(task.run.released.is_some_and(|at| at > 0), "the release is recorded");
    assert_eq!(r.compute.sandboxes().len(), 1, "the pool's one sandbox was reused");
    assert_eq!(
        r.daemon.unpublished_asked(),
        ["factory/trail-1", "main"],
        "its branch isn't pushed, so the base branch"
    );
}

#[tokio::test(start_paused = true)]
async fn a_parked_task_with_an_unpushed_diff_keeps_its_sandbox() {
    let r = releasing_after(1);
    let (parked, waiting) = parked_with_one_waiting(&r).await;
    r.tracker.lapse_lease(&parked);
    r.daemon.set_unpublished(true);

    tokio::time::advance(AFTER).await;
    assert!(r.factory.tick().await.unwrap().is_empty(), "the diff exists nowhere else");
    assert_eq!(r.tracker.task(&waiting).status, Status::Open);
    assert_eq!(r.tracker.task(&parked).run.released, None);
    assert_eq!(busy(&r.compute), ["trailhead-w1"]);
}

#[tokio::test(start_paused = true)]
async fn release_parked_after_zero_never_releases() {
    let r = releasing_after(0);
    let (parked, waiting) = parked_with_one_waiting(&r).await;
    r.tracker.lapse_lease(&parked);

    tokio::time::advance(std::time::Duration::from_secs(24 * 3600)).await;
    assert!(r.factory.tick().await.unwrap().is_empty());
    assert_eq!(r.tracker.task(&waiting).status, Status::Open);
    assert_eq!(r.tracker.task(&parked).run.released, None);
    assert!(r.daemon.unpublished_asked().is_empty(), "the daemon wasn't even asked");
}

#[tokio::test(start_paused = true)]
async fn nothing_is_released_when_no_task_is_waiting() {
    let r = releasing_after(1);
    let (parked, waiting) = parked_with_one_waiting(&r).await;
    r.tracker.lapse_lease(&parked);
    r.tracker.set_status(&waiting, Status::Cancelled);

    tokio::time::advance(AFTER).await;
    assert!(r.factory.tick().await.unwrap().is_empty());
    assert_eq!(r.tracker.task(&parked).run.released, None);
    assert_eq!(busy(&r.compute), ["trailhead-w1"]);
}

#[tokio::test(start_paused = true)]
async fn a_parked_task_with_an_open_pr_gives_up_its_sandbox_and_resumes_on_its_branch() {
    let r = releasing_after(1);
    let key = r.tracker.add_task("TRAIL", "Add a greeting", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(request_changes("Add a test for `hi()`.")));
    r.factory.tick().await.unwrap();
    // The rework asks a question instead of finishing.
    r.harness.push(Role::Worker, Script::new(RunOutcome::NeedsInput { question: "Which test style?".into() }));
    r.factory.tick().await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::NeedsInput);
    assert!(r.tracker.task(&key).run.pr.is_some());
    r.tracker.lapse_lease(&key);
    let waiting = r.tracker.add_task("TRAIL", "Next", "");
    assert!(r.factory.tick().await.unwrap().is_empty(), "not parked long enough yet");

    tokio::time::advance(AFTER).await;
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    assert!(ran.iter().any(|(t, _)| t.key == waiting), "the waiting task took the place");
    assert!(r.tracker.task(&key).run.released.is_some_and(|at| at > 0));
    assert_eq!(r.daemon.unpublished_asked(), ["factory/trail-1"], "asked against the PR branch");

    // Answered, it resumes like a rework: a pooled sandbox on its branch, with the answer in its prompt.
    r.tracker.answer(&key, "Table-driven.");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    assert!(ran.iter().any(|(t, _)| t.key == key), "the answered task started again");
    let prepared = r.daemon.prepared();
    assert_eq!(prepared.last().unwrap().1.branch.as_deref(), Some("factory/trail-1"));
    let answered =
        r.harness.contexts().iter().any(|c| c.task.context.answers.iter().any(|a| a.answer == "Table-driven."));
    assert!(answered, "the answer reached the agent");
    assert_eq!(r.tracker.task(&key).run.released, None, "the new run clears the release");
}
const GATED: &str = "
[gates.1]
require = [\"checks\", \"ci\", \"agent_review\"]
auto_merge = true
";

#[tokio::test]
async fn a_rework_of_an_open_pr_starts_before_older_fresh_tasks_of_its_priority() {
    let (r, queue) = queued_rig_with(GATED);
    // Older and of the same priority as the rework, which is filed after it; graded once the rework has its PR.
    let fresh = r.tracker.add_subtask(&queue, "Fresh", "");
    let (key, pr) = reviewed(&r, &queue, 1, "docs/a.md").await;
    assert!(fresh.0 < key.0);
    r.tracker.set_grade(&fresh, 1);
    r.codehost.set_ci(&pr, trailhead_factory::CiState::Failed { checks: vec!["test".into()] });
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::InReview);

    r.harness.push(Role::Worker, Script::new(changes_to("docs/a.md")));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    let keys: Vec<_> = ran.iter().map(|(t, _)| &t.key).collect();
    assert_eq!(keys, [&key], "the rework starts before the fresh task");
    assert_eq!(r.tracker.task(&fresh).status, Status::Open);
}

#[tokio::test]
async fn polling_never_looks_up_a_parked_task_but_still_releases_one_that_was_cancelled() {
    let r = rig();
    let parked = r.tracker.add_task("TRAIL", "Ambiguous", "");
    r.harness.push(Role::Worker, Script::new(RunOutcome::NeedsInput { question: "Already implemented?".into() }));
    r.factory.tick().await.unwrap();
    assert_eq!(r.tracker.task(&parked).status, Status::NeedsInput);
    assert_eq!(busy(&r.compute), ["trailhead-w1"]);

    let before = r.tracker.shows(&parked);
    for _ in 0..3 {
        r.factory.tick().await.unwrap();
    }
    assert_eq!(r.tracker.shows(&parked), before, "the in-flight listing answered for it");
    assert_eq!(busy(&r.compute), ["trailhead-w1"], "still parked");

    r.tracker.set_status(&parked, Status::Cancelled);
    r.factory.tick().await.unwrap();
    assert!(r.tracker.shows(&parked) > before, "a task out of the listing is looked up");
    assert!(busy(&r.compute).is_empty(), "its sandbox was released");
}

#[tokio::test]
async fn reworks_that_hold_their_own_sandboxes_start_with_the_cap_full() {
    let config = CONFIG.replace("name = \"trailhead\"", "name = \"trailhead\"\nqueue = \"TRAIL-1\"\nmax_sandboxes = 2");
    let mut r = rig_with(&format!(
        "{config}\n[gates.1]\nrequire = [\"checks\", \"ci\", \"agent_review\"]\nauto_merge = true\n[merge]\nmode = \"on\"\n"
    ));
    let queue = r.tracker.add_task("TRAIL", "Factory queue", "");
    r.factory = r.factory.with_codehost(Arc::new(r.codehost.clone()));
    let mut keys = Vec::new();
    for i in 0..2 {
        let key = r.tracker.add_subtask(&queue, "Do it", "");
        r.tracker.set_grade(&key, 1);
        r.harness.push(Role::Worker, Script::new(changes_to(&format!("docs/{i}.md"))));
        r.harness.push(Role::Reviewer, Script::new(approve()));
        r.factory.tick().await.unwrap();
        keys.push(key);
    }
    r.codehost.set_conflicts(true);
    for key in &keys {
        let pr = r.tracker.task(key).run.pr.expect("a PR");
        r.codehost.set_ci(&pr, trailhead_factory::CiState::Passed);
    }
    r.factory.reconcile().await.unwrap();
    for key in &keys {
        assert!(r.tracker.task(key).run.pending_changes().is_some_and(|c| !c.kind.is_review()));
    }
    // The run before the restart left each task's pooled sandbox held for it, as a parked task's is.
    let mut labels = r.compute.get("trailhead-w1").unwrap().labels;
    labels.insert(label::STATE.into(), label::BUSY.into());
    labels.insert(label::ROLE.into(), Role::Worker.as_str().into());
    labels.insert(label::TASK.into(), keys[0].0.clone());
    r.compute.set_labels(&trailhead_factory::SandboxId("trailhead-w1".into()), &labels).await.unwrap();
    labels.insert(label::TASK.into(), keys[1].0.clone());
    labels.remove(label::WARM);
    let spec = SandboxSpec { name: "trailhead-w2".into(), labels, egress: Default::default() };
    r.compute.create(&spec).await.unwrap();
    assert_eq!(r.compute.sandboxes().len(), 2, "each holds its own sandbox, filling the cap");

    r.codehost.set_conflicts(false);
    for key in &keys {
        let pr = r.tracker.task(key).run.pr.expect("a PR");
        r.codehost.set_mergeable(&pr, Some(false));
    }
    for i in 0..2 {
        r.harness.push(Role::Worker, Script::new(changes_to(&format!("docs/{i}.md"))));
        r.harness.push(Role::Reviewer, Script::new(approve()));
    }
    let ran = r.factory.tick().await.unwrap();
    assert_eq!(ran.len(), 2, "both reworks start");
    for (_, outcome) in &ran {
        assert!(!matches!(outcome, Outcome::Settled { .. }), "a rework ran: {outcome:?}");
    }
    assert_eq!(r.compute.sandboxes().len(), 2, "no third sandbox");
    let mut names: Vec<String> = r.compute.sandboxes().into_iter().map(|s| s.name).collect();
    names.sort();
    assert_eq!(names, ["trailhead-w1", "trailhead-w2"], "the reworks ran in the sandboxes they held");
}

#[tokio::test]
async fn a_task_holds_one_pooled_sandbox_so_a_stale_label_doesnt_fill_the_cap() {
    let config = CONFIG.replace("name = \"trailhead\"", "name = \"trailhead\"\nmax_sandboxes = 2");
    let r = rig_with(&config);
    let key = r.tracker.add_task("TRAIL", "Expensive", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(10, 2_000_000));
    r.factory.tick().await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::NeedsInput);
    // An earlier attempt's sandbox kept the label after the task moved on.
    let mut labels = r.compute.get("trailhead-w1").unwrap().labels;
    labels.remove(label::WARM);
    let spec = SandboxSpec { name: "trailhead-w2".into(), labels, egress: Default::default() };
    r.compute.create(&spec).await.unwrap();
    assert_eq!(r.compute.sandboxes().len(), 2);

    // The stale one is released, so the other task starts without a third sandbox past the cap.
    let next = r.tracker.add_task("TRAIL", "Next", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    assert!(ran.iter().any(|(t, _)| t.key == next), "the other task started");
    assert_eq!(r.compute.sandboxes().len(), 2, "no third sandbox");
    let kept = r.compute.get("trailhead-w1").unwrap();
    assert_eq!(kept.labels.get(label::TASK), Some(&key.0), "the parked task keeps the sandbox it ran on");
}

#[tokio::test]
async fn taking_a_parked_task_off_its_queue_frees_its_place() {
    let (r, queue) = queued_rig();
    let key = r.tracker.add_subtask(&queue, "Ambiguous", "");
    r.tracker.set_grade(&key, 3);
    r.harness.push(Role::Worker, Script::new(RunOutcome::NeedsInput { question: "Which crate?".into() }));
    r.factory.tick().await.unwrap();
    assert_eq!(busy(&r.compute), ["trailhead-w1"]);
    assert_eq!(r.tracker.task(&key).status, Status::NeedsInput);

    let next = r.tracker.add_subtask(&queue, "Next", "");
    r.tracker.set_grade(&next, 3);
    assert!(r.factory.tick().await.unwrap().is_empty(), "the parked task holds the one place");

    r.tracker.detach(&key);
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    assert!(ran.iter().any(|(t, _)| t.key == next), "the next task started in the released place");
    assert_eq!(r.tracker.task(&key).status, Status::NeedsInput, "the detached task is left to its new owner");
}

#[tokio::test]
async fn a_failed_in_flight_listing_doesnt_release_a_parked_tasks_sandbox() {
    let (r, queue) = queued_rig();
    let key = r.tracker.add_subtask(&queue, "Ambiguous", "");
    r.tracker.set_grade(&key, 3);
    r.harness.push(Role::Worker, Script::new(RunOutcome::NeedsInput { question: "Which crate?".into() }));
    r.factory.tick().await.unwrap();
    assert_eq!(busy(&r.compute), ["trailhead-w1"]);

    // The task is off its queue, which a good listing would show, but the listing fails: the guard keeps the sandbox.
    r.tracker.detach(&key);
    r.tracker.fail_next("factory", TrackerOp::InFlight, 100);
    let next = r.tracker.add_subtask(&queue, "Next", "");
    r.tracker.set_grade(&next, 3);
    r.factory.tick().await.unwrap();
    assert_eq!(busy(&r.compute), ["trailhead-w1"], "a tracker hiccup frees no parked sandbox");
    assert_eq!(r.compute.get("trailhead-w1").unwrap().labels.get(label::TASK), Some(&key.0));
    assert_eq!(r.tracker.task(&next).status, Status::Open, "the one place is still taken");

    // Once the listing works again the detached task's sandbox is released.
    r.tracker.fail_next("factory", TrackerOp::InFlight, 0);
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    assert!(ran.iter().any(|(t, _)| t.key == next), "the next task started in the released place");
}

#[tokio::test(start_paused = true)]
async fn a_parked_task_still_claimed_is_left_alone_without_waking_its_daemon() {
    let r = releasing_after(1);
    let (parked, waiting) = parked_with_one_waiting(&r).await;

    tokio::time::advance(AFTER).await;
    assert!(r.factory.tick().await.unwrap().is_empty(), "the asker's claim blocks the release");
    assert!(r.daemon.unpublished_asked().is_empty(), "the daemon wasn't asked while the claim stood");
    assert_eq!(r.tracker.task(&waiting).status, Status::Open);
    assert_eq!(r.tracker.task(&parked).run.released, None);
}

/// A worker sandbox another repo's factory holds, taking a place under the host's cap.
async fn held_by_beta(compute: &trailhead_testkit::FakeCompute, name: &str) {
    let labels = trailhead_factory::Labels::from([
        (label::FACTORY.into(), "trailhead".into()),
        (label::REPO.into(), "lanesawyer/beta".into()),
        (label::TASK.into(), "TRAIL-99".into()),
        (label::ROLE.into(), "worker".into()),
    ]);
    compute.create(&SandboxSpec { name: name.into(), labels, egress: Default::default() }).await.unwrap();
}

/// Runs the loop for a few polls, then stops it.
async fn poll_a_while(factory: &Arc<trailhead_control::Factory>) {
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(factory.clone().run(
        Arc::new(tokio::sync::Notify::new()),
        std::time::Duration::from_millis(5),
        async {
            let _ = stopped.await;
        },
    ));
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    stop.send(()).unwrap();
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_host_cap_filled_by_another_repo_isnt_blamed_on_parked_tasks_but_the_repos_own_cap_is() {
    let host = Host::new(&trailhead_factory::HostConfig { max_sandboxes: Some(3), ..Default::default() });
    let r = rig_on(&CONFIG.replace("name = \"trailhead\"", "name = \"trailhead\"\nmax_sandboxes = 2"), host);
    let park = || r.harness.push(Role::Worker, Script::new(RunOutcome::NeedsInput { question: "Which crate?".into() }));
    r.tracker.add_task("TRAIL", "Parks", "");
    park();
    r.factory.tick().await.unwrap();
    assert_eq!(busy(&r.compute).len(), 1, "one parked sandbox");

    // Beta's two workers fill the host's cap; this repo is under its own, with a ready task waiting.
    held_by_beta(&r.compute, "beta-1").await;
    held_by_beta(&r.compute, "beta-2").await;
    let waiting = r.tracker.add_task("TRAIL", "Waits", "");
    let (logs, _capture) = Logs::capture();

    let factory = Arc::new(r.factory);
    poll_a_while(&factory).await;
    assert_eq!(r.tracker.task(&waiting).status, Status::Open, "no place under the host's cap");
    assert_eq!(r.compute.sandboxes().len(), 3, "nothing started");
    assert!(!logs.text().contains("held by parked tasks"), "beta's runs will free the place: {}", logs.text());

    // Beta gives a place back and this repo's own cap fills with parked sandboxes.
    let beta = r.compute.sandboxes().into_iter().find(|s| s.name == "beta-1").unwrap();
    r.compute.destroy(&beta.id).await.unwrap();
    park();
    assert_eq!(factory.tick().await.unwrap().len(), 1);
    assert_eq!(r.tracker.task(&waiting).status, Status::NeedsInput, "the waiting task took the place and parked");
    let next = r.tracker.add_task("TRAIL", "Still waits", "");
    poll_a_while(&factory).await;
    assert_eq!(r.tracker.task(&next).status, Status::Open, "both of the repo's places are parked");
    assert!(logs.text().contains("held by parked tasks"), "{}", logs.text());
}

#[tokio::test(start_paused = true)]
async fn a_parked_task_with_a_pushed_branch_and_no_pr_is_compared_with_its_branch() {
    let r = releasing_after(1);
    let (parked, waiting) = parked_with_one_waiting(&r).await;
    r.tracker.lapse_lease(&parked);
    // The branch was pushed; only the PR is missing.
    r.daemon.set_remote_branches(&["main", "factory/trail-1"]);
    assert!(r.factory.tick().await.unwrap().is_empty(), "not parked long enough yet");

    tokio::time::advance(AFTER).await;
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    assert!(ran.iter().any(|(t, _)| t.key == waiting), "the waiting task took the place");
    assert!(r.tracker.task(&parked).run.released.is_some_and(|at| at > 0));
    assert_eq!(r.daemon.unpublished_asked(), ["factory/trail-1"], "asked against its own branch only");

    // Answered, it resumes from the branch it pushed, if the remote still has it.
    r.tracker.answer(&parked, "Go on.");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    assert!(ran.iter().any(|(t, _)| t.key == parked), "the answered task started again");
    let resumed = r
        .daemon
        .prepared()
        .into_iter()
        .any(|(_, p)| p.branch.as_deref() == Some("factory/trail-1") && p.branch_if_present);
    assert!(resumed, "it resumes on the branch it pushed");
}
