//! An app's own data, from the app data store (`docs/app-platform.md`, "The app data store and connectors"): the
//! record mirror beside the task mirror ([`crate::mirror`]), and the writes an app sends through the commands.
//!
//! An app says what it keeps with [`crate::ShellApp::collections`]: its `collections.toml`, and for each collection
//! it reads, the Rust type its records' data deserializes into ([`Collections::keep`]). While the connection is live,
//! the net thread loads each such app's collections and follows its event stream ([`crate::record_sync`]), and
//! [`RecordItem`]s keep the mirror:
//!
//! - Each record is an entity holding a [`Record<T>`] (its id, key, version and typed data) and a [`RecordOf`]
//!   naming its app and collection. An app reads them with an ordinary query, `Query<&Record<Note>>`.
//! - A snapshot replaces everything held of its app; an event replaces one record only if its version is newer than
//!   what is held, and a removal despawns it unless what is held is newer than what was removed. So the replays the
//!   paging rule brings, and an event the stream repeats, are harmless.
//! - A record whose data doesn't deserialize into the app's type is left out, said once in the log, and an older copy
//!   stays.
//! - [`RecordMirror::loaded`] says a collection has been read at least once, so an app can tell "empty" from "not yet".
//!
//! Writes go through the outbox as task writes do ([`crate::commands`]): [`create`], [`put`], and on a held record
//! [`Record::change`], [`Record::replace`] and [`Record::delete`] make the [`Request`] to send in a `Submit`. The
//! result comes back through the stream. A write to a collection filled from a connector's feed is refused at once,
//! when the declaration says so, as `Outcome::Forbidden`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bevy::prelude::*;
use jiff::Timestamp;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use trailhead_client::http::Call;
use trailhead_model::app_data::{AppEvent, BatchWrite, Declaration, Record as Stored};
use uuid::Uuid;

use crate::AppId;
use crate::commands::Request;
use crate::record_sync::{AppSpec, RecordItem, RecordSnapshot, RecordStatus};

/// What an app keeps of the app data store, from [`crate::ShellApp::collections`]: its `collections.toml`, and the
/// collections it reads, each with the type its records deserialize into.
///
/// ```ignore
/// fn collections(&self) -> Collections {
///     Collections::declared(include_str!("../collections.toml")).keep::<Note>("notes")
/// }
/// ```
#[derive(Default)]
pub struct Collections {
    toml: Option<&'static str>,
    declaration: Declaration,
    kept: Vec<Kept>,
}

/// One collection an app reads, and how its records become components.
#[derive(Clone)]
struct Kept {
    collection: &'static str,
    put: Put,
}

/// Reads a record's data as the app's type and inserts its [`Record<T>`] on the entity; `Err` when it doesn't read.
type Put = Arc<dyn Fn(&mut EntityCommands, AppId, &'static str, &Stored) -> serde_json::Result<()> + Send + Sync>;

impl Collections {
    /// An app that keeps nothing: the default.
    pub fn none() -> Self {
        Self::default()
    }

    /// The app's `collections.toml`, as text (`include_str!("../collections.toml")`). The shell learns from it which
    /// collections are filled from a feed, and so read-only, and a test registers it on its server from it.
    ///
    /// # Panics
    ///
    /// If the text isn't a declaration: every test that builds the app says so.
    pub fn declared(collections_toml: &'static str) -> Self {
        let declaration = Declaration::parse(collections_toml).unwrap_or_else(|e| panic!("collections.toml: {e}"));
        Collections { toml: Some(collections_toml), declaration, kept: Vec::new() }
    }

    /// Mirrors `collection`, each record's data read as a `T`.
    ///
    /// # Panics
    ///
    /// If the declaration doesn't have the collection, or it is kept twice.
    #[must_use]
    pub fn keep<T: DeserializeOwned + Send + Sync + 'static>(mut self, collection: &'static str) -> Self {
        assert!(
            self.toml.is_none() || self.declaration.collections.contains_key(collection),
            "`{collection}` is kept but collections.toml doesn't declare it"
        );
        assert!(self.kept.iter().all(|k| k.collection != collection), "`{collection}` is kept twice");
        let put: Put = Arc::new(|entity, app, collection, stored| {
            let data: T = serde_json::from_value(Value::Object(stored.data.clone()))?;
            entity.insert(Record {
                app,
                collection,
                id: stored.id,
                key: stored.key.clone(),
                version: stored.version,
                created_at: stored.created_at,
                updated_at: stored.updated_at,
                data,
            });
            Ok(())
        });
        self.kept.push(Kept { collection, put });
        self
    }

    /// Whether the app keeps nothing.
    pub fn is_empty(&self) -> bool {
        self.kept.is_empty()
    }

    /// The `collections.toml` given to [`Collections::declared`].
    pub fn toml(&self) -> Option<&'static str> {
        self.toml
    }

    /// The names of the collections kept, in the order given.
    pub fn kept(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.kept.iter().map(|k| k.collection)
    }

    pub(crate) fn spec(&self, app: AppId) -> AppSpec {
        AppSpec { app: app.0.to_string(), collections: self.kept().map(str::to_string).collect() }
    }
}

/// One record of an app's collection, its data read as the app's type `T`. Spawned, replaced in place and despawned by
/// the record mirror; an app never writes to it, and writes through [`Record::change`] and the others.
#[derive(Component, Debug, Clone, PartialEq)]
pub struct Record<T: Send + Sync + 'static> {
    pub app: AppId,
    pub collection: &'static str,
    pub id: Uuid,
    /// The name something outside gave it, unique in its collection.
    pub key: Option<String>,
    /// 1 when created, and one more each change. A write sends it as `If-Match`.
    pub version: i64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub data: T,
}

impl<T: Send + Sync + 'static> Record<T> {
    /// Merges `patch` into the record's fields (a JSON merge patch: a field given as `null` is removed), if it is
    /// still at this version.
    pub fn change(&self, patch: Value) -> Request {
        let call = Call::PatchRecord { app: self.app.0.into(), collection: self.collection.into(), id: self.id, patch };
        Request::new(call, Some(self.version))
    }

    /// Removes the record, if it is still at this version.
    pub fn delete(&self) -> Request {
        let call = Call::DeleteRecord { app: self.app.0.into(), collection: self.collection.into(), id: self.id };
        Request::new(call, Some(self.version))
    }
}

impl<T: Serialize + Send + Sync + 'static> Record<T> {
    /// Replaces the record's fields with `data`, whole, if it is still at this version. Its key stays.
    pub fn replace(&self, data: &T) -> Request {
        let (app, collection) = (self.app.0.into(), self.collection.into());
        Request::new(Call::ReplaceRecord { app, collection, id: self.id, data: to_data(data) }, Some(self.version))
    }
}

fn to_data<T: Serialize>(data: &T) -> Value {
    serde_json::to_value(data).expect("a record's data serializes to JSON")
}

/// Creates a record in the app's `collection`.
pub fn create<T: Serialize>(app: AppId, collection: &str, data: &T) -> Request {
    let call = Call::CreateRecord { app: app.0.into(), collection: collection.into(), key: None, data: to_data(data) };
    Request::new(call, None)
}

/// Creates a record with a key, unique in its collection: a taken key is refused.
pub fn create_keyed<T: Serialize>(app: AppId, collection: &str, key: &str, data: &T) -> Request {
    let (app, collection, key) = (app.0.into(), collection.into(), Some(key.into()));
    Request::new(Call::CreateRecord { app, collection, key, data: to_data(data) }, None)
}

/// Makes the record with `key` look like `data`, creating it if there is none: writing the same thing twice leaves one
/// record, and a write that changes nothing writes nothing.
pub fn put<T: Serialize>(app: AppId, collection: &str, key: &str, data: &T) -> Request {
    let call =
        Call::PutRecord { app: app.0.into(), collection: collection.into(), key: key.into(), data: to_data(data) };
    Request::new(call, None)
}

/// On every record's entity: which app and collection it is in, and its id.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct RecordOf {
    pub app: AppId,
    pub collection: &'static str,
    pub id: Uuid,
}

/// What the mirror holds of one record.
#[derive(Debug, Clone, Copy)]
struct Held {
    entity: Entity,
    version: i64,
    collection: &'static str,
}

/// What the mirror knows of one app.
#[derive(Debug, Default)]
struct AppHeld {
    /// The app's declaration as the shell was given it, then as the server last said it.
    declaration: Declaration,
    status: Option<RecordStatus>,
    last_event: i64,
    snapshots: u32,
    /// Collections read at least once.
    loaded: HashSet<String>,
}

/// The record mirror: which entity holds which record, what each app keeps, and how far each app's stream has been
/// applied. Present with a connection (or a test's), like the task [`crate::mirror::Mirror`].
#[derive(Resource, Default)]
pub struct RecordMirror {
    kept: HashMap<(String, String), (AppId, Kept)>,
    apps: HashMap<String, AppHeld>,
    index: HashMap<(String, Uuid), Held>,
    /// The records whose data didn't read, already said in the log.
    warned: HashSet<(String, Uuid)>,
}

impl RecordMirror {
    /// Starts mirroring what `collections` keeps for `app`.
    pub(crate) fn keep(&mut self, app: AppId, collections: &Collections) {
        let held = self.apps.entry(app.0.to_string()).or_default();
        held.declaration = collections.declaration.clone();
        for kept in &collections.kept {
            self.kept.insert((app.0.to_string(), kept.collection.to_string()), (app, kept.clone()));
        }
    }

    /// Whether `collection` of `app` has been read at least once: before that, no records means "not yet", after it
    /// "none". It stays true through a drop of the connection, as the records do.
    pub fn loaded(&self, app: AppId, collection: &str) -> bool {
        self.apps.get(app.0).is_some_and(|a| a.loaded.contains(collection))
    }

    /// Where `app`'s sync is, once it has said: `None` before the connection is live.
    pub fn status(&self, app: AppId) -> Option<&RecordStatus> {
        self.apps.get(app.0).and_then(|a| a.status.as_ref())
    }

    /// The id of the last event of `app`'s applied, or of its snapshot before any.
    pub fn last_event(&self, app: AppId) -> i64 {
        self.apps.get(app.0).map_or(0, |a| a.last_event)
    }

    /// How many snapshots of `app` have been applied: 1 after the first load, more after each reload.
    pub fn snapshots(&self, app: AppId) -> u32 {
        self.apps.get(app.0).map_or(0, |a| a.snapshots)
    }

    /// The entity holding `app`'s record `id`.
    pub fn entity(&self, app: AppId, id: Uuid) -> Option<Entity> {
        self.index.get(&(app.0.to_string(), id)).map(|h| h.entity)
    }

    /// The collections `app` keeps.
    pub fn kept(&self, app: AppId) -> Vec<&str> {
        let mut kept: Vec<&str> =
            self.kept.iter().filter(|((a, _), _)| a == app.0).map(|((_, c), _)| c.as_str()).collect();
        kept.sort_unstable();
        kept
    }

    /// The feed `collection` of `app` is filled from, when it is: such a collection is read-only to the app.
    pub fn feed(&self, app: &str, collection: &str) -> Option<&str> {
        self.apps.get(app)?.declaration.collections.get(collection)?.from.as_deref()
    }

    /// Why `call` can't succeed, when the declaration already says: a write to a collection filled from a feed.
    pub(crate) fn refusal(&self, call: &Call) -> Option<String> {
        let refuse = |app: &str, collection: &str| {
            self.feed(app, collection).map(|feed| {
                format!("`{collection}` is filled from the feed `{feed}`: nothing but the feed writes to it")
            })
        };
        match call {
            Call::CreateRecord { app, collection, .. }
            | Call::PatchRecord { app, collection, .. }
            | Call::ReplaceRecord { app, collection, .. }
            | Call::DeleteRecord { app, collection, .. }
            | Call::PutRecord { app, collection, .. } => refuse(app, collection),
            Call::AppBatch { app, writes } => writes.iter().find_map(|write| match write {
                BatchWrite::Create { collection, .. } | BatchWrite::Put { collection, .. } => refuse(app, collection),
                BatchWrite::Patch { id, .. } | BatchWrite::Replace { id, .. } | BatchWrite::Delete { id, .. } => {
                    let held = self.index.get(&(app.clone(), *id))?;
                    refuse(app, held.collection)
                }
            }),
            _ => None,
        }
    }

    /// Writes the record if it is new or newer than what is held (or `force`), as `kept` reads it.
    fn put(&mut self, commands: &mut Commands, app: &str, stored: &Stored, force: bool) {
        let Some((app_id, kept)) = self.kept.get(&(app.to_string(), stored.collection.clone())).cloned() else {
            return;
        };
        let key = (app.to_string(), stored.id);
        let held = self.index.get(&key).copied();
        if !force && held.is_some_and(|h| h.version >= stored.version) {
            return;
        }
        let entity = match held {
            Some(h) => h.entity,
            None => commands.spawn(RecordOf { app: app_id, collection: kept.collection, id: stored.id }).id(),
        };
        let mut entity_commands = commands.entity(entity);
        match (kept.put)(&mut entity_commands, app_id, kept.collection, stored) {
            Ok(()) => {
                self.index.insert(key, Held { entity, version: stored.version, collection: kept.collection });
            }
            Err(e) => {
                if held.is_none() {
                    entity_commands.despawn();
                }
                if self.warned.insert(key) {
                    warn!(app, collection = stored.collection, id = %stored.id, error = %e,
                        "left out of the record mirror: its data doesn't read as the app's type");
                }
            }
        }
    }

    fn remove(&mut self, commands: &mut Commands, app: &str, id: Uuid) {
        if let Some(held) = self.index.remove(&(app.to_string(), id)) {
            commands.entity(held.entity).despawn();
        }
    }

    fn apply_snapshot(&mut self, commands: &mut Commands, snapshot: RecordSnapshot) {
        let app = snapshot.app;
        let gone: Vec<(String, Uuid)> = self.index.keys().filter(|(a, _)| *a == app).cloned().collect();
        for (a, id) in gone {
            self.remove(commands, &a, id);
        }
        let held = self.apps.entry(app.clone()).or_default();
        held.last_event = snapshot.at;
        held.snapshots += 1;
        held.loaded = snapshot.collections.iter().map(|(c, _)| c.clone()).collect();
        if let Some(declaration) = snapshot.declaration {
            held.declaration = declaration;
        }
        for (_, records) in snapshot.collections {
            for stored in records {
                self.put(commands, &app, &stored, true);
            }
        }
    }

    fn apply_event(&mut self, commands: &mut Commands, app: &str, event: AppEvent) {
        let held = self.apps.entry(app.to_string()).or_default();
        // The stream is in order, so anything at or before what's applied is a repeat.
        if event.id <= held.last_event {
            return;
        }
        held.last_event = event.id;
        if !self.kept.contains_key(&(app.to_string(), event.collection.clone())) {
            return;
        }
        match event.after.map(serde_json::from_value::<Stored>) {
            Some(Ok(stored)) => self.put(commands, app, &stored, false),
            Some(Err(e)) => warn!(app, event = event.id, error = %e, "an app event's record doesn't read"),
            None => {
                // Removed. A copy newer than the one removed (from a page read after it was put back) stays.
                let removed = event.before.as_ref().and_then(|b| b["version"].as_i64()).unwrap_or(i64::MAX);
                let newer = self.index.get(&(app.to_string(), event.record_id)).is_some_and(|h| h.version > removed);
                if !newer {
                    self.remove(commands, app, event.record_id);
                }
            }
        }
    }
}

/// Applies the record sync's items to the mirror.
pub fn apply_records(mut items: MessageReader<RecordItem>, mut mirror: ResMut<RecordMirror>, mut commands: Commands) {
    for item in items.read() {
        match item.clone() {
            RecordItem::Snapshot(snapshot) => mirror.apply_snapshot(&mut commands, snapshot),
            RecordItem::Event { app, event } => mirror.apply_event(&mut commands, &app, event),
            RecordItem::Status { app, status } => mirror.apps.entry(app).or_default().status = Some(status),
        }
    }
}

/// Keeps the record mirror from [`RecordItem`] messages. The shell registers what each app keeps once the apps are
/// given.
pub struct RecordsPlugin;

impl Plugin for RecordsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RecordMirror>().add_message::<RecordItem>().add_systems(PreUpdate, apply_records);
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use serde_json::json;

    use super::*;
    use crate::fixture::{app_event, id, record_json};

    const NOTES: AppId = AppId("notes");

    #[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
    struct Note {
        text: String,
    }

    const TOML: &str =
        "[collections.notes]\nfields = { text = \"string\" }\n\n[collections.mail]\nfrom = \"mail.inbox\"\n";

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(RecordsPlugin);
        let kept = Collections::declared(TOML).keep::<Note>("notes").keep::<Note>("mail");
        app.world_mut().resource_mut::<RecordMirror>().keep(NOTES, &kept);
        app
    }

    fn send(app: &mut App, items: impl IntoIterator<Item = RecordItem>) {
        for item in items {
            app.world_mut().write_message(item);
        }
        app.update();
    }

    fn note(name: &str, text: &str, version: i64) -> Stored {
        serde_json::from_value(record_json(
            json!({"id": name, "collection": "notes", "data": {"text": text}, "version": version}),
        ))
        .unwrap()
    }

    fn snapshot(at: i64, records: Vec<Stored>) -> RecordItem {
        RecordItem::Snapshot(RecordSnapshot {
            app: "notes".into(),
            at,
            declaration: None,
            collections: vec![("notes".into(), records)],
        })
    }

    fn event(event_id: i64, name: &str, before: Option<Value>, after: Option<Value>) -> RecordItem {
        let event = app_event(
            json!({"id": event_id, "collection": "notes", "record_id": name, "before": before, "after": after}),
        );
        RecordItem::Event { app: "notes".into(), event }
    }

    fn texts(app: &mut App) -> Vec<(String, i64)> {
        let world = app.world_mut();
        let mut texts: Vec<_> =
            world.query::<&Record<Note>>().iter(world).map(|r| (r.data.text.clone(), r.version)).collect();
        texts.sort();
        texts
    }

    fn stored_json(name: &str, text: &str, version: i64) -> Value {
        serde_json::to_value(note(name, text, version)).unwrap()
    }

    #[test]
    fn a_snapshot_fills_and_events_replace_in_place_only_when_newer() {
        let mut app = app();
        assert!(!app.world().resource::<RecordMirror>().loaded(NOTES, "notes"));
        send(&mut app, [snapshot(5, vec![note("r1", "milk", 2)])]);
        assert!(app.world().resource::<RecordMirror>().loaded(NOTES, "notes"));
        let entity = app.world().resource::<RecordMirror>().entity(NOTES, id("r1")).unwrap();
        // A repeat of what the snapshot already holds, by event id, then by version.
        send(&mut app, [event(4, "r1", None, Some(stored_json("r1", "old", 1)))]);
        send(&mut app, [event(6, "r1", None, Some(stored_json("r1", "stale", 2)))]);
        assert_eq!(texts(&mut app), [("milk".into(), 2)]);
        send(&mut app, [event(7, "r1", None, Some(stored_json("r1", "oat milk", 3)))]);
        assert_eq!(texts(&mut app), [("oat milk".into(), 3)]);
        assert_eq!(app.world().resource::<RecordMirror>().entity(NOTES, id("r1")), Some(entity), "in place");
        send(&mut app, [event(8, "r2", None, Some(stored_json("r2", "eggs", 1)))]);
        assert_eq!(texts(&mut app).len(), 2);
        send(&mut app, [event(9, "r1", Some(stored_json("r1", "oat milk", 3)), None)]);
        assert_eq!(texts(&mut app), [("eggs".into(), 1)]);
        assert_eq!(app.world().resource::<RecordMirror>().last_event(NOTES), 9);
    }

    #[test]
    fn a_removal_older_than_the_copy_held_leaves_it() {
        let mut app = app();
        // Read on a later page, after it was removed and put back.
        send(&mut app, [snapshot(5, vec![note("r1", "back", 4)])]);
        send(&mut app, [event(6, "r1", Some(stored_json("r1", "gone", 3)), None)]);
        assert_eq!(texts(&mut app), [("back".into(), 4)]);
    }

    #[test]
    fn a_record_that_does_not_read_is_left_out_and_an_older_copy_stays() {
        let mut app = app();
        let mut bad: Stored = note("r2", "x", 1);
        bad.data = serde_json::from_value(json!({"words": 3})).unwrap();
        send(&mut app, [snapshot(5, vec![note("r1", "milk", 1), bad.clone()])]);
        assert_eq!(texts(&mut app), [("milk".into(), 1)]);
        let world = app.world_mut();
        assert_eq!(world.query::<&RecordOf>().iter(world).count(), 1, "no entity for the bad one");
        let mut worse = note("r1", "x", 2);
        worse.data = bad.data;
        send(&mut app, [event(6, "r1", None, Some(serde_json::to_value(&worse).unwrap()))]);
        assert_eq!(texts(&mut app), [("milk".into(), 1)]);
    }

    #[test]
    fn a_new_snapshot_replaces_what_the_app_held() {
        let mut app = app();
        send(&mut app, [snapshot(5, vec![note("r1", "milk", 1), note("r2", "eggs", 1)])]);
        send(&mut app, [snapshot(3, vec![note("r3", "bread", 1)])]);
        assert_eq!(texts(&mut app), [("bread".into(), 1)]);
        let mirror = app.world().resource::<RecordMirror>();
        assert_eq!((mirror.last_event(NOTES), mirror.snapshots(NOTES)), (3, 2));
    }

    #[test]
    fn writes_name_the_record_and_its_version_and_a_fed_collection_refuses_them() {
        let mut app = app();
        send(&mut app, [snapshot(5, vec![note("r1", "milk", 2)])]);
        let world = app.world_mut();
        let record = world.query::<&Record<Note>>().single(world).unwrap().clone();
        let change = record.change(json!({"text": "oat milk"}));
        assert_eq!(
            (change.call.path(), change.if_match),
            (format!("/v1/apps/notes/records/notes/{}", id("r1")), Some(2))
        );
        let replace = record.replace(&Note { text: "tea".into() });
        assert_eq!(replace.call.body(), Some(json!({"text": "tea"})));
        assert_eq!(crate::commands::Method::of(&replace.call), crate::commands::Method::Put);
        assert_eq!(record.delete().if_match, Some(2));
        let made = create(NOTES, "notes", &Note { text: "jam".into() });
        assert_eq!(made.call.body(), Some(json!({"key": null, "data": {"text": "jam"}})));
        let keyed = put(NOTES, "notes", "a/b", &Note { text: "jam".into() });
        assert_eq!(keyed.call.path(), "/v1/apps/notes/keys/notes/a%2Fb");

        let mirror = app.world().resource::<RecordMirror>();
        assert_eq!(mirror.refusal(&made.call), None);
        let fed = create(NOTES, "mail", &Note { text: "hi".into() });
        assert!(mirror.refusal(&fed.call).unwrap().contains("feed `mail.inbox`"));
        assert_eq!(mirror.feed("notes", "mail"), Some("mail.inbox"));
    }

    #[test]
    #[should_panic(expected = "doesn't declare it")]
    fn keeping_an_undeclared_collection_is_refused() {
        let _ = Collections::declared(TOML).keep::<Note>("todos");
    }
}
