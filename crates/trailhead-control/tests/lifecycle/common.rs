//! Fixtures shared by the lifecycle tests.

use std::sync::Arc;

use trailhead_control::{Factory, Host};
use trailhead_factory::{
    Commit, Compute, FactoryConfig, FileChange, Role, RunOutcome, SandboxSpec, TaskKey, Tracker, Usd, VerdictKind,
    conventional_title, label, run_name,
};
use trailhead_testkit::{FakeCodeHost, FakeCompute, FakeProvider, FakeTracker, LocalDaemon, Script, ScriptedHarness};

pub const CONFIG: &str = r#"
[repo]
owner = "lanesawyer"
name = "trailhead"

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

pub struct Rig {
    pub factory: Factory,
    pub tracker: FakeTracker,
    pub compute: FakeCompute,
    pub codehost: FakeCodeHost,
    pub harness: Arc<ScriptedHarness>,
    pub daemon: Arc<LocalDaemon>,
}

pub fn rig() -> Rig {
    rig_with(CONFIG)
}

/// One micro-dollar per token, so 1,000,000 tokens is $1.
pub fn rig_with(config: &str) -> Rig {
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
        FactoryConfig::parse_table(config, "TRAIL", None).unwrap(),
        Arc::new(compute.clone()),
        Arc::new(tracker.clone()),
        Arc::new(FakeProvider::new(1)),
        daemon.clone(),
        Host::new(None),
    );
    Rig { factory, tracker, compute, codehost, harness, daemon }
}

pub fn changes() -> RunOutcome {
    RunOutcome::Changes {
        commits: vec![Commit {
            message: conventional_title(Some("feat(core): greet"), &TaskKey("TRAIL-1".into()), "").0,
            changes: vec![FileChange { path: "src/greet.rs".into(), contents: Some(b"pub fn hi() {}".to_vec()) }],
            merges: vec![],
        }],
        summary: "Adds a greeting.".into(),
    }
}

pub fn approve() -> RunOutcome {
    RunOutcome::Verdict {
        kind: VerdictKind::Approve,
        body: "Looks right; tests cover it.".into(),
        follow_ups: vec![],
        raise_grade: None,
    }
}

/// A worker run the last control plane left going: its task in progress under the run's sub-agent, on a busy pooled
/// sandbox, with `before` already spent by earlier runs and the run started `ago` back.
pub async fn going(r: &Rig, before: Usd, ago: i64) -> TaskKey {
    going_on(r, before, ago, "trailhead-w1").await
}

/// [`going`] on the sandbox named `host`.
pub async fn going_on(r: &Rig, before: Usd, ago: i64, host: &str) -> TaskKey {
    let key = r.tracker.add_task("TRAIL", "Going", "");
    let name = run_name(&key, 1);
    let tracker: Arc<dyn Tracker> = Arc::new(r.tracker.clone());
    tracker.create_sub_agent(&name, "TRAIL", std::time::Duration::from_secs(3600)).await.unwrap();
    let sub = tracker.sub_agent(&name);
    sub.claim(&key).await.unwrap();
    let started = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 - ago;
    let report = trailhead_factory::RunUpdate {
        host: Some(host.into()),
        started: Some(started),
        spend: Some(before),
        ..update(1, &name)
    };
    sub.report(&key, report).await.unwrap();
    let labels = trailhead_factory::Labels::from([
        (label::FACTORY.into(), "trailhead".into()),
        (label::REPO.into(), "lanesawyer/trailhead".into()),
        (label::POOL.into(), "worker".into()),
        (label::STATE.into(), label::BUSY.into()),
        (label::TASK.into(), key.0.clone()),
        (label::ROLE.into(), "worker".into()),
    ]);
    r.compute.create(&SandboxSpec { name: host.into(), labels, egress: Default::default() }).await.unwrap();
    key
}

pub fn spent(tokens: u64) -> trailhead_factory::RunEvent {
    let usage = trailhead_factory::Usage { input_tokens: tokens, ..Default::default() };
    trailhead_factory::RunEvent::Usage { model: trailhead_factory::ModelId("sonnet".into()), usage }
}

/// Sandboxes that aren't idle in the pool: busy, parked, or not pooled at all.
pub fn busy(compute: &FakeCompute) -> Vec<String> {
    compute
        .sandboxes()
        .into_iter()
        .filter(|s| s.labels.get(label::STATE).map(String::as_str) != Some(label::IDLE))
        .map(|s| s.name)
        .collect()
}

pub fn update(attempt: u32, sandbox: &str) -> trailhead_factory::RunUpdate {
    trailhead_factory::RunUpdate { attempt: Some(attempt), sandbox: Some(sandbox.into()), ..Default::default() }
}

pub async fn live(compute: &FakeCompute, name: &str, task: &TaskKey, role: &str) {
    let labels = trailhead_factory::Labels::from([
        (label::FACTORY.into(), "trailhead".into()),
        (label::REPO.into(), "lanesawyer/trailhead".into()),
        (label::TASK.into(), task.0.clone()),
        (label::ROLE.into(), role.into()),
    ]);
    compute.create(&SandboxSpec { name: name.into(), labels, egress: Default::default() }).await.unwrap();
}

/// The config with a stall timeout past the wall clock, for tests of the wall clock.
pub fn no_stall() -> String {
    CONFIG.replacen(
        "budget = { usd = 5",
        "stall_timeout = 600
budget = { usd = 5",
        1,
    )
}

pub async fn until(what: &str, mut cond: impl FnMut() -> bool) {
    for _ in 0..500 {
        if cond() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for {what}");
}

pub fn request_changes(body: &str) -> RunOutcome {
    RunOutcome::Verdict { kind: VerdictKind::RequestChanges, body: body.into(), follow_ups: vec![], raise_grade: None }
}

/// A rig whose factory works only the subtasks of TRAIL-1 and reads PRs through the code host, which grading needs.
pub fn queued_rig() -> (Rig, TaskKey) {
    queued_rig_with("")
}

pub fn queued_rig_with(extra: &str) -> (Rig, TaskKey) {
    let config = CONFIG.replace("name = \"trailhead\"", "name = \"trailhead\"\nqueue = \"TRAIL-1\"");
    let mut r = rig_with(&format!("{config}\n{extra}"));
    let queue = r.tracker.add_task("TRAIL", "Factory queue", "");
    let codehost = Arc::new(r.codehost.clone());
    r.factory = r.factory.with_codehost(codehost);
    (r, queue)
}

pub fn changes_to(path: &str) -> RunOutcome {
    RunOutcome::Changes {
        commits: vec![Commit {
            message: "docs(docs): note it [TRAIL-2]".into(),
            changes: vec![FileChange { path: path.into(), contents: Some(b"x".to_vec()) }],
            merges: vec![],
        }],
        summary: "Done.".into(),
    }
}

pub fn notes(r: &Rig, key: &TaskKey) -> Vec<String> {
    r.tracker.comments(key).into_iter().filter(|c| c.kind == "note").map(|c| c.body).collect()
}

/// A graded task worked up to an approved review, with CI still pending.
pub async fn reviewed(r: &Rig, queue: &TaskKey, grade: u8, path: &str) -> (TaskKey, trailhead_factory::PrRef) {
    let key = r.tracker.add_subtask(queue, "Do it", "");
    r.tracker.set_grade(&key, grade);
    r.harness.push(Role::Worker, Script::new(changes_to(path)));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();
    let pr = r.tracker.task(&key).run.pr.expect("a PR");
    (key, pr)
}

pub fn said(r: &Rig) -> Vec<String> {
    r.codehost.prs()[0].comments.iter().skip(1).cloned().collect()
}
