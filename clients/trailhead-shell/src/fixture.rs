//! Whole model values from the few fields a test cares about, for the tests of the views and the mirror. Short names
//! stand in for ids (`"t7"` for a task, `"a1"` for an agent): every id field takes one and gets a stable UUID from it
//! ([`id`]), so a test can write `"parent_id": "t7"` and find the task by `id("t7")`.

use serde_json::{Value, json};
use trailhead_model::{Agent, Attachment, Claim, Comment, Notification, Project, Task, User};

use crate::sync::SyncItem;
use uuid::Uuid;

/// The UUID a short name stands for. A real UUID is itself.
pub fn id(name: &str) -> Uuid {
    if let Ok(id) = Uuid::parse_str(name) {
        return id;
    }
    let mut bytes = [0u8; 16];
    for (i, b) in name.bytes().enumerate() {
        bytes[i % 16] ^= b;
    }
    Uuid::from_bytes(bytes)
}

/// The string form of [`id`], as a view's `Subject` or `Card` carries it.
pub fn sid(name: &str) -> String {
    id(name).to_string()
}

const IDS: [&str; 15] = [
    "id",
    "workspace_id",
    "project_id",
    "task_id",
    "parent_id",
    "holder_id",
    "on_behalf_of",
    "author_id",
    "addressed_to",
    "created_by",
    "owner_user_id",
    "duplicate_of",
    "raised_by",
    "record_id",
    "batch_id",
];

/// `defaults` with `fields` over them, each id field's short name turned into its UUID.
pub fn whole(defaults: Value, fields: Value) -> Value {
    let mut v = defaults;
    let (Some(out), Value::Object(fields)) = (v.as_object_mut(), fields) else { panic!("fixtures are JSON objects") };
    out.extend(fields);
    for key in IDS {
        if let Some(Value::String(s)) = out.get(key) {
            let full = sid(s);
            out.insert(key.to_string(), Value::String(full));
        }
    }
    v
}

const AT: &str = "2026-10-01T00:00:00Z";

/// An event off the stream, whole from the fields a test gives (`id`, `entity`, `entity_id` and `after`, mostly).
pub fn sync_event(fields: Value) -> SyncItem {
    let defaults = json!({
        "id": 0, "workspace_id": "w", "batch_id": "b", "actor_id": "u", "on_behalf_of": "u", "task_id": null,
        "entity": "task", "entity_id": "", "kind": "", "before": null, "after": null, "undoes": null, "at": AT,
    });
    let mut event = whole(defaults, fields);
    for key in ["batch_id", "actor_id"] {
        if let Some(Value::String(s)) = event.get(key) {
            event[key] = Value::String(sid(s));
        }
    }
    SyncItem::Event(serde_json::from_value(event).expect("a whole event"))
}

/// A task's JSON as the API sends it.
pub fn task_json(fields: Value) -> Value {
    let defaults = json!({
        "id": "t0", "workspace_id": "w", "project_id": "p1", "key": "TRAIL-0", "title": "", "description": "",
        "status": "open", "priority": 3, "grade": null, "due_at": null, "due_tz": null, "parent_id": null,
        "metadata": {}, "version": 1, "source": null, "external_id": null, "created_by": "u",
        "created_at": AT, "updated_at": AT, "deleted_at": null,
    });
    whole(defaults, fields)
}

pub fn task(fields: Value) -> Task {
    serde_json::from_value(task_json(fields)).expect("a whole task")
}

pub fn project_json(fields: Value) -> Value {
    let defaults = json!({
        "id": "p1", "workspace_id": "w", "key": "TRAIL", "name": "", "brief": "", "status": "active",
        "metadata": {}, "claim_ttl": null, "require_review": false, "version": 1, "created_at": AT,
        "updated_at": AT, "deleted_at": null,
    });
    whole(defaults, fields)
}

pub fn project(fields: Value) -> Project {
    serde_json::from_value(project_json(fields)).expect("a whole project")
}

pub fn comment_json(fields: Value) -> Value {
    let defaults = json!({
        "id": "c0", "workspace_id": "w", "task_id": "t0", "author_id": "u", "on_behalf_of": "u", "kind": "note",
        "addressed_to": null, "body": "", "resolved_at": null, "created_at": AT, "deleted_at": null,
    });
    whole(defaults, fields)
}

pub fn comment(fields: Value) -> Comment {
    serde_json::from_value(comment_json(fields)).expect("a whole comment")
}

/// A picture's metadata as the API sends it. Give `sha256` to say two pictures are the same bytes.
pub fn attachment_json(fields: Value) -> Value {
    let defaults = json!({
        "id": "f0", "workspace_id": "w", "task_id": "t0", "name": "view", "content_type": "image/png", "size": 100,
        "sha256": "0".repeat(64), "width": 320, "height": 200, "created_by": "u", "on_behalf_of": "u",
        "created_at": AT, "deleted_at": null,
    });
    whole(defaults, fields)
}

pub fn attachment(fields: Value) -> Attachment {
    serde_json::from_value(attachment_json(fields)).expect("a whole attachment")
}

pub fn claim_json(fields: Value) -> Value {
    let defaults = json!({
        "task_id": "t0", "workspace_id": "w", "holder_id": "a1", "on_behalf_of": "u", "claimed_at": AT,
        "expires_at": null,
    });
    whole(defaults, fields)
}

pub fn claim(fields: Value) -> Claim {
    serde_json::from_value(claim_json(fields)).expect("a whole claim")
}

pub fn agent_json(fields: Value) -> Value {
    let defaults = json!({
        "id": "a1", "workspace_id": "w", "owner_user_id": "u", "name": "agent", "created_at": AT,
    });
    whole(defaults, fields)
}

pub fn agent(fields: Value) -> Agent {
    serde_json::from_value(agent_json(fields)).expect("a whole agent")
}

pub fn user_json(fields: Value) -> Value {
    let defaults = json!({"id": "u1", "workspace_id": "w", "name": "user", "email": null, "created_at": AT});
    whole(defaults, fields)
}

pub fn user(fields: Value) -> User {
    serde_json::from_value(user_json(fields)).expect("a whole user")
}

/// A notification's JSON as the API sends it: raised by the server, unread.
pub fn notification_json(fields: Value) -> Value {
    let defaults = json!({
        "id": "n0", "workspace_id": "w", "source": "server", "kind": "task.question", "title": "", "body": "",
        "actions": [], "raised_by": "u", "created_at": AT, "read_at": null, "resolved_at": null,
    });
    whole(defaults, fields)
}

pub fn notification(fields: Value) -> Notification {
    serde_json::from_value(notification_json(fields)).expect("a whole notification")
}

/// An app's record as the API sends it (`{id, collection, key, data, version, ...}`): give the short name in `id`, the
/// `collection` and its `data`. A test of an app fills its collections with these ([`crate::testing::fill_records`]).
pub fn record_json(fields: Value) -> Value {
    let defaults = json!({
        "id": "r0", "collection": "", "key": null, "data": {}, "version": 1, "created_at": AT, "updated_at": AT,
        "created_by": "u",
    });
    whole(defaults, fields)
}

/// One event off an app's stream, whole from the fields a test gives (`id`, `collection`, `record_id`, and `after`
/// as a [`record_json`], or `before` and no `after` for a removal). `kind` follows from them unless given.
pub fn app_event(fields: Value) -> trailhead_model::app_data::AppEvent {
    let kind =
        match (fields.get("before").is_some_and(|b| !b.is_null()), fields.get("after").is_some_and(|a| !a.is_null())) {
            (false, true) => "record.created",
            (true, true) => "record.updated",
            _ => "record.deleted",
        };
    let defaults = json!({
        "id": 0, "batch_id": "b", "actor_id": "u", "on_behalf_of": "u", "collection": "", "record_id": "r0",
        "kind": kind, "before": null, "after": null, "undoes": null, "at": AT,
    });
    let mut event = whole(defaults, fields);
    if let Some(Value::String(s)) = event.get("actor_id") {
        event["actor_id"] = Value::String(sid(s));
    }
    serde_json::from_value(event).expect("a whole app event")
}

/// A `created_at` from a small number (0 to 59), for tests that order comments: `1` is a second after [`AT`].
pub fn at(n: u8) -> String {
    format!("2026-10-01T00:00:{n:02}Z")
}
