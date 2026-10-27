//! The lifecycle against the real task engine: claims, leases, review rules,
//! sub-agents and their tokens are Trailhead's own, not the fake's.

use std::sync::Arc;

use trailhead_control::{CoreTracker, Factory, Outcome};
use trailhead_core::{CommentKind, Ctx, NewProject, NewTask, Store, TaskPatch};
use trailhead_factory::{
    Commit, FactoryConfig, FileChange, Role, RunOutcome, RunUpdate, Status, TaskKey, Tracker, Usd, VerdictKind,
};
use trailhead_testkit::{Connect, FakeCodeHost, FakeCompute, FakeProvider, LocalDaemon, Script, ScriptedHarness};

const CONFIG: &str = r#"
[repo]
owner = "lanesawyer"
name = "trailhead"
project = "TRAIL"

[checks]
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
    store: Arc<Store>,
    human: Ctx,
    factory: Factory,
    tracker: CoreTracker,
    compute: FakeCompute,
    codehost: FakeCodeHost,
    harness: Arc<ScriptedHarness>,
}

async fn rig() -> Rig {
    let store = Arc::new(Store::open_local(":memory:").await.unwrap());
    let (ws, user) = store.bootstrap_local("Lane").await.unwrap();
    let human = Ctx::user(ws.id, user.id);
    store
        .create_project(&human, NewProject { key: "TRAIL".into(), name: "Trailhead".into(), ..Default::default() })
        .await
        .unwrap();
    let agent = store.create_agent(&human, "factory").await.unwrap();
    let ctx = store.agent_ctx(ws.id, agent.id).await.unwrap();
    let tracker = CoreTracker::new(store.clone(), ctx, "factory");

    let compute = FakeCompute::new();
    let codehost = FakeCodeHost::new();
    let harness = Arc::new(ScriptedHarness::new());
    let s = store.clone();
    let connect: Connect = Arc::new(move |cred| {
        let s = s.clone();
        Box::pin(async move { Ok(Arc::new(CoreTracker::authenticate(s, &cred).await?) as Arc<dyn Tracker>) })
    });
    let daemon = Arc::new(LocalDaemon::connecting(harness.clone(), Arc::new(codehost.clone()), connect));
    let factory = Factory {
        config: FactoryConfig::parse(CONFIG).unwrap(),
        compute: Arc::new(compute.clone()),
        tracker: Arc::new(tracker.clone()),
        provider: Arc::new(FakeProvider::new(1)),
        daemon,
    };
    Rig { store, human, factory, tracker, compute, codehost, harness }
}

impl Rig {
    async fn task(&self, title: &str) -> TaskKey {
        let new = NewTask { project: "TRAIL".into(), title: title.into(), ..Default::default() };
        TaskKey(self.store.create_task(&self.human, new).await.unwrap().key)
    }

    async fn show(&self, key: &TaskKey) -> trailhead_factory::Task {
        self.tracker.show(key).await.unwrap()
    }

    async fn revoked(&self, agent: &str) -> bool {
        let agents = self.store.list_agents(&self.human).await.unwrap();
        agents.iter().find(|a| a.name == agent).unwrap_or_else(|| panic!("no agent {agent}")).revoked_at.is_some()
    }
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

#[tokio::test]
async fn spec_to_reviewed_pr_on_the_real_engine() {
    let r = rig().await;
    let key = r.task("Add a greeting").await;
    r.harness.push(Role::Worker, Script::new(changes()).steps(2, 100_000));
    r.harness
        .push(Role::Reviewer, Script::new(RunOutcome::Verdict { kind: VerdictKind::Approve, body: "LGTM".into() }));

    let ran = r.factory.tick().await.unwrap();
    let Outcome::Reviewed { pr, verdict } = &ran[0].1 else { panic!("{:?}", ran[0].1) };
    assert_eq!(verdict.reviewer, "factory/trail-1-a1-r1");

    let task = r.show(&key).await;
    assert_eq!(task.status, Status::InReview);
    assert_eq!(task.holder, None, "moving to review releases the claim");
    assert_eq!(task.run.pr.as_ref(), Some(pr));
    assert_eq!(task.run.spend, Some(Usd::dollars(0.2)));
    assert_eq!(task.run.reviewed, Some(1));

    let comments = r.store.list_comments(&r.human, &key.0).await.unwrap();
    let review = comments.iter().find(|c| c.kind == CommentKind::Review).expect("a typed review");
    assert_eq!(review.verdict, Some(trailhead_core::Verdict::Approve));
    assert_eq!(review.model.as_deref(), Some("fake-opus"));
    assert!(r.revoked("factory/trail-1-a1").await);
    assert!(r.revoked("factory/trail-1-a1-r1").await);
    assert!(r.compute.sandboxes().is_empty());
    assert_eq!(r.codehost.prs().len(), 1);

    // An agent can't finish a reviewed task; Lane can.
    assert!(r.tracker.finish(&key, "merged").await.is_err());
    let done = TaskPatch { status: Some(trailhead_core::Status::Done), ..Default::default() };
    r.store.update_task(&r.human, &key.0, done, None).await.unwrap();
    assert_eq!(r.show(&key).await.status, Status::Done);
}

#[tokio::test]
async fn breach_parks_with_the_claim_held_by_the_sandbox() {
    let r = rig().await;
    let key = r.task("Expensive").await;
    r.harness.push(Role::Worker, Script::new(changes()).steps(10, 2_000_000));

    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Parked { .. }), "{:?}", ran[0].1);
    let task = r.show(&key).await;
    assert_eq!(task.status, Status::NeedsInput);
    assert_eq!(task.holder.as_deref(), Some("factory/trail-1-a1"));
    assert_eq!(task.run.spend, Some(Usd::dollars(6.0)));
    let questions =
        r.store.open_questions(&r.human, &[r.store.get_task(&r.human, &key.0).await.unwrap().id]).await.unwrap();
    assert_eq!(questions.values().flatten().count(), 1);

    // The control plane itself can't touch it; only the sub-agent can.
    let update = RunUpdate { attempt: Some(9), ..Default::default() };
    assert!(r.tracker.report(&key, update).await.is_err());
}

#[tokio::test]
async fn sandbox_tokens_are_scoped_and_die_with_the_sub_agent() {
    let r = rig().await;
    r.store
        .create_project(&r.human, NewProject { key: "OTHER".into(), name: "Other".into(), ..Default::default() })
        .await
        .unwrap();
    let other = TaskKey(
        r.store
            .create_task(&r.human, NewTask { project: "OTHER".into(), title: "x".into(), ..Default::default() })
            .await
            .unwrap()
            .key,
    );
    let cred = r.tracker.create_sub_agent("trail-1-a1", "TRAIL", std::time::Duration::from_secs(3600)).await.unwrap();
    let sandbox = CoreTracker::authenticate(r.store.clone(), &cred).await.unwrap();
    assert!(sandbox.show(&other).await.is_err(), "scoped to TRAIL");

    r.tracker.revoke_sub_agent("trail-1-a1").await.unwrap();
    assert!(CoreTracker::authenticate(r.store.clone(), &cred).await.is_err());
}

#[tokio::test]
async fn reconcile_requeues_an_interrupted_run() {
    let r = rig().await;
    let key = r.task("Crashed").await;
    r.tracker.create_sub_agent("trail-1-a1", "TRAIL", std::time::Duration::from_secs(3600)).await.unwrap();
    let sub = r.tracker.sub_agent("trail-1-a1");
    sub.claim(&key).await.unwrap();
    sub.report(&key, RunUpdate { attempt: Some(1), sandbox: Some("trail-1-a1".into()), ..Default::default() })
        .await
        .unwrap();

    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.requeued, std::slice::from_ref(&key.0));
    let task = r.show(&key).await;
    assert_eq!(task.status, Status::Open);
    assert_eq!(task.holder, None);
    assert!(r.revoked("factory/trail-1-a1").await);

    // The next tick picks it up as attempt 2.
    r.harness.push(Role::Worker, Script::new(RunOutcome::NeedsInput { question: "?".into() }));
    r.factory.tick().await.unwrap();
    assert_eq!(r.show(&key).await.run.attempt, Some(2));
}
