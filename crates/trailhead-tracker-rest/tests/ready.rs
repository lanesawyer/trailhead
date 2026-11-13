//! `RestTracker::ready` lists what `CoreTracker::ready` does: reworks too, ahead of new work in a priority.

use std::sync::Arc;

use trailhead_control::CoreTracker;
use trailhead_core::{
    CommentKind, Ctx, NewComment, NewProject, NewTask, Principal, Priority, Status, Store, TaskPatch, Verdict,
};
use trailhead_factory::{Credential, TaskKey, Tracker};
use trailhead_tracker_rest::RestTracker;

async fn add(s: &Store, h: &Ctx, title: &str, priority: u8) -> TaskKey {
    let new = NewTask {
        project: "TRAIL".into(),
        title: title.into(),
        priority: Priority::new(priority),
        ..Default::default()
    };
    TaskKey(s.create_task(h, new).await.unwrap().key.to_string())
}

/// Puts the task in review with its newest review asking for changes.
async fn rework(s: &Store, h: &Ctx, key: &TaskKey) {
    let patch = TaskPatch { status: Some(Status::InReview), ..Default::default() };
    s.update_task(h, &key.0, patch, None).await.unwrap();
    let review =
        NewComment { verdict: Some(Verdict::RequestChanges), ..NewComment::new(CommentKind::Review, "Fix it.") };
    s.add_comment(h, &key.0, review).await.unwrap();
}

#[tokio::test]
async fn ready_lists_reworks_ahead_of_new_work() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_local(dir.path().join("t.db").to_str().unwrap()).await.unwrap();
    let (ws, user) = store.bootstrap_local("Lane").await.unwrap();
    let human = Ctx::user(ws.id, user.id);
    let project = NewProject { key: "TRAIL".into(), name: "Trailhead".into(), ..Default::default() };
    store.create_project(&human, project).await.unwrap();
    let agent = store.create_agent(&human, "factory").await.unwrap();
    let (_, token) = store.create_token(&human, Principal::Agent(agent.id), "t").await.unwrap();

    let fresh = add(&store, &human, "new, same priority", 2).await;
    let waiting = add(&store, &human, "rework", 2).await;
    let blocked = add(&store, &human, "blocked rework", 2).await;
    let blocker = add(&store, &human, "blocker", 3).await;
    let held = add(&store, &human, "held rework", 2).await;
    let urgent = add(&store, &human, "urgent, blocked by the rework", 1).await;
    let reviewed = add(&store, &human, "approved, not a rework", 2).await;
    for key in [&waiting, &blocked, &held] {
        rework(&store, &human, key).await;
    }
    store.add_dependency(&human, &blocker.0, &blocked.0).await.unwrap();
    store.add_dependency(&human, &waiting.0, &urgent.0).await.unwrap();
    store.claim(&human, &held.0, None).await.unwrap();
    let patch = TaskPatch { status: Some(Status::InReview), ..Default::default() };
    store.update_task(&human, &reviewed.0, patch, None).await.unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = trailhead_server::router(Arc::new(trailhead_server::AppState::new(store)));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let tracker = RestTracker::new(format!("http://{addr}"), Credential { agent: "factory".into(), token });

    let ready = tracker.ready("TRAIL", &[], 10).await.unwrap();
    let listed: Vec<_> = ready.iter().map(|t| (t.key.0.as_str(), t.priority.get(), t.changes_requested)).collect();
    // The rework blocks an urgent task, so it is raised to that priority and leads, and the blocker is raised to what it
    // blocks; the blocked and held reworks and
    // the plain in-review task are left out.
    assert_eq!(
        listed,
        [(waiting.0.as_str(), 1, true), (fresh.0.as_str(), 2, false), (blocker.0.as_str(), 2, false)],
        "{listed:?}"
    );

    let cut = tracker.ready("TRAIL", &[], 1).await.unwrap();
    assert_eq!(cut.len(), 1);
    assert_eq!(cut[0].key, waiting);
}

/// The control plane reads the queue through `CoreTracker` and a daemon through `RestTracker`: one store, one answer.
#[tokio::test]
async fn the_rest_tracker_and_the_core_tracker_list_the_same_ready_tasks_in_the_same_order() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_local(dir.path().join("t.db").to_str().unwrap()).await.unwrap());
    let (ws, user) = store.bootstrap_local("Lane").await.unwrap();
    let human = Ctx::user(ws.id, user.id);
    let project = NewProject { key: "TRAIL".into(), name: "Trailhead".into(), ..Default::default() };
    store.create_project(&human, project).await.unwrap();
    let agent = store.create_agent(&human, "factory").await.unwrap();
    let (_, token) = store.create_token(&human, Principal::Agent(agent.id), "t").await.unwrap();

    let queue = add(&store, &human, "queue", 4).await;
    let under = |title: &str, priority: u8| NewTask {
        project: "TRAIL".into(),
        title: title.into(),
        priority: Priority::new(priority),
        parent: Some(queue.0.clone().into()),
        ..Default::default()
    };
    let mut queued = Vec::new();
    for (title, priority) in [("first", 2), ("second", 2), ("rework", 3), ("low blocker of urgent work", 4)] {
        queued.push(TaskKey(store.create_task(&human, under(title, priority)).await.unwrap().key.to_string()));
    }
    let urgent = add(&store, &human, "urgent, blocked", 1).await;
    add(&store, &human, "outside the queue", 1).await;
    rework(&store, &human, &queued[2]).await;
    store.add_dependency(&human, &queued[3].0, &urgent.0).await.unwrap();

    let core = CoreTracker::new(store.clone(), store.agent_ctx(ws.id, agent.id).await.unwrap(), "factory");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = trailhead_server::router(Arc::new(trailhead_server::AppState::new(store.clone())));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let rest = RestTracker::new(format!("http://{addr}"), Credential { agent: "factory".into(), token });

    let listed = |tasks: Vec<trailhead_factory::Task>| -> Vec<(String, u8, bool)> {
        tasks.into_iter().map(|t| (t.key.0, t.priority.get(), t.changes_requested)).collect()
    };
    for queue in [vec![], vec![queue.clone()]] {
        for limit in [10, 2, 1] {
            let from_core = listed(core.ready("TRAIL", &queue, limit).await.unwrap());
            let from_rest = listed(rest.ready("TRAIL", &queue, limit).await.unwrap());
            assert_eq!(from_rest, from_core, "queue {queue:?}, limit {limit}");
        }
    }
    let first = core.ready("TRAIL", &[queue], 1).await.unwrap();
    assert_eq!(first[0].key, queued[3], "the blocker of urgent work leads, raised to its priority");
}
