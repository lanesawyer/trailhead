//! The lifecycle against the real task engine: claims, leases, review rules,
//! sub-agents and their tokens are Trailhead's own, not the fake's.

use std::sync::Arc;

use trailhead_control::{CoreTracker, Factory, Outcome};
use trailhead_core::{CommentKind, Ctx, NewProject, NewTask, ProjectPatch, Store, TaskPatch};
use trailhead_factory::{
    CodeHost, Commit, FactoryConfig, FileChange, FollowUp, ModelId, Role, RunOutcome, RunUpdate, Status, TaskKey,
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
    )
    .with_codehost(Arc::new(codehost.clone()));
    Rig { store, human, factory, tracker, compute, codehost, harness, daemon }
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
                priority: Some(2),
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
    assert_eq!(filed.priority, 2);
    assert_eq!(filed.status, trailhead_core::Status::Open);
    assert!(filed.description.starts_with("No test for empty input."), "{}", filed.description);
    assert!(filed.description.contains(&key.0), "{}", filed.description);

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

    let ready = r.tracker.ready("TRAIL", None, 10).await.unwrap();
    assert_eq!(ready.iter().map(|t| &t.key).collect::<Vec<_>>(), [&good]);
    assert!(r.tracker.in_flight("TRAIL").await.unwrap().is_empty());
    r.factory.reconcile().await.unwrap();
    assert!(r.tracker.show(&bad).await.is_err());
}

#[tokio::test]
async fn with_a_queue_only_its_subtasks_run() {
    let mut r = rig().await;
    let urgent = NewTask {
        project: "TRAIL".into(),
        title: "An epic, not for the factory".into(),
        priority: Some(1),
        ..Default::default()
    };
    let epic = TaskKey(r.store.create_task(&r.human, urgent).await.unwrap().key);
    let queue = r.task("Factory queue").await;
    let sub = NewTask {
        project: "TRAIL".into(),
        title: "Add a greeting".into(),
        parent: Some(queue.0.clone()),
        grade: Some(1),
        ..Default::default()
    };
    let handed = TaskKey(r.store.create_task(&r.human, sub).await.unwrap().key);
    r.factory.config.repo.queue = Some(queue.0.clone());

    let ready = r.tracker.ready("TRAIL", Some(&queue), 10).await.unwrap();
    assert_eq!(ready.iter().map(|t| &t.key).collect::<Vec<_>>(), [&handed]);

    r.harness.push(Role::Worker, Script::new(RunOutcome::NeedsInput { question: "Which greeting?".into() }));
    let ran = r.factory.tick().await.unwrap();
    assert_eq!(ran.iter().map(|(t, _)| &t.key).collect::<Vec<_>>(), [&handed]);
    assert_eq!(r.show(&epic).await.status, Status::Open, "the epic is left alone");
    assert_eq!(r.show(&queue).await.status, Status::Open, "and so is the queue itself");
}

#[tokio::test]
async fn the_factory_can_raise_a_grade_a_human_set_but_not_lower_it() {
    let r = rig().await;
    let new = NewTask { project: "TRAIL".into(), title: "Graded".into(), grade: Some(2), ..Default::default() };
    let key = TaskKey(r.store.create_task(&r.human, new).await.unwrap().key);
    assert_eq!(r.show(&key).await.grade, Some(2));
    r.tracker.raise_grade(&key, 3).await.unwrap();
    assert_eq!(r.show(&key).await.grade, Some(3));
    assert!(r.tracker.raise_grade(&key, 1).await.is_err());
    let ungraded = r.task("Ungraded").await;
    assert!(r.tracker.raise_grade(&ungraded, 2).await.is_err(), "the first grade is a human's");
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
async fn requested_changes_reopen_then_rework_on_the_real_engine() {
    let r = rig().await;
    let key = r.task("Add a greeting").await;
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(request_changes("Add a test.")));
    r.factory.tick().await.unwrap();
    let task = r.show(&key).await;
    assert_eq!(task.status, Status::Open, "the control plane can reopen a task in review");
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
    let handoff = comments.iter().find(|c| c.kind == CommentKind::Handoff).expect("a handoff");
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
async fn an_open_task_waiting_for_a_rework_whose_pr_is_merged_is_finished() {
    let r = rig().await;
    let (key, pr) = in_review(&r, "Add a greeting").await;
    r.codehost.set_mergeable(&pr, Some(false));
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.show(&key).await.status, Status::Open);
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
    assert!(question.body.contains(&pr.url) && question.body.contains("closed without being merged"));
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
    assert_eq!(task.status, Status::Open, "back in the queue for a rework");
    assert_eq!(task.run.review_rounds, Some(1), "a conflict leaves the review rounds alone");
    assert_eq!(task.run.conflict_reworks, Some(1));
    let wanted = task.run.changes_requested.expect("the rework is told why");
    assert!(wanted.conflict && wanted.body.contains(&pr.url), "{wanted:?}");

    r.codehost.set_mergeable(&pr, Some(true));
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
    assert_eq!(task.status, Status::InReview, "reviewed again");
    assert_eq!(task.run.review_rounds, Some(1), "the re-review after a conflict rework uses no round");
    assert!(r.factory.reconcile().await.unwrap().conflicted.is_empty());
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
        assert_eq!(r.show(key).await.status, Status::Open);
        r.codehost.set_mergeable(pr, Some(true));
        r.harness.push(Role::Worker, Script::new(changes()));
        r.harness.push(Role::Reviewer, Script::new(approve()));
        r.factory.tick().await.unwrap();
        assert_eq!(r.show(key).await.status, Status::InReview);
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
    r.codehost.set_mergeable(&pr, Some(true));
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
    assert_eq!(task.status, Status::Open, "reworked, not parked");
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
    r.codehost.set_mergeable(&pr, Some(true));
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
    r.codehost.set_mergeable(&pr, Some(true));
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

    r.codehost.set_mergeable(&pr, Some(true));
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

    r.codehost.set_mergeable(&pr, Some(true));
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

    r.codehost.set_mergeable(&pr, Some(true));
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
    let new = NewTask { project: "TRAIL".into(), title: "Add a greeting".into(), grade: Some(1), ..Default::default() };
    let key = TaskKey(r.store.create_task(&r.human, new).await.unwrap().key);
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
async fn an_approved_pr_that_starts_conflicting_is_unapproved_and_reworked() {
    let config = format!(
        "{CONFIG}\n[gates.1]\nrequire = [\"checks\", \"ci\", \"agent_review\"]\nauto_merge = true\n\n[merge]\nmode = \"shadow\"\n"
    );
    let r = rig_with(&config).await;
    let reviewers = ProjectPatch { reviewers: Some(vec!["factory".into()]), ..Default::default() };
    r.store.update_project(&r.human, "TRAIL", reviewers, None).await.unwrap();
    let new = NewTask { project: "TRAIL".into(), title: "Add a greeting".into(), grade: Some(1), ..Default::default() };
    let key = TaskKey(r.store.create_task(&r.human, new).await.unwrap().key);
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
    assert_eq!(task.status, Status::Open, "back in the queue for a rework");
    let wanted = task.run.changes_requested.expect("the rework is told why");
    assert!(wanted.conflict && wanted.body.contains(&pr.url), "{wanted:?}");
}
