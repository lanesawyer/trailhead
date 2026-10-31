mod common;

use common::Env;
use jiff::SignedDuration;
use jiff::civil::date;
use serde_json::json;
use trailhead_core::parse_at;
use trailhead_core::{
    CommentKind, Error, EventFilter, GradeFilter, MatchField, NewComment, NewProject, NewTask, ProjectPatch,
    ProjectStatus, SearchFilter, Status, TaskFilter, TaskPatch, TaskSort, Verdict,
};

fn status(s: Status) -> TaskPatch {
    TaskPatch { status: Some(s), ..Default::default() }
}

fn comment(kind: CommentKind, body: &str) -> NewComment {
    NewComment::new(kind, body)
}

#[tokio::test]
async fn task_keys_are_sequential_per_project() {
    let env = Env::new().await;
    env.store
        .create_project(&env.human, NewProject { key: "web".into(), name: "Web".into(), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(env.task("a").await.key, "AUTH-1");
    assert_eq!(env.task("b").await.key, "AUTH-2");
    let w = env.task_with(NewTask { project: "WEB".into(), title: "c".into(), ..Default::default() }).await;
    assert_eq!(w.key, "WEB-1");
    assert_eq!(env.store.get_task(&env.human, "auth-2").await.unwrap().title, "b");
}

#[tokio::test]
async fn ready_queue_orders_and_filters() {
    let env = Env::new().await;
    let low = env.task_with(NewTask { title: "low".into(), priority: Some(4), ..Default::default() }).await;
    let late = env
        .task_with(NewTask {
            title: "late".into(),
            priority: Some(2),
            due_at: Some(env.at_in(SignedDuration::from_hours(9 * 24))),
            ..Default::default()
        })
        .await;
    let soon = env
        .task_with(NewTask {
            title: "soon".into(),
            priority: Some(2),
            due_at: Some(env.at_in(SignedDuration::from_hours(24))),
            ..Default::default()
        })
        .await;
    let undated = env.task_with(NewTask { title: "undated".into(), priority: Some(2), ..Default::default() }).await;
    let urgent = env.task_with(NewTask { title: "urgent".into(), priority: Some(1), ..Default::default() }).await;
    assert_eq!(
        env.ready_keys().await,
        [urgent.key.clone(), soon.key.clone(), late.key.clone(), undated.key.clone(), low.key.clone()]
    );

    env.store.add_dependency(&env.human, &low.key, &urgent.key).await.unwrap();
    let agent = env.agent("a").await;
    env.store.claim(&agent, &soon.key, None).await.unwrap();
    env.store.update_task(&env.human, &late.key, status(Status::Done), None).await.unwrap();
    assert_eq!(env.ready_keys().await, [undated.key.clone(), low.key.clone()]);

    let err = env.store.claim(&agent, &urgent.key, None).await.unwrap_err();
    assert!(matches!(err, Error::BlockedBy(ref k) if k == std::slice::from_ref(&low.key)));
    env.store.update_task(&env.human, &low.key, status(Status::Cancelled), None).await.unwrap();
    assert_eq!(env.ready_keys().await, [urgent.key.clone(), undated.key.clone()]);
}

#[tokio::test]
async fn expired_claims_free_the_task() {
    let env = Env::new().await;
    let t = env.task("lease").await;
    let a = env.agent("a").await;
    let b = env.agent("b").await;
    let claimed = env.store.claim(&a, &t.key, Some(60)).await.unwrap();
    assert_eq!(claimed.task.status, Status::InProgress);
    assert!(env.ready_keys().await.is_empty());
    match env.store.claim(&b, &t.key, None).await {
        Err(Error::ClaimHeld { holder, holder_id, .. }) => {
            assert_eq!(holder, "a");
            assert_eq!(holder_id, a.actor_id().to_string());
        }
        other => panic!("expected claim_held, got {other:?}"),
    }

    env.advance(40);
    env.store.renew(&a, &t.key, Some(60)).await.unwrap();
    env.advance(40);
    assert!(matches!(env.store.claim(&b, &t.key, None).await, Err(Error::ClaimHeld { .. })));

    env.advance(30);
    assert_eq!(env.ready_keys().await, vec![t.key.clone()]);
    let events = env
        .store
        .list_events(&env.human, EventFilter { task: Some(t.key.clone()), ..Default::default() })
        .await
        .unwrap();
    let expired = events.iter().find(|e| e.kind == "claim.expired").unwrap();
    assert_eq!(expired.actor_id, a.actor_id());
    assert!(matches!(env.store.renew(&a, &t.key, None).await, Err(Error::ClaimLost(_))));
    env.store.claim(&b, &t.key, None).await.unwrap();
}

#[tokio::test]
async fn claim_ttl_comes_from_project_then_workspace() {
    let env = Env::new().await;
    let t = env.task("ttl").await;
    let a = env.agent("a").await;
    let c = env.store.claim(&a, &t.key, None).await.unwrap().claim;
    assert_eq!(c.expires_at.unwrap().duration_since(c.claimed_at), SignedDuration::from_mins(30));
    env.store
        .update_project(&env.human, "AUTH", ProjectPatch { claim_ttl: Some(Some(120)), ..Default::default() }, None)
        .await
        .unwrap();
    let c = env.store.renew(&a, &t.key, None).await.unwrap();
    assert_eq!(c.expires_at.unwrap().duration_since(env.now()), SignedDuration::from_secs(120));
    assert!(matches!(env.store.renew(&a, &t.key, Some(90_000)).await, Err(Error::Invalid(_))));
}

#[tokio::test]
async fn human_claims_never_expire_unless_given_a_ttl() {
    let env = Env::new().await;
    let t = env.task("mine").await;
    let a = env.agent("a").await;
    let c = env.store.claim(&env.human, &t.key, None).await.unwrap().claim;
    assert_eq!(c.expires_at, None);
    env.advance(30 * 86_400);
    assert_eq!(env.store.reap_expired_claims(&env.human).await.unwrap(), 0);
    assert!(env.ready_keys().await.is_empty());
    assert!(matches!(env.store.claim(&a, &t.key, None).await, Err(Error::ClaimHeld { expires_at: None, .. })));
    assert_eq!(env.store.renew(&env.human, &t.key, None).await.unwrap(), c);
    let held = env.store.my_claims(&env.human).await.unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].claim.expires_at, None);

    let leased = env.store.renew(&env.human, &t.key, Some(60)).await.unwrap();
    assert_eq!(leased.expires_at, Some(env.now() + SignedDuration::from_secs(60)));
    env.advance(61);
    env.store.claim(&a, &t.key, None).await.unwrap();
}

#[tokio::test]
async fn agents_respect_each_others_claims_but_humans_can_override() {
    let env = Env::new().await;
    let t = env.task("mine").await;
    let a = env.agent("a").await;
    let b = env.agent("b").await;
    env.store.claim(&a, &t.key, None).await.unwrap();
    let patch = TaskPatch { title: Some("hijack".into()), ..Default::default() };
    assert!(matches!(env.store.update_task(&b, &t.key, patch.clone(), None).await, Err(Error::ClaimHeld { .. })));
    assert!(matches!(env.store.release(&b, &t.key).await, Err(Error::NotClaimHolder)));
    env.store.update_task(&env.human, &t.key, patch, None).await.unwrap();
    let released = env.store.release(&env.human, &t.key).await.unwrap();
    assert_eq!(released.status, Status::Open);
}

#[tokio::test]
async fn finishing_drops_the_claim() {
    let env = Env::new().await;
    let t = env.task("finish").await;
    let a = env.agent("a").await;
    env.store.claim(&a, &t.key, None).await.unwrap();
    env.store.update_task(&a, &t.key, status(Status::Done), None).await.unwrap();
    assert!(env.store.task_detail(&env.human, &t.key).await.unwrap().claim.is_none());
    let reopened = env.store.update_task(&env.human, &t.key, status(Status::Open), None).await.unwrap();
    assert_eq!(reopened.status, Status::Open);
    assert!(matches!(
        env.store.update_task(&env.human, &t.key, status(Status::NeedsInput), None).await.map(|t| t.status),
        Ok(Status::NeedsInput)
    ));
    assert!(matches!(
        env.store.update_task(&env.human, &t.key, status(Status::InReview), None).await.map(|t| t.status),
        Ok(Status::InReview)
    ));
}

#[tokio::test]
async fn claim_takes_parked_and_reviewed_work_directly() {
    let env = Env::new().await;
    let a = env.agent("a").await;
    for to in [Status::NeedsInput, Status::InReview] {
        let t = env.task("take over").await;
        env.store.update_task(&env.human, &t.key, status(to), None).await.unwrap();
        let claimed = env.store.claim(&a, &t.key, None).await.unwrap();
        assert_eq!(claimed.task.status, Status::InProgress);
    }
}

#[tokio::test]
async fn review_requires_a_human() {
    let env = Env::new().await;
    env.store
        .update_project(&env.human, "AUTH", ProjectPatch { require_review: Some(true), ..Default::default() }, None)
        .await
        .unwrap();
    let t = env.task("reviewed").await;
    let a = env.agent("a").await;
    env.store.claim(&a, &t.key, None).await.unwrap();
    assert!(matches!(env.store.update_task(&a, &t.key, status(Status::Done), None).await, Err(Error::ReviewRequired)));
    env.store.update_task(&a, &t.key, status(Status::InReview), None).await.unwrap();
    assert!(matches!(env.store.update_task(&a, &t.key, status(Status::Done), None).await, Err(Error::ReviewRequired)));
    let done = env.store.update_task(&env.human, &t.key, status(Status::Done), None).await.unwrap();
    assert_eq!(done.status, Status::Done);
}

#[tokio::test]
async fn only_humans_can_loosen_review_or_archive() {
    let env = Env::new().await;
    let a = env.agent("a").await;
    let patch = |p: ProjectPatch| async { env.store.update_project(&a.next_batch(), "AUTH", p, None).await };

    // Tightening is fine, and so is restating the current setting.
    patch(ProjectPatch { require_review: Some(true), ..Default::default() }).await.unwrap();
    patch(ProjectPatch { require_review: Some(true), name: Some("Auth".into()), ..Default::default() }).await.unwrap();
    assert!(matches!(
        patch(ProjectPatch { require_review: Some(false), ..Default::default() }).await,
        Err(Error::Forbidden(_))
    ));
    assert!(matches!(
        patch(ProjectPatch { status: Some(ProjectStatus::Archived), ..Default::default() }).await,
        Err(Error::Forbidden(_))
    ));
    let project = env.store.get_project(&env.human, "AUTH").await.unwrap();
    assert!(project.require_review);
    assert_eq!(project.status, ProjectStatus::Active);

    // Nor by undoing a human's change: here, undoing the human turning review back on.
    env.store
        .update_project(&env.human, "AUTH", ProjectPatch { require_review: Some(false), ..Default::default() }, None)
        .await
        .unwrap();
    let on = env.human.next_batch();
    env.store
        .update_project(&on, "AUTH", ProjectPatch { require_review: Some(true), ..Default::default() }, None)
        .await
        .unwrap();
    assert!(matches!(env.store.undo_batch(&a.next_batch(), on.batch_id).await, Err(Error::Forbidden(_))));
    assert!(env.store.get_project(&env.human, "AUTH").await.unwrap().require_review);
    env.store.undo_batch(&env.human.next_batch(), on.batch_id).await.unwrap();
    assert!(!env.store.get_project(&env.human, "AUTH").await.unwrap().require_review);
}

#[tokio::test]
async fn handoff_is_returned_to_the_next_claimer() {
    let env = Env::new().await;
    let t = env.task("relay").await;
    let a = env.agent("a").await;
    let b = env.agent("b").await;
    env.store.claim(&a, &t.key, None).await.unwrap();
    env.store.add_comment(&a, &t.key, comment(CommentKind::Handoff, "old handoff")).await.unwrap();
    env.store.add_comment(&a, &t.key, comment(CommentKind::Handoff, "migrations written, tests left")).await.unwrap();
    env.store.add_comment(&a, &t.key, comment(CommentKind::Progress, "stopping")).await.unwrap();
    env.store.release(&a, &t.key).await.unwrap();
    let got = env.store.claim(&b, &t.key, None).await.unwrap();
    assert_eq!(got.handoff.unwrap().body, "migrations written, tests left");
}

#[tokio::test]
async fn questions_park_the_task_until_answered() {
    let env = Env::new().await;
    let t = env.task("ask").await;
    let a = env.agent("a").await;
    env.store.claim(&a, &t.key, None).await.unwrap();
    let q1 = env
        .store
        .add_comment(
            &a,
            &t.key,
            NewComment {
                kind: CommentKind::Question,
                body: "which provider?".into(),
                addressed_to: Some(env.human.user_id),
                verdict: None,
                model: None,
            },
        )
        .await
        .unwrap();
    let q2 = env.store.add_comment(&a, &t.key, comment(CommentKind::Question, "deadline?")).await.unwrap();
    assert_eq!(env.store.get_task(&env.human, &t.key).await.unwrap().status, Status::NeedsInput);

    env.store.resolve_comment(&env.human, q1.id, Some("GitHub".into())).await.unwrap();
    assert_eq!(env.store.get_task(&env.human, &t.key).await.unwrap().status, Status::NeedsInput);
    env.store.resolve_comment(&env.human, q2.id, None).await.unwrap();
    assert_eq!(env.store.get_task(&env.human, &t.key).await.unwrap().status, Status::InProgress);

    let comments = env.store.list_comments(&env.human, &t.key).await.unwrap();
    let answer = comments.iter().find(|c| c.body == "GitHub").unwrap();
    assert_eq!(answer.addressed_to, Some(a.actor_id()));

    // Unclaimed by the time it's answered: back to the ready queue.
    let u = env.task("unclaimed question").await;
    let q = env.store.add_comment(&a, &u.key, comment(CommentKind::Question, "?")).await.unwrap();
    assert!(!env.ready_keys().await.contains(&u.key));
    env.store.resolve_comment(&env.human, q.id, None).await.unwrap();
    assert!(env.ready_keys().await.contains(&u.key));
}

#[tokio::test]
async fn subtask_blocking_parent_unblocks_on_done() {
    let env = Env::new().await;
    let parent = env.task("parent").await;
    let a = env.agent("a").await;
    env.store.claim(&a, &parent.key, None).await.unwrap();
    let sub = env
        .store
        .create_task(
            &a,
            NewTask {
                project: "AUTH".into(),
                title: "sub".into(),
                parent: Some(parent.key.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(sub.parent_id, Some(parent.id));
    env.store.add_dependency(&a, &sub.key, &parent.key).await.unwrap();
    env.store.release(&a, &parent.key).await.unwrap();
    assert_eq!(env.ready_keys().await, vec![sub.key.clone()]);
    env.store.update_task(&env.human, &sub.key, status(Status::Done), None).await.unwrap();
    assert_eq!(env.ready_keys().await, vec![parent.key.clone()]);
    let detail = env.store.task_detail(&env.human, &sub.key).await.unwrap();
    assert_eq!(detail.blocks, vec![parent.key.clone()]);
}

#[tokio::test]
async fn parent_cycles_are_rejected() {
    let env = Env::new().await;
    let a = env.task("a").await;
    let b = env.task("b").await;
    let c = env.task("c").await;
    let under = |p: &str| TaskPatch { parent: Some(Some(p.to_string())), ..Default::default() };
    env.store.update_task(&env.human, &a.key, under(&b.key), None).await.unwrap();
    env.store.update_task(&env.human, &b.key, under(&c.key), None).await.unwrap();
    // Direct and indirect ancestors can't become the parent.
    for (task, parent) in [(&b, &a), (&c, &a), (&c, &b), (&a, &a)] {
        let err = env.store.update_task(&env.human, &task.key, under(&parent.key), None).await.unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err:?}");
    }
    let d = env.task("d").await;
    env.store.update_task(&env.human, &d.key, under(&a.key), None).await.unwrap();
}

#[tokio::test]
async fn undo_refuses_to_restore_a_parent_cycle() {
    let env = Env::new().await;
    let a = env.task("a").await;
    let b = env.task("b").await;
    let under = |p: Option<&str>| TaskPatch { parent: Some(p.map(str::to_string)), ..Default::default() };
    env.store.update_task(&env.human, &a.key, under(Some(&b.key)), None).await.unwrap();
    let unparent = env.human.next_batch();
    env.store.update_task(&unparent, &a.key, under(None), None).await.unwrap();
    env.store.update_task(&env.human, &b.key, under(Some(&a.key)), None).await.unwrap();
    // Undoing the unparenting would put a back under b while b is under a.
    let err = env.store.undo_batch(&env.human, unparent.batch_id).await.unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err:?}");
}

#[tokio::test]
async fn moving_under_an_existing_parent_cycle_is_rejected_without_hanging() {
    let env = Env::new().await;
    let a = env.task("a").await;
    let b = env.task("b").await;
    let c = env.task("c").await;
    // The API can't build a cycle, so write one straight into the table.
    let conn = env.raw_conn().await;
    for (task, parent) in [(&a, &b), (&b, &a)] {
        conn.execute(
            "UPDATE tasks SET parent_id = ?1 WHERE id = ?2",
            libsql::params![parent.id.to_string(), task.id.to_string()],
        )
        .await
        .unwrap();
    }
    let under_a = TaskPatch { parent: Some(Some(a.key.clone())), ..Default::default() };
    let moved = env.store.update_task(&env.human, &c.key, under_a, None);
    let err = tokio::time::timeout(std::time::Duration::from_secs(10), moved).await.expect("walk must terminate");
    assert!(matches!(err, Err(Error::Invalid(_))), "{err:?}");
}

#[tokio::test]
async fn dependency_cycles_are_rejected() {
    let env = Env::new().await;
    let a = env.task("a").await;
    let b = env.task("b").await;
    let c = env.task("c").await;
    env.store.add_dependency(&env.human, &a.key, &b.key).await.unwrap();
    env.store.add_dependency(&env.human, &b.key, &c.key).await.unwrap();
    assert!(matches!(env.store.add_dependency(&env.human, &c.key, &a.key).await, Err(Error::DependencyCycle)));
    assert!(matches!(env.store.add_dependency(&env.human, &a.key, &a.key).await, Err(Error::Invalid(_))));
}

#[tokio::test]
async fn undo_batch_reverses_a_claim() {
    let env = Env::new().await;
    let t = env.task("undo claim").await;
    let a = env.agent("a").await;
    env.store.claim(&a, &t.key, None).await.unwrap();
    let report = env.store.undo_batch(&env.human, a.batch_id).await.unwrap();
    assert_eq!(report.reverted.len(), 2);
    let detail = env.store.task_detail(&env.human, &t.key).await.unwrap();
    assert_eq!(detail.task.status, Status::Open);
    assert!(detail.claim.is_none());
    assert_eq!(env.ready_keys().await, vec![t.key.clone()]);
}

#[tokio::test]
async fn undo_create_soft_deletes_and_undo_again_restores() {
    let env = Env::new().await;
    let ctx = env.human.next_batch();
    let t = env
        .store
        .create_task(&ctx, NewTask { project: "AUTH".into(), title: "oops".into(), ..Default::default() })
        .await
        .unwrap();
    let undo_ctx = env.human.next_batch();
    env.store.undo_batch(&undo_ctx, ctx.batch_id).await.unwrap();
    assert!(matches!(env.store.get_task(&env.human, &t.key).await, Err(Error::NotFound(_))));
    env.store.undo_batch(&env.human.next_batch(), undo_ctx.batch_id).await.unwrap();
    let back = env.store.get_task(&env.human, &t.key).await.unwrap();
    assert_eq!(back.title, "oops");
}

#[tokio::test]
async fn undo_refuses_to_clobber_later_changes() {
    let env = Env::new().await;
    let t = env.task("v1").await;
    let first = env.human.next_batch();
    env.store
        .update_task(&first, &t.key, TaskPatch { title: Some("v2".into()), ..Default::default() }, None)
        .await
        .unwrap();
    env.store
        .update_task(
            &env.human.next_batch(),
            &t.key,
            TaskPatch { title: Some("v3".into()), ..Default::default() },
            None,
        )
        .await
        .unwrap();
    assert!(matches!(
        env.store.undo_batch(&env.human.next_batch(), first.batch_id).await,
        Err(Error::UndoConflict { .. })
    ));
    assert_eq!(env.store.get_task(&env.human, &t.key).await.unwrap().title, "v3");
}

#[tokio::test]
async fn revert_actor_skips_what_others_touched() {
    let env = Env::new().await;
    let keep = env.task("keep").await;
    let touched = env.task("touched").await;
    let a = env.agent("rogue").await;
    let since = env.now();
    env.advance(1);
    env.store
        .update_task(
            &a.next_batch(),
            &keep.key,
            TaskPatch { title: Some("vandalised".into()), ..Default::default() },
            None,
        )
        .await
        .unwrap();
    env.store
        .update_task(
            &a.next_batch(),
            &touched.key,
            TaskPatch { title: Some("vandalised".into()), ..Default::default() },
            None,
        )
        .await
        .unwrap();
    let junk = env
        .store
        .create_task(&a.next_batch(), NewTask { project: "AUTH".into(), title: "junk".into(), ..Default::default() })
        .await
        .unwrap();
    env.store.add_comment(&a.next_batch(), &keep.key, comment(CommentKind::Note, "spam")).await.unwrap();
    env.store
        .update_task(
            &env.human,
            &touched.key,
            TaskPatch { description: Some("human fix".into()), ..Default::default() },
            None,
        )
        .await
        .unwrap();

    let report = env.store.revert_actor(&env.human.next_batch(), a.actor_id(), since).await.unwrap();
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].entity_id, touched.id.to_string());
    assert_eq!(env.store.get_task(&env.human, &keep.key).await.unwrap().title, "keep");
    assert_eq!(env.store.get_task(&env.human, &touched.key).await.unwrap().title, "vandalised");
    assert!(env.store.get_task(&env.human, &junk.key).await.is_err());
    assert!(env.store.list_comments(&env.human, &keep.key).await.unwrap().is_empty());
}

#[tokio::test]
async fn search_covers_titles_descriptions_and_comments() {
    let env = Env::new().await;
    let a = env.task("Rotate OAuth secrets").await;
    let b = env
        .task_with(NewTask { title: "Login page".into(), description: "uses oauthlib".into(), ..Default::default() })
        .await;
    let c = env.task("Billing").await;
    env.store.add_comment(&env.human, &c.key, comment(CommentKind::Note, "blocked on OAuth too")).await.unwrap();
    let mut hits: Vec<_> =
        env.store.search(&env.human, "oauth", None).await.unwrap().into_iter().map(|t| t.key).collect();
    hits.sort();
    assert_eq!(hits, [a.key, b.key, c.key]);
    assert!(env.store.search(&env.human, "\"unbalanced AND (", None).await.unwrap().is_empty());
}

#[tokio::test]
async fn imports_dedupe_on_external_id() {
    let env = Env::new().await;
    let new = NewTask {
        title: "from todoist".into(),
        source: Some("todoist".into()),
        external_id: Some("123".into()),
        ..Default::default()
    };
    let first = env.task_with(new.clone()).await;
    let again = env.task_with(new).await;
    assert_eq!(first.id, again.id);
}

#[tokio::test]
async fn metadata_merges_and_filters() {
    let env = Env::new().await;
    let t = env
        .task_with(NewTask {
            title: "m".into(),
            metadata: Some(json!({"team": "web", "size": 3})),
            ..Default::default()
        })
        .await;
    env.task("other").await;
    let patched = env
        .store
        .update_task(
            &env.human,
            &t.key,
            TaskPatch { metadata: Some(json!({"size": null, "area": {"x": 1}})), ..Default::default() },
            None,
        )
        .await
        .unwrap();
    assert_eq!(patched.metadata, json!({"team": "web", "area": {"x": 1}}));
    let filter = TaskFilter {
        metadata: vec![("team".into(), "web".into()), ("area.x".into(), "1".into())],
        ..Default::default()
    };
    let found = env.store.list_tasks(&env.human, filter).await.unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].key, t.key);
}

#[tokio::test]
async fn every_write_is_attributed() {
    let env = Env::new().await;
    let a = env.agent("a").await;
    let t = env
        .store
        .create_task(&a, NewTask { project: "AUTH".into(), title: "x".into(), ..Default::default() })
        .await
        .unwrap();
    let events =
        env.store.list_events(&env.human, EventFilter { task: Some(t.key), ..Default::default() }).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].actor_id, a.actor_id());
    assert_eq!(events[0].on_behalf_of, env.human.user_id);
    assert_eq!(events[0].batch_id, a.batch_id);
}

#[tokio::test]
async fn in_memory_store_works() {
    let store = trailhead_core::Store::open_local(":memory:").await.unwrap();
    let (ws, user) = store.bootstrap_local("Lane").await.unwrap();
    let ctx = trailhead_core::Ctx::user(ws.id, user.id);
    store
        .create_project(&ctx, NewProject { key: "T".repeat(2), name: "t".into(), ..Default::default() })
        .await
        .unwrap();
    store.create_task(&ctx, NewTask { project: "TT".into(), title: "x".into(), ..Default::default() }).await.unwrap();
    assert_eq!(store.ready(&ctx, None, None, GradeFilter::Any).await.unwrap().len(), 1);
    let (ws2, _) = store.bootstrap_local("ignored").await.unwrap();
    assert_eq!(ws.id, ws2.id);
}

#[tokio::test]
async fn due_tz_must_be_a_real_zone() {
    let env = Env::new().await;
    let due = Some(env.at_in(SignedDuration::from_hours(24)));
    let ok = NewTask {
        title: "ok".into(),
        due_at: due.clone(),
        due_tz: Some("America/Los_Angeles".into()),
        ..Default::default()
    };
    assert_eq!(env.task_with(ok).await.due_tz.as_deref(), Some("America/Los_Angeles"));
    let bad = NewTask {
        project: "AUTH".into(),
        title: "bad".into(),
        due_at: due.clone(),
        due_tz: Some("Mars/Olympus_Mons".into()),
        ..Default::default()
    };
    assert!(matches!(env.store.create_task(&env.human, bad).await, Err(Error::Invalid(_))));
    // Slim container images have no /usr/share/zoneinfo; jiff falls back to its bundled copy.
    assert!(jiff::tz::TimeZoneDatabase::bundled().get("America/Los_Angeles").is_ok());
}

#[tokio::test]
async fn scheduling_is_separate_from_the_due_date() {
    let env = Env::new().await;
    let day = |d: i8| Some(date(2026, 10, d));
    let due = Some(env.at_in(SignedDuration::from_hours(7 * 24)));
    let earlier = env.task_with(NewTask { title: "earlier".into(), scheduled_on: day(16), ..Default::default() }).await;
    let today = env
        .task_with(NewTask { title: "today".into(), scheduled_on: day(17), due_at: due.clone(), ..Default::default() })
        .await;
    env.task_with(NewTask { title: "later".into(), scheduled_on: day(18), ..Default::default() }).await;
    env.task("unscheduled").await;
    assert_eq!(today.scheduled_on, day(17));
    assert!(today.due_at.is_some());

    let through = |d| TaskFilter { scheduled_through: d, ..Default::default() };
    let keys = |ts: Vec<trailhead_core::Task>| {
        let mut keys = ts.into_iter().map(|t| t.key).collect::<Vec<_>>();
        keys.sort();
        keys
    };
    let plan = env.store.list_tasks(&env.human, through(day(17))).await.unwrap();
    assert_eq!(keys(plan), vec![earlier.key.clone(), today.key.clone()]);

    let ctx = env.human.next_batch();
    let patch = TaskPatch { scheduled_on: Some(day(18)), ..Default::default() };
    let moved = env.store.update_task(&ctx, &earlier.key, patch, None).await.unwrap();
    assert_eq!(moved.scheduled_on, day(18));
    assert_eq!(keys(env.store.list_tasks(&env.human, through(day(17))).await.unwrap()), vec![today.key.clone()]);

    env.store.undo_batch(&env.human.next_batch(), ctx.batch_id).await.unwrap();
    assert_eq!(env.store.get_task(&env.human, &earlier.key).await.unwrap().scheduled_on, day(16));

    let clear = TaskPatch { scheduled_on: Some(None), ..Default::default() };
    let cleared = env.store.update_task(&env.human, &today.key, clear, None).await.unwrap();
    assert_eq!(cleared.scheduled_on, None);
    assert_eq!(cleared.due_at, today.due_at);
}

#[tokio::test]
async fn scheduled_times_set_the_day_in_their_zone() {
    let env = Env::new().await;
    let la = Some("America/Los_Angeles".to_string());
    // 6pm in Los Angeles is already the 18th in UTC.
    let evening = parse_at("2026-10-17T18:00:00-07:00").unwrap();
    let new = |title: &str| NewTask { title: title.into(), ..Default::default() };
    let t = env
        .task_with(NewTask { scheduled_at: Some(evening.clone()), scheduled_tz: la.clone(), ..new("evening") })
        .await;
    assert_eq!(t.scheduled_on, Some(date(2026, 10, 17)));
    assert_eq!(t.scheduled_at, Some(evening.timestamp()));

    // Without a zone, the timestamp's own offset decides the day, and it sticks.
    let bare = env.task_with(NewTask { scheduled_at: Some(evening.clone()), ..new("bare") }).await;
    assert_eq!(bare.scheduled_on, Some(date(2026, 10, 17)));
    let renamed = TaskPatch { title: Some("renamed".into()), ..Default::default() };
    let bare = env.store.update_task(&env.human, &bare.key, renamed, None).await.unwrap();
    assert_eq!(bare.scheduled_on, Some(date(2026, 10, 17)));

    let wrong_day = NewTask {
        project: "AUTH".into(),
        scheduled_on: Some(date(2026, 10, 18)),
        scheduled_at: Some(evening.clone()),
        scheduled_tz: la.clone(),
        ..new("wrong")
    };
    assert!(matches!(env.store.create_task(&env.human, wrong_day).await, Err(Error::Invalid(_))));
    let zone_alone = NewTask { project: "AUTH".into(), scheduled_tz: la.clone(), ..new("zone") };
    assert!(matches!(env.store.create_task(&env.human, zone_alone).await, Err(Error::Invalid(_))));

    // Dropping the time keeps the day; a new day drops the time; moving the time moves the day.
    let untimed = TaskPatch { scheduled_at: Some(None), ..Default::default() };
    let t2 = env.store.update_task(&env.human, &t.key, untimed, None).await.unwrap();
    assert_eq!((t2.scheduled_on, t2.scheduled_at, t2.scheduled_tz.as_deref()), (Some(date(2026, 10, 17)), None, None));
    let timed =
        TaskPatch { scheduled_at: Some(Some(evening.clone())), scheduled_tz: Some(la.clone()), ..Default::default() };
    env.store.update_task(&env.human, &t.key, timed, None).await.unwrap();
    let next_day = TaskPatch { scheduled_on: Some(Some(date(2026, 10, 19))), ..Default::default() };
    let t3 = env.store.update_task(&env.human, &t.key, next_day, None).await.unwrap();
    assert_eq!((t3.scheduled_on, t3.scheduled_at), (Some(date(2026, 10, 19)), None));
    let morning = parse_at("2026-10-20T09:00:00-07:00").unwrap();
    let moved =
        TaskPatch { scheduled_at: Some(Some(morning.clone())), scheduled_tz: Some(la.clone()), ..Default::default() };
    let t4 = env.store.update_task(&env.human, &t.key, moved, None).await.unwrap();
    assert_eq!((t4.scheduled_on, t4.scheduled_at), (Some(date(2026, 10, 20)), Some(morning.timestamp())));
}

#[tokio::test]
async fn deadlines_are_a_day_with_an_optional_time() {
    let env = Env::new().await;
    let la = Some("America/Los_Angeles".to_string());
    let new = |title: &str| NewTask { title: title.into(), ..Default::default() };
    let friday = Some(date(2026, 10, 23));
    let day_only = env.task_with(NewTask { due_on: friday, due_tz: la.clone(), ..new("by friday") }).await;
    assert_eq!((day_only.due_on, day_only.due_at), (friday, None));
    let five = parse_at("2026-10-23T17:00:00-07:00").unwrap();
    let timed = env.task_with(NewTask { due_at: Some(five.clone()), due_tz: la.clone(), ..new("friday 5pm") }).await;
    assert_eq!((timed.due_on, timed.due_at), (friday, Some(five.timestamp())));

    // A day-only deadline lasts to the end of its day in its zone.
    let keys = |f: TaskFilter| async {
        env.store.list_tasks(&env.human, f).await.unwrap().into_iter().map(|t| t.key).collect::<Vec<_>>()
    };
    let before = |t| TaskFilter { due_before: Some(t), ..Default::default() };
    assert_eq!(keys(before(parse_at("2026-10-23T18:00:00-07:00").unwrap().timestamp())).await, vec![timed.key.clone()]);
    assert_eq!(
        keys(before(parse_at("2026-10-24T00:00:00-07:00").unwrap().timestamp())).await,
        vec![timed.key.clone(), day_only.key.clone()]
    );
    assert_eq!(keys(TaskFilter::default()).await, vec![timed.key.clone(), day_only.key.clone()]);

    let zone_alone = NewTask { project: "AUTH".into(), due_tz: la.clone(), ..new("zone") };
    assert!(matches!(env.store.create_task(&env.human, zone_alone).await, Err(Error::Invalid(_))));

    let untimed = TaskPatch { due_at: Some(None), ..Default::default() };
    let t = env.store.update_task(&env.human, &timed.key, untimed, None).await.unwrap();
    assert_eq!((t.due_on, t.due_at, t.due_tz.as_deref()), (friday, None, Some("America/Los_Angeles")));
    let moved = TaskPatch { due_on: Some(Some(date(2026, 10, 26))), ..Default::default() };
    let t = env.store.update_task(&env.human, &timed.key, moved, None).await.unwrap();
    assert_eq!((t.due_on, t.due_at), (Some(date(2026, 10, 26)), None));
    let cleared = TaskPatch { due_on: Some(None), ..Default::default() };
    let t = env.store.update_task(&env.human, &timed.key, cleared, None).await.unwrap();
    assert_eq!((t.due_on, t.due_at, t.due_tz), (None, None, None));
}

#[tokio::test]
async fn scheduled_sort_puts_timed_tasks_in_order_within_a_day() {
    let env = Env::new().await;
    let new = |title: &str| NewTask { title: title.into(), ..Default::default() };
    let day = Some(date(2026, 10, 17));
    env.task_with(NewTask { scheduled_on: day, priority: Some(1), ..new("untimed urgent") }).await;
    env.task_with(NewTask { scheduled_at: Some(parse_at("2026-10-17T15:00:00Z").unwrap()), ..new("3pm") }).await;
    env.task_with(NewTask { scheduled_at: Some(parse_at("2026-10-17T09:00:00Z").unwrap()), ..new("9am") }).await;
    env.task_with(NewTask { scheduled_on: Some(date(2026, 10, 16)), ..new("yesterday") }).await;
    env.task_with(new("unscheduled")).await;
    let filter = TaskFilter { sort: TaskSort::Scheduled, ..Default::default() };
    let titles: Vec<_> = env.store.list_tasks(&env.human, filter).await.unwrap().into_iter().map(|t| t.title).collect();
    assert_eq!(titles, ["yesterday", "9am", "3pm", "untimed urgent", "unscheduled"]);
}

#[test]
fn snapshots_from_before_scheduling_still_load() {
    let mut v = serde_json::to_value(trailhead_core::Task {
        id: uuid::Uuid::nil(),
        workspace_id: uuid::Uuid::nil(),
        project_id: uuid::Uuid::nil(),
        key: "AUTH-1".into(),
        title: "t".into(),
        description: String::new(),
        status: Status::Open,
        priority: 3,
        grade: None,
        duplicate_of: None,
        due_on: None,
        due_at: None,
        due_tz: None,
        scheduled_on: Some(date(2026, 10, 17)),
        scheduled_at: None,
        scheduled_tz: None,
        parent_id: None,
        metadata: json!({}),
        version: 1,
        source: None,
        external_id: None,
        created_by: uuid::Uuid::nil(),
        created_at: jiff::Timestamp::UNIX_EPOCH,
        updated_at: jiff::Timestamp::UNIX_EPOCH,
        deleted_at: None,
    })
    .unwrap();
    assert_eq!(v["scheduled_on"], "2026-10-17");
    for k in ["scheduled_on", "scheduled_at", "scheduled_tz", "due_on"] {
        v.as_object_mut().unwrap().remove(k);
    }
    let old: trailhead_core::Task = serde_json::from_value(v).unwrap();
    assert_eq!(old.scheduled_on, None);
}

#[tokio::test]
async fn lost_claims_are_reported_on_renew() {
    let env = Env::new().await;
    let t = env.task("lease").await;
    let a = env.agent("a").await;
    let b = env.agent("b").await;
    env.store.claim(&a, &t.key, Some(60)).await.unwrap();
    let mine = env.store.my_claims(&a).await.unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].key, t.key);
    assert!(env.store.my_claims(&b).await.unwrap().is_empty());

    // Expired but not yet reaped.
    env.advance(61);
    assert!(env.store.my_claims(&a).await.unwrap().is_empty());
    assert!(matches!(env.store.renew(&a, &t.key, None).await, Err(Error::ClaimLost(ref r)) if r.contains("expired")));

    // Reaped, then taken by someone else.
    env.store.claim(&b, &t.key, None).await.unwrap();
    match env.store.renew(&a, &t.key, None).await {
        Err(Error::ClaimLost(reason)) => assert!(reason.starts_with("b claimed it"), "{reason}"),
        other => panic!("expected claim_lost, got {other:?}"),
    }

    // Never held it at all: plain not_claim_holder.
    let c = env.agent("c").await;
    assert!(matches!(env.store.renew(&c, &t.key, None).await, Err(Error::NotClaimHolder)));
}

#[tokio::test]
async fn agent_names_are_unique() {
    let env = Env::new().await;
    env.store.create_agent(&env.human, "bot").await.unwrap();
    assert!(matches!(env.store.create_agent(&env.human, "bot").await, Err(Error::AlreadyExists(_))));
    assert!(matches!(env.store.create_agent(&env.human, " bot ").await, Err(Error::AlreadyExists(_))));
}

#[tokio::test]
async fn delegated_reviewers_can_approve() {
    let env = Env::new().await;
    let worker = env.agent("worker").await;
    let lead = env.agent("lead").await;
    let lead_name = vec!["lead".to_string()];
    // Agents can't hand themselves approval rights.
    assert!(matches!(
        env.store
            .update_project(
                &lead,
                "AUTH",
                ProjectPatch { reviewers: Some(lead_name.clone()), ..Default::default() },
                None
            )
            .await,
        Err(Error::Forbidden(_))
    ));
    let patch = ProjectPatch { require_review: Some(true), reviewers: Some(lead_name), ..Default::default() };
    let project = env.store.update_project(&env.human, "AUTH", patch, None).await.unwrap();
    assert_eq!(project.reviewers, vec![lead.agent_id.unwrap()]);

    let t = env.task("needs review").await;
    env.store.claim(&worker, &t.key, None).await.unwrap();
    assert!(matches!(
        env.store.update_task(&worker, &t.key, status(Status::Done), None).await,
        Err(Error::ReviewRequired)
    ));
    env.store.update_task(&worker, &t.key, status(Status::InReview), None).await.unwrap();
    assert!(matches!(
        env.store.update_task(&worker, &t.key, status(Status::Done), None).await,
        Err(Error::ReviewRequired)
    ));
    let done = env.store.update_task(&lead, &t.key, status(Status::Done), None).await.unwrap();
    assert_eq!(done.status, Status::Done);
}

#[tokio::test]
async fn approved_sits_between_review_and_done() {
    let env = Env::new().await;
    let worker = env.agent("worker").await;
    let lead = env.agent("lead").await;
    let patch = ProjectPatch { reviewers: Some(vec!["lead".to_string()]), ..Default::default() };
    env.store.update_project(&env.human, "AUTH", patch, None).await.unwrap();

    let t = env.task("approve me").await;
    env.store.claim(&worker, &t.key, None).await.unwrap();
    // Only work in review can be approved.
    assert!(matches!(
        env.store.update_task(&env.human, &t.key, status(Status::Approved), None).await,
        Err(Error::InvalidTransition { .. })
    ));
    env.store.update_task(&worker, &t.key, status(Status::InReview), None).await.unwrap();
    // The worker can't approve or finish its own work.
    for to in [Status::Approved, Status::Done] {
        assert!(matches!(env.store.update_task(&worker, &t.key, status(to), None).await, Err(Error::ReviewRequired)));
    }
    // A delegated reviewer approves; the claim stays released and the task isn't ready.
    let approved = env.store.update_task(&lead, &t.key, status(Status::Approved), None).await.unwrap();
    assert_eq!(approved.status, Status::Approved);
    assert!(env.store.task_detail(&env.human, &t.key).await.unwrap().claim.is_none());
    assert!(env.ready_keys().await.is_empty());
    let listed = TaskFilter { statuses: vec![Status::Approved], ..Default::default() };
    assert_eq!(env.store.list_tasks(&env.human, listed).await.unwrap().len(), 1);
    // Finishing needs the same rights as finishing from review.
    assert!(matches!(
        env.store.update_task(&worker, &t.key, status(Status::Done), None).await,
        Err(Error::ReviewRequired)
    ));
    // Changes needed: back to review, then approved again, then done by a human.
    let back = env.store.update_task(&env.human, &t.key, status(Status::InReview), None).await.unwrap();
    assert_eq!(back.status, Status::InReview);
    env.store.update_task(&env.human, &t.key, status(Status::Approved), None).await.unwrap();
    let done = env.store.update_task(&env.human, &t.key, status(Status::Done), None).await.unwrap();
    assert_eq!(done.status, Status::Done);

    // Approved can also go back to open, and in_review can finish directly.
    let u = env.task("sent back").await;
    env.store.update_task(&env.human, &u.key, status(Status::InReview), None).await.unwrap();
    let ev = env.store.update_task(&env.human, &u.key, status(Status::Approved), None).await.unwrap();
    assert_eq!(ev.status, Status::Approved);
    env.store.update_task(&env.human, &u.key, status(Status::Open), None).await.unwrap();
    assert_eq!(env.ready_keys().await, vec![u.key.clone()]);
    env.store.update_task(&env.human, &u.key, status(Status::InReview), None).await.unwrap();
    let direct = env.store.update_task(&env.human, &u.key, status(Status::Done), None).await.unwrap();
    assert_eq!(direct.status, Status::Done);
}

#[tokio::test]
async fn reviewers_cannot_approve_their_own_work() {
    let env = Env::new().await;
    let lead = env.agent("lead").await;
    let other = env.agent("other").await;
    let patch = ProjectPatch {
        require_review: Some(true),
        reviewers: Some(vec!["lead".into(), "other".into()]),
        ..Default::default()
    };
    env.store.update_project(&env.human, "AUTH", patch, None).await.unwrap();

    let t = env.task("mine").await;
    env.store.claim(&lead, &t.key, None).await.unwrap();
    // Holding it, it can't skip review.
    assert!(matches!(
        env.store.update_task(&lead, &t.key, status(Status::Approved), None).await,
        Err(Error::InvalidTransition { .. })
    ));
    env.store.update_task(&lead, &t.key, status(Status::InReview), None).await.unwrap();
    for to in [Status::Approved, Status::Done] {
        assert!(matches!(env.store.update_task(&lead, &t.key, status(to), None).await, Err(Error::Forbidden(_))));
    }
    env.store.update_task(&other, &t.key, status(Status::Approved), None).await.unwrap();
    // Nor can it finish what another reviewer approved.
    assert!(matches!(env.store.update_task(&lead, &t.key, status(Status::Done), None).await, Err(Error::Forbidden(_))));
    let done = env.store.update_task(&other, &t.key, status(Status::Done), None).await.unwrap();
    assert_eq!(done.status, Status::Done);

    // A reviewer who only moved a task to review (no claim) can't approve it either.
    let u = env.task("pushed").await;
    env.store.update_task(&lead, &u.key, status(Status::InReview), None).await.unwrap();
    assert!(matches!(
        env.store.update_task(&lead, &u.key, status(Status::Approved), None).await,
        Err(Error::Forbidden(_))
    ));

    // A reviewer holding in-progress work still finishes it where the plan allows.
    let w = env.task("held").await;
    env.store.claim(&lead, &w.key, None).await.unwrap();
    assert!(env.store.update_task(&lead, &w.key, status(Status::Done), None).await.is_ok());
}

#[tokio::test]
async fn a_reviewer_approves_work_it_only_parked_in_an_earlier_round() {
    let env = Env::new().await;
    let factory = env.agent("factory").await;
    let worker = env.agent("worker").await;
    let patch = ProjectPatch { reviewers: Some(vec!["factory".into()]), ..Default::default() };
    env.store.update_project(&env.human, "AUTH", patch, None).await.unwrap();

    // The factory claims a task only to park it; a worker later does the work.
    let t = env.task("parked once").await;
    env.store.claim(&factory, &t.key, None).await.unwrap();
    env.store.update_task(&factory, &t.key, status(Status::Open), None).await.unwrap();
    env.store.claim(&worker, &t.key, None).await.unwrap();
    env.store.update_task(&worker, &t.key, status(Status::InReview), None).await.unwrap();
    env.store.update_task(&factory, &t.key, status(Status::Approved), None).await.unwrap();
    let done = env.store.update_task(&factory, &t.key, status(Status::Done), None).await.unwrap();
    assert_eq!(done.status, Status::Done);

    // The last claim before the move into review still counts when someone else makes the move.
    let u = env.task("sent by another").await;
    env.store.claim(&factory, &u.key, None).await.unwrap();
    env.store.update_task(&env.human, &u.key, status(Status::InReview), None).await.unwrap();
    assert!(matches!(
        env.store.update_task(&factory, &u.key, status(Status::Done), None).await,
        Err(Error::Forbidden(_))
    ));
}

#[tokio::test]
async fn held_work_cannot_skip_to_approved() {
    let env = Env::new().await;
    let lead = env.agent("lead").await;
    let patch = ProjectPatch { reviewers: Some(vec!["lead".to_string()]), ..Default::default() };
    env.store.update_project(&env.human, "AUTH", patch, None).await.unwrap();
    // A held task can't jump to approved.
    let held = env.task("held").await;
    env.store.claim(&lead, &held.key, None).await.unwrap();
    assert!(matches!(
        env.store.update_task(&lead, &held.key, status(Status::Approved), None).await,
        Err(Error::InvalidTransition { .. })
    ));
    // A reviewer still finishes its own work in a project without required review.
    let own = env.task("own").await;
    env.store.claim(&lead, &own.key, None).await.unwrap();
    let done = env.store.update_task(&lead, &own.key, status(Status::Done), None).await.unwrap();
    assert_eq!(done.status, Status::Done);
}

#[tokio::test]
async fn undo_reverses_approval() {
    let env = Env::new().await;
    let t = env.task("undo approve").await;
    env.store.update_task(&env.human, &t.key, status(Status::InReview), None).await.unwrap();
    let human = env.human.next_batch();
    env.store.update_task(&human, &t.key, status(Status::Approved), None).await.unwrap();
    env.store.undo_batch(&env.human, human.batch_id).await.unwrap();
    assert_eq!(env.store.get_task(&env.human, &t.key).await.unwrap().status, Status::InReview);
}

#[tokio::test]
async fn list_tasks_sorts_and_pages() {
    let env = Env::new().await;
    let a = env.task_with(NewTask { title: "a".into(), priority: Some(1), ..Default::default() }).await;
    env.advance(1);
    let b = env.task_with(NewTask { title: "b".into(), priority: Some(3), ..Default::default() }).await;
    env.advance(1);
    let c = env.task_with(NewTask { title: "c".into(), priority: Some(2), ..Default::default() }).await;
    env.advance(1);
    env.store.update_task(&env.human, &a.key, status(Status::Done), None).await.unwrap();
    let list = async |sort: &str, limit, offset| {
        let filter = TaskFilter {
            statuses: vec![Status::Open, Status::Done],
            sort: sort.parse().unwrap(),
            limit,
            offset,
            ..Default::default()
        };
        env.store.list_tasks(&env.human, filter).await.unwrap().into_iter().map(|t| t.key).collect::<Vec<_>>()
    };
    assert_eq!(list("priority", None, None).await, [a.key.clone(), c.key.clone(), b.key.clone()]);
    assert_eq!(list("created_at", None, None).await, [a.key.clone(), b.key.clone(), c.key.clone()]);
    assert_eq!(list("-created_at", None, None).await, [c.key.clone(), b.key.clone(), a.key.clone()]);
    assert_eq!(list("-updated_at", None, None).await, [a.key.clone(), c.key.clone(), b.key.clone()]);
    assert_eq!(list("updated_at", Some(2), None).await, [b.key.clone(), c.key.clone()]);
    assert_eq!(list("updated_at", Some(2), Some(2)).await, std::slice::from_ref(&a.key));
    assert!(matches!("updated".parse::<TaskSort>(), Err(Error::Invalid(_))));
}

#[tokio::test]
async fn list_events_filters_kinds_before_limit() {
    let env = Env::new().await;
    let t = env.task("x").await;
    let a = env.agent("a").await;
    env.store.claim(&a, &t.key, None).await.unwrap();
    for i in 0..5 {
        let patch = TaskPatch { title: Some(format!("x{i}")), ..Default::default() };
        env.store.update_task(&env.human, &t.key, patch, None).await.unwrap();
    }
    env.store.add_comment(&a, &t.key, comment(CommentKind::Note, "hi")).await.unwrap();
    let filter = EventFilter {
        kinds: vec!["claim.".into(), "comment.created".into()],
        newest_first: true,
        limit: Some(2),
        ..Default::default()
    };
    let kinds: Vec<_> = env.store.list_events(&env.human, filter).await.unwrap().into_iter().map(|e| e.kind).collect();
    assert_eq!(kinds.len(), 2);
    assert_eq!(kinds[0], "comment.created");
    assert!(kinds[1].starts_with("claim."), "{kinds:?}");
}

#[tokio::test]
async fn list_events_filters_by_project_and_pages_back() {
    let env = Env::new().await;
    env.store
        .create_project(&env.human, NewProject { key: "WEB".into(), name: "Web".into(), ..Default::default() })
        .await
        .unwrap();
    let auth = env.task("auth").await;
    let web = env.task_with(NewTask { project: "WEB".into(), title: "web".into(), ..Default::default() }).await;
    env.store.add_comment(&env.human, &auth.key, comment(CommentKind::Note, "a")).await.unwrap();
    env.store.add_comment(&env.human, &web.key, comment(CommentKind::Note, "w")).await.unwrap();
    let list = async |project: &str, before, limit| {
        let filter = EventFilter {
            project: Some(project.into()),
            before_id: before,
            limit,
            newest_first: true,
            ..Default::default()
        };
        env.store.list_events(&env.human, filter).await.unwrap()
    };
    let web_events = list("web", None, None).await;
    assert_eq!(
        web_events.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
        ["comment.created", "task.created", "project.created"]
    );
    assert!(web_events.iter().all(|e| e.task_key.as_deref().is_none_or(|k| k == web.key)));
    let first = list("WEB", None, Some(1)).await;
    let rest = list("WEB", Some(first[0].id), None).await;
    assert_eq!(rest.len(), 2);
    assert_eq!(rest[0].kind, "task.created");
    assert!(matches!(
        env.store.list_events(&env.human, EventFilter { project: Some("NOPE".into()), ..Default::default() }).await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn search_filters_and_says_where_it_matched() {
    let env = Env::new().await;
    env.store
        .create_project(&env.human, NewProject { key: "WEB".into(), name: "Web".into(), ..Default::default() })
        .await
        .unwrap();
    let titled = env.task("Rotate the signing keys").await;
    let described = env
        .task_with(NewTask {
            title: "Housekeeping".into(),
            description: "Before launch we must rotate every credential the old vendor saw.".into(),
            ..Default::default()
        })
        .await;
    let commented =
        env.task_with(NewTask { project: "WEB".into(), title: "Login page".into(), ..Default::default() }).await;
    let note = env
        .store
        .add_comment(&env.human, &commented.key, comment(CommentKind::Note, "remember to rotate the cookie secret"))
        .await
        .unwrap();
    env.store.update_task(&env.human, &titled.key, status(Status::Done), None).await.unwrap();

    let search = async |project: Option<&str>, statuses: Vec<Status>| {
        let filter = SearchFilter { query: "rotate".into(), project: project.map(Into::into), statuses, limit: None };
        env.store.search_hits(&env.human, filter).await.unwrap()
    };
    let hits = search(None, vec![]).await;
    let by_key = |k: &str| hits.iter().find(|h| h.task.key == k).unwrap();
    assert_eq!(hits.last().unwrap().task.key, titled.key, "finished work sorts last");
    assert_eq!(by_key(&titled.key).matched.field, MatchField::Title);
    assert_eq!(by_key(&described.key).matched.field, MatchField::Description);
    assert!(by_key(&described.key).matched.snippet.contains("rotate every credential"));
    let c = &by_key(&commented.key).matched;
    assert_eq!(c.field, MatchField::Comment);
    assert_eq!(c.comment_id, Some(note.id));
    assert_eq!(c.snippet, "remember to rotate the cookie secret");

    let web: Vec<_> = search(Some("WEB"), vec![]).await.into_iter().map(|h| h.task.key).collect();
    assert_eq!(web, std::slice::from_ref(&commented.key));
    let open: Vec<_> = search(None, vec![Status::Open]).await.into_iter().map(|h| h.task.key).collect();
    assert_eq!(open.len(), 2);
    assert!(!open.contains(&titled.key));
}

#[tokio::test]
async fn search_by_key_puts_that_task_first() {
    let env = Env::new().await;
    let target = env.task("Rate limits per process").await;
    let mentions = env.task("Shared limiter").await;
    env.store
        .add_comment(&env.human, &mentions.key, comment(CommentKind::Note, &format!("split out of {}", target.key)))
        .await
        .unwrap();

    let hits = env.store.search(&env.human, &target.key.to_lowercase(), None).await.unwrap();
    let keys: Vec<_> = hits.iter().map(|t| t.key.clone()).collect();
    assert_eq!(keys, [target.key.clone(), mentions.key.clone()]);

    let filter = SearchFilter { query: target.key.clone(), statuses: vec![Status::Done], ..Default::default() };
    assert!(env.store.search_hits(&env.human, filter).await.unwrap().is_empty(), "filters still apply");
}

#[tokio::test]
async fn reviews_need_a_verdict_and_only_reviews_have_one() {
    let env = Env::new().await;
    let t = env.task("rotate keys").await;
    let s = &env.store;

    let r = s.add_comment(&env.human, &t.key, NewComment::review(Verdict::Approve, "looks right")).await.unwrap();
    assert_eq!((r.kind, r.verdict), (CommentKind::Review, Some(Verdict::Approve)));
    let back = s.list_comments(&env.human, &t.key).await.unwrap();
    assert_eq!(back, [r]);

    let no_verdict = NewComment::new(CommentKind::Review, "hmm");
    assert!(matches!(s.add_comment(&env.human, &t.key, no_verdict).await, Err(Error::Invalid(_))));
    let stray = NewComment { verdict: Some(Verdict::Approve), ..NewComment::new(CommentKind::Note, "x") };
    assert!(matches!(s.add_comment(&env.human, &t.key, stray).await, Err(Error::Invalid(_))));
    let stray = NewComment { model: Some("opus".into()), ..NewComment::new(CommentKind::Note, "x") };
    assert!(matches!(s.add_comment(&env.human, &t.key, stray).await, Err(Error::Invalid(_))));
    assert_eq!(s.get_task(&env.human, &t.key).await.unwrap().status, Status::Open, "a review doesn't move the task");
}

#[tokio::test]
async fn only_humans_can_set_lower_or_clear_a_grade() {
    let env = Env::new().await;
    let a = env.agent("a").await;
    let graded = |g: Option<Option<i64>>| TaskPatch { grade: g, ..Default::default() };
    let new = |title: &str, grade| NewTask { project: "AUTH".into(), title: title.into(), grade, ..Default::default() };

    // An agent can't create a graded task, though an ungraded one is fine.
    assert!(matches!(env.store.create_task(&a.next_batch(), new("graded", Some(2))).await, Err(Error::Forbidden(_))));
    let task = env.store.create_task(&a.next_batch(), new("plain", None)).await.unwrap();
    assert_eq!(task.grade, None);

    // Nor set the first grade; a human can, and validates the range.
    let update =
        async |ctx: &trailhead_core::Ctx, g| env.store.update_task(&ctx.next_batch(), &task.key, graded(g), None).await;
    assert!(matches!(update(&a, Some(Some(1))).await, Err(Error::Forbidden(_))));
    assert!(matches!(update(&env.human, Some(Some(5))).await, Err(Error::Invalid(_))));
    assert!(matches!(update(&env.human, Some(Some(0))).await, Err(Error::Invalid(_))));
    assert_eq!(update(&env.human, Some(Some(2))).await.unwrap().grade, Some(2));

    // An agent can raise it, restate it, and change other fields, but not lower or clear it.
    update(&a, Some(Some(2))).await.unwrap();
    assert_eq!(update(&a, Some(Some(3))).await.unwrap().grade, Some(3));
    assert!(matches!(update(&a, Some(Some(2))).await, Err(Error::Forbidden(_))));
    assert!(matches!(update(&a, Some(None)).await, Err(Error::Forbidden(_))));
    let retitled = TaskPatch { title: Some("renamed".into()), ..Default::default() };
    env.store.update_task(&a.next_batch(), &task.key, retitled, None).await.unwrap();
    assert_eq!(env.store.get_task(&env.human, &task.key).await.unwrap().grade, Some(3));

    // A human can lower and clear it.
    assert_eq!(update(&env.human, Some(Some(1))).await.unwrap().grade, Some(1));
    assert_eq!(update(&env.human, Some(None)).await.unwrap().grade, None);

    // The event log records the change.
    let log =
        env.store.list_events(&env.human, EventFilter { task: Some(task.key.clone()), ..Default::default() }).await;
    assert!(log.unwrap().iter().any(|e| e.kind == "task.updated" && e.after.as_ref().is_some_and(|t| t["grade"] == 3)));
}

#[tokio::test]
async fn agents_cannot_undo_or_revert_a_humans_grade_change() {
    let env = Env::new().await;
    let a = env.agent("a").await;
    let task = env.task_with(NewTask { title: "t".into(), grade: Some(1), ..Default::default() }).await;
    assert_eq!(task.grade, Some(1));
    let patch = |g| TaskPatch { grade: Some(g), ..Default::default() };

    // The human raises 1 to 3. An agent undoing that would lower it.
    let raise = env.human.next_batch();
    env.store.update_task(&raise, &task.key, patch(Some(3)), None).await.unwrap();
    assert!(matches!(env.store.undo_batch(&a.next_batch(), raise.batch_id).await, Err(Error::Forbidden(_))));
    assert_eq!(env.store.get_task(&env.human, &task.key).await.unwrap().grade, Some(3));

    // Likewise undoing the human clearing it would set a grade.
    let clear = env.human.next_batch();
    env.store.update_task(&clear, &task.key, patch(None), None).await.unwrap();
    assert!(matches!(env.store.undo_batch(&a.next_batch(), clear.batch_id).await, Err(Error::Forbidden(_))));
    assert_eq!(env.store.get_task(&env.human, &task.key).await.unwrap().grade, None);

    // A human can undo it, and an agent can undo its own raise only by being human, so it can't.
    env.store.undo_batch(&env.human.next_batch(), clear.batch_id).await.unwrap();
    assert_eq!(env.store.get_task(&env.human, &task.key).await.unwrap().grade, Some(3));
    let own = a.next_batch();
    env.store.update_task(&own, &task.key, patch(Some(4)), None).await.unwrap();
    assert!(matches!(env.store.undo_batch(&a.next_batch(), own.batch_id).await, Err(Error::Forbidden(_))));
    env.store.undo_batch(&env.human.next_batch(), own.batch_id).await.unwrap();
    assert_eq!(env.store.get_task(&env.human, &task.key).await.unwrap().grade, Some(3));

    // Reverting an agent's work: a human can, another agent can't lower the grade.
    let b = env.agent("b").await;
    let other = env.task_with(NewTask { title: "other".into(), grade: Some(2), ..Default::default() }).await;
    let since = env.now() - jiff::SignedDuration::from_secs(60);
    env.store.update_task(&a.next_batch(), &other.key, patch(Some(4)), None).await.unwrap();
    let report = env.store.revert_actor(&b.next_batch(), a.actor_id(), since).await;
    assert!(report.is_err() || report.unwrap().reverted.is_empty());
    assert_eq!(env.store.get_task(&env.human, &other.key).await.unwrap().grade, Some(4));
    env.store.revert_actor(&env.human.next_batch(), a.actor_id(), since).await.unwrap();
    assert_eq!(env.store.get_task(&env.human, &other.key).await.unwrap().grade, Some(2));
}

#[tokio::test]
async fn ready_and_list_filter_by_grade() {
    let env = Env::new().await;
    let graded = |title: &str, g| NewTask { title: title.into(), grade: Some(g), ..Default::default() };
    env.task("none").await;
    env.task_with(graded("one", 1)).await;
    env.task_with(graded("three", 3)).await;
    let titles = |tasks: Vec<trailhead_core::Task>| tasks.into_iter().map(|t| t.title).collect::<Vec<_>>();
    let ready = async |g| titles(env.store.ready(&env.human, None, None, g).await.unwrap());
    let list = async |g| {
        let f = TaskFilter { grade: g, sort: TaskSort::CreatedAt, ..Default::default() };
        titles(env.store.list_tasks(&env.human, f).await.unwrap())
    };
    assert_eq!(ready(GradeFilter::Any).await.len(), 3);
    assert_eq!(ready(GradeFilter::Ungraded).await, ["none"]);
    assert_eq!(ready(GradeFilter::Is(3)).await, ["three"]);
    assert_eq!(list(GradeFilter::Graded).await, ["one", "three"]);
    assert_eq!(list(GradeFilter::Ungraded).await, ["none"]);
    assert_eq!(list(GradeFilter::Is(1)).await, ["one"]);
    assert!(GradeFilter::parse("9").is_err());
    assert_eq!(GradeFilter::parse("graded").unwrap(), GradeFilter::Graded);
}

#[tokio::test]
async fn a_cancelled_task_records_what_it_duplicates() {
    let env = Env::new().await;
    let orig = env.task("CI is red on main").await;
    let dup = env.task("main CI broken").await;
    let dup_of = |key: Option<&str>, status| TaskPatch {
        status,
        duplicate_of: Some(key.map(String::from)),
        ..Default::default()
    };

    // Only a cancelled task can be a duplicate, and not of itself.
    let open = dup_of(Some(&orig.key), None);
    assert!(matches!(env.store.update_task(&env.human, &dup.key, open, None).await, Err(Error::Invalid(_))));
    let itself = dup_of(Some(&dup.key), Some(Status::Cancelled));
    assert!(matches!(env.store.update_task(&env.human, &dup.key, itself, None).await, Err(Error::Invalid(_))));

    let done = dup_of(Some(&orig.key), Some(Status::Cancelled));
    let cancelled = env.store.update_task(&env.human, &dup.key, done, None).await.unwrap();
    assert_eq!(cancelled.duplicate_of, Some(orig.id));
    let d = env.store.task_detail(&env.human, &dup.key).await.unwrap();
    assert_eq!(d.duplicate_of.as_deref(), Some(orig.key.as_str()));
    let d = env.store.task_detail(&env.human, &orig.key).await.unwrap();
    assert_eq!(d.duplicates, vec![dup.key.clone()]);

    // Reopening clears it.
    let reopened = env.store.update_task(&env.human, &dup.key, status(Status::Open), None).await.unwrap();
    assert_eq!(reopened.duplicate_of, None);
    assert!(env.store.task_detail(&env.human, &orig.key).await.unwrap().duplicates.is_empty());
}

#[tokio::test]
async fn undoing_a_duplicate_cancel_restores_the_task() {
    let env = Env::new().await;
    let orig = env.task("CI is red on main").await;
    let dup = env.task("main CI broken").await;
    let patch =
        TaskPatch { status: Some(Status::Cancelled), duplicate_of: Some(Some(orig.key.clone())), ..Default::default() };
    let batch = env.human.next_batch();
    env.store.update_task(&batch, &dup.key, patch, None).await.unwrap();
    env.store.undo_batch(&env.human, batch.batch_id).await.unwrap();
    let back = env.store.get_task(&env.human, &dup.key).await.unwrap();
    assert_eq!((back.status, back.duplicate_of), (Status::Open, None));
}

#[tokio::test]
async fn similar_tasks_finds_unfinished_titles_that_share_most_words() {
    let env = Env::new().await;
    let ci = env.task("CI is failing on the main branch").await;
    env.task("Write the release notes").await;
    let finished = env.task("main branch CI failing again").await;
    env.store.update_task(&env.human, &finished.key, status(Status::Cancelled), None).await.unwrap();
    let new = env.task("Fix failing CI on main").await;

    let keys = |ts: Vec<trailhead_core::Task>| ts.into_iter().map(|t| t.key).collect::<Vec<_>>();
    let similar = env.store.similar_tasks(&env.human, "AUTH", &new.title, Some(new.id)).await.unwrap();
    assert_eq!(keys(similar), vec![ci.key.clone()]);
    // One shared word isn't enough.
    assert!(env.store.similar_tasks(&env.human, "AUTH", "Notes on main", None).await.unwrap().is_empty());
}
