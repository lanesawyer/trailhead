mod common;

use std::sync::{Arc, Mutex};

use common::Env;
use time::{Duration, OffsetDateTime};
use trailhead_core::{
    Ctx, DEFAULT_REMINDER_WINDOW, DUE_SOON, EventFilter, NewProject, NewTask, OVERDUE, Status, Store, TaskPatch,
};

const WINDOW: Duration = DEFAULT_REMINDER_WINDOW;

async fn emit(env: &Env) -> Vec<(String, String)> {
    let events = env.store.emit_due_reminders(&env.human.next_batch(), WINDOW).await.unwrap();
    events.into_iter().map(|e| (e.task_key.unwrap_or_else(|| e.task_id.unwrap().to_string()), e.kind)).collect()
}

async fn due_task(env: &Env, title: &str, due_in: Duration) -> trailhead_core::Task {
    env.task_with(NewTask { title: title.into(), due_at: Some(env.now() + due_in), ..Default::default() }).await
}

fn kinds(fired: &[(String, String)]) -> Vec<&str> {
    fired.iter().map(|(_, k)| k.as_str()).collect()
}

#[tokio::test]
async fn fires_once_inside_the_window_and_not_before() {
    let env = Env::new().await;
    let t = due_task(&env, "ship", Duration::hours(30)).await;
    env.task("no due date").await;

    assert!(emit(&env).await.is_empty(), "30h out is outside the 24h window");

    env.advance(7 * 3600);
    let fired = env.store.emit_due_reminders(&env.human.next_batch(), WINDOW).await.unwrap();
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].kind, DUE_SOON);
    assert_eq!(fired[0].task_id, Some(t.id));
    assert_eq!(fired[0].actor_id, env.human.user_id, "attributed to the workspace owner");

    env.advance(60);
    assert!(emit(&env).await.is_empty(), "never twice for the same due date");

    env.advance(23 * 3600);
    assert_eq!(kinds(&emit(&env).await), vec![OVERDUE]);
    env.advance(3600);
    assert!(emit(&env).await.is_empty());
}

#[tokio::test]
async fn first_seen_late_only_fires_overdue() {
    let env = Env::new().await;
    due_task(&env, "late", Duration::hours(1)).await;
    env.advance(2 * 3600);
    assert_eq!(kinds(&emit(&env).await), vec![OVERDUE]);
}

#[tokio::test]
async fn skips_done_cancelled_and_deleted_tasks() {
    let env = Env::new().await;
    let done = due_task(&env, "done", Duration::hours(1)).await;
    let cancelled = due_task(&env, "cancelled", Duration::hours(1)).await;
    let deleted = due_task(&env, "deleted", Duration::hours(1)).await;
    let open = due_task(&env, "open", Duration::hours(1)).await;
    let set = |s| TaskPatch { status: Some(s), ..Default::default() };
    env.store.update_task(&env.human, &done.key, set(Status::Done), None).await.unwrap();
    env.store.update_task(&env.human, &cancelled.key, set(Status::Cancelled), None).await.unwrap();
    env.store.delete_task(&env.human, &deleted.key, None).await.unwrap();

    let fired = env.store.emit_due_reminders(&env.human.next_batch(), WINDOW).await.unwrap();
    assert_eq!(fired.iter().map(|e| e.task_id.unwrap()).collect::<Vec<_>>(), vec![open.id]);
}

#[tokio::test]
async fn fires_again_after_due_date_moves_later() {
    let env = Env::new().await;
    let t = due_task(&env, "moving", Duration::hours(2)).await;
    assert_eq!(kinds(&emit(&env).await), vec![DUE_SOON]);

    let later = env.now() + Duration::hours(48);
    let patch = TaskPatch { due_at: Some(Some(later)), ..Default::default() };
    env.store.update_task(&env.human, &t.key, patch, None).await.unwrap();
    assert!(emit(&env).await.is_empty(), "the new due date is outside the window");

    env.advance(25 * 3600);
    assert_eq!(kinds(&emit(&env).await), vec![DUE_SOON]);
    assert!(emit(&env).await.is_empty());
}

#[tokio::test]
async fn undo_treats_reminders_as_no_ops() {
    let env = Env::new().await;
    let t = due_task(&env, "undo me", Duration::hours(2)).await;
    let edit = env.human.next_batch();
    let patch = TaskPatch { title: Some("renamed".into()), ..Default::default() };
    env.store.update_task(&edit, &t.key, patch, None).await.unwrap();

    let reminders = env.human.next_batch();
    assert_eq!(env.store.emit_due_reminders(&reminders, WINDOW).await.unwrap().len(), 1);

    // "Undo my last change" looks past notifications to the edit.
    let last = env
        .store
        .list_events(
            &env.human,
            EventFilter {
                actor_id: Some(env.human.user_id),
                newest_first: true,
                changes_only: true,
                limit: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(last[0].batch_id, edit.batch_id);

    let report = env.store.undo_batch(&env.human.next_batch(), reminders.batch_id).await.unwrap();
    assert!(report.reverted.is_empty());
    assert_eq!(env.store.get_task(&env.human, &t.key).await.unwrap().title, "renamed");

    // A reminder after an edit is not a conflict for undoing the edit.
    env.store.undo_batch(&env.human.next_batch(), edit.batch_id).await.unwrap();
    assert_eq!(env.store.get_task(&env.human, &t.key).await.unwrap().title, "undo me");
}

#[tokio::test]
async fn exactly_once_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let clock = Arc::new(Mutex::new(OffsetDateTime::now_utc()));
    let open = async |clock: Arc<Mutex<OffsetDateTime>>| {
        let c = clock.clone();
        Store::open_local(&path).await.unwrap().with_clock(Arc::new(move || *c.lock().unwrap()))
    };

    let store = open(clock.clone()).await;
    let (ws, user) = store.bootstrap_local("Lane").await.unwrap();
    let ctx = Ctx::user(ws.id, user.id);
    store
        .create_project(&ctx, NewProject { key: "AUTH".into(), name: "Auth".into(), ..Default::default() })
        .await
        .unwrap();
    let due = Some(*clock.lock().unwrap() + Duration::hours(1));
    let new = NewTask { project: "AUTH".into(), title: "t".into(), due_at: due, ..Default::default() };
    store.create_task(&ctx, new).await.unwrap();
    assert_eq!(store.emit_due_reminders(&ctx.next_batch(), WINDOW).await.unwrap().len(), 1);
    drop(store);

    let store = open(clock.clone()).await;
    assert!(store.emit_due_reminders(&ctx.next_batch(), WINDOW).await.unwrap().is_empty());
}
