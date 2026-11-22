//! What the factory's tests set up over and over, written once: a run request, the commit a scripted worker makes,
//! and a poll for something another task does. A test changes what it cares about and takes the rest from here, so
//! a new field on `RunRequest` is given its test value in one place.

use std::time::Duration;

use trailhead_factory::{
    Commit, Credential, Endpoint, FileChange, ModelId, Repo, Role, RunOutcome, RunRequest, TaskKey, TaskSpec,
};

/// A worker's run `r1` on `key` in the repo `o/r`, on its branch off `main`, for five steps on the fake model. A
/// test that wants another role, repo, run name or credential says so over it:
/// `RunRequest { role: Role::Reviewer, ..run_request(key) }`.
pub fn run_request(key: TaskKey) -> RunRequest {
    RunRequest {
        grade: None,
        run: "r1".into(),
        role: Role::Worker,
        task: TaskSpec {
            key,
            title: "Add a greeting".into(),
            description: String::new(),
            previous_failure: None,
            context: Default::default(),
            path: None,
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
        shots: None,
        plan: None,
    }
}

/// What a scripted worker hands back: one commit, `feat: greet`, adding `src/greet.rs`.
pub fn changes() -> RunOutcome {
    RunOutcome::Changes {
        commits: vec![Commit {
            message: "feat: greet".into(),
            changes: vec![FileChange::new("src/greet.rs", Some(b"pub fn hi() {}".to_vec()))],
            merges: vec![],
        }],
        summary: "Adds a greeting.".into(),
    }
}

/// Waits for `done`, looked at every few milliseconds, and panics saying `what` it waited for after about five
/// seconds: for what another task does in its own time, where a sleep of a guessed length would be a flake.
pub async fn until(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..1000 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for {what}");
}
