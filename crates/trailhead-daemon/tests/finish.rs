//! What the daemon reports when it fails after the agent's outcome is known (TRAIL-214).

use std::sync::Arc;

use tokio::sync::{Notify, mpsc};
use trailhead_daemon::{Connect, RENEW_EVERY, Runner};
use trailhead_factory::{
    Credential, Endpoint, ModelId, Repo, Role, RunOutcome, RunRequest, RunResult, TaskKey, TaskSpec, Tracker,
    VerdictKind,
};
use trailhead_testkit::{FakeCodeHost, FakeTracker, FakeWorkspace, Script, ScriptedHarness};

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
        credential: Credential { agent: "factory".into(), token: String::new() },
    }
}
