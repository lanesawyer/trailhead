mod common;

use common::Env;
use jiff::SignedDuration;
use trailhead_core::{Error, EventFilter, Status, TaskPatch};

const DAY: i64 = 24 * 60 * 60;

fn history(task: &str, include_archived: bool) -> EventFilter {
    EventFilter { task: Some(task.into()), include_archived, ..Default::default() }
}

async fn ids(env: &Env, filter: EventFilter) -> Vec<i64> {
    env.store.list_events(&env.human, filter).await.unwrap().into_iter().map(|e| e.id).collect()
}

#[tokio::test]
async fn archives_after_ninety_days_keeping_ids() {
    let env = Env::new().await;
    let t = env.task("old").await;
    let patch = TaskPatch { status: Some(Status::Done), ..Default::default() };
    env.store.update_task(&env.human.next_batch(), &t.key, patch, None).await.unwrap();
    let before = ids(&env, history(&t.key, false)).await;
    assert_eq!(before.len(), 2);

    env.advance(89 * DAY);
    assert_eq!(env.store.archive_events().await.unwrap(), 0);
    assert_eq!(ids(&env, history(&t.key, false)).await, before);

    env.advance(2 * DAY);
    let recent = env.task("recent").await;
    let moved = env.store.archive_events().await.unwrap();
    assert!(moved >= 2, "moved {moved}");
    assert_eq!(env.store.archive_events().await.unwrap(), 0);

    assert!(ids(&env, history(&t.key, false)).await.is_empty());
    assert_eq!(ids(&env, history(&t.key, true)).await, before);
    let all = ids(&env, EventFilter::default()).await;
    assert!(all.iter().all(|id| !before.contains(id)));
    assert_eq!(ids(&env, history(&recent.key, false)).await.len(), 1);
}

#[tokio::test]
async fn undo_refuses_events_past_the_window() {
    let env = Env::new().await;
    let ctx = env.human.next_batch();
    let t = env
        .store
        .create_task(&ctx, trailhead_core::NewTask { title: "x".into(), project: "AUTH".into(), ..Default::default() })
        .await
        .unwrap();
    let event = ids(&env, history(&t.key, false)).await[0];

    env.advance(91 * DAY);
    let undo = env.store.undo_event(&env.human.next_batch(), event).await;
    assert!(matches!(undo, Err(Error::UndoWindowExpired(_))), "{undo:?}");

    assert!(env.store.archive_events().await.unwrap() >= 1);
    let undo = env.store.undo_event(&env.human.next_batch(), event).await;
    assert!(matches!(undo, Err(Error::UndoWindowExpired(_))), "{undo:?}");
    let undo = env.store.undo_batch(&env.human.next_batch(), ctx.batch_id).await;
    assert!(matches!(undo, Err(ref e) if e.code() == "undo_window_expired"), "{undo:?}");

    assert!(matches!(env.store.undo_event(&env.human.next_batch(), 999_999).await, Err(Error::NotFound(_))));
}

#[tokio::test]
async fn revert_clamps_since_to_the_window() {
    let env = Env::new().await;
    let a = env.agent("bot").await;
    let t = env.task("t").await;
    let start = env.now();
    let title = |s: &str| TaskPatch { title: Some(s.into()), ..Default::default() };
    env.store.update_task(&a.next_batch(), &t.key, title("old edit"), None).await.unwrap();
    env.advance(91 * DAY);
    env.store.update_task(&a.next_batch(), &t.key, title("new edit"), None).await.unwrap();

    let report = env
        .store
        .revert_actor(&env.human.next_batch(), a.actor_id(), start - SignedDuration::from_hours(24))
        .await
        .unwrap();
    assert_eq!(report.reverted.len(), 1);
    assert!(report.skipped.is_empty());
    assert_eq!(env.store.get_task(&env.human, &t.key).await.unwrap().title, "old edit");
}

#[tokio::test]
async fn archive_moves_only_a_prefix_by_id() {
    let env = Env::new().await;
    let first = env.task("first").await;
    *env.clock.lock().unwrap() -= SignedDuration::from_hours(100 * 24);
    let backdated = env.task("backdated").await;
    *env.clock.lock().unwrap() += SignedDuration::from_hours(100 * 24);

    // `backdated` is old by its timestamp but follows an event inside the
    // window, so it stays put: nothing in `events` may predate the archive.
    assert_eq!(env.store.archive_events().await.unwrap(), 0);
    assert_eq!(ids(&env, history(&backdated.key, false)).await.len(), 1);

    env.advance(91 * DAY);
    assert!(env.store.archive_events().await.unwrap() >= 2);
    assert!(ids(&env, history(&first.key, false)).await.is_empty());
    assert!(ids(&env, history(&backdated.key, false)).await.is_empty());
}
