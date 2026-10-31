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
