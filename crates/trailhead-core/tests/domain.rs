mod common;

use common::Env;
use serde_json::json;
use time::Duration;
use trailhead_core::{
    CommentKind, Error, EventFilter, NewComment, NewProject, NewTask, ProjectPatch, Status, TaskFilter, TaskPatch,
};

fn status(s: Status) -> TaskPatch {
    TaskPatch { status: Some(s), ..Default::default() }
}

fn comment(kind: CommentKind, body: &str) -> NewComment {
    NewComment { kind, body: body.into(), addressed_to: None }
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
    let now = env.now();
    let low = env.task_with(NewTask { title: "low".into(), priority: Some(4), ..Default::default() }).await;
    let late = env
        .task_with(NewTask {
            title: "late".into(),
            priority: Some(2),
            due_at: Some(now + Duration::days(9)),
            ..Default::default()
        })
        .await;
    let soon = env
        .task_with(NewTask {
            title: "soon".into(),
            priority: Some(2),
            due_at: Some(now + Duration::days(1)),
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
    assert_eq!(c.expires_at - c.claimed_at, Duration::minutes(30));
    env.store
        .update_project(&env.human, "AUTH", ProjectPatch { claim_ttl: Some(Some(120)), ..Default::default() }, None)
        .await
        .unwrap();
    let c = env.store.renew(&a, &t.key, None).await.unwrap();
    assert_eq!(c.expires_at - env.now(), Duration::seconds(120));
    assert!(matches!(env.store.renew(&a, &t.key, Some(90_000)).await, Err(Error::Invalid(_))));
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
        env.store.update_task(&env.human, &t.key, status(Status::InReview), None).await,
        Err(Error::InvalidTransition { .. })
    ));
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
    assert_eq!(store.ready(&ctx, None, None).await.unwrap().len(), 1);
    let (ws2, _) = store.bootstrap_local("ignored").await.unwrap();
    assert_eq!(ws.id, ws2.id);
}

#[tokio::test]
async fn due_tz_must_be_a_real_zone() {
    let env = Env::new().await;
    let due = Some(env.now() + Duration::days(1));
    let ok =
        NewTask { title: "ok".into(), due_at: due, due_tz: Some("America/Los_Angeles".into()), ..Default::default() };
    assert_eq!(env.task_with(ok).await.due_tz.as_deref(), Some("America/Los_Angeles"));
    let bad = NewTask {
        project: "AUTH".into(),
        title: "bad".into(),
        due_at: due,
        due_tz: Some("Mars/Olympus_Mons".into()),
        ..Default::default()
    };
    assert!(matches!(env.store.create_task(&env.human, bad).await, Err(Error::Invalid(_))));
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
