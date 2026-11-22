//! The whole of a task (`GET /v1/tasks/{key}`): what the mirror doesn't hold, such as its dependencies and every
//! comment (the mirror keeps only open questions and the last handoff). [`Detail`] follows one resource that names a
//! task ([`Followed`]): the inspector's [`Selection`] for the task modal, and the graph's page for a plan's
//! conversation. It is fetched when that task changes and again when the stream brings an event about it, and the
//! newest answer wins. Following a task also marks the unread notifications about it read: the person is looking at
//! it.

use std::marker::PhantomData;

use bevy::prelude::*;
use trailhead_client::http::Client;
use trailhead_model::{Task, TaskDetail, TaskKey};
use trailhead_shell::connection::{Connection, SendNet};
use trailhead_shell::mirror::Mirrored;
use trailhead_shell::net::{Fetch, NetCommand};
use trailhead_shell::notifications::ReadNotificationsAbout;
use trailhead_shell::sync::{SyncItem, task_of};

use crate::inspector::{Selection, Subject};

/// A resource naming the task (its id) a [`Detail`] fetches.
pub trait Followed: Resource {
    /// The task to fetch, if any.
    fn followed(&self) -> Option<String>;
}

impl Followed for Selection {
    fn followed(&self) -> Option<String> {
        match &self.0 {
            Some(Subject::Task(id)) => Some(id.clone()),
            _ => None,
        }
    }
}

/// The server's answer to a fetch for the [`Detail`] following `F`, as the net thread hands it back.
#[derive(Message, Debug, Clone, PartialEq)]
pub struct DetailReply<F = Selection> {
    /// Which ask this answers.
    pub seq: u64,
    /// Boxed: a whole task is large beside the net thread's other events.
    pub result: Result<Box<TaskDetail>, String>,
    of: PhantomData<fn() -> F>,
}

impl<F> DetailReply<F> {
    pub fn new(seq: u64, result: Result<Box<TaskDetail>, String>) -> Self {
        DetailReply { seq, result, of: PhantomData }
    }
}

/// Fetch the selected task again: an edit lost to a newer change, so the modal should show it. Every [`Detail`]
/// hears it.
#[derive(Message, Debug, Clone, Copy)]
pub struct Refetch;

/// Asks the server for the task `key` in full.
pub async fn fetch(api: &Client, key: &str) -> Result<Box<TaskDetail>, String> {
    api.show_task(&TaskKey(key.to_string())).await.map(Box::new).map_err(|e| e.chain())
}

/// The task `F` names, in full. It changes only when `F` or the answer does, so a view that rebuilds on its change
/// doesn't rebuild every frame; the asking is kept in [`Fetching`].
#[derive(Resource, Debug)]
pub struct Detail<F = Selection> {
    /// The task (its id) this is about; none when `F` names none.
    pub id: Option<String>,
    /// The last answer for it; none until one has come.
    pub data: Option<TaskDetail>,
    /// Why the last fetch failed, if it did.
    pub error: Option<String>,
    of: PhantomData<fn() -> F>,
}

impl<F> Default for Detail<F> {
    fn default() -> Self {
        Detail { id: None, data: None, error: None, of: PhantomData }
    }
}

impl<F> Detail<F> {
    /// The fetched task, if it is the one with this id.
    pub fn of(&self, id: &str) -> Option<&TaskDetail> {
        self.data.as_ref().filter(|_| self.id.as_deref() == Some(id))
    }
}

/// The fetch in flight for the [`Detail`] following `F`, and whether the answer we have is out of date.
#[derive(Resource, Debug)]
struct Fetching<F> {
    /// The ask in flight: its number and the task's id.
    asked: Option<(u64, String)>,
    /// The last ask's number; never reset, so an old answer can't match a new ask.
    next: u64,
    /// The followed task needs (another) fetch.
    stale: bool,
    of: PhantomData<fn() -> F>,
}

impl<F> Default for Fetching<F> {
    fn default() -> Self {
        Fetching { asked: None, next: 0, stale: false, of: PhantomData }
    }
}

/// Keeps the [`Detail`] following `F` (the [`Selection`] unless said); runs headless.
pub struct DetailPlugin<F = Selection>(PhantomData<fn() -> F>);

impl<F> Default for DetailPlugin<F> {
    fn default() -> Self {
        DetailPlugin(PhantomData)
    }
}

impl<F: Followed + Default> Plugin for DetailPlugin<F> {
    fn build(&self, app: &mut App) {
        app.init_resource::<Detail<F>>()
            .init_resource::<Fetching<F>>()
            .init_resource::<F>()
            .add_message::<DetailReply<F>>()
            .add_message::<SendNet>()
            .add_message::<SyncItem>()
            .add_message::<Refetch>()
            .add_systems(Update, (follow::<F>, mark_stale::<F>, take_replies::<F>, ask::<F>).chain())
            .add_systems(OnEnter(Connection::Live), |mut fetching: ResMut<Fetching<F>>| {
                // Whatever was asked before the link dropped won't be answered; ask again.
                fetching.asked = None;
                fetching.stale = true;
            });
    }
}

/// Another task followed starts over: nothing shown from the last task, and a fetch for the new one. Its unread
/// notifications are marked read.
fn follow<F: Followed>(
    followed: Res<F>,
    mut detail: ResMut<Detail<F>>,
    mut fetching: ResMut<Fetching<F>>,
    mut commands: Commands,
) {
    if !followed.is_changed() {
        return;
    }
    let id = followed.followed();
    if detail.id != id {
        if let Some(task_id) = id.as_deref().and_then(|id| uuid::Uuid::parse_str(id).ok()) {
            commands.trigger(ReadNotificationsAbout { task_id });
        }
        *detail = Detail { id: id.clone(), ..Detail::default() };
        fetching.asked = None;
        fetching.stale = id.is_some();
    }
}

/// An event about the followed task (it, a comment on it, its claim) makes the answer we have out of date.
fn mark_stale<F: Followed>(
    mut items: MessageReader<SyncItem>,
    mut refetch: MessageReader<Refetch>,
    detail: Res<Detail<F>>,
    mut fetching: ResMut<Fetching<F>>,
) {
    let asked = refetch.read().count() > 0;
    let Some(id) = detail.id.as_deref() else {
        items.clear();
        return;
    };
    if asked && !fetching.stale {
        fetching.stale = true;
    }
    for item in items.read() {
        let SyncItem::Event(e) = item else { continue };
        if task_of(e) == Some(id) && !fetching.stale {
            fetching.stale = true;
        }
    }
}

/// Keeps an answer only if it is to the ask in flight for the task still followed.
fn take_replies<F: Followed>(
    mut replies: MessageReader<DetailReply<F>>,
    mut fetching: ResMut<Fetching<F>>,
    mut detail: ResMut<Detail<F>>,
) {
    for reply in replies.read() {
        let Some((seq, id)) = fetching.asked.clone() else { continue };
        if seq != reply.seq {
            continue;
        }
        fetching.asked = None;
        if detail.id.as_deref() != Some(id.as_str()) {
            continue;
        }
        match &reply.result {
            Ok(task) => {
                detail.data = Some((**task).clone());
                detail.error = None;
            }
            Err(why) => detail.error = Some(why.clone()),
        }
    }
}

/// Asks for the followed task when it is out of date and nothing is in flight. The key comes from the mirror, since
/// the API takes a key or an id and the mirror's copy is the one the board shows.
fn ask<F: Followed>(
    connection: Res<State<Connection>>,
    detail: Res<Detail<F>>,
    tasks: Query<&Mirrored<Task>>,
    mut fetching: ResMut<Fetching<F>>,
    mut net: MessageWriter<SendNet>,
) {
    if !fetching.stale || fetching.asked.is_some() || *connection.get() != Connection::Live {
        return;
    }
    let Some(id) = detail.id.clone() else { return };
    let key = tasks.iter().find(|t| t.0.id.to_string() == id).map_or_else(|| id.clone(), |t| t.0.key.0.clone());
    fetching.next += 1;
    let seq = fetching.next;
    fetching.asked = Some((seq, id));
    fetching.stale = false;
    net.write(SendNet(NetCommand::Fetch(read::<F>(seq, key))));
}

/// The read of the task `key`, answered with `seq`.
fn read<F: Followed>(seq: u64, key: String) -> Fetch {
    Fetch::new(move |api| {
        let key = key.clone();
        async move {
            let result = match api {
                Some(api) => fetch(&api, &key).await,
                None => Err("not connected".into()),
            };
            DetailReply::<F>::new(seq, result)
        }
    })
}

#[cfg(test)]
mod tests {
    use bevy::state::app::StatesPlugin;
    use serde_json::json;
    use trailhead_shell::fixture::{sid, task_json};

    use super::*;

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin))
            .init_state::<Connection>()
            .add_plugins(DetailPlugin::<Selection>::default());
        app.world_mut().resource_mut::<NextState<Connection>>().set(Connection::Live);
        app.update();
        app
    }

    /// The fetches sent since the last call, as the task's id. A read is opaque, so what it asks is taken from
    /// [`Fetching`]: at most one is sent a frame, and `ask` runs last.
    fn asked(app: &mut App) -> Vec<(u64, String)> {
        let sent: Vec<_> = app.world_mut().resource_mut::<Messages<SendNet>>().drain().collect();
        let fetches = sent.iter().filter(|SendNet(c)| matches!(c, NetCommand::Fetch(_))).count();
        assert!(fetches <= 1, "{fetches} fetches in one frame");
        let asked = app.world().resource::<Fetching<Selection>>().asked.clone();
        (fetches == 1).then(|| asked.expect("a fetch sent is the one asked")).into_iter().collect()
    }

    fn select(app: &mut App, name: &str) {
        app.world_mut().resource_mut::<Selection>().0 = Some(Subject::Task(sid(name)));
        app.update();
    }

    fn detail(name: &str, title: &str) -> TaskDetail {
        serde_json::from_value(json!({
            "task": task_json(json!({"id": name, "key": "TRAIL-7", "title": title})),
            "claim": null, "blocked_by": [], "blocks": ["TRAIL-9"], "comments": [], "duplicate_of": null, "duplicates": [],
        }))
        .unwrap()
    }

    fn reply(app: &mut App, seq: u64, result: Result<Box<TaskDetail>, String>) {
        app.world_mut().write_message(DetailReply::<Selection>::new(seq, result));
        app.update();
    }

    fn event(app: &mut App, entity: &str, after: serde_json::Value) {
        let id = after["id"].clone();
        app.world_mut().write_message(trailhead_shell::fixture::sync_event(
            json!({"id": 9, "entity": entity, "entity_id": id, "after": after}),
        ));
        app.update();
    }

    #[test]
    fn selecting_a_task_asks_for_its_notifications_to_be_marked_read() {
        #[derive(Resource, Default)]
        struct Read(Vec<uuid::Uuid>);
        let mut app = app();
        app.init_resource::<Read>()
            .add_observer(|read: On<ReadNotificationsAbout>, mut seen: ResMut<Read>| seen.0.push(read.task_id));
        select(&mut app, "t7");
        app.update();
        select(&mut app, "t7");
        assert_eq!(app.world().resource::<Read>().0, [trailhead_shell::fixture::id("t7")], "once, as it is opened");
    }

    #[test]
    fn selecting_a_task_fetches_it_once() {
        let mut app = app();
        select(&mut app, "t7");
        let sent = asked(&mut app);
        assert_eq!(sent.len(), 1);
        app.update();
        app.update();
        assert!(asked(&mut app).is_empty(), "no second ask while the first is out");
        reply(&mut app, sent[0].0, Ok(Box::new(detail("t7", "do it"))));
        let d = app.world().resource::<Detail>();
        assert_eq!(d.of(&sid("t7")).unwrap().blocks, ["TRAIL-9"]);
        assert!(d.of(&sid("t8")).is_none());
    }

    #[test]
    fn an_event_about_the_task_fetches_it_again_and_others_dont() {
        let mut app = app();
        select(&mut app, "t7");
        let (seq, _) = asked(&mut app).remove(0);
        reply(&mut app, seq, Ok(Box::new(detail("t7", "do it"))));
        event(&mut app, "task", task_json(json!({"id": "t8"})));
        assert!(asked(&mut app).is_empty(), "another task's event");
        event(&mut app, "comment", json!({"id": sid("c1"), "task_id": sid("t7")}));
        assert_eq!(asked(&mut app).len(), 1);
    }

    #[test]
    fn an_answer_for_a_task_no_longer_selected_is_dropped() {
        let mut app = app();
        select(&mut app, "t7");
        let (old, _) = asked(&mut app).remove(0);
        select(&mut app, "t8");
        let (new, _) = asked(&mut app).remove(0);
        assert_ne!(old, new);
        reply(&mut app, old, Ok(Box::new(detail("t7", "stale"))));
        assert!(app.world().resource::<Detail>().data.is_none());
        reply(&mut app, new, Ok(Box::new(detail("t8", "fresh"))));
        assert_eq!(app.world().resource::<Detail>().of(&sid("t8")).unwrap().task.title, "fresh");
    }

    #[test]
    fn a_failed_fetch_says_why() {
        let mut app = app();
        select(&mut app, "t7");
        let (seq, _) = asked(&mut app).remove(0);
        reply(&mut app, seq, Err("the server answered 404".into()));
        assert_eq!(app.world().resource::<Detail>().error.as_deref(), Some("the server answered 404"));
    }

    #[test]
    fn detail_stays_unchanged_while_idle() {
        #[derive(Resource, Default)]
        struct Changes(usize);
        let mut app = app();
        app.init_resource::<Changes>().add_systems(PostUpdate, |d: Res<Detail>, mut c: ResMut<Changes>| {
            if d.is_changed() {
                c.0 += 1;
            }
        });
        select(&mut app, "t7");
        let (seq, _) = asked(&mut app).remove(0);
        reply(&mut app, seq, Ok(Box::new(detail("t7", "do it"))));
        app.world_mut().resource_mut::<Changes>().0 = 0;
        for _ in 0..10 {
            app.update();
        }
        assert_eq!(app.world().resource::<Changes>().0, 0);
    }
}
