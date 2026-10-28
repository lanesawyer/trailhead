//! The MVP lifecycle on fakes (`docs/factory/mvp.md`): spec to reviewed PR,
//! budget breaches, failed checks, and reconcile after a crash.

use std::sync::Arc;

use trailhead_control::{Factory, Outcome, Reconciled};
use trailhead_factory::{
    CodeHost, Commit, Compute, FactoryConfig, FileChange, MergeOutcome, Role, RunOutcome, SandboxSpec, Status, TaskKey,
    Tracker, Usd, VerdictKind, label, run_name,
};
use trailhead_testkit::{
    FAKE_BASE, FakeCodeHost, FakeCompute, FakeProvider, FakeTracker, LocalDaemon, Script, ScriptedHarness,
};

const CONFIG: &str = r#"
[repo]
owner = "lanesawyer"
name = "trailhead"
project = "TRAIL"

[checks]
fmt = "cargo fmt --check"
lint = "cargo clippy -- -D warnings"
test = "cargo test"

[profiles.worker]
role = "worker"
adapter = "claude-code"
provider = "anthropic"
model = "sonnet"
budget = { usd = 5, minutes = 60, steps = 40 }

[profiles.reviewer]
role = "reviewer"
adapter = "claude-code"
provider = "anthropic"
model = "opus"
budget = { usd = 2, minutes = 20, steps = 20 }
"#;

struct Rig {
    factory: Factory,
    tracker: FakeTracker,
    compute: FakeCompute,
    codehost: FakeCodeHost,
    harness: Arc<ScriptedHarness>,
    daemon: Arc<LocalDaemon>,
}

fn rig() -> Rig {
    rig_with(CONFIG)
}

/// One micro-dollar per token, so 1,000,000 tokens is $1.
fn rig_with(config: &str) -> Rig {
    let tracker = FakeTracker::new("factory");
    let compute = FakeCompute::new();
    let codehost = FakeCodeHost::new();
    let harness = Arc::new(ScriptedHarness::new());
    let daemon = Arc::new(LocalDaemon::new(harness.clone(), Arc::new(codehost.clone()), tracker.clone()));
    let factory = Factory::new(
        FactoryConfig::parse(config).unwrap(),
        Arc::new(compute.clone()),
        Arc::new(tracker.clone()),
        Arc::new(FakeProvider::new(1)),
        daemon.clone(),
    );
    Rig { factory, tracker, compute, codehost, harness, daemon }
}

fn changes() -> RunOutcome {
    RunOutcome::Changes {
        commits: vec![Commit {
            message: "feat: greet".into(),
            changes: vec![FileChange { path: "src/greet.rs".into(), contents: Some(b"pub fn hi() {}".to_vec()) }],
        }],
        summary: "Adds a greeting.".into(),
    }
}

fn approve() -> RunOutcome {
    RunOutcome::Verdict { kind: VerdictKind::Approve, body: "Looks right; tests cover it.".into() }
}

#[tokio::test]
async fn spec_to_reviewed_pr_then_human_merge() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Add a greeting", "Acceptance: `hi()` exists.");
    r.harness.push(Role::Worker, Script::new(changes()).steps(3, 100_000));
    r.harness.push(Role::Reviewer, Script::new(approve()).steps(1, 50_000));

    let ran = r.factory.tick().await.unwrap();
    assert_eq!(ran.len(), 1);
    let Outcome::Reviewed { pr, verdict } = &ran[0].1 else { panic!("{:?}", ran[0].1) };
    assert_eq!(verdict.kind, VerdictKind::Approve);
    assert_eq!(verdict.reviewer, "factory/trail-1-a1-r1");

    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::InReview);
    assert_eq!(task.run.pr.as_ref(), Some(pr));
    assert_eq!(task.run.spend, Some(Usd::dollars(0.35)));
    assert_eq!(task.run.reviewed, Some(1));
    assert_eq!(task.run.sandbox.as_deref(), Some("trail-1-a1"));

    let opened = &r.codehost.prs()[0];
    assert_eq!(opened.spec.branch, "factory/trail-1");
    let start = r.codehost.start(&opened.pr.repo, "factory/trail-1");
    assert_eq!(start.as_deref(), Some(FAKE_BASE), "published on the commit the checkout started from");
    assert_eq!(
        opened.comments,
        ["🔍 **Review by `factory/trail-1-a1-r1` (`fake-opus`) · Verdict: Approve**\n\nLooks right; tests cover it."]
    );
    assert!(
        opened
            .spec
            .body
            .ends_with("🤖 Opened by the Trailhead factory · worker `factory/trail-1-a1` · `fake-sonnet` · TRAIL-1"),
        "{}",
        opened.spec.body
    );
    let review = r.harness.contexts().into_iter().find(|c| c.role == Role::Reviewer).unwrap();
    assert!(review.diff.unwrap().contains("src/greet.rs"), "reviewer sees the diff");
    assert_eq!(review.model.0, "fake-opus");

    assert_eq!(busy(&r.compute), Vec::<String>::new(), "no sandbox stays busy after the run");
    let pooled = r.compute.get("trailhead-w1").expect("the worker's sandbox stays in the pool");
    assert_eq!(pooled.labels.get(label::STATE).map(String::as_str), Some(label::IDLE));
    assert!(!pooled.labels.contains_key(label::ROLE), "an idle sandbox has no connector access");
    assert!(r.compute.egress("trailhead-w1").unwrap().domains.is_empty(), "and no egress");
    assert!(r.compute.get("trail-1-a1-r1").is_none(), "reviewer sandboxes aren't pooled");
    assert!(r.tracker.is_revoked("factory/trail-1-a1"));
    assert!(r.tracker.is_revoked("factory/trail-1-a1-r1"));
    assert!(r.tracker.comments(&key).iter().any(|c| c.kind == "review" && c.body.starts_with("approve")));

    // The human merges and finishes.
    assert_eq!(r.codehost.merge(pr).await.unwrap(), MergeOutcome::Merged);
    r.tracker.finish(&key, "merged").await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::Done);
    assert!(r.factory.tick().await.unwrap().is_empty());
}

#[tokio::test]
async fn budget_breach_parks_the_task_and_suspends_its_sandbox() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Expensive", "");
    // $2 a step against a $5 budget: the third step breaches, the rest never run.
    r.harness.push(Role::Worker, Script::new(changes()).steps(10, 2_000_000));

    let ran = r.factory.tick().await.unwrap();
    let Outcome::Parked { reason } = &ran[0].1 else { panic!("{:?}", ran[0].1) };
    assert!(reason.contains("budget"), "{reason}");

    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput);
    assert_eq!(task.holder.as_deref(), Some("factory/trail-1-a1"));
    assert_eq!(task.run.spend, Some(Usd::dollars(6.0)));
    assert!(r.tracker.comments(&key).iter().any(|c| c.kind == "question"));

    let sandbox = r.compute.get("trailhead-w1").expect("sandbox kept for resuming");
    assert!(!sandbox.labels.contains_key(label::ROLE), "connector access cut");
    assert!(r.compute.egress("trailhead-w1").unwrap().domains.is_empty());
    assert_eq!(r.compute.checkpoints("trailhead-w1").last().unwrap(), &format!("parked: {reason}"));
    assert!(r.codehost.prs().is_empty());
}

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
async fn too_many_steps_is_a_breach() {
    let r = rig();
    r.tracker.add_task("TRAIL", "Chatty", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(50, 1));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Parked { reason } if reason.contains("steps")));
}

#[tokio::test]
async fn failed_check_requeues_and_next_attempt_gets_a_fresh_sandbox() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Lint me", "");
    r.daemon.fail_check("lint", "warning: unused variable");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 10));

    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Failed { reason } if reason.contains("lint")));
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::Open);
    assert_eq!(task.holder, None);
    assert_eq!(busy(&r.compute), Vec::<String>::new());
    assert!(r.codehost.prs().is_empty());
    let notes: Vec<_> = r.tracker.comments(&key).into_iter().filter(|c| c.kind == "note").collect();
    assert_eq!(notes.len(), 1, "the failure is noted on the task");
    assert!(
        notes[0].body.contains("`lint` failed") && notes[0].body.contains("warning: unused variable"),
        "{}",
        notes[0].body
    );
    assert_eq!(notes[0].author, "factory/trail-1-a1");

    // Fixed; the next tick is attempt 2 in a new sandbox under a new sub-agent.
    r.daemon.pass_check("lint");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 10));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Reviewed { .. }), "{:?}", ran[0].1);
    assert_eq!(r.tracker.task(&key).run.attempt, Some(2));
    assert!(r.tracker.agents().contains(&"factory/trail-1-a2".to_string()));
}

#[tokio::test]
async fn check_that_passes_on_rerun_still_opens_the_pr_with_a_note() {
    let r = rig();
    r.tracker.add_task("TRAIL", "Flaky", "");
    r.daemon.fail_check_times("lint", "flaked", 1);
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 10));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Reviewed { .. }), "{:?}", ran[0].1);
    let prs = r.codehost.prs();
    assert_eq!(prs.len(), 1);
    assert!(prs[0].spec.body.contains("Check `lint` failed, then passed when rerun."), "{}", prs[0].spec.body);
}

#[tokio::test]
async fn check_that_fails_twice_fails_the_attempt() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Broken", "");
    r.daemon.fail_check_times("lint", "still broken", 2);
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 10));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Failed { reason } if reason.contains("lint")), "{:?}", ran[0].1);
    assert_eq!(r.tracker.task(&key).status, Status::Open);
    assert!(r.codehost.prs().is_empty());
}

#[tokio::test]
async fn agent_question_parks_the_task() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Ambiguous", "");
    r.harness.push(Role::Worker, Script::new(RunOutcome::NeedsInput { question: "Which crate?".into() }));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Parked { reason } if reason == "Which crate?"));
    assert_eq!(r.tracker.task(&key).status, Status::NeedsInput);
    assert_eq!(busy(&r.compute), ["trailhead-w1"], "kept for resuming");
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

#[tokio::test]
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
    let done = factory.reconcile().await.unwrap();
    assert_eq!(
        done,
        Reconciled {
            destroyed: vec!["trail-1-a1".into(), "trail-99-a1".into()],
            released: vec![],
            requeued: vec![crashed.0.clone()],
            reviewed: vec![unreviewed.0.clone()],
            abandoned: vec![],
        }
    );
    let names: Vec<String> = r.compute.sandboxes().into_iter().map(|s| s.name).collect();
    assert_eq!(names, ["trailhead-w1"], "the pooled sandbox stays, holding the parked task");
    assert_eq!(r.tracker.task(&crashed).status, Status::Open);
    assert!(r.tracker.is_revoked("factory/trail-1-a1"));
    assert_eq!(r.tracker.task(&unreviewed).run.reviewed, Some(1));
    assert_eq!(r.tracker.task(&unreviewed).run.review_rounds, Some(2));
    assert!(r.tracker.is_revoked("factory/trail-2-a1-r1"));
    assert!(r.tracker.is_revoked("factory/trail-2-a1-r2"));
    assert_eq!(r.tracker.task(&parked).status, Status::NeedsInput);

    // A second pass finds nothing to do.
    assert_eq!(factory.reconcile().await.unwrap(), Reconciled::default());
}

/// Sandboxes that aren't idle in the pool: busy, parked, or not pooled at all.
fn busy(compute: &FakeCompute) -> Vec<String> {
    compute
        .sandboxes()
        .into_iter()
        .filter(|s| s.labels.get(label::STATE).map(String::as_str) != Some(label::IDLE))
        .map(|s| s.name)
        .collect()
}

fn update(attempt: u32, sandbox: &str) -> trailhead_factory::RunUpdate {
    trailhead_factory::RunUpdate { attempt: Some(attempt), sandbox: Some(sandbox.into()), ..Default::default() }
}

async fn live(compute: &FakeCompute, name: &str, task: &TaskKey, role: &str) {
    let labels = trailhead_factory::Labels::from([
        (label::FACTORY.into(), "trailhead".into()),
        (label::REPO.into(), "lanesawyer/trailhead".into()),
        (label::TASK.into(), task.0.clone()),
        (label::ROLE.into(), role.into()),
    ]);
    compute.create(&SandboxSpec { name: name.into(), labels, egress: Default::default() }).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_daemon_that_ignores_cancel_is_cut_off_and_parked() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Wedged", "");
    r.daemon.ignore_cancel();
    r.harness.push(Role::Worker, Script::new(changes()).steps(3, 2_000_000).hang());

    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Parked { reason } if reason.contains("budget")), "{:?}", ran[0].1);
    assert_eq!(r.tracker.task(&key).status, Status::NeedsInput);
    let sandbox = r.compute.get("trailhead-w1").unwrap();
    assert!(!sandbox.labels.contains_key(label::ROLE));
    assert!(r.compute.egress("trailhead-w1").unwrap().domains.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_silent_hung_agent_hits_the_wall_clock() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Stuck", "");
    r.harness.push(Role::Worker, Script::new(changes()).hang());
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Parked { reason } if reason.contains("60 minutes")), "{:?}", ran[0].1);
    assert_eq!(r.tracker.task(&key).status, Status::NeedsInput);
}

fn two_at_a_time() -> Rig {
    rig_with(&CONFIG.replace("project = \"TRAIL\"", "project = \"TRAIL\"\nmax_sandboxes = 2"))
}

async fn until(what: &str, mut cond: impl FnMut() -> bool) {
    for _ in 0..500 {
        if cond() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for {what}");
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
async fn a_refused_publish_parks_the_task_instead_of_redoing_the_work() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Fix CI", "");
    r.codehost.refuse_publish("github 404 Not Found on POST /repos/lanesawyer/trailhead/git/refs: Not Found");
    r.harness.push(Role::Worker, Script::new(changes()));

    let ran = r.factory.tick().await.unwrap();
    assert!(
        matches!(&ran[0].1, Outcome::Parked { reason } if reason.contains("publishing to GitHub failed")),
        "{:?}",
        ran[0].1
    );
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput, "parked for Lane, not requeued");
    assert_eq!(task.run.attempt, Some(1));
    let question = r.tracker.comments(&key).into_iter().find(|c| c.kind == "question").unwrap();
    assert!(question.body.contains("POST /repos/lanesawyer/trailhead/git/refs"), "{}", question.body);
    assert_eq!(busy(&r.compute), ["trailhead-w1"], "the sandbox keeps the finished checkout");
}

#[tokio::test]
async fn a_task_that_keeps_failing_is_parked_after_max_attempts_and_retries_see_the_last_failure() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Lint me", "");
    r.daemon.fail_check("lint", "warning: unused variable");
    for attempt in 1..=3 {
        r.harness.push(Role::Worker, Script::new(changes()).steps(1, 10));
        let ran = r.factory.tick().await.unwrap();
        assert_eq!(ran.len(), 1, "attempt {attempt} runs");
        if attempt < 3 {
            assert!(matches!(&ran[0].1, Outcome::Failed { .. }), "{:?}", ran[0].1);
            assert_eq!(r.tracker.task(&key).status, Status::Open);
        } else {
            assert!(matches!(&ran[0].1, Outcome::Parked { reason } if reason.contains("lint")), "{:?}", ran[0].1);
        }
    }
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput);
    assert_eq!(task.run.attempt, Some(3));
    let questions: Vec<_> = r.tracker.comments(&key).into_iter().filter(|c| c.kind == "question").collect();
    assert_eq!(questions.len(), 1);
    assert!(questions[0].body.contains("3 failed attempts") && questions[0].body.contains("unused variable"));
    assert!(r.factory.tick().await.unwrap().is_empty(), "a parked task isn't retried");

    let ctxs = r.harness.contexts();
    assert_eq!(ctxs.len(), 3);
    assert_eq!(ctxs[0].task.previous_failure, None);
    for ctx in &ctxs[1..] {
        let note = ctx.task.previous_failure.as_deref().expect("the last failure is passed on");
        assert!(note.contains("`lint` failed") && note.contains("warning: unused variable"), "{note}");
    }
}
