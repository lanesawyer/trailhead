//! The MVP lifecycle on fakes (`docs/factory/mvp.md`): spec to reviewed PR,
//! budget breaches, failed checks, and reconcile after a crash.

use std::sync::Arc;

use trailhead_control::{Factory, Outcome, Reconciled};
use trailhead_factory::{
    CodeHost, Commit, Compute, FactoryConfig, FileChange, FollowUp, MergeOutcome, RaiseGrade, Role, RunOutcome,
    SandboxSpec, Status, TaskKey, Tracker, Usd, VerdictKind, conventional_title, label, run_name,
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
    let heads = codehost.clone();
    let daemon = Arc::new(
        LocalDaemon::new(harness.clone(), Arc::new(codehost.clone()), tracker.clone())
            .with_heads(move |repo, branch| heads.head(repo, branch)),
    );
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
            message: conventional_title(Some("feat(core): greet"), &TaskKey("TRAIL-1".into()), "").0,
            changes: vec![FileChange { path: "src/greet.rs".into(), contents: Some(b"pub fn hi() {}".to_vec()) }],
            merges: vec![],
        }],
        summary: "Adds a greeting.".into(),
    }
}

fn approve() -> RunOutcome {
    RunOutcome::Verdict {
        kind: VerdictKind::Approve,
        body: "Looks right; tests cover it.".into(),
        follow_ups: vec![],
        raise_grade: None,
    }
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
    assert_eq!(
        opened.spec.title, "feat(core): greet [TRAIL-1]",
        "the PR is titled with the commit subject the repo squashes to"
    );
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
    let names: Vec<_> = review.checks.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["fmt", "lint", "test"], "the reviewer is told which checks the worker ran");
    assert!(review.checks.iter().all(|c| c.passed));

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
async fn the_next_worker_sees_the_answer_to_its_question() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Ambiguous", "");
    r.harness.push(Role::Worker, Script::new(RunOutcome::NeedsInput { question: "Which crate?".into() }));
    r.factory.tick().await.unwrap();
    r.tracker.answer(&key, "The core crate.");
    r.factory.reconcile().await.unwrap();
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();

    let ctxs = r.harness.contexts();
    assert!(ctxs[0].task.context.is_empty());
    let answers = &ctxs[1].task.context.answers;
    assert_eq!(answers.len(), 1);
    assert_eq!((answers[0].question.as_str(), answers[0].answer.as_str()), ("Which crate?", "The core crate."));
    assert!(ctxs[1].task.context.notes.is_empty(), "the factory's own comments aren't passed on");
}

#[tokio::test]
async fn a_decision_on_a_queued_task_reaches_the_first_run() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Lock", "");
    r.tracker.decide(&key, "Use a per-machine flock.");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();
    let notes = &r.harness.contexts()[0].task.context.notes;
    assert_eq!(notes.len(), 1);
    assert_eq!((notes[0].author.as_str(), notes[0].body.as_str()), ("lane", "Use a per-machine flock."));
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
    r.compute.fail(&trailhead_factory::review_name(&unreviewed, 1, 2), "set_egress");

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
async fn a_review_left_from_before_a_restart_doesnt_hold_up_workers() {
    let r = rig();
    let unreviewed = r.tracker.add_task("TRAIL", "Unreviewed", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(RunOutcome::Failed { reason: "control plane died".into() }));
    r.factory.run_task(&r.tracker.task(&unreviewed)).await.unwrap();

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

fn request_changes(body: &str) -> RunOutcome {
    RunOutcome::Verdict { kind: VerdictKind::RequestChanges, body: body.into(), follow_ups: vec![], raise_grade: None }
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
    assert_eq!(prepared.iter().filter(|(_, p)| p.branch.is_some()).count(), 1, "only the rework checks out the PR");
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
    let r = rig_with(&CONFIG.replace("project = \"TRAIL\"", "project = \"TRAIL\"\nmax_review_rounds = 2"));
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

#[tokio::test]
async fn failures_before_the_agent_starts_dont_use_up_attempts() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Broken host", "");
    for n in 1..=3 {
        r.compute.fail(&format!("trailhead-w{n}"), "set_labels");
    }
    for _ in 0..3 {
        let task = r.tracker.task(&key);
        assert!(r.factory.run_task(&task).await.is_err());
        let task = r.tracker.task(&key);
        assert_eq!(task.status, Status::Open, "not parked");
        assert_eq!(task.holder, None);
        assert_eq!(task.run.attempt.unwrap_or(0), 0);
    }
    assert!(r.tracker.comments(&key).iter().all(|c| c.kind != "question"));

    assert!(r.factory.tick().await.unwrap().is_empty(), "not retried before its backoff is over");
    assert!(r.harness.contexts().is_empty());

    // Fixed: the next attempt is attempt 1, under a name none of the failed runs used.
    for n in 1..=3 {
        r.compute.heal(&format!("trailhead-w{n}"));
    }
    r.factory.reconcile().await.unwrap();
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 10));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.run_task(&r.tracker.task(&key)).await.unwrap();
    assert!(matches!(ran, Outcome::Reviewed { .. }), "{ran:?}");
    assert_eq!(r.tracker.task(&key).run.attempt, Some(1));
    assert!(r.tracker.agents().contains(&"factory/trail-1-a1-3".to_string()), "{:?}", r.tracker.agents());
}

#[tokio::test(start_paused = true)]
async fn a_failure_before_the_agent_starts_is_retried_after_a_backoff() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Flaky host", "");
    for n in 1..=3 {
        r.compute.fail(&format!("trailhead-w{n}"), "set_labels");
    }
    assert!(r.factory.run_task(&r.tracker.task(&key)).await.is_err());
    for n in 1..=3 {
        r.compute.heal(&format!("trailhead-w{n}"));
    }
    r.factory.reconcile().await.unwrap();
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    assert!(r.factory.tick().await.unwrap().is_empty(), "still backing off");
    tokio::time::advance(std::time::Duration::from_secs(61)).await;
    let ran = r.factory.tick().await.unwrap();
    assert_eq!(ran.len(), 1, "retried once the backoff is over");
    assert_eq!(r.tracker.task(&key).run.attempt, Some(1));
}

#[tokio::test(start_paused = true)]
async fn a_deliberate_stop_gives_the_attempt_back() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Hangs", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 100).hang());
    let factory = Arc::new(r.factory);
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(factory.clone().run(
        Arc::new(tokio::sync::Notify::new()),
        std::time::Duration::from_secs(60),
        async {
            let _ = stopped.await;
        },
    ));
    let tracker = r.tracker.clone();
    until("the task to start", || tracker.task(&key).status == Status::InProgress && !r.daemon.requests().is_empty())
        .await;
    stop.send(()).unwrap();
    handle.await.unwrap().unwrap();
    assert_eq!(r.tracker.task(&key).run.attempt, Some(0), "the stopped run doesn't count");

    // The next boot requeues it, and its retry runs as attempt 1 under a fresh name.
    factory.reconcile().await.unwrap();
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = factory.run_task(&r.tracker.task(&key)).await.unwrap();
    assert!(matches!(ran, Outcome::Reviewed { .. }), "{ran:?}");
    assert_eq!(r.tracker.task(&key).run.attempt, Some(1));
    assert!(r.tracker.agents().contains(&"factory/trail-1-a1-1".to_string()), "{:?}", r.tracker.agents());
}

fn api_timeout() -> RunOutcome {
    RunOutcome::Failed { reason: "claude: API Error: No response from API (waited 3m)".into() }
}

#[tokio::test]
async fn the_agents_api_timeouts_dont_use_up_attempts() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Slow API", "");
    for _ in 0..2 {
        r.harness.push(Role::Worker, Script::new(api_timeout()));
        let ran = r.factory.run_task(&r.tracker.task(&key)).await.unwrap();
        assert!(matches!(ran, Outcome::Failed { .. }), "{ran:?}");
        let task = r.tracker.task(&key);
        assert_eq!(task.status, Status::Open, "not parked");
        assert_eq!(task.holder, None);
        assert_eq!(task.run.attempt, Some(0));
    }
    assert_eq!(r.tracker.task(&key).run.infra_failures, Some(2));
    assert!(r.tracker.comments(&key).iter().all(|c| c.kind != "question"));
    assert!(r.factory.tick().await.unwrap().is_empty(), "backing off");

    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.run_task(&r.tracker.task(&key)).await.unwrap();
    assert!(matches!(ran, Outcome::Reviewed { .. }), "{ran:?}");
    let task = r.tracker.task(&key);
    assert_eq!(task.run.attempt, Some(1));
    assert_eq!(task.run.infra_failures, Some(0), "an agent that ends the run resets the count");
    let agents = r.tracker.agents();
    for name in ["factory/trail-1-a1-1", "factory/trail-1-a1-2"] {
        assert!(agents.contains(&name.to_string()), "{name} in {agents:?}");
    }
}

#[tokio::test]
async fn infrastructure_failures_past_the_cap_park_the_task_as_the_infrastructures_fault() {
    let r = rig_with(&CONFIG.replace("project = \"TRAIL\"", "project = \"TRAIL\"\nmax_infra_failures = 3"));
    let key = r.tracker.add_task("TRAIL", "Lost daemon", "");
    r.daemon.fail_runs(3, "not found: run trail-1 on this daemon");
    for n in 1..=3 {
        let err = r.factory.run_task(&r.tracker.task(&key)).await.unwrap_err();
        assert!(err.to_string().contains("not found: run"), "{err}");
        let task = r.tracker.task(&key);
        if n < 3 {
            assert_eq!(task.status, Status::Open, "failure {n} is retried");
        }
    }
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput);
    assert_eq!(task.run.attempt, Some(0), "no attempt was used");
    let questions: Vec<_> = r.tracker.comments(&key).into_iter().filter(|c| c.kind == "question").collect();
    assert_eq!(questions.len(), 1);
    let body = &questions[0].body;
    assert!(body.contains("it's not the task") && body.contains("not found: run trail-1"), "{body}");
    assert!(!body.contains("failed attempts"), "{body}");
    assert!(r.factory.tick().await.unwrap().is_empty(), "a parked task isn't retried");
}

#[tokio::test(start_paused = true)]
async fn infrastructure_retries_wait_longer_each_time_up_to_a_cap() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Down for a while", "");
    r.daemon.fail_runs(7, "connection lost");
    let secs = std::time::Duration::from_secs;
    // 1, 2, 4, 8, then 15 minutes at most.
    for (n, wait) in [60, 120, 240, 480, 900, 900, 900].into_iter().enumerate() {
        assert!(r.factory.tick().await.is_err(), "failure {}", n + 1);
        assert_eq!(r.tracker.task(&key).run.infra_failures, Some(n as u32 + 1));
        // Reconcile frees the failed run's sandbox, as the run loop does; the task itself still waits.
        r.factory.reconcile().await.unwrap();
        assert!(r.factory.tick().await.unwrap().is_empty(), "backing off for {wait}s");
        tokio::time::advance(secs(wait - 1)).await;
        assert!(r.factory.tick().await.unwrap().is_empty(), "still a second short of {wait}s");
        tokio::time::advance(secs(2)).await;
    }
    assert_eq!(r.tracker.task(&key).status, Status::Open, "under the cap of 10");
}

#[tokio::test]
async fn failures_that_are_the_agents_or_the_tasks_still_use_up_attempts() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Gives up", "");
    // Not API trouble, though it mentions one: a 400 about max_tokens: 5000.
    for reason in ["I couldn't make the build pass", "API Error: 400 invalid request, max_tokens: 5000"] {
        r.harness.push(Role::Worker, Script::new(RunOutcome::Failed { reason: reason.into() }));
    }
    for _ in 0..2 {
        let ran = r.factory.run_task(&r.tracker.task(&key)).await.unwrap();
        assert!(matches!(ran, Outcome::Failed { .. }), "{ran:?}");
    }
    let task = r.tracker.task(&key);
    assert_eq!(task.run.attempt, Some(2));
    assert_eq!(task.run.infra_failures.unwrap_or(0), 0);

    r.daemon.fail_check("lint", "warning: unused variable");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 10));
    let ran = r.factory.run_task(&r.tracker.task(&key)).await.unwrap();
    assert!(matches!(&ran, Outcome::Parked { .. }), "{ran:?}");
    let body = r.tracker.comments(&key).into_iter().find(|c| c.kind == "question").unwrap().body;
    assert!(body.contains("Gave up after 3 failed attempts"), "{body}");
}

#[tokio::test]
async fn two_runs_starting_together_never_take_the_same_idle_sandbox() {
    let r = rig_with(&CONFIG.replace("project = \"TRAIL\"", "project = \"TRAIL\"\nmax_sandboxes = 4"));
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
async fn a_sandbox_that_wont_release_after_the_agent_ends_the_run_doesnt_give_the_attempt_back() {
    for (outcome, status) in [
        (changes(), Status::InReview),
        (RunOutcome::Failed { reason: "I couldn't make the build pass".into() }, Status::Open),
    ] {
        let r = rig();
        let key = r.tracker.add_task("TRAIL", "Ends the run", "");
        r.harness.push(Role::Worker, Script::new(outcome));
        // Restoring and destroying are all releasing does after the run; preparing a new sandbox does neither.
        r.compute.fail("trailhead-w1", "restore");
        r.compute.fail("trailhead-w1", "destroy");
        let err = r.factory.run_task(&r.tracker.task(&key)).await.unwrap_err();
        assert!(err.to_string().contains("Storage not ready"), "{err}");
        let task = r.tracker.task(&key);
        assert_eq!(task.status, status);
        assert_eq!(task.run.attempt, Some(1), "the agent ended the run, so the attempt counts");
        assert_eq!(task.run.infra_failures.unwrap_or(0), 0);
        assert_eq!(task.run.retries.unwrap_or(0), 0);
    }
}

#[tokio::test]
async fn a_task_parked_for_infrastructure_failures_starts_without_a_wait_when_reopened() {
    let r = rig_with(
        &CONFIG.replace("project = \"TRAIL\"", "project = \"TRAIL\"\nmax_infra_failures = 2\nmax_sandboxes = 2"),
    );
    let key = r.tracker.add_task("TRAIL", "Lost daemon", "");
    r.daemon.fail_runs(2, "connection lost");
    for _ in 0..2 {
        r.factory.run_task(&r.tracker.task(&key)).await.unwrap_err();
        r.factory.reconcile().await.unwrap();
    }
    assert_eq!(r.tracker.task(&key).status, Status::NeedsInput);

    r.tracker.set_status(&key, Status::Open);
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    assert_eq!(ran.len(), 1, "no backoff left over from before the park");
}

#[tokio::test(start_paused = true)]
async fn reviews_that_fail_on_the_infrastructure_back_off_and_park_at_the_cap() {
    let r = rig_with(&CONFIG.replace("project = \"TRAIL\"", "project = \"TRAIL\"\nmax_infra_failures = 3"));
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

#[tokio::test]
async fn a_harness_that_publishes_an_unconventional_subject_still_gets_a_conventional_pr_title() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Add a greeting", "");
    let outcome = RunOutcome::Changes {
        commits: vec![Commit {
            message: "added a greeting".into(),
            changes: vec![FileChange { path: "src/greet.rs".into(), contents: Some(b"pub fn hi() {}".to_vec()) }],
            merges: vec![],
        }],
        summary: "Adds a greeting.".into(),
    };
    r.harness.push(Role::Worker, Script::new(outcome));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.run_task(&r.tracker.task(&key)).await.unwrap();
    let opened = r.codehost.prs().pop().expect("a PR");
    assert_eq!(opened.spec.title, "chore: Add a greeting [TRAIL-1]");
}

/// A rig whose factory works only the subtasks of TRAIL-1 and reads PRs through the code host, which grading needs.
fn queued_rig() -> (Rig, TaskKey) {
    let mut r = rig_with(&CONFIG.replace("project = \"TRAIL\"", "project = \"TRAIL\"\nqueue = \"TRAIL-1\""));
    let queue = r.tracker.add_task("TRAIL", "Factory queue", "");
    let codehost = Arc::new(r.codehost.clone());
    r.factory = r.factory.with_codehost(codehost);
    (r, queue)
}

fn changes_to(path: &str) -> RunOutcome {
    RunOutcome::Changes {
        commits: vec![Commit {
            message: "docs(docs): note it [TRAIL-2]".into(),
            changes: vec![FileChange { path: path.into(), contents: Some(b"x".to_vec()) }],
            merges: vec![],
        }],
        summary: "Done.".into(),
    }
}

fn notes(r: &Rig, key: &TaskKey) -> Vec<String> {
    r.tracker.comments(key).into_iter().filter(|c| c.kind == "note").map(|c| c.body).collect()
}

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
