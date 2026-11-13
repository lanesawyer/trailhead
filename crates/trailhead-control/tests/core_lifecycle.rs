//! The lifecycle against the real task engine: claims, leases, review rules,
//! sub-agents and their tokens are Trailhead's own, not the fake's.

use std::sync::Arc;

#[path = "support/logs.rs"]
mod logs;
use logs::Logs;

use trailhead_control::{CoreTracker, Factory, Host, Outcome};
use trailhead_core::{CommentKind, Ctx, Grade, NewProject, NewTask, Priority, ProjectPatch, Store, TaskPatch};

fn gr(n: u8) -> Grade {
    Grade::new(n).unwrap()
}

fn pr(n: u8) -> Priority {
    Priority::new(n).unwrap()
}
use trailhead_factory::HostConfig;
use trailhead_factory::{
    CodeHost, Commit, FactoryConfig, FileChange, FollowUp, ModelId, Patch, Role, RunOutcome, RunPatch, Status, TaskKey,
    Tracker, Usd, Verdict, VerdictKind, label,
};
use trailhead_testkit::{
    Connect, FAKE_BASE_MERGE, FakeCodeHost, FakeCompute, FakeProvider, LocalDaemon, Script, ScriptedHarness,
};

const CONFIG: &str = r#"
[repo]
owner = "lanesawyer"
name = "trailhead"

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
    daemon: Arc<LocalDaemon>,
}

async fn rig() -> Rig {
    rig_with(CONFIG).await
}

async fn rig_with(config: &str) -> Rig {
    rig_paged(config, None).await
}

/// A rig whose tracker reads lists `page_size` tasks at a time, when given.
async fn rig_paged(config: &str, page_size: Option<usize>) -> Rig {
    let store = Arc::new(Store::open_local(":memory:").await.unwrap());
    let (ws, user) = store.bootstrap_local("Lane").await.unwrap();
    let human = Ctx::user(ws.id, user.id);
    store
        .create_project(&human, NewProject { key: "TRAIL".into(), name: "Trailhead".into(), ..Default::default() })
        .await
        .unwrap();
    let agent = store.create_agent(&human, "factory").await.unwrap();
    let ctx = store.agent_ctx(ws.id, agent.id).await.unwrap();
    let mut tracker = CoreTracker::new(store.clone(), ctx, "factory");
    if let Some(n) = page_size {
        tracker = tracker.with_page_size(n);
    }

    let compute = FakeCompute::new();
    let codehost = FakeCodeHost::new();
    let harness = Arc::new(ScriptedHarness::new());
    let s = store.clone();
    let connect: Connect = Arc::new(move |cred| {
        let s = s.clone();
        Box::pin(async move { Ok(Arc::new(CoreTracker::authenticate(s, &cred).await?) as Arc<dyn Tracker>) })
    });
    let heads = codehost.clone();
    let daemon = Arc::new(
        LocalDaemon::connecting(harness.clone(), Arc::new(codehost.clone()), connect)
            .with_heads(move |repo, branch| heads.head(repo, branch)),
    );
    let factory = Factory::new(
        FactoryConfig::parse_table(config, "TRAIL", None).unwrap(),
        Arc::new(compute.clone()),
        Arc::new(tracker.clone()),
        Arc::new(FakeProvider::new(1)),
        daemon.clone(),
        Host::new(&HostConfig::default()),
    )
    .with_codehost(Arc::new(codehost.clone()));
    Rig { store, human, factory, tracker, compute, codehost, harness, daemon }
}

impl Rig {
    async fn task(&self, title: &str) -> TaskKey {
        let new = NewTask { project: "TRAIL".into(), title: title.into(), ..Default::default() };
        self.store.create_task(&self.human, new).await.unwrap().key
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
            changes: vec![FileChange::new("src/greet.rs", Some(b"pub fn hi() {}".to_vec()))],
            merges: vec![],
        }],
        summary: "Adds a greeting.".into(),
    }
}

#[tokio::test]
async fn spec_to_reviewed_pr_on_the_real_engine() {
    let r = rig().await;
    let key = r.task("Add a greeting").await;
    r.harness.push(Role::Worker, Script::new(changes()).steps(2, 100_000));
    r.harness.push(
        Role::Reviewer,
        Script::new(RunOutcome::Verdict {
            kind: VerdictKind::Approve,
            body: "LGTM".into(),
            follow_ups: vec![FollowUp {
                title: "Cover the edge".into(),
                description: "No test for empty input.".into(),
                priority: Some(pr(2)),
            }],
            raise_grade: None,
        }),
    );

    let ran = r.factory.tick().await.unwrap();
    let Outcome::Reviewed { pr, verdict } = &ran[0].1 else { panic!("{:?}", ran[0].1) };
    assert_eq!(verdict.reviewer, "factory/trail-1-a1-r1");

    let filed_key = r.tracker.find_open_by_title("TRAIL", "cover the edge ").await.unwrap().expect("a filed follow-up");
    assert_ne!(filed_key, key);
    // The reviewer's comment lists the key only if create_task returned it.
    let comment = r.codehost.prs()[0].comments.last().unwrap().clone();
    assert!(comment.contains(&format!("`{}` Cover the edge", filed_key.0)), "{comment}");
    assert!(!comment.contains("(already open)"), "{comment}");
    let filed = r.store.get_task(&r.human, &filed_key.0).await.unwrap();
    assert_eq!(filed.title, "Cover the edge");
    assert_eq!(filed.priority.get(), 2);
    assert_eq!(filed.status, trailhead_core::Status::Open);
    assert!(filed.description.starts_with("No test for empty input."), "{}", filed.description);
    assert!(filed.description.contains(&key.0), "{}", filed.description);

    // The real engine lists only unfinished follow-ups (TaskFilter::default()).
    let listed = r.tracker.follow_ups(&key).await.unwrap();
    assert_eq!(listed.iter().map(|f| &f.key).collect::<Vec<_>>(), [&filed_key]);
    assert_eq!(listed[0].title, "Cover the edge");
    let finish = TaskPatch { status: Some(trailhead_core::Status::Done), ..Default::default() };
    r.store.update_task(&r.human, &filed_key.0, finish, None).await.unwrap();
    assert!(r.tracker.follow_ups(&key).await.unwrap().is_empty(), "a finished follow-up is no longer listed");

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
    let left: Vec<_> =
        r.compute.sandboxes().into_iter().map(|s| (s.name, s.labels.get(label::STATE).cloned())).collect();
    assert_eq!(left, [("trailhead-w1".to_string(), Some(label::IDLE.to_string()))], "only the idle pooled worker");
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
    let update = trailhead_factory::RunPatch { attempt: trailhead_factory::Patch::Set(9), ..Default::default() };
    assert!(r.tracker.report(&key, update).await.is_err());
}

#[tokio::test]
async fn sandbox_tokens_are_scoped_and_die_with_the_sub_agent() {
    let r = rig().await;
    r.store
        .create_project(&r.human, NewProject { key: "OTHER".into(), name: "Other".into(), ..Default::default() })
        .await
        .unwrap();
    let other = r
        .store
        .create_task(&r.human, NewTask { project: "OTHER".into(), title: "x".into(), ..Default::default() })
        .await
        .unwrap()
        .key;
    let cred = r.tracker.create_sub_agent("trail-1-a1", "TRAIL", std::time::Duration::from_secs(3600)).await.unwrap();
    let sandbox = CoreTracker::authenticate(r.store.clone(), &cred).await.unwrap();
    assert!(sandbox.show(&other).await.is_err(), "scoped to TRAIL");

    r.tracker.revoke_sub_agent("trail-1-a1").await.unwrap();
    assert!(CoreTracker::authenticate(r.store.clone(), &cred).await.is_err());
}

#[tokio::test]
async fn reconcile_reaches_a_factory_task_past_the_first_page_of_tasks() {
    let r = rig_paged(CONFIG, Some(2)).await;
    for i in 0..5 {
        r.task(&format!("Filler {i}")).await;
    }
    let key = r.task("Crashed, far down the list").await;
    r.tracker.create_sub_agent("trail-6-a1", "TRAIL", std::time::Duration::from_secs(3600)).await.unwrap();
    let sub = r.tracker.sub_agent("trail-6-a1");
    sub.claim(&key).await.unwrap();
    sub.report(
        &key,
        RunPatch { attempt: Patch::Set(1), sandbox: Patch::Set("trail-6-a1".into()), ..RunPatch::default() },
    )
    .await
    .unwrap();

    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.requeued, std::slice::from_ref(&key.0));
}

#[tokio::test]
async fn reconcile_requeues_an_interrupted_run() {
    let r = rig().await;
    let key = r.task("Crashed").await;
    r.tracker.create_sub_agent("trail-1-a1", "TRAIL", std::time::Duration::from_secs(3600)).await.unwrap();
    let sub = r.tracker.sub_agent("trail-1-a1");
    sub.claim(&key).await.unwrap();
    sub.report(
        &key,
        RunPatch { attempt: Patch::Set(1), sandbox: Patch::Set("trail-1-a1".into()), ..RunPatch::default() },
    )
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

#[tokio::test]
async fn a_task_with_malformed_factory_metadata_is_skipped_not_fatal() {
    let r = rig().await;
    let good = r.task("Good").await;
    let bad = r.task("Bad").await;
    let patch = TaskPatch {
        metadata: Some(serde_json::json!({ "factory": { "attempt_base": "3x" } })),
        ..TaskPatch::default()
    };
    r.store.update_task(&r.human, &bad.0, patch, None).await.unwrap();

    let ready = r.tracker.ready("TRAIL", &[], 10).await.unwrap();
    assert_eq!(ready.iter().map(|t| &t.key).collect::<Vec<_>>(), [&good]);
    assert!(r.tracker.in_flight("TRAIL", &[]).await.unwrap().is_empty());
    r.factory.reconcile().await.unwrap();
    assert!(r.tracker.show(&bad).await.is_err());

    // The task says why it was skipped, once however many passes see it.
    r.tracker.ready("TRAIL", &[], 10).await.unwrap();
    r.tracker.in_flight("TRAIL", &[]).await.unwrap();
    let notes: Vec<_> = r.store.list_comments(&r.human, &bad.0).await.unwrap();
    assert_eq!(notes.len(), 1);
    assert!(notes[0].body.contains("attempt_base"), "{}", notes[0].body);
    assert!(!notes[0].body.contains("bad factory metadata"), "{}", notes[0].body);
    assert!(!notes[0].body.contains(&bad.0), "{}", notes[0].body);
}

#[tokio::test]
async fn a_task_with_non_object_factory_metadata_is_skipped_without_naming_a_field() {
    let r = rig().await;
    let good = r.task("Good").await;
    let mut bad = Vec::new();
    for (title, value) in [("String", serde_json::json!("fast")), ("Array", serde_json::json!(["attempt_base"]))] {
        let task = r.task(title).await;
        let patch = TaskPatch { metadata: Some(serde_json::json!({ "factory": value })), ..TaskPatch::default() };
        r.store.update_task(&r.human, &task.0, patch, None).await.unwrap();
        bad.push(task);
    }

    let ready = r.tracker.ready("TRAIL", &[], 10).await.unwrap();
    assert_eq!(ready.iter().map(|t| &t.key).collect::<Vec<_>>(), [&good]);

    for task in &bad {
        let notes = r.store.list_comments(&r.human, &task.0).await.unwrap();
        assert_eq!(notes.len(), 1, "{task:?}");
        assert!(notes[0].body.contains("fix or remove the `factory` metadata"), "{}", notes[0].body);
        assert!(!notes[0].body.contains("fix or remove that field"), "{}", notes[0].body);
        assert!(!notes[0].body.contains("malformed in "), "{}", notes[0].body);
    }
}

#[tokio::test]
async fn with_a_queue_only_its_subtasks_run() {
    let mut r = rig().await;
    let urgent = NewTask {
        project: "TRAIL".into(),
        title: "An epic, not for the factory".into(),
        priority: Some(pr(1)),
        ..Default::default()
    };
    let epic = r.store.create_task(&r.human, urgent).await.unwrap().key;
    let queue = r.task("Factory queue").await;
    let sub = NewTask {
        project: "TRAIL".into(),
        title: "Add a greeting".into(),
        parent: Some(queue.clone()),
        grade: Some(gr(1)),
        ..Default::default()
    };
    let handed = r.store.create_task(&r.human, sub).await.unwrap().key;
    r.factory.config.repo.queue = vec![queue.0.clone()];

    let ready = r.tracker.ready("TRAIL", std::slice::from_ref(&queue), 10).await.unwrap();
    assert_eq!(ready.iter().map(|t| &t.key).collect::<Vec<_>>(), [&handed]);

    r.harness.push(Role::Worker, Script::new(RunOutcome::NeedsInput { question: "Which greeting?".into() }));
    let ran = r.factory.tick().await.unwrap();
    assert_eq!(ran.iter().map(|(t, _)| &t.key).collect::<Vec<_>>(), [&handed]);
    assert_eq!(r.show(&epic).await.status, Status::Open, "the epic is left alone");
    assert_eq!(r.show(&queue).await.status, Status::Open, "and so is the queue itself");
}

#[tokio::test]
async fn a_queue_list_reads_each_parents_direct_children_ordered_by_priority_and_once() {
    let mut r = rig().await;
    let q1 = r.task("Queue one").await;
    let q2 = r.task("Queue two").await;
    let nested = r.task("Nested parent").await;
    let mk = |title: &str, parent: &TaskKey, priority: u8, grade: Option<u8>| NewTask {
        project: "TRAIL".into(),
        title: title.into(),
        parent: Some(parent.clone()),
        priority: Some(pr(priority)),
        grade: grade.map(gr),
        ..Default::default()
    };
    let low = r.store.create_task(&r.human, mk("low", &q1, 4, Some(1))).await.unwrap().key;
    let high = r.store.create_task(&r.human, mk("high", &q2, 1, Some(1))).await.unwrap().key;
    let ungraded = r.store.create_task(&r.human, mk("ungraded", &q2, 1, None)).await.unwrap().key;
    let deep = r.store.create_task(&r.human, mk("deep", &nested, 1, Some(1))).await.unwrap().key;
    let _grandchild = r.store.create_task(&r.human, mk("grandchild", &deep, 1, Some(1))).await.unwrap();
    r.factory.config.repo.queue = vec![q1.0.clone(), q2.0.clone(), q2.0.clone()];

    let ready = r.tracker.ready("TRAIL", &[q1.clone(), q2.clone(), q2.clone()], 10).await.unwrap();
    let keys: Vec<_> = ready.iter().map(|t| &t.key).collect();
    assert_eq!(keys.iter().filter(|k| ***k == high).count(), 1, "a task listed twice is read once");
    assert!(!keys.contains(&&deep), "a queue reaches direct children only");
    assert_eq!(keys.last(), Some(&&low), "priority orders across queues");

    r.factory.config.repo.max_sandboxes = 1;
    r.harness.push(Role::Worker, Script::new(RunOutcome::NeedsInput { question: "Which?".into() }));
    let ran = r.factory.tick().await.unwrap();
    assert_eq!(ran.iter().map(|(t, _)| &t.key).collect::<Vec<_>>(), [&high], "the best graded task, under the cap");
    assert_eq!(r.show(&ungraded).await.status, Status::Open, "an ungraded one waits for a grade");
}
#[tokio::test]
async fn the_factory_can_raise_a_grade_a_human_set_but_not_lower_it() {
    let r = rig().await;
    let new = NewTask { project: "TRAIL".into(), title: "Graded".into(), grade: Some(gr(2)), ..Default::default() };
    let key = r.store.create_task(&r.human, new).await.unwrap().key;
    assert_eq!(r.show(&key).await.grade, Some(gr(2)));
    r.tracker.raise_grade(&key, gr(3)).await.unwrap();
    assert_eq!(r.show(&key).await.grade, Some(gr(3)));
    assert!(r.tracker.raise_grade(&key, gr(1)).await.is_err());
    let ungraded = r.task("Ungraded").await;
    assert!(r.tracker.raise_grade(&ungraded, gr(2)).await.is_err(), "the first grade is a human's");
}

#[tokio::test]
async fn human_context_pairs_answers_and_skips_the_factorys_own_notes() {
    let r = rig().await;
    let key = r.task("Ambiguous").await;
    let decision = trailhead_core::NewComment::new(CommentKind::Decision, "Use a flock.");
    r.store.add_comment(&r.human, &key.0, decision).await.unwrap();
    r.harness.push(Role::Worker, Script::new(RunOutcome::NeedsInput { question: "Which crate?".into() }));
    r.factory.tick().await.unwrap();
    r.tracker.note(&key, "bookkeeping").await.unwrap();

    let question = r.store.list_comments(&r.human, &key.0).await.unwrap();
    let question = question.iter().find(|c| c.kind == CommentKind::Question).unwrap();
    r.store.resolve_comment(&r.human, question.id, Some("The core crate.".into())).await.unwrap();

    let ctx = r.tracker.human_context(&key).await.unwrap();
    assert_eq!(ctx.answers.len(), 1);
    assert_eq!((ctx.answers[0].question.as_str(), ctx.answers[0].answer.as_str()), ("Which crate?", "The core crate."));
    assert_eq!(ctx.answers[0].by, "Lane");
    assert_eq!(ctx.notes.len(), 1, "{:?}", ctx.notes);
    assert_eq!((ctx.notes[0].author.as_str(), ctx.notes[0].body.as_str()), ("Lane", "Use a flock."));
}

#[tokio::test]
async fn last_review_names_its_reviewer_like_human_context_does() {
    let r = rig().await;
    let key = r.task("Reviewed by hand").await;
    assert!(r.tracker.last_review(&key).await.unwrap().is_none());
    let review = trailhead_core::NewComment::review(trailhead_core::Verdict::RequestChanges, "Add a test.");
    r.store.add_comment(&r.human, &key.0, review).await.unwrap();
    let verdict = r.tracker.last_review(&key).await.unwrap().unwrap();
    assert_eq!((verdict.reviewer.as_str(), verdict.body.as_str()), ("Lane", "Add a test."));
}

fn request_changes(body: &str) -> RunOutcome {
    RunOutcome::Verdict { kind: VerdictKind::RequestChanges, body: body.into(), follow_ups: vec![], raise_grade: None }
}

#[tokio::test]
async fn requested_changes_stay_in_review_then_rework_on_the_real_engine() {
    let r = rig().await;
    let key = r.task("Add a greeting").await;
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(request_changes("Add a test.")));
    r.factory.tick().await.unwrap();
    let task = r.show(&key).await;
    assert_eq!(task.status, Status::InReview, "a rework waits in review, not in the backlog");
    assert!(task.changes_requested, "the reviewer's verdict is the newest review comment, with no handoff after it");
    assert_eq!(task.run.reviewed, Some(1));

    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(
        Role::Reviewer,
        Script::new(RunOutcome::Verdict {
            kind: VerdictKind::Approve,
            body: "LGTM".into(),
            follow_ups: vec![],
            raise_grade: None,
        }),
    );
    let ran = r.factory.tick().await.unwrap();
    assert!(
        matches!(&ran[0].1, Outcome::Reviewed { verdict, .. } if verdict.kind == VerdictKind::Approve),
        "{:?}",
        ran[0].1
    );
    assert_eq!(r.show(&key).await.status, Status::InReview);
    assert_eq!(r.codehost.prs().len(), 1);
}

#[tokio::test]
async fn requested_changes_past_the_cap_park_on_the_real_engine() {
    let r = rig_with(&CONFIG.replace("name = \"trailhead\"", "name = \"trailhead\"\nmax_review_rounds = 1")).await;
    let key = r.task("Add a greeting").await;
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(request_changes("Still wrong.")));
    r.factory.tick().await.unwrap();

    // Parking reopens the task, then claims it and asks as the control plane's own identity.
    let task = r.show(&key).await;
    assert_eq!(task.status, Status::NeedsInput);
    assert_eq!(task.holder.as_deref(), Some("factory"));
    let comments = r.store.list_comments(&r.human, &key.0).await.unwrap();
    let question = comments.iter().find(|c| c.kind == CommentKind::Question).expect("a question for Lane");
    assert!(question.body.contains("after 1 review rounds") && question.body.contains("Still wrong."));
}

/// Runs a task to a reviewed PR, leaving it in review. Lane has added the factory to the project's reviewers, as the
/// merger needs, so it can finish a task whose PR merged.
async fn in_review(r: &Rig, title: &str) -> (TaskKey, trailhead_factory::PrRef) {
    let reviewers = ProjectPatch { reviewers: Some(vec!["factory".into()]), ..Default::default() };
    r.store.update_project(&r.human, "TRAIL", reviewers, None).await.unwrap();
    let key = r.task(title).await;
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(
        Role::Reviewer,
        Script::new(RunOutcome::Verdict {
            kind: VerdictKind::Approve,
            body: "LGTM".into(),
            follow_ups: vec![],
            raise_grade: None,
        }),
    );
    r.factory.tick().await.unwrap();
    let task = r.show(&key).await;
    assert_eq!(task.status, Status::InReview);
    (key, task.run.pr.unwrap())
}

#[tokio::test]
async fn a_merged_pr_finishes_its_task_and_unblocks_what_is_chained_behind_it() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    let before = r.factory.reconcile().await.unwrap();
    assert!(before.merged.is_empty(), "an open PR leaves the task in review");
    assert_eq!(r.show(&key).await.status, Status::InReview);

    let next = r.task("Use the greeting").await;
    r.store.add_dependency(&r.human, &key.0, &next.0).await.unwrap();
    let blocked = r.store.blockers(&r.human, &next.0).await.unwrap();
    assert_eq!(blocked.len(), 1);

    let head = CodeHost::head(&r.codehost, &pr).await.unwrap();
    r.codehost.merge(&pr, &head).await.unwrap();
    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.merged, std::slice::from_ref(&key.0));
    let task = r.show(&key).await;
    assert_eq!(task.status, Status::Done);
    let merged = task.run.merged.expect("who merged is recorded");
    assert_eq!(merged.by, trailhead_factory::MergedBy::Human);
    assert_eq!(merged.commit.as_deref(), Some(FakeCodeHost::merge_commit(&pr).as_str()));
    let comments = r.store.list_comments(&r.human, &key.0).await.unwrap();
    let handoff = comments.iter().rev().find(|c| c.kind == CommentKind::Handoff).expect("a handoff");
    assert!(handoff.body.contains(&pr.url) && handoff.body.contains("merge-1"), "{}", handoff.body);
    assert!(r.store.blockers(&r.human, &next.0).await.unwrap().is_empty(), "the chained task is unblocked");

    assert!(r.factory.reconcile().await.unwrap().merged.is_empty(), "done tasks are left alone");
}

#[tokio::test]
async fn a_parked_task_whose_pr_is_merged_is_finished_and_unblocks_what_is_chained_behind_it() {
    let r = rig_with(&CONFIG.replace("name = \"trailhead\"", "name = \"trailhead\"\nmax_conflict_reworks = 1")).await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    r.codehost.set_mergeable(&pr, Some(false));
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.show(&key).await.status, Status::NeedsInput);
    let next = r.task("Use the greeting").await;
    r.store.add_dependency(&r.human, &key.0, &next.0).await.unwrap();

    let done = r.factory.reconcile().await.unwrap();
    assert!(done.merged.is_empty() && done.closed.is_empty(), "an open PR leaves the task parked");
    assert_eq!(r.show(&key).await.status, Status::NeedsInput);

    let head = CodeHost::head(&r.codehost, &pr).await.unwrap();
    r.codehost.merge(&pr, &head).await.unwrap();
    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.merged, std::slice::from_ref(&key.0));
    let task = r.show(&key).await;
    assert_eq!(task.status, Status::Done);
    assert_eq!(task.run.merged.expect("merge recorded").by, trailhead_factory::MergedBy::Human);
    assert!(r.store.blockers(&r.human, &next.0).await.unwrap().is_empty(), "the chained task is unblocked");
}

#[tokio::test]
async fn a_parked_task_whose_pr_was_closed_is_not_asked_about_again() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    r.codehost.close(&pr);
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.show(&key).await.status, Status::NeedsInput);
    let done = r.factory.reconcile().await.unwrap();
    assert!(done.closed.is_empty() && done.merged.is_empty());
    let comments = r.store.list_comments(&r.human, &key.0).await.unwrap();
    assert_eq!(comments.iter().filter(|c| c.kind == CommentKind::Question).count(), 1);
}

#[tokio::test]
async fn a_task_waiting_in_review_for_a_rework_whose_pr_is_merged_is_finished() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    r.codehost.set_mergeable(&pr, Some(false));
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.show(&key).await.status, Status::InReview);
    let next = r.task("Use the greeting").await;
    r.store.add_dependency(&r.human, &key.0, &next.0).await.unwrap();

    let head = CodeHost::head(&r.codehost, &pr).await.unwrap();
    r.codehost.merge(&pr, &head).await.unwrap();
    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.merged, std::slice::from_ref(&key.0));
    assert_eq!(r.show(&key).await.status, Status::Done);
    assert!(r.store.blockers(&r.human, &next.0).await.unwrap().is_empty(), "the chained task is unblocked");
}

#[tokio::test]
async fn a_pr_closed_without_merging_parks_its_task_for_lane() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    r.codehost.close(&pr);
    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.closed, std::slice::from_ref(&key.0));
    assert!(done.merged.is_empty());
    let task = r.show(&key).await;
    assert_eq!(task.status, Status::NeedsInput);
    assert_eq!(task.run.merged, None);
    let comments = r.store.list_comments(&r.human, &key.0).await.unwrap();
    let question = comments.iter().find(|c| c.kind == CommentKind::Question).expect("a question for Lane");
    assert_eq!(question.body, trailhead_factory::messages::pr_closed_question(&pr.url));
    assert!(!question.body.contains("  "), "no stray runs of spaces: {}", question.body);
    assert!(r.factory.reconcile().await.unwrap().closed.is_empty(), "asked once");
}

fn approve() -> RunOutcome {
    RunOutcome::Verdict { kind: VerdictKind::Approve, body: "LGTM".into(), follow_ups: vec![], raise_grade: None }
}

#[tokio::test]
async fn a_pr_that_no_longer_merges_cleanly_is_reworked_with_the_base_merged_in() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;

    r.codehost.set_mergeable(&pr, None);
    assert!(r.factory.reconcile().await.unwrap().conflicted.is_empty(), "no answer yet: ask again later");
    assert_eq!(r.show(&key).await.status, Status::InReview);

    r.codehost.set_mergeable(&pr, Some(false));
    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.conflicted, std::slice::from_ref(&key.0));
    let task = r.show(&key).await;
    assert_eq!((task.status, task.changes_requested), (Status::InReview, true), "waiting in review for a rework");
    assert_eq!(task.run.review_rounds, Some(1), "a conflict leaves the review rounds alone");
    assert_eq!(task.run.conflict_reworks, Some(1));
    let wanted = task.run.changes_requested.expect("the rework is told why");
    assert!(wanted.kind == trailhead_factory::RequestKind::Conflict && wanted.body.contains(&pr.url), "{wanted:?}");

    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    let Outcome::Reviewed { pr: again, .. } = &ran[0].1 else { panic!("{:?}", ran[0].1) };
    assert_eq!(again, &pr, "pushed to the same PR");

    let prepared = r.daemon.prepared();
    let rework: Vec<_> =
        prepared.iter().filter(|(id, p)| p.branch.is_some() && id.0.starts_with("trailhead-w")).collect();
    assert_eq!(rework.len(), 1);
    assert!(rework[0].1.merge_base, "the checkout has the base branch merged in");
    let commits = r.codehost.commits(&pr.repo, "factory/trail-1");
    assert!(commits[0].merges.is_empty());
    assert_eq!(commits.last().unwrap().merges, [FAKE_BASE_MERGE], "published as a merge commit");
    let task = r.show(&key).await;
    assert_eq!(
        (task.status, task.changes_requested),
        (Status::InReview, false),
        "reviewed again, the handoff answering the request"
    );
    assert_eq!(task.run.review_rounds, Some(1), "the re-review after a conflict rework uses no round");
    assert!(r.factory.reconcile().await.unwrap().conflicted.is_empty());
}

#[tokio::test]
async fn a_conflict_the_host_no_longer_reports_goes_back_to_review_without_a_worker() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;

    r.codehost.set_mergeable(&pr, Some(false));
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.show(&key).await.status, Status::InReview);

    // The base moved back, or the conflict was fixed, before the rework started.
    r.codehost.set_mergeable(&pr, Some(true));
    let sandboxes = r.daemon.prepared().len();
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(ran[0].1, Outcome::Settled { .. }), "{:?}", ran[0].1);
    assert_eq!(r.daemon.prepared().len(), sandboxes, "no sandbox for a rework with nothing to resolve");
    let task = r.show(&key).await;
    assert_eq!(task.status, Status::InReview);
    assert_eq!(task.run.attempt, Some(1), "no attempt used up");
    assert_eq!(task.run.conflict_reworks, Some(1), "the count of reworks that ran isn't reset");
    assert!(task.run.changes_requested.is_some_and(|c| c.settled), "the request is marked settled instead");

    // Reconcile doesn't take the dropped request up again.
    let done = r.factory.reconcile().await.unwrap();
    assert!(done.reworked.is_empty() && done.conflicted.is_empty(), "{done:?}");
    let task = r.show(&key).await;
    assert_ne!(task.status, Status::Open);
    assert_eq!(task.run.conflict_reworks, Some(1));
}

/// A conflict that clears before its rework starts must leave the PR where it was: reviewed and approved on this
/// head. The gate reads the reviewer's verdict, not the factory's own conflict request, and the factory hasn't
/// become the one who did the work, so it can still approve (TRAIL-1468).
#[tokio::test]
async fn a_conflict_that_clears_leaves_the_reviewers_approval_standing_and_the_factory_able_to_approve() {
    let config = format!(
        "{CONFIG}\n[gates.1]\nrequire = [\"checks\", \"ci\", \"agent_review\"]\nauto_merge = true\n\n[merge]\nmode = \"on\"\n"
    );
    let r = rig_with(&config).await;
    let reviewers = ProjectPatch { reviewers: Some(vec!["factory".into()]), ..Default::default() };
    r.store.update_project(&r.human, "TRAIL", reviewers, None).await.unwrap();
    let new =
        NewTask { project: "TRAIL".into(), title: "Add a greeting".into(), grade: Some(gr(1)), ..Default::default() };
    let key = r.store.create_task(&r.human, new).await.unwrap().key;
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();
    let pr = r.show(&key).await.run.pr.unwrap();

    r.codehost.set_mergeable(&pr, Some(false));
    r.factory.reconcile().await.unwrap();
    r.codehost.set_mergeable(&pr, Some(true));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(ran[0].1, Outcome::Settled { .. }), "{:?}", ran[0].1);
    let task = r.show(&key).await;
    assert_eq!((task.status, task.holder, task.changes_requested), (Status::InReview, None, false));

    r.codehost.set_ci(&pr, trailhead_factory::CiState::Passed);
    r.factory.reconcile().await.unwrap();
    let task = r.show(&key).await;
    assert_eq!(task.status, Status::Done, "merged on the review that stood: {:?}", task.run.merge);
    assert!(r.codehost.prs()[0].merged);
    assert_eq!(task.run.merged.expect("recorded").by, trailhead_factory::MergedBy::Merger);
}

/// A person who takes the task out of review while a conflict request stands gets the run they asked for once the
/// conflict clears: the request is dropped, and nothing sends the task back to review in the factory's name.
#[tokio::test]
async fn a_conflict_that_clears_on_a_task_a_person_reopened_is_dropped_and_the_task_runs() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    r.codehost.set_mergeable(&pr, Some(false));
    r.factory.reconcile().await.unwrap();
    let back = TaskPatch { status: Some(trailhead_core::Status::Open), ..TaskPatch::default() };
    r.store.update_task(&r.human, &key.0, back, None).await.unwrap();

    r.codehost.set_mergeable(&pr, Some(true));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(ran[0].1, Outcome::Settled { .. }), "{:?}", ran[0].1);
    let task = r.show(&key).await;
    assert_eq!((task.status, task.holder), (Status::Open, None), "left for the next tick, not sent to review");
    assert!(task.run.pending_changes().is_none(), "the conflict request is dropped");

    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(ran[0].1, Outcome::Reviewed { .. }), "{:?}", ran[0].1);
    assert_eq!(r.show(&key).await.run.attempt, Some(2));
}

/// A task whose worker pushed a PR and whose review failed before it gave a verdict: in review, with the review owed.
async fn awaiting_review(r: &Rig, title: &str) -> (TaskKey, trailhead_factory::PrRef) {
    let reviewers = ProjectPatch { reviewers: Some(vec!["factory".into()]), ..Default::default() };
    r.store.update_project(&r.human, "TRAIL", reviewers, None).await.unwrap();
    let key = r.task(title).await;
    // No reviewer script: the review ends without a verdict and backs off like an infrastructure failure.
    r.harness.push(Role::Worker, Script::new(changes()));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(ran[0].1, Outcome::AwaitingReview { .. }), "{:?}", ran[0].1);
    let task = r.show(&key).await;
    assert_eq!((task.status, task.run.reviewed), (Status::InReview, None));
    (key, task.run.pr.unwrap())
}

/// Only a review marks an attempt reviewed. A conflict request on a PR whose review is still owed, dropped because
/// the conflict cleared, must leave the review owed, or the merger waits on it forever (TRAIL-1470).
#[tokio::test(start_paused = true)]
async fn a_conflict_request_that_clears_leaves_an_owed_review_owed() {
    let r = rig().await;
    let (key, pr) = awaiting_review(&r, "Add a greeting").await;

    r.codehost.set_mergeable(&pr, Some(false));
    assert_eq!(r.factory.reconcile().await.unwrap().conflicted, std::slice::from_ref(&key.0));
    r.codehost.set_mergeable(&pr, Some(true));
    // Past the failed review's backoff, which holds the task's next run back too.
    tokio::time::advance(std::time::Duration::from_secs(3600)).await;
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(ran[0].1, Outcome::Settled { .. }), "{:?}", ran[0].1);
    assert_eq!(r.show(&key).await.run.reviewed, None, "no review has read the PR");

    r.harness.push(Role::Reviewer, Script::new(approve()));
    assert_eq!(r.factory.reconcile().await.unwrap().reviewed, std::slice::from_ref(&key.0));
    assert_eq!(r.show(&key).await.run.reviewed, Some(1));
}

/// A request reconcile takes up from a verdict alone records the head it was on as the reviewed head. Without it, a
/// person's push afterwards answers the request and nothing ever sees the new head as owed a review (TRAIL-1470).
#[tokio::test(start_paused = true)]
async fn a_verdict_reconcile_takes_up_records_its_head_so_a_push_since_is_reviewed() {
    let r = rig().await;
    let (key, pr) = awaiting_review(&r, "Add a greeting").await;
    let head = CodeHost::head(&r.codehost, &pr).await.unwrap();

    reviewer_requests_changes(&r, &key, "Add a test.").await;
    assert_eq!(r.factory.reconcile().await.unwrap().reworked, std::slice::from_ref(&key.0));
    assert_eq!(r.show(&key).await.run.reviewed_head, Some(head));

    // A person pushes the fix themselves: the request no longer applies, and the new head wants a review.
    r.codehost.push_to(&pr);
    r.factory.reconcile().await.unwrap();
    assert!(!r.show(&key).await.changes_requested, "the stale request was answered");
    tokio::time::advance(std::time::Duration::from_secs(3600)).await;
    r.harness.push(Role::Reviewer, Script::new(approve()));
    assert_eq!(r.factory.reconcile().await.unwrap().reviewed, std::slice::from_ref(&key.0));
    let task = r.show(&key).await;
    assert_eq!(task.run.reviewed_head, Some(CodeHost::head(&r.codehost, &pr).await.unwrap()));
}

async fn approve_as_lane(r: &Rig, key: &TaskKey) {
    let approved = TaskPatch { status: Some(trailhead_core::Status::Approved), ..TaskPatch::default() };
    r.store.update_task(&r.human, &key.0, approved, None).await.unwrap();
}

/// A person approves while a review's request for changes stands. Only a task in review is listed for a rework, and
/// the gate still wants the review to approve, so the task would sit approved forever: the request outranks the
/// approval and the task goes back to review (TRAIL-1469).
#[tokio::test]
async fn an_approval_over_a_request_for_changes_goes_back_to_review_for_the_rework() {
    let r = rig().await;
    let reviewers = ProjectPatch { reviewers: Some(vec!["factory".into()]), ..Default::default() };
    r.store.update_project(&r.human, "TRAIL", reviewers, None).await.unwrap();
    let key = r.task("Add a greeting").await;
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(request_changes("Add a test.")));
    r.factory.tick().await.unwrap();
    approve_as_lane(&r, &key).await;

    r.factory.reconcile().await.unwrap();
    let task = r.show(&key).await;
    assert_eq!((task.status, task.changes_requested), (Status::InReview, true));

    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(ran[0].1, Outcome::Reviewed { .. }), "{:?}", ran[0].1);
    assert_eq!(r.show(&key).await.run.attempt, Some(2));
}

/// The request needn't be one the factory recorded: a person's own, left just before they approved, counts the
/// same. And a conflict found on the same pass waits its turn: its request would replace this one, and clearing
/// it would answer both (TRAIL-1469).
#[tokio::test]
async fn an_approval_over_a_persons_request_goes_back_to_review_before_any_conflict_is_looked_at() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    reviewer_requests_changes(&r, &key, "Add a test.").await;
    approve_as_lane(&r, &key).await;
    r.codehost.set_mergeable(&pr, Some(false));

    let done = r.factory.reconcile().await.unwrap();
    assert!(done.conflicted.is_empty(), "{done:?}");
    let task = r.show(&key).await;
    assert_eq!((task.status, task.changes_requested), (Status::InReview, true));

    // In review again, the verdict is taken up as the request.
    assert_eq!(r.factory.reconcile().await.unwrap().reworked, std::slice::from_ref(&key.0));
    let asked = r.show(&key).await.run.pending_changes().cloned().expect("a request");
    assert_eq!((asked.kind, asked.body.as_str()), (trailhead_factory::RequestKind::Review, "Add a test."));
}

/// Whoever merged, a finish core refuses parks the task once for a person to mark it done. A merge by the merger
/// used to be retried, and refused, on every pass (TRAIL-1462).
#[tokio::test]
async fn a_merge_by_the_merger_whose_finish_is_refused_parks_the_task_once() {
    let config = format!("{CONFIG}\n[merge]\nmode = \"on\"\n");
    let r = rig_with(&config).await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    // The factory itself takes the task and sends it to review again, so in core's eyes it did the work.
    let back = TaskPatch { status: Some(trailhead_core::Status::Open), ..TaskPatch::default() };
    r.store.update_task(&r.human, &key.0, back, None).await.unwrap();
    r.tracker.claim(&key).await.unwrap();
    r.tracker.request_review(&key, "pushed").await.unwrap();
    approve_as_lane(&r, &key).await;
    r.codehost.set_ci(&pr, trailhead_factory::CiState::Passed);

    r.factory.reconcile().await.unwrap();
    assert!(r.codehost.prs()[0].merged, "Lane's approval merges it");
    let task = r.show(&key).await;
    assert_eq!(task.status, Status::NeedsInput, "{:?}", task.run.merge);
    assert_eq!(task.run.merged.as_ref().expect("recorded").by, trailhead_factory::MergedBy::Merger);
    let comments = r.store.list_comments(&r.human, &key.0).await.unwrap();
    let question = comments.iter().rev().find(|c| c.kind == CommentKind::Question).expect("a question");
    assert!(question.body.contains("The factory merged the PR") && question.body.contains("Mark it done"));

    let version = r.store.get_task(&r.human, &key.0).await.unwrap().version;
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.store.get_task(&r.human, &key.0).await.unwrap().version, version, "asked once, then left alone");
    assert_eq!(r.store.list_comments(&r.human, &key.0).await.unwrap().len(), comments.len());
}

/// A person approves before the attempt's review has run. Reviews were only started for tasks in review, so the
/// merger waited on one forever. It runs, and the approval stays, so an approving review merges (TRAIL-1469).
#[tokio::test(start_paused = true)]
async fn a_review_still_owed_when_a_person_approves_runs_and_the_approval_stays() {
    let config = format!("{CONFIG}\n[merge]\nmode = \"on\"\n");
    let r = rig_with(&config).await;
    let (key, pr) = awaiting_review(&r, "Add a greeting").await;
    let graded = TaskPatch { grade: Some(Some(gr(3))), ..TaskPatch::default() };
    r.store.update_task(&r.human, &key.0, graded, None).await.unwrap();
    approve_as_lane(&r, &key).await;
    r.codehost.set_ci(&pr, trailhead_factory::CiState::Passed);

    tokio::time::advance(std::time::Duration::from_secs(3600)).await;
    r.harness.push(Role::Reviewer, Script::new(approve()));
    assert_eq!(r.factory.reconcile().await.unwrap().reviewed, std::slice::from_ref(&key.0));
    r.factory.reconcile().await.unwrap();
    let task = r.show(&key).await;
    assert_eq!(task.status, Status::Done, "{:?}", task.run.merge);
    assert!(r.codehost.prs()[0].merged, "merged on Lane's approval once the review approved");
}

/// A person pushes to a reviewed PR and then approves it, before the factory has seen the push. The approval is of
/// the new head, which no review has read, so the review is owed while the task is approved (TRAIL-1469).
#[tokio::test]
async fn a_head_a_person_pushed_and_then_approved_is_still_reviewed() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    r.codehost.push_to(&pr);
    approve_as_lane(&r, &key).await;

    r.harness.push(Role::Reviewer, Script::new(approve()));
    assert_eq!(r.factory.reconcile().await.unwrap().reviewed, std::slice::from_ref(&key.0));
    let task = r.show(&key).await;
    assert_eq!(task.status, Status::Approved, "Lane's approval stays");
    assert_eq!(task.run.reviewed_head, Some(CodeHost::head(&r.codehost, &pr).await.unwrap()));
}

/// TRAIL-1418: a settled conflict request isn't a live conflict rework, so a reopened task isn't dropped again
/// by the check that sends a rework back to review when the host reports no conflict.
#[tokio::test]
async fn a_reopened_task_with_a_settled_conflict_request_is_not_dropped_as_a_gone_conflict() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    r.codehost.set_mergeable(&pr, Some(false));
    r.factory.reconcile().await.unwrap();
    r.codehost.set_mergeable(&pr, Some(true));
    r.factory.tick().await.unwrap();
    assert!(r.show(&key).await.run.changes_requested.is_some_and(|c| c.settled));

    r.tracker.reopen(&key).await.unwrap();
    let sandboxes = r.daemon.prepared().len();
    r.harness.push(Role::Worker, Script::new(changes()));
    let ran = r.factory.tick().await.unwrap();
    assert!(!matches!(ran[0].1, Outcome::Settled { .. }), "{:?}", ran[0].1);
    assert!(r.daemon.prepared().len() > sandboxes, "a worker ran");
}

/// A reviewer's request for changes that lands after the conflict request but before it is recorded on the task
/// (TRAIL-1418): settling the dropped conflict would post a handoff that answers the verdict.
async fn reviewer_requests_changes(r: &Rig, key: &TaskKey, body: &str) {
    let cred =
        r.tracker.create_sub_agent("person-review", "TRAIL", std::time::Duration::from_secs(3600)).await.unwrap();
    let reviewer = CoreTracker::authenticate(r.store.clone(), &cred).await.unwrap();
    let verdict = Verdict {
        kind: VerdictKind::RequestChanges,
        body: body.into(),
        reviewer: String::new(),
        model: None,
        raise_grade: None,
    };
    reviewer.record_verdict(key, &verdict).await.unwrap();
}

#[tokio::test]
async fn a_review_that_asks_for_changes_before_it_is_recorded_survives_a_dropped_conflict_rework() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    r.codehost.set_mergeable(&pr, Some(false));
    r.factory.reconcile().await.unwrap();
    assert!(
        r.show(&key).await.run.pending_changes().is_some_and(|c| c.kind == trailhead_factory::RequestKind::Conflict)
    );

    // The verdict is the newest review comment, but `changes_requested` still holds the conflict request.
    reviewer_requests_changes(&r, &key, "The diff against main is empty.").await;
    r.codehost.set_mergeable(&pr, Some(true));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(ran[0].1, Outcome::Settled { .. }), "{:?}", ran[0].1);
    let task = r.show(&key).await;
    assert!(task.changes_requested, "still waiting on a rework: no handoff answered the verdict");
    assert!(task.run.pending_changes().is_some_and(|c| !c.settled), "the request wasn't settled");

    // Reconcile takes the verdict up as the request, so the rework starts.
    r.factory.reconcile().await.unwrap();
    let task = r.show(&key).await;
    let asked = task.run.pending_changes().expect("a request");
    assert_eq!(asked.kind, trailhead_factory::RequestKind::Review);
    assert_eq!(asked.body, "The diff against main is empty.");
    assert!(task.changes_requested);
}

#[tokio::test]
async fn a_review_that_asks_for_changes_after_a_settled_conflict_request_is_taken_up() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    r.codehost.set_mergeable(&pr, Some(false));
    r.factory.reconcile().await.unwrap();
    r.codehost.set_mergeable(&pr, Some(true));
    r.factory.tick().await.unwrap();
    assert!(r.show(&key).await.run.changes_requested.is_some_and(|c| c.settled));

    // A person's verdict after the settled request isn't answered as stale.
    reviewer_requests_changes(&r, &key, "Add a test.").await;
    r.factory.reconcile().await.unwrap();
    let task = r.show(&key).await;
    assert!(task.changes_requested, "the verdict still waits for a rework");
    // Taken up as the request (here it hit the round limit, so it is parked on), not answered with a handoff.
    let asked = task.run.last_changes().expect("a request");
    assert_eq!((asked.kind, asked.body.as_str()), (trailhead_factory::RequestKind::Review, "Add a test."));
}

#[tokio::test(start_paused = true)]
async fn a_conflict_the_host_has_not_worked_out_yet_is_asked_again_before_a_worker_runs() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;

    r.codehost.set_mergeable(&pr, Some(false));
    r.factory.reconcile().await.unwrap();

    // The base moved, and the host is still working `mergeable` out when the rework starts; it settles on clean.
    r.codehost.set_mergeable(&pr, None);
    let (codehost, later) = (r.codehost.clone(), pr.clone());
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        codehost.set_mergeable(&later, Some(true));
    });
    let sandboxes = r.daemon.prepared().len();
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(ran[0].1, Outcome::Settled { .. }), "{:?}", ran[0].1);
    assert_eq!(r.daemon.prepared().len(), sandboxes, "no sandbox for a conflict that went away");
    let task = r.show(&key).await;
    assert_eq!((task.status, task.run.attempt), (Status::InReview, Some(1)));
}

fn reviewers_started(r: &Rig) -> usize {
    r.daemon.requests().iter().filter(|req| req.role == Role::Reviewer).count()
}

#[tokio::test]
async fn a_rework_that_conflicts_with_a_base_that_moved_during_it_is_reworked_again_without_a_review() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    r.codehost.set_mergeable(&pr, Some(false));
    r.factory.reconcile().await.unwrap();
    let reviewed = reviewers_started(&r);

    // The base moves again while the rework runs, so its push conflicts too.
    r.codehost.keep_conflict_on_push(&pr);
    r.harness.push(Role::Worker, Script::new(changes()));
    let ran = r.factory.tick().await.unwrap();
    assert_eq!(ran[0].1, Outcome::Conflicted { pr: pr.clone() });
    assert_eq!(reviewers_started(&r), reviewed, "no reviewer for a PR that already conflicts");
    let task = r.show(&key).await;
    assert_eq!((task.status, task.changes_requested), (Status::InReview, true), "waiting in review for another rework");
    assert_eq!(task.run.conflict_reworks, Some(2));
    assert_eq!(task.run.review_rounds, Some(1));
    let head = CodeHost::head(&r.codehost, &pr).await.unwrap();
    assert_eq!(task.run.changes_requested.and_then(|c| c.head), Some(head), "the request is for the new head");
}

#[tokio::test(start_paused = true)]
async fn a_conflict_reading_for_the_head_from_before_the_push_does_not_skip_the_review() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    let old = CodeHost::head(&r.codehost, &pr).await.unwrap();
    r.codehost.set_mergeable(&pr, Some(false));
    r.factory.reconcile().await.unwrap();

    // The host hasn't caught up with the push: it still shows the old head and its conflict.
    r.codehost.set_lagging_head(&pr, Some(&old));
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(ran[0].1, Outcome::Reviewed { .. }), "{:?}", ran[0].1);
    assert_eq!(r.show(&key).await.run.conflict_reworks, Some(1));
}

#[tokio::test]
async fn a_pr_that_keeps_conflicting_parks_for_lane_at_the_conflict_rework_cap() {
    let r = rig_with(&CONFIG.replace("name = \"trailhead\"", "name = \"trailhead\"\nmax_conflict_reworks = 1")).await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    r.codehost.set_mergeable(&pr, Some(false));
    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.conflicted, std::slice::from_ref(&key.0));
    let task = r.show(&key).await;
    assert_eq!(task.status, Status::NeedsInput);
    let comments = r.store.list_comments(&r.human, &key.0).await.unwrap();
    let question = comments.iter().find(|c| c.kind == CommentKind::Question).expect("a question for Lane");
    assert!(
        question.body.contains("keeps conflicting") && question.body.contains("max_conflict_reworks"),
        "{}",
        question.body
    );
    assert!(r.factory.reconcile().await.unwrap().conflicted.is_empty(), "parked once");
}

/// Conflicts the PR, reworks it and gets it approved again, `times` times in a row.
async fn conflict_and_rework(r: &Rig, key: &TaskKey, pr: &trailhead_factory::PrRef, times: u32) {
    for _ in 0..times {
        r.codehost.set_mergeable(pr, Some(false));
        assert_eq!(r.factory.reconcile().await.unwrap().conflicted, std::slice::from_ref(&key.0));
        assert_eq!(r.show(key).await.status, Status::InReview);
        r.harness.push(Role::Worker, Script::new(changes()));
        r.harness.push(Role::Reviewer, Script::new(approve()));
        r.factory.tick().await.unwrap();
        assert_eq!(r.show(key).await.status, Status::InReview);
        // The host has answered for the pushed head, which settles the stale-head mark.
        assert!(r.factory.reconcile().await.unwrap().conflicted.is_empty());
    }
}

#[tokio::test]
async fn conflicts_never_park_a_pr_on_review_rounds() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    conflict_and_rework(&r, &key, &pr, 4).await;
    let task = r.show(&key).await;
    assert_eq!(task.run.review_rounds, Some(1));
    assert_eq!(task.run.conflict_reworks, Some(4));
}

#[tokio::test]
async fn conflict_reworks_up_to_the_cap_park_the_task() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    conflict_and_rework(&r, &key, &pr, 4).await;
    r.codehost.set_mergeable(&pr, Some(false));
    r.factory.reconcile().await.unwrap();
    let task = r.show(&key).await;
    assert_eq!(task.status, Status::NeedsInput);
    assert_eq!(task.run.conflict_reworks, Some(0), "a fresh count once a human reopens it");
    let comments = r.store.list_comments(&r.human, &key.0).await.unwrap();
    let question = comments.iter().find(|c| c.kind == CommentKind::Question).expect("a question for Lane");
    assert!(question.body.contains("max_conflict_reworks") && question.body.contains('5'), "{}", question.body);
}

#[tokio::test]
async fn a_request_for_changes_after_conflicts_counts_as_one_round() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    r.codehost.set_mergeable(&pr, Some(false));
    r.factory.reconcile().await.unwrap();
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(
        Role::Reviewer,
        Script::new(RunOutcome::Verdict {
            kind: VerdictKind::RequestChanges,
            body: "Add a test.".into(),
            follow_ups: vec![],
            raise_grade: None,
        }),
    );
    r.factory.tick().await.unwrap();
    let task = r.show(&key).await;
    assert_eq!((task.status, task.changes_requested), (Status::InReview, true), "reworked, not parked");
    assert_eq!(task.run.review_rounds, Some(2), "the first review and this one");
    assert_eq!(task.run.conflict_reworks, Some(1), "a review doesn't reset the conflict count");
}

#[tokio::test]
async fn a_merge_the_agent_started_itself_is_published_as_a_merge_commit() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    r.codehost.set_mergeable(&pr, Some(false));
    r.factory.reconcile().await.unwrap();

    // The checkout had no merge under way when prepared; the agent runs `git merge --no-commit` itself.
    r.daemon.agent_merges();
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();

    let commits = r.codehost.commits(&pr.repo, "factory/trail-1");
    assert_eq!(commits.last().unwrap().merges, [FAKE_BASE_MERGE], "published as a merge commit");
    assert_eq!(r.show(&key).await.status, Status::InReview);
}

#[tokio::test]
async fn a_merge_the_agent_committed_itself_is_still_published_as_a_merge_commit() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    r.codehost.set_mergeable(&pr, Some(false));
    r.factory.reconcile().await.unwrap();

    // Prepare left the merge uncommitted; the agent's `git commit` removes MERGE_HEAD, but HEAD now has the base.
    r.daemon.agent_commits_merge();
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();

    let commits = r.codehost.commits(&pr.repo, "factory/trail-1");
    assert_eq!(commits.last().unwrap().merges, [FAKE_BASE_MERGE], "published as a merge commit");
    assert_eq!(r.show(&key).await.status, Status::InReview);
}

#[tokio::test]
async fn a_merge_the_agent_started_and_committed_itself_is_published_as_a_merge_commit() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    r.codehost.set_mergeable(&pr, Some(false));
    r.factory.reconcile().await.unwrap();

    // Prepare started no merge, and the agent's `git merge` and `git commit` leave no MERGE_HEAD behind.
    r.daemon.agent_merges_and_commits();
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();

    let commits = r.codehost.commits(&pr.repo, "factory/trail-1");
    assert_eq!(commits.last().unwrap().merges, [FAKE_BASE_MERGE], "published as a merge commit");
    assert_eq!(r.show(&key).await.status, Status::InReview);
}

#[tokio::test]
async fn a_merge_the_agent_aborted_is_not_published() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    r.codehost.set_mergeable(&pr, Some(false));
    r.factory.reconcile().await.unwrap();

    r.daemon.agent_aborts_merge();
    let before = r.codehost.commits(&pr.repo, "factory/trail-1").len();
    r.harness.push(Role::Worker, Script::new(changes()));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(ran[0].1, Outcome::Failed { .. }), "{:?}", ran[0].1);
    assert_eq!(r.codehost.commits(&pr.repo, "factory/trail-1").len(), before, "nothing published");
    let comments = r.store.list_comments(&r.human, &key.0).await.unwrap();
    assert!(comments.iter().any(|c| c.body.contains("abandoned")), "{comments:?}");
}

#[tokio::test]
async fn a_branch_the_agent_rebased_is_not_published() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    r.codehost.set_mergeable(&pr, Some(false));
    r.factory.reconcile().await.unwrap();

    r.daemon.agent_rebases();
    let before = r.codehost.commits(&pr.repo, "factory/trail-1").len();
    r.harness.push(Role::Worker, Script::new(changes()));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(ran[0].1, Outcome::Failed { .. }), "{:?}", ran[0].1);
    assert_eq!(r.codehost.commits(&pr.repo, "factory/trail-1").len(), before, "nothing published");
    let comments = r.store.list_comments(&r.human, &key.0).await.unwrap();
    assert!(comments.iter().any(|c| c.body.contains("rewritten")), "{comments:?}");
}

#[tokio::test]
async fn a_merge_with_conflict_markers_left_in_it_is_not_published() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    r.codehost.set_mergeable(&pr, Some(false));
    r.factory.reconcile().await.unwrap();

    r.daemon.leave_conflict_markers(&["docs/guide.md"]);
    let before = r.codehost.commits(&pr.repo, "factory/trail-1").len();
    r.harness.push(Role::Worker, Script::new(changes()));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(ran[0].1, Outcome::Failed { .. }), "{:?}", ran[0].1);
    assert_eq!(r.codehost.commits(&pr.repo, "factory/trail-1").len(), before, "nothing published");
    let comments = r.store.list_comments(&r.human, &key.0).await.unwrap();
    assert!(comments.iter().any(|c| c.body.contains("docs/guide.md")), "{comments:?}");
}

#[tokio::test]
async fn last_review_reads_the_reviewers_verdict_and_the_latest_wins() {
    let r = rig().await;
    let key = r.task("Add a greeting").await;
    assert!(r.tracker.last_review(&key).await.unwrap().is_none(), "no review yet");

    let cred =
        r.tracker.create_sub_agent("trail-1-a1-r1", "TRAIL", std::time::Duration::from_secs(3600)).await.unwrap();
    let reviewer = CoreTracker::authenticate(r.store.clone(), &cred).await.unwrap();
    let verdict = |kind, body: &str, model: &str| Verdict {
        kind,
        body: body.into(),
        reviewer: String::new(),
        model: Some(ModelId(model.into())),
        raise_grade: None,
    };

    reviewer.record_verdict(&key, &verdict(VerdictKind::RequestChanges, "Add a test.", "opus")).await.unwrap();
    let last = r.tracker.last_review(&key).await.unwrap().expect("a review");
    assert_eq!(last.kind, VerdictKind::RequestChanges);
    assert_eq!(last.body, "Add a test.");
    assert_eq!(last.reviewer, "factory/trail-1-a1-r1");
    assert_eq!(last.model, Some(ModelId("opus".into())));

    reviewer.record_verdict(&key, &verdict(VerdictKind::Approve, "LGTM now.", "sonnet")).await.unwrap();
    let last = r.tracker.last_review(&key).await.unwrap().expect("a review");
    assert_eq!(last.kind, VerdictKind::Approve, "the later approve wins over the earlier request_changes");
    assert_eq!(last.body, "LGTM now.");
    assert_eq!(last.model, Some(ModelId("sonnet".into())));
}

#[tokio::test]
async fn last_review_names_a_sibling_reviewer_when_read_from_a_sub_agent() {
    let r = rig().await;
    let key = r.task("Add a greeting").await;
    let ttl = std::time::Duration::from_secs(3600);
    let reviewer_cred = r.tracker.create_sub_agent("trail-1-a1-r1", "TRAIL", ttl).await.unwrap();
    let reviewer = CoreTracker::authenticate(r.store.clone(), &reviewer_cred).await.unwrap();
    let verdict = Verdict {
        kind: VerdictKind::RequestChanges,
        body: "Add a test.".into(),
        reviewer: String::new(),
        model: None,
        raise_grade: None,
    };
    reviewer.record_verdict(&key, &verdict).await.unwrap();

    // The worker's sandbox reads the review under its own sub-agent ctx, as a rework run does.
    let worker_cred = r.tracker.create_sub_agent("trail-1-a2", "TRAIL", ttl).await.unwrap();
    let worker = CoreTracker::authenticate(r.store.clone(), &worker_cred).await.unwrap();
    let last = worker.last_review(&key).await.unwrap().expect("a review");
    assert_eq!(last.reviewer, "factory/trail-1-a1-r1");
    assert_eq!(last.body, "Add a test.");

    // So does the parent's `sub_agent` handle.
    let last = r.tracker.sub_agent("trail-1-a2").last_review(&key).await.unwrap().expect("a review");
    assert_eq!(last.reviewer, "factory/trail-1-a1-r1");
}

#[tokio::test]
async fn the_factory_approves_again_after_it_sent_the_task_back_for_a_new_head() {
    let config = format!(
        "{CONFIG}\n[gates.1]\nrequire = [\"checks\", \"ci\", \"agent_review\"]\nauto_merge = true\n\n[merge]\nmode = \"shadow\"\n"
    );
    let r = rig_with(&config).await;
    let reviewers = ProjectPatch { reviewers: Some(vec!["factory".into()]), ..Default::default() };
    r.store.update_project(&r.human, "TRAIL", reviewers, None).await.unwrap();
    let new =
        NewTask { project: "TRAIL".into(), title: "Add a greeting".into(), grade: Some(gr(1)), ..Default::default() };
    let key = r.store.create_task(&r.human, new).await.unwrap().key;
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();
    let pr = r.show(&key).await.run.pr.unwrap();
    r.codehost.set_ci(&pr, trailhead_factory::CiState::Passed);
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.show(&key).await.status, Status::Approved);

    r.codehost.push_to(&pr);
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.show(&key).await.status, Status::InReview);
    r.codehost.set_ci(&pr, trailhead_factory::CiState::Passed);
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let shown = r.show(&key).await;
    r.factory.review(&shown, &pr, 1, &[]).await.unwrap();
    let task = r.show(&key).await;
    assert_eq!(
        task.status,
        Status::Approved,
        "the factory didn't do the work, so it can approve again: {:?}",
        task.run.merge
    );
    assert!(task.run.merge.unwrap().problem.is_none());
}

#[tokio::test]
async fn the_factory_cant_approve_work_it_did_and_says_so_without_asking_for_a_reviewer() {
    let config = format!(
        "{CONFIG}\n[gates.1]\nrequire = [\"checks\", \"ci\", \"agent_review\"]\nauto_merge = true\n\n[merge]\nmode = \"shadow\"\n"
    );
    let r = rig_with(&config).await;
    let reviewers = ProjectPatch { reviewers: Some(vec!["factory".into()]), ..Default::default() };
    r.store.update_project(&r.human, "TRAIL", reviewers, None).await.unwrap();
    let new =
        NewTask { project: "TRAIL".into(), title: "Add a greeting".into(), grade: Some(gr(1)), ..Default::default() };
    let key = r.store.create_task(&r.human, new).await.unwrap().key;
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();
    let pr = r.show(&key).await.run.pr.unwrap();

    // The factory itself takes the task and sends it to review again, so it did the work.
    let back = TaskPatch { status: Some(trailhead_core::Status::Open), ..TaskPatch::default() };
    r.store.update_task(&r.human, &key.0, back, None).await.unwrap();
    r.tracker.claim(&key).await.unwrap();
    r.tracker.request_review(&key, "pushed").await.unwrap();

    r.codehost.set_ci(&pr, trailhead_factory::CiState::Passed);
    r.factory.reconcile().await.unwrap();
    let task = r.show(&key).await;
    assert_eq!(task.status, Status::InReview, "{:?}", task.run.merge);
    let problem = task.run.merge.unwrap().problem.expect("the approval was refused");
    assert!(problem.contains("can't approve or finish work it did"), "{problem}");
    assert!(!problem.contains("list it as a reviewer") && !problem.contains("delegated reviewer, so"), "{problem}");
}

#[tokio::test]
async fn an_approved_pr_that_starts_conflicting_is_unapproved_and_reworked() {
    let config = format!(
        "{CONFIG}\n[gates.1]\nrequire = [\"checks\", \"ci\", \"agent_review\"]\nauto_merge = true\n\n[merge]\nmode = \"shadow\"\n"
    );
    let r = rig_with(&config).await;
    let reviewers = ProjectPatch { reviewers: Some(vec!["factory".into()]), ..Default::default() };
    r.store.update_project(&r.human, "TRAIL", reviewers, None).await.unwrap();
    let new =
        NewTask { project: "TRAIL".into(), title: "Add a greeting".into(), grade: Some(gr(1)), ..Default::default() };
    let key = r.store.create_task(&r.human, new).await.unwrap().key;
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();
    let pr = r.show(&key).await.run.pr.unwrap();
    r.codehost.set_ci(&pr, trailhead_factory::CiState::Passed);
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.show(&key).await.status, Status::Approved);

    r.codehost.set_mergeable(&pr, Some(false));
    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.conflicted, std::slice::from_ref(&key.0));
    let task = r.show(&key).await;
    assert_eq!((task.status, task.changes_requested), (Status::InReview, true), "waiting in review for a rework");
    let wanted = task.run.changes_requested.expect("the rework is told why");
    assert!(wanted.kind == trailhead_factory::RequestKind::Conflict && wanted.body.contains(&pr.url), "{wanted:?}");
}

#[tokio::test]
async fn approve_is_refused_when_the_factory_isnt_a_delegated_reviewer() {
    let r = rig().await;
    let key = r.task("Add a greeting").await;
    r.tracker.claim(&key).await.unwrap();
    r.tracker.request_review(&key, "pushed").await.unwrap();

    let err = r.tracker.approve(&key).await.expect_err("the factory isn't in the project's reviewers");
    let trailhead_factory::Error::Forbidden(why) = err else { panic!("expected Forbidden, got {err:?}") };
    assert!(
        why.contains("isn't a delegated reviewer, so a person has to list it as a reviewer of project TRAIL"),
        "{why}"
    );
    assert_eq!(r.show(&key).await.status, Status::InReview);
}

#[tokio::test]
async fn approve_is_refused_for_work_the_factory_did_without_asking_for_a_reviewer() {
    let r = rig().await;
    let reviewers = ProjectPatch { reviewers: Some(vec!["factory".into()]), ..Default::default() };
    r.store.update_project(&r.human, "TRAIL", reviewers, None).await.unwrap();
    let key = r.task("Add a greeting").await;
    r.tracker.claim(&key).await.unwrap();
    r.tracker.request_review(&key, "pushed").await.unwrap();

    let err = r.tracker.approve(&key).await.expect_err("the factory sent this task to review");
    let trailhead_factory::Error::Forbidden(why) = err else { panic!("expected Forbidden, got {err:?}") };
    assert!(why.contains("can't approve or finish work it did"), "{why}");
    assert!(!why.contains("list it as a reviewer") && !why.contains("delegated reviewer, so"), "{why}");
    assert_eq!(r.show(&key).await.status, Status::InReview);
}

/// Runs a conflict rework on the PR and returns the head it was at before and the head the rework published.
async fn rework_and_publish(r: &Rig, key: &TaskKey, pr: &trailhead_factory::PrRef) -> (String, String) {
    let before = trailhead_factory::CodeHost::head(&r.codehost, pr).await.unwrap();
    r.codehost.set_mergeable(pr, Some(false));
    assert_eq!(r.factory.reconcile().await.unwrap().conflicted, std::slice::from_ref(&key.0));
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    // The host hasn't caught up with the push, so its conflict is the old head's and the review goes ahead.
    r.codehost.set_lagging_head(pr, Some(&before));
    r.factory.tick().await.unwrap();
    r.codehost.set_lagging_head(pr, None);
    assert_eq!(r.show(key).await.status, Status::InReview);
    let published = trailhead_factory::CodeHost::head(&r.codehost, pr).await.unwrap();
    assert_ne!(before, published);
    (before, published)
}

#[tokio::test(start_paused = true)]
async fn stale_conflicts_for_the_old_head_never_start_a_second_rework() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    let (before, _) = rework_and_publish(&r, &key, &pr).await;
    assert_eq!(r.show(&key).await.run.stale_head, Some(before.clone()));

    // The host hasn't caught up with the push: it shows the old head and its `false`, however many polls it takes.
    r.codehost.set_lagging_head(&pr, Some(&before));
    for _ in 0..3 {
        assert!(r.factory.reconcile().await.unwrap().conflicted.is_empty(), "a reading for the old head");
    }
    // It has the new head but is still working `mergeable` out.
    r.codehost.set_lagging_head(&pr, None);
    r.codehost.set_mergeable(&pr, None);
    assert!(r.factory.reconcile().await.unwrap().conflicted.is_empty());
    assert_eq!(r.show(&key).await.status, Status::InReview);
    assert_eq!(r.show(&key).await.run.conflict_reworks, Some(1));

    // The answer for the new head is `false`: that one counts.
    r.codehost.set_mergeable(&pr, Some(false));
    assert_eq!(r.factory.reconcile().await.unwrap().conflicted, std::slice::from_ref(&key.0));
}

#[tokio::test(start_paused = true)]
async fn the_stale_head_is_in_the_run_record_so_a_restart_keeps_ignoring_stale_conflicts() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    let (before, _) = rework_and_publish(&r, &key, &pr).await;
    r.codehost.set_lagging_head(&pr, Some(&before));
    // Nothing of the mark is held in the control plane's memory: a fresh reconcile reads it from the task.
    assert!(r.factory.reconcile().await.unwrap().conflicted.is_empty());
    r.codehost.set_lagging_head(&pr, None);
    r.codehost.set_mergeable(&pr, Some(true));
    assert!(r.factory.reconcile().await.unwrap().conflicted.is_empty());
    // A settled reading clears the mark, so a later conflict is read as it comes.
    assert_eq!(r.show(&key).await.run.stale_head.as_deref(), None);
    r.codehost.set_mergeable(&pr, Some(false));
    assert_eq!(r.factory.reconcile().await.unwrap().conflicted, std::slice::from_ref(&key.0));
}

#[tokio::test(start_paused = true)]
async fn a_head_pushed_past_the_rework_still_has_its_conflicts_seen() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    let (before, _) = rework_and_publish(&r, &key, &pr).await;
    // A person pushes before any poll saw the rework's head settle, and the new head conflicts.
    r.codehost.set_lagging_head(&pr, Some(&before));
    assert!(r.factory.reconcile().await.unwrap().conflicted.is_empty(), "still the old head's reading");
    r.codehost.set_lagging_head(&pr, None);
    r.codehost.push_to(&pr);
    r.codehost.set_mergeable(&pr, Some(false));
    assert_eq!(r.factory.reconcile().await.unwrap().conflicted, std::slice::from_ref(&key.0));
}

#[tokio::test]
async fn a_task_that_blocks_urgent_work_is_listed_as_urgent_too() {
    let mut r = rig().await;
    let queue = r.task("Queue").await;
    let mk = |title: &str, priority: u8| NewTask {
        project: "TRAIL".into(),
        title: title.into(),
        parent: Some(queue.clone()),
        priority: Some(pr(priority)),
        grade: Some(gr(1)),
        ..Default::default()
    };
    let older = r.store.create_task(&r.human, mk("older", 3)).await.unwrap().key;
    let blocker = r.store.create_task(&r.human, mk("blocker", 3)).await.unwrap().key;
    let middle = r.store.create_task(&r.human, mk("middle", 3)).await.unwrap().key;
    let urgent = r.store.create_task(&r.human, mk("urgent", 2)).await.unwrap().key;
    r.store.add_dependency(&r.human, &blocker.0, &middle.0).await.unwrap();
    r.store.add_dependency(&r.human, &middle.0, &urgent.0).await.unwrap();

    let ready = r.tracker.ready("TRAIL", std::slice::from_ref(&queue), 10).await.unwrap();
    let priority = |k: &TaskKey| ready.iter().find(|t| &t.key == k).unwrap().priority;
    assert_eq!(priority(&blocker), pr(2), "through a chain, a p3 blocker of a p2 task runs as p2");
    assert_eq!(priority(&older), pr(3));

    let first = r.tracker.ready("TRAIL", std::slice::from_ref(&queue), 1).await.unwrap();
    assert_eq!(first.iter().map(|t| &t.key).collect::<Vec<_>>(), [&blocker], "the raise applies before the limit");
    let first = r.tracker.ready("TRAIL", &[], 1).await.unwrap();
    assert_eq!(first.iter().map(|t| &t.key).collect::<Vec<_>>(), [&blocker], "with no queue too");

    r.factory.config.repo.max_sandboxes = 1;
    r.harness.push(Role::Worker, Script::new(RunOutcome::NeedsInput { question: "Which?".into() }));
    let ran = r.factory.tick().await.unwrap();
    assert_eq!(ran.iter().map(|(t, _)| &t.key).collect::<Vec<_>>(), [&blocker], "before the older p3 task");
}

/// Gives `key` factory run metadata, as a run leaves on its task.
async fn with_run(r: &Rig, key: &TaskKey) {
    let run = trailhead_factory::RunPatch {
        attempt: trailhead_factory::Patch::Set(1),
        sandbox: trailhead_factory::Patch::Set("run".into()),
        ..Default::default()
    };
    r.tracker.report(key, run).await.unwrap();
}

async fn subtask(r: &Rig, queue: &TaskKey, title: &str) -> TaskKey {
    let new =
        NewTask { project: "TRAIL".into(), title: title.into(), parent: Some(queue.clone()), ..Default::default() };
    r.store.create_task(&r.human, new).await.unwrap().key
}

/// A factory for another repo on the rig's store, working `queue` on the given host.
fn factory_for(r: &Rig, name: &str, queue: &TaskKey, host: Arc<Host>) -> Factory {
    let config = CONFIG.replace("name = \"trailhead\"", &format!("name = \"{name}\"\nqueue = \"{}\"", queue.0));
    Factory::new(
        FactoryConfig::parse_table(&config, "TRAIL", None).unwrap(),
        Arc::new(r.compute.clone()),
        Arc::new(r.tracker.clone()),
        Arc::new(FakeProvider::new(1)),
        r.daemon.clone(),
        host,
    )
}

/// How many log lines carry `task=<key>` as a whole key, so `TRAIL-4` doesn't count `TRAIL-40`.
fn warnings_for(logs: &Logs, key: &TaskKey) -> usize {
    let field = format!("task={}", key.0);
    let text = logs.text();
    text.match_indices(&field)
        .filter(|(i, _)| !text[i + field.len()..].starts_with(|c: char| c.is_ascii_digit()))
        .count()
}

#[tokio::test]
async fn strays_are_open_factory_tasks_outside_every_queue() {
    let r = rig().await;
    let queue = r.task("Queue").await;
    let other = r.task("Other queue").await;
    let queues = [queue.clone(), other.clone()];

    let inside = subtask(&r, &queue, "Inside").await;
    let inside_other = subtask(&r, &other, "Inside the other").await;
    let outside = r.task("Moved out").await;
    let closed = r.task("Moved out, then closed").await;
    let plain = r.task("No factory metadata").await;
    for key in [&inside, &inside_other, &outside, &closed] {
        with_run(&r, key).await;
    }
    let done = TaskPatch { status: Some(trailhead_core::Status::Done), ..Default::default() };
    r.store.update_task(&r.human, &closed.0, done, None).await.unwrap();

    let strays = r.tracker.strays("TRAIL", &queues).await.unwrap();
    assert_eq!(strays.iter().map(|t| &t.key).collect::<Vec<_>>(), [&outside]);
    assert!(!strays.iter().any(|t| t.key == plain));
    // With only `queue` named, `other` is no longer a queue, so its subtask is a stray too, and `outside` still is.
    let strays = r.tracker.strays("TRAIL", std::slice::from_ref(&queue)).await.unwrap();
    let mut keys: Vec<_> = strays.iter().map(|t| t.key.clone()).collect();
    keys.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(keys, [inside_other, outside]);
}

#[tokio::test]
async fn a_stray_is_warned_about_once_across_reconciles_and_not_for_a_sibling_repos_queue() {
    let r = rig().await;
    let q1 = r.task("Alpha's queue").await;
    let q2 = r.task("Beta's queue").await;
    let host = Host::new(&HostConfig::default());
    let alpha = factory_for(&r, "alpha", &q1, host.clone());
    let _beta = factory_for(&r, "beta", &q2, host);

    // Beta's run is live under beta's queue; alpha sees the shared queues, so it isn't a stray.
    let beta_task = subtask(&r, &q2, "Beta's task").await;
    with_run(&r, &beta_task).await;
    let (logs, _capture) = Logs::capture();

    alpha.reconcile().await.unwrap();
    assert_eq!(warnings_for(&logs, &beta_task), 0, "{}", logs.text());

    let stray = r.task("Moved out of every queue").await;
    with_run(&r, &stray).await;
    alpha.reconcile().await.unwrap();
    alpha.reconcile().await.unwrap();
    let text = logs.text();
    assert_eq!(warnings_for(&logs, &stray), 1, "{text}");
    assert_eq!(warnings_for(&logs, &beta_task), 0, "{text}");
}

#[tokio::test]
async fn a_stray_moved_back_under_its_queue_and_out_again_is_warned_about_again() {
    let r = rig().await;
    let queue = r.task("Queue").await;
    let alpha = factory_for(&r, "alpha", &queue, Host::new(&HostConfig::default()));
    let task = subtask(&r, &queue, "Moves around").await;
    with_run(&r, &task).await;
    let (logs, _capture) = Logs::capture();
    let reparent = |parent: Option<TaskKey>| TaskPatch { parent: Some(parent), ..Default::default() };

    r.store.update_task(&r.human, &task.0, reparent(None), None).await.unwrap();
    alpha.reconcile().await.unwrap();
    assert_eq!(logs.text().matches(&task.0).count(), 1, "{}", logs.text());

    r.store.update_task(&r.human, &task.0, reparent(Some(queue.clone())), None).await.unwrap();
    alpha.reconcile().await.unwrap();
    r.store.update_task(&r.human, &task.0, reparent(None), None).await.unwrap();
    alpha.reconcile().await.unwrap();
    assert_eq!(logs.text().matches(&task.0).count(), 2, "{}", logs.text());
}

#[tokio::test]
async fn strays_in_different_projects_on_one_host_are_each_warned_about_once() {
    let r = rig().await;
    r.store
        .create_project(&r.human, NewProject { key: "OTHER".into(), name: "Other".into(), ..Default::default() })
        .await
        .unwrap();
    let q1 = r.task("Alpha's queue").await;
    let in_other = |title: &str| NewTask { project: "OTHER".into(), title: title.into(), ..Default::default() };
    let q2 = r.store.create_task(&r.human, in_other("Beta's queue")).await.unwrap().key;
    let host = Host::new(&HostConfig::default());
    let alpha = factory_for(&r, "alpha", &q1, host.clone());
    let config = CONFIG.replace("name = \"trailhead\"", &format!("name = \"beta\"\nqueue = \"{}\"", q2.0));
    let beta = Factory::new(
        FactoryConfig::parse_table(&config, "OTHER", None).unwrap(),
        Arc::new(r.compute.clone()),
        Arc::new(r.tracker.clone()),
        Arc::new(FakeProvider::new(1)),
        r.daemon.clone(),
        host,
    );
    let stray_a = r.task("Alpha's stray").await;
    let stray_b = r.store.create_task(&r.human, in_other("Beta's stray")).await.unwrap().key;
    with_run(&r, &stray_a).await;
    with_run(&r, &stray_b).await;
    let (logs, _capture) = Logs::capture();

    for _ in 0..3 {
        alpha.reconcile().await.unwrap();
        beta.reconcile().await.unwrap();
    }
    let text = logs.text();
    assert_eq!(warnings_for(&logs, &stray_a), 1, "{text}");
    assert_eq!(warnings_for(&logs, &stray_b), 1, "{text}");
}

#[tokio::test]
async fn ready_lists_reworks_ahead_of_new_work_only_within_a_priority() {
    let r = rig().await;
    let (rework, pull) = in_review(&r, "Add a greeting").await;
    r.codehost.set_mergeable(&pull, Some(false));
    r.factory.reconcile().await.unwrap();
    assert!(r.show(&rework).await.changes_requested);

    let urgent =
        NewTask { project: "TRAIL".into(), title: "Urgent".into(), priority: Some(pr(1)), ..Default::default() };
    let urgent = r.store.create_task(&r.human, urgent).await.unwrap().key;
    let same = r.task("Same priority as the rework").await;

    // The cut to `limit` comes after the merge: the urgent task isn't pushed out by the rework.
    let ready = r.tracker.ready("TRAIL", &[], 1).await.unwrap();
    assert_eq!(ready.iter().map(|t| &t.key).collect::<Vec<_>>(), [&urgent]);
    let ready = r.tracker.ready("TRAIL", &[], 3).await.unwrap();
    assert_eq!(ready.iter().map(|t| &t.key).collect::<Vec<_>>(), [&urgent, &rework, &same]);
}

#[tokio::test]
async fn a_merge_intent_is_cleared_when_the_head_moves_under_it() {
    let config = format!(
        "{CONFIG}\n[gates.1]\nrequire = [\"checks\", \"ci\", \"agent_review\"]\nauto_merge = true\n\n[merge]\nmode = \"shadow\"\n"
    );
    let r = rig_with(&config).await;
    let reviewers = ProjectPatch { reviewers: Some(vec!["factory".into()]), ..Default::default() };
    r.store.update_project(&r.human, "TRAIL", reviewers, None).await.unwrap();
    let new =
        NewTask { project: "TRAIL".into(), title: "Add a greeting".into(), grade: Some(gr(1)), ..Default::default() };
    let key = r.store.create_task(&r.human, new).await.unwrap().key;
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();
    let pr = r.show(&key).await.run.pr.unwrap();
    r.codehost.set_ci(&pr, trailhead_factory::CiState::Passed);
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.show(&key).await.status, Status::Approved);

    let head = trailhead_factory::CodeHost::head(&r.codehost, &pr).await.unwrap();
    r.tracker.report(&key, RunPatch { merging: Patch::Set(head), ..RunPatch::default() }).await.unwrap();
    r.codehost.push_to(&pr);
    r.factory.reconcile().await.unwrap();
    let task = r.show(&key).await;
    assert_eq!(task.status, Status::InReview);
    assert_eq!(task.run.merging.as_deref(), None);
}

#[tokio::test]
async fn no_stray_is_warned_about_when_a_sibling_repo_works_the_project_without_a_queue() {
    let r = rig().await;
    let q = r.task("Beta's queue").await;
    let host = Host::new(&HostConfig::default());
    let _queueless = Factory::new(
        FactoryConfig::parse_table(CONFIG, "TRAIL", None).unwrap(),
        Arc::new(r.compute.clone()),
        Arc::new(r.tracker.clone()),
        Arc::new(FakeProvider::new(1)),
        r.daemon.clone(),
        host.clone(),
    );
    let beta = factory_for(&r, "beta", &q, host);

    // The queueless repo's run sits under no queue, but it is that repo's own, not a stray.
    let theirs = r.task("Alpha's task").await;
    with_run(&r, &theirs).await;
    let (logs, _capture) = Logs::capture();

    beta.reconcile().await.unwrap();
    assert!(!logs.text().contains(&theirs.0), "{}", logs.text());
}

/// Runs a grade 3 task to an open PR, leaves it in review unapproved, and merges the PR as a person would.
async fn merged_by_hand_in_review(r: &Rig) -> TaskKey {
    let new =
        NewTask { project: "TRAIL".into(), title: "Add a greeting".into(), grade: Some(gr(3)), ..Default::default() };
    let key = r.store.create_task(&r.human, new).await.unwrap().key;
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();
    let task = r.show(&key).await;
    assert_eq!(task.status, Status::InReview);
    let pr = task.run.pr.unwrap();
    let head = trailhead_factory::CodeHost::head(&r.codehost, &pr).await.unwrap();
    trailhead_factory::CodeHost::merge(&r.codehost, &pr, &head).await.unwrap();
    key
}

#[tokio::test]
async fn without_reviewer_rights_a_pr_merged_by_hand_in_review_is_parked_once_for_a_person() {
    let r = rig_with(&format!("{CONFIG}\n[merge]\nmode = \"shadow\"\n")).await;
    let key = merged_by_hand_in_review(&r).await;

    r.factory.reconcile().await.unwrap();
    let task = r.show(&key).await;
    assert_eq!(task.status, Status::NeedsInput, "a person is asked to mark it done");
    assert_eq!(task.run.unapproved_merge, Some(true));
    let comments = r.store.list_comments(&r.human, &key.0).await.unwrap();
    assert_eq!(comments.iter().filter(|c| c.kind == CommentKind::Question).count(), 1);
    let handoffs = comments.iter().filter(|c| c.kind == CommentKind::Handoff).count();

    for _ in 0..3 {
        r.factory.reconcile().await.unwrap();
    }
    let after = r.store.list_comments(&r.human, &key.0).await.unwrap();
    assert_eq!(after.len(), comments.len(), "later passes leave it alone");
    assert_eq!(after.iter().filter(|c| c.kind == CommentKind::Handoff).count(), handoffs);
    assert_eq!(r.show(&key).await.status, Status::NeedsInput);
}

#[tokio::test]
async fn a_pr_a_person_merges_in_review_finishes_the_task_though_nobody_approved_it() {
    let r = rig_with(&format!("{CONFIG}\n[merge]\nmode = \"shadow\"\n")).await;
    let reviewers = ProjectPatch { reviewers: Some(vec!["factory".into()]), ..Default::default() };
    r.store.update_project(&r.human, "TRAIL", reviewers, None).await.unwrap();
    let key = merged_by_hand_in_review(&r).await;

    r.factory.reconcile().await.unwrap();
    let task = r.show(&key).await;
    assert_eq!(task.status, Status::Done, "merging settles it");
    assert_eq!(task.run.merged.expect("the merge is recorded").by, trailhead_factory::MergedBy::Human);
    assert_eq!(task.run.unapproved_merge, None);
}

#[tokio::test]
async fn a_reviewers_summary_with_trailing_whitespace_counts_one_round() {
    let r = rig().await;
    let key = r.task("Add a greeting").await;
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(request_changes("Add a test.\n")));
    r.factory.tick().await.unwrap();
    assert_eq!(r.show(&key).await.run.review_rounds, Some(1));
    r.factory.reconcile().await.unwrap();
    let task = r.show(&key).await;
    assert_eq!(task.run.review_rounds, Some(1), "the stored comment is trimmed; it is still the recorded request");
    assert!(task.changes_requested);
}

/// The store merges a written record into the one it holds, so a record that left a field out would keep the older
/// record's value: a new request for changes would be born settled and never reworked (TRAIL-1471).
#[tokio::test]
async fn a_record_written_over_an_older_one_keeps_none_of_its_fields() {
    use trailhead_factory::{ChangesRequested, Merged, MergedBy, PrGrade, PrRef, Repo, RequestKind};
    let r = rig().await;
    let key = r.task("Add a greeting").await;
    let pr = PrRef { repo: Repo { owner: "o".into(), name: "r".into() }, number: 4, url: "https://host/o/r/4".into() };
    let request = |settled: bool, head: Option<&str>| ChangesRequested {
        reviewer: "factory/trail-1-a1-r1".into(),
        body: "Add a test.".into(),
        attempt: 1,
        kind: RequestKind::Review,
        head: head.map(str::to_string),
        settled,
    };
    let older = RunPatch {
        changes_requested: Patch::Set(request(true, Some("abc"))),
        grade: Patch::Set(PrGrade { grade: gr(3), task: Some(gr(1)), reasons: vec!["Cargo.toml changed".into()] }),
        merged: Patch::Set(Merged { by: MergedBy::Merger, pr: pr.clone(), commit: Some("def".into()) }),
        ..RunPatch::default()
    };
    r.tracker.report(&key, older).await.unwrap();

    let newer = RunPatch {
        changes_requested: Patch::Set(request(false, None)),
        grade: Patch::Set(PrGrade { grade: gr(1), task: None, reasons: vec![] }),
        merged: Patch::Set(Merged { by: MergedBy::Human, pr: pr.clone(), commit: None }),
        ..RunPatch::default()
    };
    r.tracker.report(&key, newer).await.unwrap();

    let run = r.show(&key).await.run;
    assert_eq!(run.changes_requested, Some(request(false, None)), "not settled, and on no head");
    assert!(run.pending_changes().is_some(), "so it is still to be reworked");
    assert_eq!(run.grade, Some(PrGrade { grade: gr(1), task: None, reasons: vec![] }));
    assert_eq!(run.merged, Some(Merged { by: MergedBy::Human, pr, commit: None }));
}
