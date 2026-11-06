//! The ECS mirror of Trailhead (`docs/factory/ui.md#architecture`): tasks, projects, comments, claims and agents
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
//!   delete brings it back;
//! - agents have no events yet, so they change only with a snapshot.

use std::collections::HashMap;

use bevy::prelude::*;
use serde_json::Value;
use trailhead_model::{Grade, Priority, Status};

use crate::sync::{Snapshot, SyncItem};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Task,
    Project,
    Comment,
    Claim,
    Agent,
}

/// The Trailhead id: a UUID, except for a claim, which is keyed by its task's.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct TrailheadId(pub String);

/// How new the entity's data is; see the module docs.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rev(pub i64);

/// The entity as the API sent it. A task has no `claim`, `open_questions` or `last_handoff` here: those are
/// entities of their own.
#[derive(Component, Debug, Clone, PartialEq)]
pub struct Data(pub Value);

/// A task's status, if it has a known one.
pub fn status_of(task: &Value) -> Option<Status> {
    task["status"].as_str()?.parse().ok()
}

/// A task's priority, if it has a valid one.
pub fn priority_of(task: &Value) -> Option<Priority> {
    Priority::try_from(task["priority"].as_i64()?).ok()
}

/// A task's grade, if it has one.
pub fn grade_of(task: &Value) -> Option<Grade> {
    Grade::try_from(task["grade"].as_i64()?).ok()
}

#[derive(Component)]
pub struct IsTask;
#[derive(Component)]
pub struct IsProject;
#[derive(Component)]
pub struct IsComment;
#[derive(Component)]
pub struct IsClaim;
#[derive(Component)]
pub struct IsAgent;

/// Which entity holds what, and how far the stream has been applied.
#[derive(Resource, Default, Debug)]
pub struct Mirror {
    index: HashMap<(Kind, String), (Entity, Rev)>,
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

    /// Writes the entity if it is new or `rev` is newer than what is there (or `force`).
    fn put(&mut self, commands: &mut Commands, kind: Kind, id: &str, rev: Rev, data: Value, force: bool) {
        let key = (kind, id.to_string());
        match self.index.get(&key) {
            Some((_, have)) if !force && have.0 >= rev.0 => {}
            Some(&(entity, _)) => {
                commands.entity(entity).insert((rev, Data(data)));
                self.index.insert(key, (entity, rev));
            }
            None => {
                let mut entity = commands.spawn((TrailheadId(id.to_string()), rev, Data(data)));
                match kind {
                    Kind::Task => entity.insert(IsTask),
                    Kind::Project => entity.insert(IsProject),
                    Kind::Comment => entity.insert(IsComment),
                    Kind::Claim => entity.insert(IsClaim),
                    Kind::Agent => entity.insert(IsAgent),
                };
                self.index.insert(key, (entity.id(), rev));
            }
        }
    }

    fn remove(&mut self, commands: &mut Commands, kind: Kind, id: &str) {
        if let Some((entity, _)) = self.index.remove(&(kind, id.to_string())) {
            commands.entity(entity).despawn();
        }
    }

    fn apply_snapshot(&mut self, commands: &mut Commands, snapshot: Snapshot) {
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
        for mut task in snapshot.tasks {
            let id = str_of(&task, "id");
            let extras = task
                .as_object_mut()
                .map(|o| ["claim", "open_questions", "last_handoff"].map(|k| o.remove(k).unwrap_or(Value::Null)));
            let rev = version(&task);
            self.put(commands, Kind::Task, &id, rev, task, true);
            let Some([claim, questions, handoff]) = extras else { continue };
            if !claim.is_null() {
                self.put(commands, Kind::Claim, &id, Rev(0), claim, true);
            }
            let comments = questions.as_array().into_iter().flatten().chain(handoff.is_object().then_some(&handoff));
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
            // Dependencies aren't mirrored yet.
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

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(MirrorPlugin);
        app
    }

    fn task_event(id: i64, version: i64, title: &str) -> SyncItem {
        SyncItem::Event(json!({
            "id": id, "entity": "task", "entity_id": "t1", "kind": "task.updated",
            "after": {"id": "t1", "title": title, "version": version},
        }))
    }

    fn snapshot(at: i64, version: i64, title: &str) -> SyncItem {
        let task = json!({"id": "t1", "title": title, "version": version, "claim": null, "open_questions": []});
        SyncItem::Snapshot(Snapshot { at, tasks: vec![task], ..Default::default() })
    }

    fn title(app: &mut App) -> String {
        let world = app.world_mut();
        let data = world.query_filtered::<&Data, With<IsTask>>().single(world).unwrap();
        data.0["title"].as_str().unwrap().to_string()
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
        assert_eq!(world.query::<&IsTask>().iter(world).count(), 1);
    }

    fn comment_event(id: i64, resolved: bool, deleted: bool) -> SyncItem {
        let at = |on: bool| if on { json!("2026-01-01T00:00:00Z") } else { Value::Null };
        SyncItem::Event(json!({
            "id": id, "entity": "comment", "entity_id": "c1", "kind": "comment.updated",
            "after": {"id": "c1", "resolved_at": at(resolved), "deleted_at": at(deleted)},
        }))
    }

    fn data_of<C: Component>(app: &mut App) -> Vec<Value> {
        let world = app.world_mut();
        world.query_filtered::<&Data, With<C>>().iter(world).map(|d| d.0.clone()).collect()
    }

    #[test]
    fn undoing_a_resolve_reopens_the_question() {
        let mut app = app();
        send(&mut app, [snapshot(10, 1, "a"), comment_event(11, false, false), comment_event(12, true, false)]);
        assert!(!data_of::<IsComment>(&mut app)[0]["resolved_at"].is_null());
        send(&mut app, [comment_event(13, false, false)]);
        assert!(data_of::<IsComment>(&mut app)[0]["resolved_at"].is_null());
    }

    #[test]
    fn deleted_entities_are_despawned_and_undo_restores_them() {
        let mut app = app();
        send(&mut app, [snapshot(10, 1, "a"), comment_event(11, false, false)]);
        let gone = SyncItem::Event(json!({
            "id": 12, "entity": "task", "entity_id": "t1", "kind": "task.deleted",
            "after": {"id": "t1", "title": "a", "version": 2, "deleted_at": "2026-01-01T00:00:00Z"},
        }));
        send(&mut app, [gone, comment_event(13, false, true)]);
        assert!(data_of::<IsTask>(&mut app).is_empty());
        assert!(data_of::<IsComment>(&mut app).is_empty());
        send(&mut app, [task_event(14, 3, "back"), comment_event(15, false, false)]);
        assert_eq!(title(&mut app), "back");
        assert_eq!(data_of::<IsComment>(&mut app).len(), 1);
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
}
