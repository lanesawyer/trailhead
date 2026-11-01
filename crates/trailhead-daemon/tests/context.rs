//! What the daemon puts in the `RunContext` it starts the agent with: the base branch (TRAIL-233) and the PR's title
//! (TRAIL-226) are for reviewers only.

use std::sync::Arc;

use tokio::sync::{Notify, mpsc};
use trailhead_daemon::{Connect, HEARTBEAT_EVERY, RENEW_EVERY, Runner};
use trailhead_factory::{
    CodeHost, Commit, Credential, Endpoint, FileChange, ModelId, NewPr, Repo, Role, RunOutcome, RunRequest, TaskKey,
    TaskSpec, Tracker,
};
use trailhead_testkit::{FakeCodeHost, FakeTracker, FakeWorkspace, Script, ScriptedHarness};

#[tokio::test]
async fn a_reviewer_gets_the_base_branch_and_pr_title_and_a_worker_gets_neither() {
    let tracker = FakeTracker::new("factory");
    let key = tracker.add_task("TRAIL", "Add a greeting", "");
    let repo = Repo { owner: "o".into(), name: "r".into() };
    let codehost = Arc::new(FakeCodeHost::new());
    let commit = Commit {
        message: "Add a greeting".into(),
        changes: vec![FileChange { path: "hi.txt".into(), contents: Some(b"hi".to_vec()) }],
        merges: Vec::new(),
    };
    codehost.publish(&repo, "base", "factory/r1", &[commit]).await.unwrap();
    let pr = codehost
        .open_pr(
            &repo,
            NewPr {
                branch: "factory/r1".into(),
                base: "develop".into(),
                title: "feat: add a greeting".into(),
                body: String::new(),
            },
        )
        .await
        .unwrap();

    let harness = Arc::new(ScriptedHarness::new());
    harness.push(Role::Worker, Script::new(RunOutcome::Failed { reason: "not under test".into() }));
    harness.push(Role::Reviewer, Script::new(RunOutcome::Failed { reason: "not under test".into() }));
    let connect: Connect = {
        let tracker = tracker.clone();
        Arc::new(move |_| {
            let tracker = tracker.clone();
            Box::pin(async move { Ok(Arc::new(tracker) as Arc<dyn Tracker>) })
        })
    };
    let runner = Runner {
        harness: harness.clone(),
        codehost,
        workspace: Arc::new(FakeWorkspace::default()),
        connect,
        renew_every: RENEW_EVERY,
        heartbeat_every: HEARTBEAT_EVERY,
    };

    for (role, pr) in [(Role::Worker, None), (Role::Reviewer, Some(pr))] {
        let (tx, _rx) = mpsc::unbounded_channel();
        runner.run(request(key.clone(), repo.clone(), role, pr), tx, Arc::new(Notify::new())).await.unwrap();
    }

    let contexts = harness.contexts();
    assert_eq!(contexts.len(), 2);
    assert_eq!(contexts[0].role, Role::Worker);
    assert_eq!(contexts[0].base_branch, None);
    assert_eq!(contexts[0].title, None);
    assert_eq!(contexts[1].role, Role::Reviewer);
    assert_eq!(contexts[1].base_branch.as_deref(), Some("develop"));
    assert_eq!(contexts[1].title.as_deref(), Some("feat: add a greeting"));
}

fn request(key: TaskKey, repo: Repo, role: Role, pr: Option<trailhead_factory::PrRef>) -> RunRequest {
    RunRequest {
        grade: None,
        run: "r1".into(),
        role,
        task: TaskSpec {
            key,
            title: "Add a greeting".into(),
            description: String::new(),
            previous_failure: None,
            context: Default::default(),
        },
        repo,
        base_branch: "develop".into(),
        branch: "factory/r1".into(),
        pr,
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
