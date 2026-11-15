//! What the daemon reports when it fails after the agent's outcome is known (TRAIL-214).

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Notify, mpsc};
use trailhead_daemon::{Connect, HEARTBEAT_EVERY, RENEW_EVERY, Runner};
use trailhead_factory::{
    Check, Commit, Credential, Endpoint, Error, FileChange, ModelId, Repo, Role, RunOutcome, RunRequest, RunResult,
    TaskKey, TaskSpec, Tracker, VerdictKind,
};
use trailhead_testkit::{FakeCodeHost, FakeTracker, FakeWorkspace, LocalDaemon, Script, ScriptedHarness, TrackerOp};

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
                changes: vec![FileChange::new("src/greet.rs", Some(b"pub fn hi() {}".to_vec()))],
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
                changes: vec![FileChange::new("src/greet.rs", Some(b"pub fn hi() {}".to_vec()))],
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
    tracker.fail_next("factory", TrackerOp::Note, 1);
    let harness = Arc::new(ScriptedHarness::new());
    harness.push(
        Role::Worker,
        Script::new(RunOutcome::Changes {
            commits: vec![Commit {
                message: format!("feat: greet [{}]", key.0),
                changes: vec![FileChange::new("src/greet.rs", Some(b"pub fn hi() {}".to_vec()))],
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

/// Runs a worker that opens a PR, with `tracker` as the one the daemon writes to after it.
async fn open_pr_with(tracker: &FakeTracker, key: &TaskKey) -> RunResult {
    let harness = Arc::new(ScriptedHarness::new());
    harness.push(
        Role::Worker,
        Script::new(RunOutcome::Changes {
            commits: vec![Commit {
                message: format!("feat: greet [{}]", key.0),
                changes: vec![FileChange::new("src/greet.rs", Some(b"pub fn hi() {}".to_vec()))],
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
    let runner = Runner {
        harness,
        codehost: Arc::new(FakeCodeHost::new()),
        workspace: Arc::new(FakeWorkspace::default()),
        connect,
        renew_every: Duration::from_secs(10),
        heartbeat_every: HEARTBEAT_EVERY,
    };
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut req = request(key.clone());
    req.role = Role::Worker;
    runner.run(req, tx, Arc::new(Notify::new())).await.unwrap().result
}

#[tokio::test(start_paused = true)]
async fn a_failed_review_request_is_retried_without_reporting_the_pr_again() {
    let tracker = FakeTracker::new("factory");
    let key = tracker.add_task("TRAIL", "Add a greeting", "");
    tracker.fail_next("factory", TrackerOp::RequestReview, 1);

    let result = open_pr_with(&tracker, &key).await;

    assert!(matches!(result, RunResult::PrOpened(_)), "{result:?}");
    assert_eq!(tracker.reports(&key), 1);
    assert_eq!(tracker.review_requests(&key), 2);
}

#[tokio::test(start_paused = true)]
async fn a_report_the_tracker_refuses_is_not_retried() {
    for error in [Error::Forbidden("not yours".into()), Error::Rejected("bad patch".into())] {
        let tracker = FakeTracker::new("factory");
        let key = tracker.add_task("TRAIL", "Add a greeting", "");
        tracker.fail_next_with("factory", TrackerOp::Report, 5, error.clone());

        let result = open_pr_with(&tracker, &key).await;

        assert!(matches!(result, RunResult::PrOpened(_)), "{error}: {result:?}");
        assert_eq!(tracker.reports(&key), 1, "{error}");
        assert_eq!(tracker.review_requests(&key), 0, "{error}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_review_request_the_tracker_refuses_is_not_retried() {
    for error in [Error::Forbidden("not yours".into()), Error::Rejected("bad state".into())] {
        let tracker = FakeTracker::new("factory");
        let key = tracker.add_task("TRAIL", "Add a greeting", "");
        tracker.fail_next_with("factory", TrackerOp::RequestReview, 5, error.clone());

        let result = open_pr_with(&tracker, &key).await;

        assert!(matches!(result, RunResult::PrOpened(_)), "{error}: {result:?}");
        assert_eq!(tracker.reports(&key), 1, "{error}");
        assert_eq!(tracker.review_requests(&key), 1, "{error}");
    }
}

#[tokio::test(start_paused = true)]
async fn claim_renewal_keeps_its_cadence_from_driving_into_finishing() {
    let tracker = FakeTracker::new("factory");
    let key = tracker.add_task("TRAIL", "Add a greeting", "");
    let harness = Arc::new(ScriptedHarness::new());
    // The agent ends at 8s, just before the first renewal is due at 10s.
    harness.push(
        Role::Worker,
        Script::new(RunOutcome::Changes {
            commits: vec![Commit {
                message: format!("feat: greet [{}]", key.0),
                changes: vec![FileChange::new("src/greet.rs", Some(b"pub fn hi() {}".to_vec()))],
                merges: vec![],
            }],
            summary: "Adds a greeting.".into(),
        })
        .slow_tool(Duration::from_secs(8)),
    );
    let codehost = Arc::new(FakeCodeHost::new());
    // Publishing tries at 8s, 13s and 23s (waits of 5 and 10 seconds), then gets through.
    codehost.flaky_publish(2);
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
    assert_eq!(codehost.publish_attempts(), 3);
    // Renewals fall at 10s and 20s. A timer restarted when the agent ended (8s) would renew at 18s only.
    assert_eq!(tracker.renews(&key), 2);
}
