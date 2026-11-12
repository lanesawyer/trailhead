//! The ECS mirror of Trailhead (`docs/factory/ui.md#architecture`): tasks, projects, comments, claims, agents and
//! users
//! as entities keyed by their Trailhead id. The sync ([`crate::sync`]) is the only thing that changes them: a
//! [`SyncItem::Snapshot`] replaces the lot, and a [`SyncItem::Event`] replaces one entity only if it is newer, so
//! replays and duplicates are harmless.
//!
//! What "newer" means depends on what the entity has:
//! - tasks and projects carry a `version`;
//! - a comment and a claim have no version, so the id of the event that last set it is its revision (the stream is
//!   ordered, and an undo puts a comment back to an earlier state, so its state can't rank it), and a snapshot's
//!   copy, which has no event, gives way to any event for it;
//! - a deleted entity is despawned, as a snapshot leaves it out, so the stream and a snapshot agree; an undo of the
//!   delete brings it back. A deleted task takes its claim, comments and the edges into it with it, which the stream
//!   sends no events for; an undo restores the task, and those come back with the next snapshot or event for them;
//! - a dependency (a blocker edge, keyed by both ids) has no version either, so the id of its event ranks it;
//! - agents and users have no events yet, so they change only with a snapshot.
//!
//! Each entity holds its data as its `trailhead-model` type in a [`Mirrored`] component, read once as it arrives.
//! Data that doesn't read (a status a newer server added, say) is left out with a warning rather than shown half-read.

use std::collections::HashMap;

use bevy::prelude::*;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use trailhead_model::{Agent, Claim, Comment, Dependency, Project, Task, User};
use uuid::Uuid;

use crate::sync::{Snapshot, SyncItem};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Task,
    Project,
    Comment,
    Claim,
    Agent,
    User,
    /// A blocker edge, keyed `blocker_id:blocked_id`.
    Dependency,
}

/// The Trailhead id: a UUID, except for a claim, which is keyed by its task's.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct TrailheadId(pub String);

/// How new the entity's data is; see the module docs.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rev(pub i64);

/// The entity as the API sent it, read into its model type: a [`Task`], [`Project`], [`Comment`], [`Claim`] or
/// [`Agent`]. A task's `claim`, `open_questions`, `last_handoff` and `last_review` are entities of their own.
#[derive(Component, Debug, Clone, PartialEq)]
pub struct Mirrored<T: Send + Sync + 'static>(pub T);

/// The `T`s in the mirror, for a view to build from.
pub fn rows<T: Clone + Send + Sync + 'static>(q: &Query<&Mirrored<T>>) -> Vec<T> {
    q.iter().map(|m| m.0.clone()).collect()
}

/// What one kind of entity reads into.
enum Record {
    Task(Task),
    Project(Project),
    Comment(Comment),
    Claim(Claim),
    Agent(Agent),
    User(User),
    Dependency(Dependency),
}

impl Record {
    fn read(kind: Kind, data: Value) -> serde_json::Result<Record> {
        fn of<T: DeserializeOwned>(data: Value) -> serde_json::Result<T> {
            serde_json::from_value(data)
        }
        Ok(match kind {
            Kind::Task => Record::Task(of(data)?),
            Kind::Project => Record::Project(of(data)?),
            Kind::Comment => Record::Comment(of(data)?),
            Kind::Claim => Record::Claim(of(data)?),
            Kind::Agent => Record::Agent(of(data)?),
            Kind::User => Record::User(of(data)?),
            Kind::Dependency => Record::Dependency(of(data)?),
        })
    }

    fn insert(self, entity: &mut EntityCommands) {
        match self {
            Record::Task(t) => entity.insert(Mirrored(t)),
            Record::Project(p) => entity.insert(Mirrored(p)),
            Record::Comment(c) => entity.insert(Mirrored(c)),
            Record::Claim(c) => entity.insert(Mirrored(c)),
            Record::Agent(a) => entity.insert(Mirrored(a)),
            Record::User(u) => entity.insert(Mirrored(u)),
            Record::Dependency(d) => entity.insert(Mirrored(d)),
        };
    }
}

/// Which entity holds what, and how far the stream has been applied.
#[derive(Resource, Default, Debug)]
pub struct Mirror {
    index: HashMap<(Kind, String), (Entity, Rev)>,
    /// The task each claim and comment belongs to, to despawn them with it.
    owner: HashMap<(Kind, String), String>,
    last_event: i64,
    snapshots: u32,
}

impl Mirror {
    pub fn entity(&self, kind: Kind, id: &str) -> Option<Entity> {
        self.index.get(&(kind, id.to_string())).map(|(e, _)| *e)
    }

    /// The id of the last event applied, or of the snapshot before any.
    pub fn last_event(&self) -> i64 {
        self.last_event
    }

    /// How many snapshots have been applied: 1 after the first sync, more after each resync.
    pub fn snapshots(&self) -> u32 {
        self.snapshots
    }

    /// Writes the entity if it is new or `rev` is newer than what is there (or `force`). Data that doesn't read as
    /// its kind (from a newer server, say) is left out with a warning, and an older copy stays.
    fn put(&mut self, commands: &mut Commands, kind: Kind, id: &str, rev: Rev, data: Value, force: bool) {
        let key = (kind, id.to_string());
        if let Some((_, have)) = self.index.get(&key)
            && !force
            && have.0 >= rev.0
        {
            return;
        }
        let record = match Record::read(kind, data) {
            Ok(record) => record,
            Err(e) => {
                warn!(?kind, id, error = %e, "left out of the mirror: it doesn't read as its kind");
                return;
            }
        };
        match &record {
            Record::Comment(c) => self.owner.insert(key.clone(), c.task_id.to_string()),
            Record::Claim(c) => self.owner.insert(key.clone(), c.task_id.to_string()),
            Record::Dependency(d) => self.owner.insert(key.clone(), d.blocked_id.to_string()),
            _ => None,
        };
        match self.index.get(&key) {
            Some(&(entity, _)) => {
                let mut entity = commands.entity(entity);
                entity.insert(rev);
                record.insert(&mut entity);
                self.index.insert(key, (entity.id(), rev));
            }
            None => {
                let mut entity = commands.spawn((TrailheadId(id.to_string()), rev));
                record.insert(&mut entity);
                self.index.insert(key, (entity.id(), rev));
            }
        }
    }

    fn remove(&mut self, commands: &mut Commands, kind: Kind, id: &str) {
        self.owner.remove(&(kind, id.to_string()));
        if let Some((entity, _)) = self.index.remove(&(kind, id.to_string())) {
            commands.entity(entity).despawn();
        }
        if kind == Kind::Task {
            let owned: Vec<_> = self.owner.iter().filter(|(_, task)| *task == id).map(|(k, _)| k.clone()).collect();
            for (kind, owned_id) in owned {
                self.remove(commands, kind, &owned_id);
            }
        }
    }

    fn apply_snapshot(&mut self, commands: &mut Commands, snapshot: Snapshot) {
        self.owner.clear();
        for (entity, _) in self.index.drain().map(|(_, v)| v) {
            commands.entity(entity).despawn();
        }
        self.last_event = snapshot.at;
        self.snapshots += 1;
        for project in snapshot.projects {
            let (id, rev) = (str_of(&project, "id"), version(&project));
            self.put(commands, Kind::Project, &id, rev, project, true);
        }
        for agent in snapshot.agents {
            self.put(commands, Kind::Agent, &str_of(&agent, "id"), Rev(0), agent, true);
        }
        for user in snapshot.users {
            self.put(commands, Kind::User, &str_of(&user, "id"), Rev(0), user, true);
        }
        for mut task in snapshot.tasks {
            let id = str_of(&task, "id");
            let extras = task.as_object_mut().map(|o| {
                ["claim", "open_questions", "last_handoff", "last_review", "blocked_by", "blocked_by_hidden"]
                    .map(|k| o.remove(k).unwrap_or(Value::Null))
            });
            let (workspace, created) = (task["workspace_id"].clone(), task["created_at"].clone());
            let rev = version(&task);
            self.put(commands, Kind::Task, &id, rev, task, true);
            let Some([claim, questions, handoff, review, blockers, hidden]) = extras else { continue };
            // The list carries no edge records, so each is made from its task: a task created at the same time as the
            // edge is as good a stamp as any, and an event for the edge replaces it.
            for blocker in blockers.as_array().into_iter().flatten().filter_map(Value::as_str) {
                let edge = json!({
                    "workspace_id": workspace,
                    "blocker_id": blocker,
                    "blocked_id": id,
                    "created_at": created,
                });
                self.put(commands, Kind::Dependency, &format!("{blocker}:{id}"), Rev(0), edge, true);
            }
            // Blockers outside the token's scope arrive as a count. Each is an edge from the nil id, which no task has,
            // so the board counts it as unfinished with nothing to name.
            for n in 0..hidden.as_u64().unwrap_or(0) {
                let edge = json!({
                    "workspace_id": workspace,
                    "blocker_id": Uuid::nil(),
                    "blocked_id": id,
                    "created_at": created,
                });
                self.put(commands, Kind::Dependency, &format!("{}#{n}:{id}", Uuid::nil()), Rev(0), edge, true);
            }
            if !claim.is_null() {
                self.put(commands, Kind::Claim, &id, Rev(0), claim, true);
            }
            let comments = questions
                .as_array()
                .into_iter()
                .flatten()
                .chain(handoff.is_object().then_some(&handoff))
                .chain(review.is_object().then_some(&review));
            for comment in comments {
                self.put(commands, Kind::Comment, &str_of(comment, "id"), Rev(0), comment.clone(), true);
            }
        }
    }

    fn apply_event(&mut self, commands: &mut Commands, event: Value) {
        let Some(event_id) = event["id"].as_i64() else { return };
        // The stream is in order, so anything at or before what's applied is a duplicate.
        if event_id <= self.last_event {
            return;
        }
        self.last_event = event_id;
        let id = event["entity_id"].as_str().unwrap_or_default().to_string();
        let after = event["after"].clone();
        let kind = match event["entity"].as_str() {
            Some("project") => Kind::Project,
            Some("task") => Kind::Task,
            Some("comment") => Kind::Comment,
            Some("claim") => Kind::Claim,
            Some("agent") => Kind::Agent,
            Some("dependency") => Kind::Dependency,
            _ => return,
        };
        if !after.is_object() || !after["deleted_at"].is_null() {
            return self.remove(commands, kind, &id);
        }
        let rev = if matches!(kind, Kind::Task | Kind::Project) { version(&after) } else { Rev(event_id) };
        self.put(commands, kind, &id, rev, after, false);
    }
}

fn str_of(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or_default().to_string()
}

fn version(v: &Value) -> Rev {
    Rev(v["version"].as_i64().unwrap_or(0))
}

pub fn apply_sync(mut items: MessageReader<SyncItem>, mut mirror: ResMut<Mirror>, mut commands: Commands) {
    for item in items.read() {
        match item.clone() {
            SyncItem::Snapshot(snapshot) => mirror.apply_snapshot(&mut commands, snapshot),
            SyncItem::Event(event) => mirror.apply_event(&mut commands, event),
        }
    }
}

/// Keeps the mirror from [`SyncItem`] messages.
pub struct MirrorPlugin;

impl Plugin for MirrorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Mirror>().add_message::<SyncItem>().add_systems(PreUpdate, apply_sync);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::fixture::{claim_json, comment_json, sid, task_json};

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(MirrorPlugin);
        app
    }

    fn task_event(id: i64, version: i64, title: &str) -> SyncItem {
        SyncItem::Event(json!({
            "id": id, "entity": "task", "entity_id": sid("t1"), "kind": "task.updated",
            "after": task_json(json!({"id": "t1", "title": title, "version": version})),
        }))
    }

    fn snapshot(at: i64, version: i64, title: &str) -> SyncItem {
        let mut task = task_json(json!({"id": "t1", "title": title, "version": version}));
        task["claim"] = Value::Null;
        task["open_questions"] = json!([]);
        SyncItem::Snapshot(Snapshot { at, tasks: vec![task], ..Default::default() })
    }

    fn title(app: &mut App) -> String {
        let world = app.world_mut();
        world.query::<&Mirrored<Task>>().single(world).unwrap().0.title.clone()
    }

    fn send(app: &mut App, items: impl IntoIterator<Item = SyncItem>) {
        for item in items {
            app.world_mut().write_message(item);
        }
        app.update();
    }

    #[test]
    fn an_older_version_never_replaces_a_newer_one() {
        let mut app = app();
        send(&mut app, [snapshot(10, 3, "v3")]);
        // Newer event id, older version: the snapshot already saw past it.
        send(&mut app, [task_event(11, 2, "v2")]);
        assert_eq!(title(&mut app), "v3");
        // The same version again is a duplicate.
        send(&mut app, [task_event(12, 3, "dup")]);
        assert_eq!(title(&mut app), "v3");
        send(&mut app, [task_event(13, 4, "v4")]);
        assert_eq!(title(&mut app), "v4");
        assert_eq!(app.world().resource::<Mirror>().last_event(), 13);
    }

    #[test]
    fn replayed_events_are_ignored_and_one_entity_per_id_remains() {
        let mut app = app();
        send(&mut app, [snapshot(10, 1, "a"), task_event(11, 2, "b")]);
        send(&mut app, [task_event(11, 2, "b"), task_event(10, 1, "a")]);
        assert_eq!(title(&mut app), "b");
        let world = app.world_mut();
        assert_eq!(world.query::<&Mirrored<Task>>().iter(world).count(), 1);
    }

    fn comment_event(id: i64, resolved: bool, deleted: bool) -> SyncItem {
        let at = |on: bool| if on { json!("2026-01-01T00:00:00Z") } else { Value::Null };
        let after =
            comment_json(json!({"id": "c1", "task_id": "t1", "resolved_at": at(resolved), "deleted_at": at(deleted)}));
        SyncItem::Event(
            json!({"id": id, "entity": "comment", "entity_id": sid("c1"), "kind": "comment.updated", "after": after}),
        )
    }

    fn data_of<T: Clone + Send + Sync + 'static>(app: &mut App) -> Vec<T> {
        let world = app.world_mut();
        world.query::<&Mirrored<T>>().iter(world).map(|m| m.0.clone()).collect()
    }

    #[test]
    fn a_snapshot_carries_the_review_that_asks_for_changes() {
        let mut app = app();
        let mut task = task_json(json!({"id": "t1", "status": "in_review", "version": 1}));
        task["claim"] = Value::Null;
        task["open_questions"] = json!([]);
        task["last_handoff"] = Value::Null;
        task["last_review"] =
            comment_json(json!({"id": "c1", "task_id": "t1", "kind": "review", "verdict": "request_changes"}));
        send(&mut app, [SyncItem::Snapshot(Snapshot { at: 10, tasks: vec![task], ..Default::default() })]);
        let comments = data_of::<Comment>(&mut app);
        let asked = trailhead_model::changes_requested(trailhead_model::Status::InReview, &comments);
        assert_eq!(asked, Some(comments[0].id));
    }

    #[test]
    fn undoing_a_resolve_reopens_the_question() {
        let mut app = app();
        send(&mut app, [snapshot(10, 1, "a"), comment_event(11, false, false), comment_event(12, true, false)]);
        assert!(data_of::<Comment>(&mut app)[0].resolved_at.is_some());
        send(&mut app, [comment_event(13, false, false)]);
        assert!(data_of::<Comment>(&mut app)[0].resolved_at.is_none());
    }

    #[test]
    fn deleted_entities_are_despawned_and_undo_restores_them() {
        let mut app = app();
        send(&mut app, [snapshot(10, 1, "a"), comment_event(11, false, false)]);
        send(&mut app, [deleted_task_event(12, 2)]);
        assert!(data_of::<Task>(&mut app).is_empty());
        assert!(data_of::<Comment>(&mut app).is_empty(), "the task's comments go with it");
        send(&mut app, [task_event(14, 3, "back"), comment_event(15, false, false)]);
        assert_eq!(title(&mut app), "back");
        assert_eq!(data_of::<Comment>(&mut app).len(), 1);
    }

    fn claim_event(id: i64) -> SyncItem {
        let after = claim_json(json!({"task_id": "t1"}));
        SyncItem::Event(
            json!({"id": id, "entity": "claim", "entity_id": sid("t1"), "kind": "claim.created", "after": after}),
        )
    }

    fn deleted_task_event(id: i64, version: i64) -> SyncItem {
        let after =
            task_json(json!({"id": "t1", "title": "a", "version": version, "deleted_at": "2026-01-01T00:00:00Z"}));
        SyncItem::Event(
            json!({"id": id, "entity": "task", "entity_id": sid("t1"), "kind": "task.deleted", "after": after}),
        )
    }

    #[test]
    fn a_deleted_task_takes_its_claim_and_a_later_claim_or_snapshot_restores_it() {
        let mut app = app();
        let SyncItem::Snapshot(mut snap) = snapshot(10, 1, "a") else { unreachable!() };
        snap.tasks[0]["claim"] = claim_json(json!({"task_id": "t1"}));
        send(&mut app, [SyncItem::Snapshot(snap.clone())]);
        assert_eq!(data_of::<Claim>(&mut app).len(), 1);
        send(&mut app, [deleted_task_event(11, 2)]);
        assert!(data_of::<Task>(&mut app).is_empty());
        assert!(data_of::<Claim>(&mut app).is_empty(), "the task's claim goes with it");
        send(&mut app, [task_event(12, 3, "back"), claim_event(13)]);
        assert_eq!(data_of::<Claim>(&mut app).len(), 1);
        send(&mut app, [deleted_task_event(14, 4)]);
        assert!(data_of::<Claim>(&mut app).is_empty());
        snap.at = 20;
        send(&mut app, [SyncItem::Snapshot(snap)]);
        assert_eq!(data_of::<Claim>(&mut app).len(), 1);
    }

    #[test]
    fn a_new_snapshot_replaces_the_mirror() {
        let mut app = app();
        send(&mut app, [snapshot(10, 5, "old"), task_event(11, 6, "newer")]);
        // After a long disconnect the server may be older than what was mirrored (a restored database, say).
        send(&mut app, [snapshot(3, 1, "restored")]);
        assert_eq!(title(&mut app), "restored");
        let mirror = app.world().resource::<Mirror>();
        assert_eq!((mirror.last_event(), mirror.snapshots()), (3, 2));
    }

    #[test]
    fn data_that_does_not_read_is_left_out_and_the_older_copy_stays() {
        let mut app = app();
        send(&mut app, [snapshot(10, 1, "a")]);
        let mut bad = task_json(json!({"id": "t1", "version": 2}));
        bad["status"] = json!("on_hold");
        send(&mut app, [SyncItem::Event(json!({"id": 11, "entity": "task", "entity_id": sid("t1"), "after": bad}))]);
        assert_eq!(title(&mut app), "a");
        assert_eq!(app.world().resource::<Mirror>().last_event(), 11);
    }
    fn edges(app: &mut App) -> Vec<(String, String)> {
        let world = app.world_mut();
        let mut edges: Vec<_> = world
            .query::<&Mirrored<Dependency>>()
            .iter(world)
            .map(|d| (d.0.blocker_id.to_string(), d.0.blocked_id.to_string()))
            .collect();
        edges.sort();
        edges
    }

    fn edge_event(id: i64, kind: &str) -> SyncItem {
        let edge = json!({
            "workspace_id": sid("w1"), "blocker_id": sid("t2"), "blocked_id": sid("t1"),
            "created_at": "2026-10-01T00:00:00Z"
        });
        let (before, after) = if kind == "dependency.added" { (Value::Null, edge) } else { (edge, Value::Null) };
        SyncItem::Event(json!({
            "id": id, "entity": "dependency", "entity_id": format!("{}:{}", sid("t2"), sid("t1")),
            "kind": kind, "before": before, "after": after,
        }))
    }

    #[test]
    fn dependencies_come_from_the_snapshot_and_follow_the_stream() {
        let mut app = app();
        let SyncItem::Snapshot(mut snap) = snapshot(10, 1, "a") else { unreachable!() };
        snap.tasks[0]["blocked_by"] = json!([sid("t2"), sid("t3")]);
        snap.tasks[0]["blocked_by_hidden"] = json!(2);
        send(&mut app, [SyncItem::Snapshot(snap)]);
        let nil = Uuid::nil().to_string();
        assert_eq!(
            edges(&mut app),
            [(nil.clone(), sid("t1")), (nil, sid("t1")), (sid("t2"), sid("t1")), (sid("t3"), sid("t1"))]
        );
        send(&mut app, [edge_event(11, "dependency.removed")]);
        assert_eq!(edges(&mut app).len(), 3);
        send(&mut app, [edge_event(12, "dependency.added")]);
        assert_eq!(edges(&mut app).len(), 4);
        // A resync forgets edges the new snapshot lacks.
        send(&mut app, [snapshot(20, 1, "a")]);
        assert!(edges(&mut app).is_empty());
    }
}
