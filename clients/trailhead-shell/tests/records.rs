//! The record mirror against an in-process server: an app's collections loaded page by page, changes and removals
//! off its stream, writes through the outbox and their refusals, an app the server doesn't know yet, two apps each
//! on its own cursor, and a dropped stream resumed with nothing lost or doubled.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use trailhead_model::app_data::{AppEvent, BatchWrite};
use trailhead_shell::commands::{CommandDone, Outcome, Submit};
use trailhead_shell::connection::Connection;
use trailhead_shell::record_sync::{self, AppSpec, RecordItem, RecordRemote, RecordStatus, RecordTimings};
use trailhead_shell::records::{self, Collections, Record, RecordMirror};
use trailhead_shell::sync::{Resume, SyncTimings};
use trailhead_shell::testing::server::{LiveShell, Server, live, records_caught_up};
use trailhead_shell::testing::{state, until};
use trailhead_shell::{AppId, Manifest, ShellApp, ShellPlugin};
use trailhead_ui::Icon;

const NOTES: AppId = AppId("notes");
const LISTS: AppId = AppId("lists");
const TOML: &str = "[collections.notes]\nfields = { text = \"string\" }\n";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
struct Note {
    text: String,
}

/// An app that keeps one collection of notes.
struct Keeper(AppId);

impl ShellApp for Keeper {
    fn manifest(&self) -> Manifest {
        Manifest { id: self.0, name: self.0.0, icon: Icon::Board }
    }

    fn build(&self, app: &mut App) {
        app.init_resource::<Answers>().add_systems(Update, hear);
    }

    fn collections(&self) -> Collections {
        Collections::declared(TOML).keep::<Note>("notes")
    }
}

/// What came back to the test's commands.
#[derive(Resource, Default)]
struct Answers(Vec<(Entity, Outcome)>);

fn hear(mut done: MessageReader<CommandDone>, mut answers: ResMut<Answers>) {
    answers.0.extend(done.read().map(|d| (d.source, d.outcome.clone())));
}

/// `app`'s notes as the mirror holds them, by text.
fn notes(shell: &mut App, app: AppId) -> Vec<Record<Note>> {
    let world = shell.world_mut();
    let mut notes: Vec<_> = world.query::<&Record<Note>>().iter(world).filter(|r| r.app == app).cloned().collect();
    notes.sort_by(|a, b| a.data.text.cmp(&b.data.text));
    notes
}

fn texts(shell: &mut App, app: AppId) -> Vec<String> {
    notes(shell, app).into_iter().map(|n| n.data.text).collect()
}

fn mirror(shell: &App) -> &RecordMirror {
    shell.world().resource::<RecordMirror>()
}

async fn write(server: &Server, app: AppId, texts: &[&str]) {
    let writes = texts
        .iter()
        .map(|t| BatchWrite::Create { collection: "notes".into(), key: None, data: json!({"text": t}) })
        .collect();
    server.client().app_batch(app.0, writes).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_apps_records_load_page_by_page_and_follow_its_stream_in_place() {
    let server = Server::new().await;
    server.declare(NOTES, TOML).await;
    // More than two pages.
    let many: Vec<String> = (0..1001).map(|n| format!("n{n:04}")).collect();
    let many_str: Vec<&str> = many.iter().map(String::as_str).collect();
    // A batch takes at most 1000.
    write(&server, NOTES, &many_str[..1000]).await;
    write(&server, NOTES, &many_str[1000..]).await;
    let mut shell = live(&server, ShellPlugin::new().with(Keeper(NOTES))).await;
    records_caught_up(&mut shell, &server, NOTES).await;
    assert!(mirror(&shell).loaded(NOTES, "notes"));
    assert_eq!(texts(&mut shell, NOTES), many);
    assert_eq!(mirror(&shell).snapshots(NOTES), 1);
    until(&mut shell, "live", |a| mirror(a).status(NOTES) == Some(&RecordStatus::Live)).await;

    // A change made elsewhere arrives by the stream, in place.
    let first = notes(&mut shell, NOTES)[0].clone();
    let entity = mirror(&shell).entity(NOTES, first.id).unwrap();
    server.client().patch_record("notes", "notes", first.id, json!({"text": "changed"}), None).await.unwrap();
    records_caught_up(&mut shell, &server, NOTES).await;
    let changed = notes(&mut shell, NOTES).into_iter().find(|n| n.id == first.id).unwrap();
    assert_eq!((changed.data.text.as_str(), changed.version), ("changed", 2));
    assert_eq!(mirror(&shell).entity(NOTES, first.id), Some(entity), "the same entity");

    // A removal despawns it.
    server.client().delete_record("notes", "notes", first.id, None).await.unwrap();
    records_caught_up(&mut shell, &server, NOTES).await;
    assert_eq!(notes(&mut shell, NOTES).len(), 1000);
    assert_eq!(mirror(&shell).entity(NOTES, first.id), None);
    let world = shell.world_mut();
    assert!(world.get_entity(entity).is_err());
}

fn submit(shell: &mut LiveShell, request: trailhead_shell::commands::Request) -> Entity {
    let source = shell.world_mut().spawn_empty().id();
    shell.world_mut().write_message(Submit { source, request });
    source
}

async fn answered(shell: &mut LiveShell, source: Entity) -> Outcome {
    until(shell, "an answer", |a| a.world().resource::<Answers>().0.iter().any(|(s, _)| *s == source)).await;
    let answers = &shell.world().resource::<Answers>().0;
    answers.iter().find(|(s, _)| *s == source).unwrap().1.clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn each_write_goes_through_the_outbox_and_comes_back_by_the_stream() {
    let server = Server::new().await;
    server.declare(NOTES, TOML).await;
    let mut shell = live(&server, ShellPlugin::new().with(Keeper(NOTES))).await;
    records_caught_up(&mut shell, &server, NOTES).await;
    assert!(texts(&mut shell, NOTES).is_empty());

    let made = submit(&mut shell, records::create(NOTES, "notes", &Note { text: "milk".into() }));
    assert!(matches!(answered(&mut shell, made).await, Outcome::Ok { version: Some(1) }));
    let keyed = submit(&mut shell, records::put(NOTES, "notes", "shop/eggs", &Note { text: "eggs".into() }));
    assert!(matches!(answered(&mut shell, keyed).await, Outcome::Ok { .. }));
    records_caught_up(&mut shell, &server, NOTES).await;
    assert_eq!(texts(&mut shell, NOTES), ["eggs", "milk"]);
    let eggs = notes(&mut shell, NOTES)[0].clone();
    assert_eq!(eggs.key.as_deref(), Some("shop/eggs"));

    let milk = notes(&mut shell, NOTES)[1].clone();
    let changed = submit(&mut shell, milk.change(json!({"text": "oat milk"})));
    assert!(matches!(answered(&mut shell, changed).await, Outcome::Ok { version: Some(2) }));
    // The version the control saw is stale now.
    let stale = submit(&mut shell, milk.replace(&Note { text: "tea".into() }));
    assert!(matches!(answered(&mut shell, stale).await, Outcome::Conflict { current_version: Some(2), .. }));
    // Data the collection doesn't take: the server's words.
    let invalid = submit(&mut shell, records::create(NOTES, "notes", &json!({"words": 1})));
    match answered(&mut shell, invalid).await {
        Outcome::Failed(why) => assert!(why.contains("words"), "{why}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    records_caught_up(&mut shell, &server, NOTES).await;
    let milk = notes(&mut shell, NOTES)[1].clone();
    assert_eq!((milk.data.text.as_str(), milk.version), ("oat milk", 2));
    let replaced = submit(&mut shell, milk.replace(&Note { text: "tea".into() }));
    assert!(matches!(answered(&mut shell, replaced).await, Outcome::Ok { .. }));
    let removed = submit(&mut shell, eggs.delete());
    assert!(matches!(answered(&mut shell, removed).await, Outcome::Ok { .. }));
    records_caught_up(&mut shell, &server, NOTES).await;
    assert_eq!(texts(&mut shell, NOTES), ["tea"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_app_the_server_does_not_know_is_asked_about_slowly_until_it_is_declared() {
    let server = Server::new().await;
    let mut shell = live(&server, ShellPlugin::new().with(Keeper(NOTES))).await;
    until(&mut shell, "not registered", |a| mirror(a).status(NOTES) == Some(&RecordStatus::NotRegistered)).await;
    assert!(!mirror(&shell).loaded(NOTES, "notes"), "not loaded: not yet, rather than empty");
    // The tasks' connection is no worse for it.
    assert_eq!(state(&shell), Connection::Live);

    server.declare(NOTES, TOML).await;
    write(&server, NOTES, &["milk"]).await;
    records_caught_up(&mut shell, &server, NOTES).await;
    assert_eq!(texts(&mut shell, NOTES), ["milk"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn two_apps_each_follow_their_own_cursor() {
    let server = Server::new().await;
    server.declare(NOTES, TOML).await;
    server.declare(LISTS, TOML).await;
    write(&server, NOTES, &["a", "b", "c"]).await;
    write(&server, NOTES, &["d"]).await;
    let mut shell = live(&server, ShellPlugin::new().with(Keeper(NOTES)).with(Keeper(LISTS))).await;
    records_caught_up(&mut shell, &server, NOTES).await;
    records_caught_up(&mut shell, &server, LISTS).await;
    let (notes_at, lists_at) = (mirror(&shell).last_event(NOTES), mirror(&shell).last_event(LISTS));
    assert!(notes_at > lists_at, "each file numbers its own events: {notes_at} and {lists_at}");

    // An event in one app, whose ids overlap the other's, lands in that app only.
    write(&server, LISTS, &["x"]).await;
    records_caught_up(&mut shell, &server, LISTS).await;
    assert_eq!(texts(&mut shell, LISTS), ["x"]);
    assert_eq!(texts(&mut shell, NOTES), ["a", "b", "c", "d"]);
    assert_eq!(mirror(&shell).last_event(NOTES), notes_at);
}

fn fast() -> RecordTimings {
    let sync = SyncTimings {
        retry: Duration::from_millis(50),
        long_disconnect: Duration::from_secs(60),
        silence: Duration::from_secs(5),
    };
    RecordTimings { sync, unregistered: Duration::from_millis(200) }
}

/// A TCP forwarder in front of the server that can cut every open connection.
struct Proxy {
    addr: SocketAddr,
    conns: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl Proxy {
    async fn new(to: SocketAddr) -> Proxy {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let conns = Arc::new(Mutex::new(Vec::<JoinHandle<()>>::new()));
        let c = conns.clone();
        tokio::spawn(async move {
            while let Ok((mut client, _)) = listener.accept().await {
                let handle = tokio::spawn(async move {
                    if let Ok(mut upstream) = TcpStream::connect(to).await {
                        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                    }
                });
                c.lock().unwrap().push(handle);
            }
        });
        Proxy { addr, conns }
    }

    fn cut(&self) {
        for conn in self.conns.lock().unwrap().drain(..) {
            conn.abort();
        }
    }
}

/// One app's record sync, run on its own, whose items can be waited on.
struct Running {
    _task: JoinHandle<()>,
    items: std::sync::mpsc::Receiver<RecordItem>,
    seen: Vec<RecordItem>,
}

impl Running {
    fn start(server: SocketAddr, token: &str, timings: RecordTimings) -> Running {
        let (tx, items) = std::sync::mpsc::channel();
        let remote = RecordRemote::new(format!("http://{server}"), token.to_string(), &timings.sync);
        let spec = AppSpec { app: "notes".into(), collections: vec!["notes".into()] };
        let task =
            tokio::spawn(record_sync::run(remote, timings, Resume::default(), spec, move |item| tx.send(item).is_ok()));
        Running { _task: task, items, seen: Vec::new() }
    }

    async fn until(&mut self, what: &str, done: impl Fn(&Running) -> bool) {
        for _ in 0..200 {
            self.seen.extend(self.items.try_iter());
            if done(self) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("never reached: {what}; saw {:?}", self.seen);
    }

    fn statuses(&self) -> Vec<&RecordStatus> {
        self.seen
            .iter()
            .filter_map(|i| match i {
                RecordItem::Status { status, .. } => Some(status),
                _ => None,
            })
            .collect()
    }

    fn snapshots(&self) -> usize {
        self.seen.iter().filter(|i| matches!(i, RecordItem::Snapshot(_))).count()
    }

    fn events(&self) -> Vec<&AppEvent> {
        self.seen
            .iter()
            .filter_map(|i| match i {
                RecordItem::Event { event, .. } => Some(event),
                _ => None,
            })
            .collect()
    }

    fn has(&self, text: &str) -> bool {
        self.events().iter().any(|e| e.after.as_ref().is_some_and(|a| a["data"]["text"] == text))
    }

    fn lives(&self) -> usize {
        self.statuses().iter().filter(|s| ***s == RecordStatus::Live).count()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_stream_resumes_from_its_cursor_with_nothing_lost_or_doubled() {
    let server = Server::new().await;
    server.declare(NOTES, TOML).await;
    write(&server, NOTES, &["before"]).await;
    let proxy = Proxy::new(server.server).await;
    let mut run = Running::start(proxy.addr, &server.token, fast());
    run.until("live", |r| r.lives() == 1).await;
    write(&server, NOTES, &["one"]).await;
    run.until("one", |r| r.has("one")).await;

    proxy.cut();
    write(&server, NOTES, &["two", "three"]).await;
    run.until("two and three", |r| r.has("two") && r.has("three")).await;
    run.until("live again", |r| r.lives() == 2).await;
    write(&server, NOTES, &["four"]).await;
    run.until("four", |r| r.has("four")).await;

    assert_eq!(run.snapshots(), 1, "it resumed rather than loading again");
    assert!(run.statuses().contains(&&RecordStatus::Reconnecting));
    let ids: Vec<i64> = run.events().iter().map(|e| e.id).collect();
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "in order, none twice: {ids:?}");
    assert_eq!(ids.len(), 4, "one, two, three and four: {ids:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_token_that_does_not_reach_the_app_is_said_and_asked_again_slowly() {
    let server = Server::new().await;
    server.declare(NOTES, TOML).await;
    // An agent's token names no app, so it reaches none.
    let mut run = Running::start(server.server, &server.agent_token, fast());
    run.until("forbidden", |r| r.statuses().iter().any(|s| matches!(s, RecordStatus::Forbidden(_)))).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    run.seen.extend(run.items.try_iter());
    // Asked again, but the status is said once, and nothing was loaded.
    let forbidden = run.statuses().iter().filter(|s| matches!(s, RecordStatus::Forbidden(_))).count();
    assert_eq!((forbidden, run.snapshots()), (1, 0));
}
