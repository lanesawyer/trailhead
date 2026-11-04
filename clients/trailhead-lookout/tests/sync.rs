//! The sync and the mirror against an in-process server: writes made while the snapshot loads, a dropped stream,
//! and a disconnect too long to resume.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::prelude::*;
use serde_json::Value;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use trailhead_core::{Ctx, NewComment, NewProject, NewTask, Principal, Store, TaskPatch};
use trailhead_lookout::mirror::{Data, IsClaim, IsComment, IsProject, IsTask, Mirror, MirrorPlugin, Rev};
use trailhead_lookout::sync::{self, Remote, Report, SyncItem, SyncTimings};

struct Rig {
    _dir: tempfile::TempDir,
    state: Arc<trailhead_server::AppState>,
    ctx: Ctx,
    server: SocketAddr,
    token: String,
}

impl Rig {
    async fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("trailhead.db");
        let store = Store::open_local(db.to_str().unwrap()).await.unwrap();
        let (ws, user) = store.bootstrap_local("Lane").await.unwrap();
        let ctx = Ctx::user(ws.id, user.id);
        let project = NewProject { key: "TRAIL".into(), name: "Trailhead".into(), ..Default::default() };
        store.create_project(&ctx, project).await.unwrap();
        let (_, token) = store.create_token(&ctx, Principal::User(user.id), "ui").await.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = listener.local_addr().unwrap();
        let state = Arc::new(trailhead_server::AppState::new(store));
        let app = trailhead_server::router(state.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Rig { _dir: dir, state, ctx, server, token }
    }

    async fn task(&self, title: &str) -> Value {
        let new = NewTask { project: "TRAIL".into(), title: title.into(), ..Default::default() };
        serde_json::to_value(self.state.store.create_task(&self.ctx, new).await.unwrap()).unwrap()
    }

    async fn retitle(&self, task: &str, title: &str) {
        let patch = TaskPatch { title: Some(title.into()), ..Default::default() };
        self.state.store.update_task(&self.ctx, task, patch, None).await.unwrap();
    }

    fn remote(&self, base: SocketAddr, timings: &SyncTimings) -> Remote {
        Remote::new(format!("http://{base}"), self.token.clone(), timings)
    }
}

fn fast() -> SyncTimings {
    SyncTimings {
        retry: Duration::from_millis(50),
        long_disconnect: Duration::from_secs(60),
        silence: Duration::from_secs(5),
    }
}

/// A TCP forwarder in front of the server that can cut every open connection, or refuse new ones.
struct Proxy {
    addr: SocketAddr,
    conns: Arc<Mutex<Vec<JoinHandle<()>>>>,
    down: Arc<AtomicBool>,
}

impl Proxy {
    async fn new(to: SocketAddr) -> Proxy {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let conns = Arc::new(Mutex::new(Vec::<JoinHandle<()>>::new()));
        let down = Arc::new(AtomicBool::new(false));
        let (c, d) = (conns.clone(), down.clone());
        tokio::spawn(async move {
            while let Ok((mut client, _)) = listener.accept().await {
                if d.load(Ordering::SeqCst) {
                    continue; // dropped: the client sees a closed connection
                }
                let handle = tokio::spawn(async move {
                    if let Ok(mut upstream) = TcpStream::connect(to).await {
                        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                    }
                });
                c.lock().unwrap().push(handle);
            }
        });
        Proxy { addr, conns, down }
    }

    fn cut(&self) {
        for conn in self.conns.lock().unwrap().drain(..) {
            conn.abort();
        }
    }
}

/// A running sync whose items and reports can be waited on.
struct Running {
    task: JoinHandle<()>,
    items: std::sync::mpsc::Receiver<SyncItem>,
    seen: Vec<SyncItem>,
    reports: Arc<Mutex<Vec<Report>>>,
}

impl Running {
    fn start(remote: Remote, timings: SyncTimings) -> Running {
        let (tx, items) = std::sync::mpsc::channel();
        let reports = Arc::new(Mutex::new(Vec::new()));
        let r = reports.clone();
        let task = tokio::spawn(sync::run(
            remote,
            timings,
            move |item| tx.send(item).is_ok(),
            move |report| r.lock().unwrap().push(report),
        ));
        Running { task, items, seen: Vec::new(), reports }
    }

    async fn until(&mut self, what: &str, done: impl Fn(&[SyncItem], &[Report]) -> bool) {
        for _ in 0..200 {
            self.seen.extend(self.items.try_iter());
            if done(&self.seen, &self.reports.lock().unwrap()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("never reached: {what}; saw {:?}, reports {:?}", self.seen.len(), self.reports.lock().unwrap());
    }

    fn snapshots(&self) -> usize {
        self.seen.iter().filter(|i| matches!(i, SyncItem::Snapshot(_))).count()
    }

    fn event_ids(&self) -> Vec<i64> {
        self.seen
            .iter()
            .filter_map(|i| match i {
                SyncItem::Event(e) => e["id"].as_i64(),
                SyncItem::Snapshot(_) => None,
            })
            .collect()
    }
}

fn has_title(items: &[SyncItem], title: &str) -> bool {
    items.iter().any(|i| matches!(i, SyncItem::Event(e) if e["after"]["title"] == title))
}

fn mirror_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins).add_plugins(MirrorPlugin);
    app
}

fn feed(app: &mut App, items: impl IntoIterator<Item = SyncItem>) {
    for item in items {
        app.world_mut().write_message(item);
    }
    app.update();
}

/// Each task's title and version, sorted by title.
fn tasks(app: &mut App) -> Vec<(String, i64)> {
    let world = app.world_mut();
    let mut out: Vec<_> = world
        .query_filtered::<(&Data, &Rev), With<IsTask>>()
        .iter(world)
        .map(|(d, r)| (d.0["title"].as_str().unwrap().to_string(), r.0))
        .collect();
    out.sort();
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_during_the_snapshot_are_neither_lost_nor_applied_twice() {
    let rig = Rig::new().await;
    let a = rig.task("A").await;
    let a_id = a["id"].as_str().unwrap().to_string();
    let remote = rig.remote(rig.server, &fast());

    // Step 1, then writes while step 2 is under way: the snapshot sees them, and so will the stream.
    let at = remote.newest_event_id().await.unwrap();
    rig.retitle(&a_id, "A2").await;
    rig.task("B").await;
    let snapshot = remote.load_snapshot(at).await.unwrap();
    assert_eq!(snapshot.at, at);
    // A write after step 2 has read: only the stream carries it.
    rig.task("C").await;
    rig.retitle(&a_id, "A3").await;
    let newest = remote.newest_event_id().await.unwrap();

    // Step 3 from the id noted in step 1.
    let (tx, rx) = std::sync::mpsc::channel();
    let mut last = at;
    let _ = tokio::time::timeout(Duration::from_secs(2), remote.stream(&mut last, || {}, |item| tx.send(item).is_ok()))
        .await;
    let events: Vec<_> = rx.try_iter().collect();
    assert_eq!(last, newest);
    let ids: Vec<_> = events
        .iter()
        .map(|i| match i {
            SyncItem::Event(e) => e["id"].as_i64().unwrap(),
            SyncItem::Snapshot(_) => unreachable!(),
        })
        .collect();
    assert!(ids.iter().all(|id| *id > at), "the stream starts after step 1's id: {ids:?}");

    let mut app = mirror_app();
    feed(&mut app, [SyncItem::Snapshot(snapshot)]);
    assert_eq!(tasks(&mut app), [("A2".to_string(), 2), ("B".to_string(), 1)]);

    // Replaying what the snapshot already has changes nothing; only what came after does.
    feed(&mut app, events.clone());
    assert_eq!(tasks(&mut app), [("A3".to_string(), 3), ("B".to_string(), 1), ("C".to_string(), 1)]);
    assert_eq!(app.world().resource::<Mirror>().last_event(), newest);

    // Delivering the whole stream a second time is harmless too.
    feed(&mut app, events);
    assert_eq!(tasks(&mut app), [("A3".to_string(), 3), ("B".to_string(), 1), ("C".to_string(), 1)]);
    let world = app.world_mut();
    assert_eq!(world.query::<&IsTask>().iter(world).count(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_stream_resumes_with_nothing_missed() {
    let rig = Rig::new().await;
    rig.task("before").await;
    let proxy = Proxy::new(rig.server).await;
    let mut run = Running::start(rig.remote(proxy.addr, &fast()), fast());
    run.until("live", |_, r| r.contains(&Report::Live)).await;

    rig.task("one").await;
    run.until("one", |s, _| has_title(s, "one")).await;

    // Cut the stream, and write while it is down.
    proxy.cut();
    rig.task("two").await;
    rig.task("three").await;
    run.until("two and three", |s, _| has_title(s, "two") && has_title(s, "three")).await;
    run.until("live again", |_, r| r.iter().filter(|r| **r == Report::Live).count() >= 2).await;
    rig.task("four").await;
    run.until("four", |s, _| has_title(s, "four")).await;

    // One snapshot: it resumed rather than starting over, and every event came once, in order.
    assert_eq!(run.snapshots(), 1);
    assert!(run.reports.lock().unwrap().contains(&Report::Reconnecting));
    let ids = run.event_ids();
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "in order, none twice: {ids:?}");
    assert_eq!(ids.len(), 4, "task.created for one, two, three and four: {ids:?}");

    // And the mirror ends up with all five.
    let mut app = mirror_app();
    feed(&mut app, run.seen.clone());
    let titles: Vec<_> = tasks(&mut app).into_iter().map(|t| t.0).collect();
    assert_eq!(titles, ["before", "four", "one", "three", "two"]);
    run.task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_long_disconnect_starts_over_from_a_new_snapshot() {
    let rig = Rig::new().await;
    rig.task("before").await;
    let proxy = Proxy::new(rig.server).await;
    let timings = SyncTimings { long_disconnect: Duration::from_millis(400), ..fast() };
    let mut run = Running::start(rig.remote(proxy.addr, &timings), timings);
    run.until("live", |_, r| r.contains(&Report::Live)).await;

    proxy.down.store(true, Ordering::SeqCst);
    proxy.cut();
    rig.task("missed").await;
    run.until("reconnecting", |_, r| r.contains(&Report::Reconnecting)).await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    proxy.down.store(false, Ordering::SeqCst);
    run.until("a second snapshot", |s, _| s.iter().filter(|i| matches!(i, SyncItem::Snapshot(_))).count() == 2).await;

    let mut app = mirror_app();
    feed(&mut app, run.seen.clone());
    let titles: Vec<_> = tasks(&mut app).into_iter().map(|t| t.0).collect();
    assert_eq!(titles, ["before", "missed"]);
    assert_eq!(app.world().resource::<Mirror>().snapshots(), 2);
    run.task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn claims_comments_and_projects_are_mirrored() {
    let rig = Rig::new().await;
    let task = rig.task("work").await;
    let id = task["id"].as_str().unwrap().to_string();
    let agent = rig.state.store.create_agent(&rig.ctx, "bot").await.unwrap();
    let bot = Ctx::agent(rig.ctx.workspace_id, rig.ctx.user_id, agent.id);
    let q = rig
        .state
        .store
        .add_comment(&bot, &id, NewComment::new(trailhead_core::CommentKind::Question, "why?"))
        .await
        .unwrap();
    rig.state.store.claim(&bot, &id, None).await.unwrap();

    let mut run = Running::start(rig.remote(rig.server, &fast()), fast());
    run.until("live", |_, r| r.contains(&Report::Live)).await;
    let mut app = mirror_app();
    feed(&mut app, run.seen.clone());
    {
        let world = app.world_mut();
        assert_eq!(world.query::<&IsProject>().iter(world).count(), 1);
        assert_eq!(world.query::<&IsClaim>().iter(world).count(), 1);
        assert_eq!(world.query::<&IsComment>().iter(world).count(), 1);
    }

    // Resolving the question and releasing the claim arrive on the stream.
    rig.state.store.resolve_comment(&rig.ctx, q.id, Some("because".into())).await.unwrap();
    rig.state.store.release(&bot, &id).await.unwrap();
    run.until("release", |s, _| s.iter().any(|i| matches!(i, SyncItem::Event(e) if e["kind"] == "claim.released")))
        .await;
    feed(&mut app, run.seen.clone());
    let world = app.world_mut();
    assert_eq!(world.query::<&IsClaim>().iter(world).count(), 0);
    let resolved =
        world.query_filtered::<&Data, With<IsComment>>().iter(world).filter(|d| !d.0["resolved_at"].is_null()).count();
    assert_eq!(resolved, 1);
    run.task.abort();
}
