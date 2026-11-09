//! What the daemon reports when it fails after the agent's outcome is known (TRAIL-214).

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Notify, mpsc};
use trailhead_daemon::{Connect, HEARTBEAT_EVERY, RENEW_EVERY, Runner};
use trailhead_factory::{
    Check, Commit, Credential, Endpoint, FileChange, ModelId, Repo, Role, RunOutcome, RunRequest, RunResult, TaskKey,
    TaskSpec, Tracker, VerdictKind,
};
use trailhead_testkit::{FakeCodeHost, FakeTracker, FakeWorkspace, LocalDaemon, Script, ScriptedHarness};

#[tokio::test]
async fn an_error_finishing_a_run_is_reported_as_its_outcome() {
    let tracker = FakeTracker::new("factory");
    // Not in review, so the tracker refuses the verdict the reviewer ends with.
    let key = tracker.add_task("TRAIL", "Add a greeting", "");
    let harness = Arc::new(ScriptedHarness::new());
    harness.push(
        Role::Reviewer,
        Script::new(RunOutcome::Verdict {
            kind: VerdictKind::Approve,
            body: "Looks right.".into(),
            follow_ups: vec![],
            raise_grade: None,
        }),
    );
    let connect: Connect = {
        let tracker = tracker.clone();
        Arc::new(move |_| {
            let tracker = tracker.clone();
            Box::pin(async move { Ok(Arc::new(tracker) as Arc<dyn Tracker>) })
        })
    };
    let runner = Runner {
        harness,
        codehost: Arc::new(FakeCodeHost::new()),
        workspace: Arc::new(FakeWorkspace::default()),
        connect,
        renew_every: RENEW_EVERY,
        heartbeat_every: HEARTBEAT_EVERY,
    };
    let (tx, _rx) = mpsc::unbounded_channel();

    let report = runner.run(request(key), tx, Arc::new(Notify::new())).await.unwrap();

    let RunResult::Failed(reason) = report.result else { panic!("{:?}", report.result) };
    assert!(reason.contains("finishing the run failed"), "{reason}");
}

fn request(key: TaskKey) -> RunRequest {
    RunRequest {
        grade: None,
        run: "r1".into(),
        role: Role::Reviewer,
        task: TaskSpec {
            key,
            title: "Add a greeting".into(),
            description: String::new(),
            previous_failure: None,
            context: Default::default(),
        },
        repo: Repo { owner: "o".into(), name: "r".into() },
        base_branch: "main".into(),
        branch: "factory/r1".into(),
        pr: None,
        feedback: None,
        model: ModelId("fake-sonnet".into()),
        endpoint: Endpoint { base_url: "http://gateway.invalid".into() },
        max_steps: 5,
        required_checks: Vec::new(),
        worker_checks: Vec::new(),
        blocked_hosts: Vec::new(),
        allowed_hosts: Vec::new(),
        env: Default::default(),
        credential: Credential { agent: "factory".into(), token: String::new() },
    }
}

#[tokio::test(start_paused = true)]
async fn a_worker_keeps_renewing_its_claim_while_publishing_retries() {
    let tracker = FakeTracker::new("factory");
    let key = tracker.add_task("TRAIL", "Add a greeting", "");
    let harness = Arc::new(ScriptedHarness::new());
    harness.push(
        Role::Worker,
        Script::new(RunOutcome::Changes {
            commits: vec![Commit {
                message: format!("feat: greet [{}]", key.0),
                changes: vec![FileChange { path: "src/greet.rs".into(), contents: Some(b"pub fn hi() {}".to_vec()) }],
                merges: vec![],
            }],
            summary: "Adds a greeting.".into(),
        }),
    );
    let codehost = Arc::new(FakeCodeHost::new());
    // Waits of 5, 10 and 20 seconds before the fourth try gets through.
    codehost.flaky_publish(3);
    let connect: Connect = {
        let tracker = tracker.clone();
        Arc::new(move |_| {
            let tracker = tracker.clone();
            Box::pin(async move { Ok(Arc::new(tracker) as Arc<dyn Tracker>) })
        })
    };
    let runner = Runner {
        harness,
        codehost: codehost.clone(),
        workspace: Arc::new(FakeWorkspace::default()),
        connect,
        renew_every: Duration::from_secs(10),
        heartbeat_every: HEARTBEAT_EVERY,
    };
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut req = request(key.clone());
    req.role = Role::Worker;

    let report = runner.run(req, tx, Arc::new(Notify::new())).await.unwrap();

    assert!(matches!(report.result, RunResult::PrOpened(_)), "{:?}", report.result);
    assert_eq!(codehost.publish_attempts(), 4);
    assert!(tracker.renews(&key) >= 3, "renewed {} times", tracker.renews(&key));
}

#[tokio::test(start_paused = true)]
async fn a_publish_that_landed_behind_a_5xx_is_not_a_conflict_on_retry() {
    let tracker = FakeTracker::new("factory");
    let key = tracker.add_task("TRAIL", "Add a greeting", "");
    let harness = Arc::new(ScriptedHarness::new());
    harness.push(
        Role::Worker,
        Script::new(RunOutcome::Changes {
            commits: vec![Commit {
                message: format!("feat: greet [{}]", key.0),
                changes: vec![FileChange { path: "src/greet.rs".into(), contents: Some(b"pub fn hi() {}".to_vec()) }],
                merges: vec![],
            }],
            summary: "Adds a greeting.".into(),
        }),
    );
    let codehost = Arc::new(FakeCodeHost::new());
    codehost.flaky_publish_after_applying(1);
    let connect: Connect = {
        let tracker = tracker.clone();
        Arc::new(move |_| {
            let tracker = tracker.clone();
            Box::pin(async move { Ok(Arc::new(tracker) as Arc<dyn Tracker>) })
        })
    };
    let runner = Runner {
        harness,
        codehost: codehost.clone(),
        workspace: Arc::new(FakeWorkspace::default()),
        connect,
        renew_every: Duration::from_secs(10),
        heartbeat_every: HEARTBEAT_EVERY,
    };
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut req = request(key.clone());
    req.role = Role::Worker;

    let report = runner.run(req, tx, Arc::new(Notify::new())).await.unwrap();

    assert!(matches!(report.result, RunResult::PrOpened(_)), "{:?}", report.result);
    assert_eq!(codehost.publish_attempts(), 2);
}

#[tokio::test]
async fn the_check_results_survive_an_error_recording_a_failed_check() {
    let tracker = FakeTracker::new("factory");
    let key = tracker.add_task("TRAIL", "Add a greeting", "");
    // The checks ran and one failed, but the tracker won't take the note about it.
    tracker.fail_next("factory", "note", 1);
    let harness = Arc::new(ScriptedHarness::new());
    harness.push(
        Role::Worker,
        Script::new(RunOutcome::Changes {
            commits: vec![Commit {
                message: format!("feat: greet [{}]", key.0),
                changes: vec![FileChange { path: "src/greet.rs".into(), contents: Some(b"pub fn hi() {}".to_vec()) }],
                merges: vec![],
            }],
            summary: "Adds a greeting.".into(),
        }),
    );
    let connect: Connect = {
        let tracker = tracker.clone();
        Arc::new(move |_| {
            let tracker = tracker.clone();
            Box::pin(async move { Ok(Arc::new(tracker) as Arc<dyn Tracker>) })
        })
    };
    let daemon = LocalDaemon::connecting(harness, Arc::new(FakeCodeHost::new()), connect);
    daemon.fail_check("tests", "1 failed");
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut req = request(key);
    req.role = Role::Worker;
    req.required_checks = vec![Check { name: "tests".into(), command: "cargo test".into(), paths: vec![] }];

    let report = daemon.runner().run(req, tx, Arc::new(Notify::new())).await.unwrap();

    let RunResult::Failed(reason) = report.result else { panic!("{:?}", report.result) };
    assert!(reason.contains("finishing the run failed"), "{reason}");
    assert_eq!(report.checks.len(), 1);
    assert_eq!(report.checks[0].name, "tests");
    assert!(!report.checks[0].passed);
}
