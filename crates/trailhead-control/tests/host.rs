//! Two repos' factories on one `Host`: they pause together and share a sandbox cap.

use std::sync::Arc;

use trailhead_control::{Factory, Host};
use trailhead_factory::{Compute, FactoryConfig, Labels, Role, RunOutcome, SandboxSpec, TaskKey, VerdictKind, label};
use trailhead_testkit::{FakeCodeHost, FakeCompute, FakeProvider, FakeTracker, LocalDaemon, Script, ScriptedHarness};

fn config(repo: &str, extra: &str) -> String {
    format!(
        r#"
[repo]
owner = "lanesawyer"
name = "{repo}"
{extra}

[usage]
pause_at = {{ five_hour = 80 }}

[checks]
fmt = "cargo fmt --check"
lint = "cargo clippy -- -D warnings"
test = "cargo test"

[profiles.worker]
role = "worker"
adapter = "claude-code"
provider = "anthropic"
model = "sonnet"
budget = {{ usd = 5, minutes = 60, steps = 40 }}

[profiles.reviewer]
role = "reviewer"
adapter = "claude-code"
provider = "anthropic"
model = "opus"
budget = {{ usd = 2, minutes = 20, steps = 20 }}
"#
    )
}

struct Repo {
    factory: Factory,
    tracker: FakeTracker,
    harness: Arc<ScriptedHarness>,
}

fn repo(name: &str, extra: &str, compute: &FakeCompute, host: &Arc<Host>) -> Repo {
    let tracker = FakeTracker::new("factory");
    let codehost = FakeCodeHost::new();
    let harness = Arc::new(ScriptedHarness::new());
    let heads = codehost.clone();
    let daemon = Arc::new(
        LocalDaemon::new(harness.clone(), Arc::new(codehost.clone()), tracker.clone())
            .with_heads(move |repo, branch| heads.head(repo, branch)),
    );
    let factory = Factory::new(
        FactoryConfig::parse_table(&config(name, extra), "TRAIL", None).unwrap(),
        Arc::new(compute.clone()),
        Arc::new(tracker.clone()),
        Arc::new(FakeProvider::new(1)),
        daemon,
        host.clone(),
    );
    Repo { factory, tracker, harness }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

fn changes() -> RunOutcome {
    RunOutcome::Changes { commits: vec![], summary: "Nothing to see.".into() }
}

fn approve() -> RunOutcome {
    RunOutcome::Verdict { kind: VerdictKind::Approve, body: "Fine.".into(), follow_ups: vec![], raise_grade: None }
}

/// A worker sandbox held for `repo`.
async fn hold(compute: &FakeCompute, name: &str, repo: &str) {
    let labels = Labels::from([
        (label::FACTORY.into(), "trailhead".into()),
        (label::REPO.into(), format!("lanesawyer/{repo}")),
        (label::TASK.into(), TaskKey("TRAIL-99".into()).0),
        (label::ROLE.into(), "worker".into()),
    ]);
    compute.create(&SandboxSpec { name: name.into(), labels, egress: Default::default() }).await.unwrap();
}

fn queue(r: &Repo) {
    r.tracker.add_task("TRAIL", "A task", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
}

#[tokio::test]
async fn a_quota_reading_from_one_repo_pauses_both() {
    let compute = FakeCompute::new();
    let host = Host::new(None);
    let (a, b) = (repo("alpha", "", &compute, &host), repo("beta", "", &compute, &host));
    a.tracker.add_task("TRAIL", "First", "");
    a.harness.push(Role::Worker, Script::new(changes()).quota("five_hour", 85, unix_now() + 3600));
    a.harness.push(Role::Reviewer, Script::new(approve()));
    assert_eq!(a.factory.tick().await.unwrap().len(), 1);

    queue(&a);
    queue(&b);
    assert!(a.factory.tick().await.unwrap().is_empty(), "the reading's own repo is paused");
    assert!(b.factory.tick().await.unwrap().is_empty(), "so is the other repo");
}

#[tokio::test]
async fn a_host_cap_counts_every_repos_sandboxes() {
    let compute = FakeCompute::new();
    let host = Host::new(Some(1));
    let (a, b) =
        (repo("alpha", "max_sandboxes = 3", &compute, &host), repo("beta", "max_sandboxes = 3", &compute, &host));
    hold(&compute, "trail-99-a1", "beta").await;
    queue(&a);
    assert!(a.factory.tick().await.unwrap().is_empty(), "beta's sandbox fills the host's one place");
    assert!(b.factory.tick().await.unwrap().is_empty(), "and beta is full itself");

    compute.destroy(&compute.sandboxes()[0].id).await.unwrap();
    assert_eq!(a.factory.tick().await.unwrap().len(), 1, "a place is free again");
}

#[tokio::test]
async fn a_repos_own_cap_still_applies_under_a_roomy_host() {
    let compute = FakeCompute::new();
    let host = Host::new(Some(10));
    let a = repo("alpha", "max_sandboxes = 1", &compute, &host);
    hold(&compute, "trail-99-a1", "alpha").await;
    queue(&a);
    assert!(a.factory.tick().await.unwrap().is_empty());
}

async fn until(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..400 {
        if done() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn two_repos_loops_run_at_once_each_taking_only_its_own_projects_tasks() {
    let compute = FakeCompute::new();
    let host = Host::new(None);
    let tracker = FakeTracker::new("factory");
    let mut loops = Vec::new();
    let mut harnesses = Vec::new();
    for (name, project) in [("alpha", "ALPHA"), ("beta", "BETA")] {
        let codehost = FakeCodeHost::new();
        let harness = Arc::new(ScriptedHarness::new());
        let heads = codehost.clone();
        let daemon = Arc::new(
            LocalDaemon::new(harness.clone(), Arc::new(codehost.clone()), tracker.clone())
                .with_heads(move |repo, branch| heads.head(repo, branch)),
        );
        let factory = Arc::new(Factory::new(
            FactoryConfig::parse_table(&config(name, ""), project, None).unwrap(),
            Arc::new(compute.clone()),
            Arc::new(tracker.clone()),
            Arc::new(FakeProvider::new(1)),
            daemon,
            host.clone(),
        ));
        harness.push(Role::Worker, Script::new(changes()));
        harness.push(Role::Reviewer, Script::new(approve()));
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(factory.run(
            Arc::new(tokio::sync::Notify::new()),
            std::time::Duration::from_millis(5),
            async {
                let _ = stopped.await;
            },
        ));
        loops.push((stop, handle));
        harnesses.push((project, harness));
    }
    let alpha = tracker.add_task("ALPHA", "Alpha's task", "");
    let beta = tracker.add_task("BETA", "Beta's task", "");

    for (project, harness) in &harnesses {
        let harness = harness.clone();
        until(&format!("{project}'s run"), move || !harness.contexts().is_empty()).await;
    }
    for (stop, handle) in loops {
        stop.send(()).unwrap();
        handle.await.unwrap().unwrap();
    }
    for (project, harness) in &harnesses {
        let seen: Vec<_> = harness.contexts().iter().map(|c| c.task.key.clone()).collect();
        let own = if *project == "ALPHA" { &alpha } else { &beta };
        assert!(!seen.is_empty() && seen.iter().all(|k| k == own), "{project} saw {seen:?}");
    }
}

#[tokio::test]
async fn repos_sharing_a_project_through_different_queues_leave_each_others_in_flight_tasks_alone() {
    use trailhead_factory::{RunUpdate, Status, Tracker};

    let compute = FakeCompute::new();
    let host = Host::new(None);
    let tracker = FakeTracker::new("factory");
    let q1 = tracker.add_task("TRAIL", "Alpha's queue", "");
    let q2 = tracker.add_task("TRAIL", "Beta's queue", "");
    let mut factories = Vec::new();
    for (name, queue) in [("alpha", &q1), ("beta", &q2)] {
        let codehost = FakeCodeHost::new();
        let harness = Arc::new(ScriptedHarness::new());
        let daemon = Arc::new(LocalDaemon::new(harness, Arc::new(codehost), tracker.clone()));
        factories.push(Factory::new(
            FactoryConfig::parse_table(&config(name, &format!("queue = \"{}\"", queue.0)), "TRAIL", None).unwrap(),
            Arc::new(compute.clone()),
            Arc::new(tracker.clone()),
            Arc::new(FakeProvider::new(1)),
            daemon,
            host.clone(),
        ));
    }
    // Beta's run is live: its task is in progress, held by its run's sub-agent, with a sandbox alpha can't see.
    let task = tracker.add_subtask(&q2, "Beta's task", "");
    tracker.set_status(&task, Status::InProgress);
    tracker
        .report(
            &task,
            RunUpdate {
                sandbox: Some("beta-run".into()),
                host: Some("beta-w1".into()),
                attempt: Some(1),
                ..RunUpdate::default()
            },
        )
        .await
        .unwrap();

    let done = factories[0].reconcile().await.unwrap();
    assert!(done.requeued.is_empty() && done.reviewed.is_empty(), "alpha touched beta's task: {done:?}");
    assert_eq!(tracker.task(&task).status, Status::InProgress);
    assert!(
        tracker.in_flight("TRAIL", Some(&q1)).await.unwrap().is_empty(),
        "alpha's in-flight set holds only its own queue's tasks"
    );
    assert_eq!(tracker.in_flight("TRAIL", Some(&q2)).await.unwrap().len(), 1);
}
