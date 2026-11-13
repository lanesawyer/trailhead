//! Writes that interfaces used to compose from several calls, each now one transaction and one undo batch.

mod common;

use common::Env;
use trailhead_core::{
    CommentKind, Error, NewComment, NewTask, Principal, ProjectPatch, Status, TaskFilter, TaskKey, TaskPatch,
    TokenOptions, Verdict,
};

fn keys(ks: &[&TaskKey]) -> Vec<TaskKey> {
    ks.iter().map(|k| (*k).clone()).collect()
}

async fn titles(env: &Env) -> Vec<String> {
    env.store.list_tasks(&env.human, TaskFilter::default()).await.unwrap().into_iter().map(|t| t.title).collect()
}

fn finish_with(s: Status, handoff: &str) -> TaskPatch {
    TaskPatch { status: Some(s), handoff: Some(handoff.into()), ..Default::default() }
}

async fn handoffs(env: &Env, task: &TaskKey) -> Vec<String> {
    let comments = env.store.list_comments(&env.human, task).await.unwrap();
    comments.into_iter().filter(|c| c.kind == CommentKind::Handoff).map(|c| c.body).collect()
}

#[tokio::test]
async fn a_refused_dependency_creates_no_task() {
    let env = Env::new().await;
    let a = env.task("a").await;
    let b = env.task("b").await;
    env.store.add_dependency(&env.human, &a.key, &b.key).await.unwrap();

    // c after b, and c before a, would close a -> b -> c -> a.
    let cycle = NewTask {
        project: "AUTH".into(),
        title: "c".into(),
        blocked_by: keys(&[&b.key]),
        blocks: keys(&[&a.key]),
        ..Default::default()
    };
    let ctx = env.human.next_batch();
    assert!(matches!(env.store.create_task(&ctx, cycle).await, Err(Error::DependencyCycle)));
    let missing = NewTask {
        project: "AUTH".into(),
        title: "d".into(),
        blocked_by: vec![TaskKey("AUTH-99".into())],
        ..Default::default()
    };
    assert!(matches!(env.store.create_task(&env.human.next_batch(), missing).await, Err(Error::NotFound(_))));

    assert_eq!(titles(&env).await, ["a", "b"]);
    assert!(matches!(env.store.undo_batch(&env.human.next_batch(), ctx.batch_id).await, Err(Error::NotFound(_))));
    // The refused creates didn't use up a key either.
    assert_eq!(env.task("e").await.key, "AUTH-3");
}

#[tokio::test]
async fn dependencies_come_with_the_task_and_undo_with_it() {
    let env = Env::new().await;
    let before = env.task("before").await;
    let after = env.task("after").await;
    let ctx = env.human.next_batch();
    let new = NewTask {
        project: "AUTH".into(),
        title: "middle".into(),
        blocked_by: keys(&[&before.key]),
        blocks: keys(&[&after.key]),
        ..Default::default()
    };
    let middle = env.store.create_task(&ctx, new).await.unwrap();
    let blocker_keys = |v: Vec<trailhead_core::Task>| v.into_iter().map(|t| t.key).collect::<Vec<_>>();
    assert_eq!(
        blocker_keys(env.store.blockers(&env.human, &middle.key).await.unwrap()),
        std::slice::from_ref(&before.key)
    );
    assert_eq!(
        blocker_keys(env.store.blockers(&env.human, &after.key).await.unwrap()),
        std::slice::from_ref(&middle.key)
    );
    assert_eq!(env.ready_keys().await, std::slice::from_ref(&before.key));

    let report = env.store.undo_batch(&env.human.next_batch(), ctx.batch_id).await.unwrap();
    assert_eq!(report.reverted.len(), 3);
    assert!(matches!(env.store.get_task(&env.human, &middle.key).await, Err(Error::NotFound(_))));
    assert!(env.store.blockers(&env.human, &after.key).await.unwrap().is_empty());
    assert_eq!(env.ready_keys().await, [before.key, after.key]);
}

#[tokio::test]
async fn a_refused_status_leaves_no_handoff_and_keeps_the_review_open() {
    let env = Env::new().await;
    let s = &env.store;
    s.update_project(&env.human, "AUTH", ProjectPatch { require_review: Some(true), ..Default::default() }, None)
        .await
        .unwrap();
    let t = env.task("reworked").await;
    let a = env.agent("a").await;
    s.update_task(&env.human, &t.key, TaskPatch { status: Some(Status::InReview), ..Default::default() }, None)
        .await
        .unwrap();
    let ask = s.add_comment(&env.human, &t.key, NewComment::review(Verdict::RequestChanges, "fix it")).await.unwrap();
    s.claim(&a, &t.key, None).await.unwrap();

    // Only review finishes work here, so the agent's done is refused, and its handoff with it.
    let refused = s.update_task(&a.next_batch(), &t.key, finish_with(Status::Done, "all done"), None).await;
    assert!(matches!(refused, Err(Error::ReviewRequired)));
    assert!(handoffs(&env, &t.key).await.is_empty());
    let invalid = s.update_task(&a.next_batch(), &t.key, finish_with(Status::Approved, "approve myself"), None).await;
    assert!(matches!(invalid, Err(Error::InvalidTransition { .. })));
    assert!(handoffs(&env, &t.key).await.is_empty());

    // The review still asks for changes, so a rework given back returns to review.
    assert_eq!(s.release(&a, &t.key).await.unwrap().status, Status::InReview);
    assert_eq!(s.task_detail(&env.human, &t.key).await.unwrap().changes_requested, Some(ask.id));
}

#[tokio::test]
async fn status_and_handoff_are_one_batch() {
    let env = Env::new().await;
    let s = &env.store;
    let t = env.task("work").await;
    let a = env.agent("a").await;
    s.claim(&a.next_batch(), &t.key, None).await.unwrap();
    let ctx = a.next_batch();
    let sent = s.update_task(&ctx, &t.key, finish_with(Status::InReview, "  ready for review "), None).await.unwrap();
    assert_eq!(sent.status, Status::InReview);
    assert_eq!(handoffs(&env, &t.key).await, ["ready for review"]);
    assert!(s.task_detail(&env.human, &t.key).await.unwrap().claim.is_none());

    // Undo puts back the status and the claim and takes the handoff away.
    s.undo_batch(&a.next_batch(), ctx.batch_id).await.unwrap();
    let d = s.task_detail(&env.human, &t.key).await.unwrap();
    assert_eq!(d.task.status, Status::InProgress);
    assert_eq!(d.claim.map(|c| c.holder_id), a.agent_id);
    assert!(handoffs(&env, &t.key).await.is_empty());

    // A handoff alone is still written, and an empty one is refused.
    s.update_task(
        &a.next_batch(),
        &t.key,
        TaskPatch { handoff: Some("stepping away".into()), ..Default::default() },
        None,
    )
    .await
    .unwrap();
    assert_eq!(handoffs(&env, &t.key).await, ["stepping away"]);
    let empty = s.update_task(&a.next_batch(), &t.key, finish_with(Status::Done, " "), None).await;
    assert!(matches!(empty, Err(Error::Invalid(_))));
    assert_eq!(s.get_task(&env.human, &t.key).await.unwrap().status, Status::InProgress);
}

#[tokio::test]
async fn a_refused_token_creates_no_agent() {
    let env = Env::new().await;
    let s = &env.store;
    let (agent, token, secret) =
        s.create_agent_with_token(&env.human, "worker", TokenOptions::default()).await.unwrap();
    assert_eq!((token.principal, token.name.as_str()), (Principal::Agent(agent.id), "worker"));
    assert_eq!(s.authenticate(&secret).await.unwrap().agent_id, Some(agent.id));

    let refused = TokenOptions { rate_limit_per_minute: Some(0), ..Default::default() };
    assert!(matches!(s.create_agent_with_token(&env.human, "never", refused).await, Err(Error::Invalid(_))));
    let elsewhere = TokenOptions { projects: Some(vec!["NOPE".into()]), ..Default::default() };
    assert!(matches!(s.create_agent_with_token(&env.human, "nowhere", elsewhere).await, Err(Error::NotFound(_))));
    let names: Vec<_> = s.list_agents(&env.human).await.unwrap().into_iter().map(|a| a.name).collect();
    assert_eq!(names, ["worker"]);

    // An agent's sub-agent gets a token in the same call, capped by the agent's own.
    let (sub, sub_token, _) = s
        .create_agent_with_token(
            &s.authenticate(&secret).await.unwrap(),
            "helper",
            TokenOptions { rate_limit_per_minute: Some(5), ..Default::default() },
        )
        .await
        .unwrap();
    assert_eq!(sub.name, "worker/helper");
    assert_eq!(sub_token.rate_limit_per_minute, Some(5));
}
