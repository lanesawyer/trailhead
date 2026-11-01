use std::sync::Arc;

use trailhead_control::Outcome;
use trailhead_factory::{Compute, Role, RunOutcome, SandboxSpec, Status, TaskKey, Usd, label, run_name};
use trailhead_testkit::Script;

use crate::common::*;

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

fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

/// The test config, pausing at 80% of the five-hour window.
fn pausing_rig() -> Rig {
    rig_with(&format!("{CONFIG}\n[usage]\npause_at = {{ five_hour = 80 }}\n"))
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

#[tokio::test]
async fn runs_start_again_once_the_window_resets() {
    let r = pausing_rig();
    report_usage(&r, 95, 1).await;
    r.tracker.add_task("TRAIL", "Second", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    assert!(r.factory.tick().await.unwrap().is_empty());
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    assert_eq!(r.factory.tick().await.unwrap().len(), 1);
}

#[tokio::test]
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
    let r = pausing_rig();
    let key = r.tracker.add_task("TRAIL", "Unreviewed", "");
    let script = Script::new(changes()).quota("five_hour", 90, unix_now() + 2);
    r.harness.push(Role::Worker, script);
    r.harness.push(Role::Reviewer, Script::new(RunOutcome::Failed { reason: "control plane died".into() }));
    let ran = r.factory.run_task(&r.tracker.task(&key)).await.unwrap();
    assert!(matches!(ran, Outcome::AwaitingReview { .. }), "{ran:?}");
    r.harness.push(Role::Reviewer, Script::new(approve()));
    tokio::time::advance(std::time::Duration::from_secs(61)).await; // out of the no-verdict backoff

    let factory = Arc::new(r.factory);
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let handle =
        tokio::spawn(factory.run(Arc::new(tokio::sync::Notify::new()), std::time::Duration::from_millis(5), async {
            let _ = stopped.await;
        }));
    let tracker = r.tracker.clone();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(tracker.task(&key).run.reviewed, None, "no reviewer starts while the window is paused");
    // The window resets on the wall clock, which the paused one doesn't move.
    std::thread::sleep(std::time::Duration::from_millis(2100));
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
async fn no_runs_start_while_the_server_is_down() {
    let mut r = rig();
    let up = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = up.clone();
    r.factory = r.factory.with_server_check(Box::new(move || {
        let up = seen.load(std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move { if up { Ok(()) } else { Err("connection refused".to_string()) } })
    }));
    r.tracker.add_task("TRAIL", "First", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    assert!(r.factory.tick().await.unwrap().is_empty(), "nothing starts while the server is down");
    assert!(r.compute.list(&Default::default()).await.unwrap().is_empty(), "no sandbox was acquired");
    up.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(r.factory.tick().await.unwrap().len(), 1, "runs start once it is back");
}
