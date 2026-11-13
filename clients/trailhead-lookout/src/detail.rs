//! The whole of the selected task (`GET /v1/tasks/{key}`), for the task modal: what the mirror doesn't hold, such as
//! its dependencies and every comment (the mirror keeps only open questions and the last handoff). It is fetched when
//! the task is selected and again when the stream brings an event about it, and the newest answer wins.

use bevy::prelude::*;
use serde_json::Value;
use trailhead_client::http::Client;
use trailhead_model::{Task, TaskDetail, TaskKey};

use crate::connection::{Connection, SendNet};
use crate::inspector::{Selection, Subject};
use crate::mirror::Mirrored;
use crate::net::NetCommand;
use crate::sync::SyncItem;

/// The server's answer to a fetch, as the net thread passes it on.
#[derive(Message, Debug, Clone, PartialEq)]
pub struct DetailReply {
    /// Which ask this answers.
    pub seq: u64,
    /// Boxed: a whole task is large beside the net thread's other events.
    pub result: Result<Box<TaskDetail>, String>,
}

/// Fetch the selected task again: an edit lost to a newer change, so the modal should show it.
#[derive(Message, Debug, Clone, Copy)]
pub struct Refetch;

/// Asks the server for the task `key` in full.
pub async fn fetch(api: &Client, key: &str) -> Result<Box<TaskDetail>, String> {
    api.show_task(&TaskKey(key.to_string())).await.map(Box::new).map_err(|e| e.to_string())
}

/// The selected task in full. It changes only when the selection or the answer does, so a view that rebuilds on its
/// change doesn't rebuild every frame; the asking is kept in [`Fetching`].
#[derive(Resource, Default, Debug)]
pub struct Detail {
    /// The task (its id) this is about; none when no task is selected.
    pub id: Option<String>,
    /// The last answer for it; none until one has come.
    pub data: Option<TaskDetail>,
    /// Why the last fetch failed, if it did.
    pub error: Option<String>,
}

impl Detail {
    /// The fetched task, if it is the one with this id.
    pub fn of(&self, id: &str) -> Option<&TaskDetail> {
        self.data.as_ref().filter(|_| self.id.as_deref() == Some(id))
    }
}

/// The fetch in flight, and whether the answer we have is out of date.
#[derive(Resource, Default, Debug)]
struct Fetching {
    /// The ask in flight: its number and the task's id.
    asked: Option<(u64, String)>,
    /// The last ask's number; never reset, so an old answer can't match a new ask.
    next: u64,
    /// The selected task needs (another) fetch.
    stale: bool,
}

/// Keeps [`Detail`]; runs headless.
pub struct DetailPlugin;

impl Plugin for DetailPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Detail>()
            .init_resource::<Fetching>()
            .init_resource::<Selection>()
            .add_message::<DetailReply>()
            .add_message::<SendNet>()
            .add_message::<SyncItem>()
            .add_message::<Refetch>()
            .add_systems(Update, (follow_selection, mark_stale, take_replies, ask).chain())
            .add_systems(OnEnter(Connection::Live), |mut fetching: ResMut<Fetching>| {
                // Whatever was asked before the link dropped won't be answered; ask again.
                fetching.asked = None;
                fetching.stale = true;
            });
    }
}

/// A different selection starts over: nothing shown from the last task, and a fetch for the new one.
fn follow_selection(selection: Res<Selection>, mut detail: ResMut<Detail>, mut fetching: ResMut<Fetching>) {
    if !selection.is_changed() {
        return;
    }
    let id = match &selection.0 {
        Some(Subject::Task(id)) => Some(id.clone()),
        _ => None,
    };
    if detail.id != id {
        *detail = Detail { id: id.clone(), ..Detail::default() };
        fetching.asked = None;
        fetching.stale = id.is_some();
    }
}

/// An event about the selected task (it, a comment on it, its claim) makes the answer we have out of date.
fn mark_stale(
    mut items: MessageReader<SyncItem>,
    mut refetch: MessageReader<Refetch>,
    detail: Res<Detail>,
    mut fetching: ResMut<Fetching>,
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
        if about(e) == Some(id) && !fetching.stale {
            fetching.stale = true;
        }
    }
}

/// The task an event is about.
fn about(event: &Value) -> Option<&str> {
    match event["entity"].as_str()? {
        "task" => event["entity_id"].as_str(),
        "comment" | "claim" => event["after"]["task_id"].as_str().or_else(|| event["before"]["task_id"].as_str()),
        _ => None,
    }
}

/// Keeps an answer only if it is to the ask in flight for the task still selected.
fn take_replies(mut replies: MessageReader<DetailReply>, mut fetching: ResMut<Fetching>, mut detail: ResMut<Detail>) {
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

/// Asks for the selected task when it is out of date and nothing is in flight. The key comes from the mirror, since
/// the API takes a key or an id and the mirror's copy is the one the board shows.
fn ask(
    connection: Res<State<Connection>>,
    detail: Res<Detail>,
    tasks: Query<&Mirrored<Task>>,
    mut fetching: ResMut<Fetching>,
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
    net.write(SendNet(NetCommand::Detail { seq, key }));
}

#[cfg(test)]
mod tests {
    use bevy::state::app::StatesPlugin;
    use serde_json::json;

    use super::*;
    use crate::fixture::{sid, task_json};

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin)).init_state::<Connection>().add_plugins(DetailPlugin);
        app.world_mut().resource_mut::<NextState<Connection>>().set(Connection::Live);
        app.update();
        app
    }

    fn asked(app: &mut App) -> Vec<(u64, String)> {
        let sent: Vec<_> = app.world_mut().resource_mut::<Messages<SendNet>>().drain().collect();
        sent.into_iter()
            .filter_map(|SendNet(c)| match c {
                NetCommand::Detail { seq, key } => Some((seq, key)),
                _ => None,
            })
            .collect()
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
        app.world_mut().write_message(DetailReply { seq, result });
        app.update();
    }

    fn event(app: &mut App, entity: &str, after: Value) {
        let id = after["id"].clone();
        app.world_mut()
            .write_message(SyncItem::Event(json!({"id": 9, "entity": entity, "entity_id": id, "after": after})));
        app.update();
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
