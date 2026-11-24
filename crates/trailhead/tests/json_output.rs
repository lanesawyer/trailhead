//! Pins the exact keys and the value formats (types, snake_case enums, RFC 3339 timestamps) of
//! `--json` for the claim, renew, release, comment, ask, answer, verdict and handoff commands, so
//! moving them onto typed clients can't change what scripts and agents parse. Each output is
//! described once, as a whole-object shape (which pins its keys too); only `release`, which prints
//! the task `claim` already pins in full, is checked by its keys alone.

mod common;

use common::Instance;
use serde_json::{Value, json};

const TASK: &[&str] = &[
    "created_at",
    "created_by",
    "deleted_at",
    "description",
    "due_at",
    "due_on",
    "due_tz",
    "duplicate_of",
    "external_id",
    "grade",
    "id",
    "key",
    "metadata",
    "parent_id",
    "priority",
    "project_id",
    "scheduled_at",
    "scheduled_on",
    "scheduled_tz",
    "source",
    "status",
    "title",
    "updated_at",
    "version",
    "workspace_id",
];

fn json(th: &Instance, args: &[&str]) -> Value {
    let mut full = vec!["--json"];
    full.extend_from_slice(args);
    let out = th.run(&full);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

#[track_caller]
fn assert_keys(v: &Value, expected: &[&str]) {
    let mut want: Vec<&str> = expected.to_vec();
    want.sort();
    let mut got: Vec<&str> =
        v.as_object().unwrap_or_else(|| panic!("not an object: {v}")).keys().map(String::as_str).collect();
    got.sort();
    assert_eq!(got, want, "{v}");
}

fn setup() -> Instance {
    let th = Instance::new();
    th.stdout(&["project", "add", "JS", "Json"]);
    th.stdout(&["add", "one", "-p", "JS"]);
    th
}

#[test]
fn release_keeps_the_task_keys() {
    let th = setup();
    th.stdout(&["claim", "JS-1"]);
    assert_keys(&json(&th, &["release", "JS-1"]), TASK);
}

fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter()
            .enumerate()
            .all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { *c == b'-' } else { c.is_ascii_hexdigit() })
}

/// RFC 3339 in UTC, ending in `Z`. The fraction varies (jiff's `Display` drops trailing zeros,
/// and the fraction when it is zero), so its width is not pinned.
fn is_timestamp(s: &str) -> bool {
    s.ends_with('Z') && s.parse::<jiff::Timestamp>().is_ok()
}

/// Replaces the volatile values (uuids, timestamps) with placeholders. A string that is
/// neither stays as is, so a changed format shows in the diff.
fn mask(v: &Value) -> Value {
    match v {
        Value::String(s) if is_uuid(s) => json!("<id>"),
        Value::String(s) if is_timestamp(s) => json!("<timestamp>"),
        Value::Array(a) => Value::Array(a.iter().map(mask).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), mask(v))).collect()),
        other => other.clone(),
    }
}

#[track_caller]
fn assert_shape(v: &Value, expected: &Value) {
    assert_eq!(&mask(v), expected, "{v}");
}

fn claim_shape() -> Value {
    json!({
        "claimed_at": "<timestamp>",
        "expires_at": null,
        "holder_id": "<id>",
        "on_behalf_of": "<id>",
        "task_id": "<id>",
        "workspace_id": "<id>",
    })
}

fn comment_shape(kind: &str, body: &str, resolved_at: Value) -> Value {
    json!({
        "addressed_to": null,
        "author_id": "<id>",
        "body": body,
        "created_at": "<timestamp>",
        "deleted_at": null,
        "id": "<id>",
        "kind": kind,
        "on_behalf_of": "<id>",
        "resolved_at": resolved_at,
        "task_id": "<id>",
        "workspace_id": "<id>",
    })
}

#[test]
fn values_keep_their_types_and_formats() {
    let th = setup();

    let claimed = json(&th, &["claim", "JS-1"]);
    assert_shape(
        &claimed,
        &json!({
            "claim": claim_shape(),
            "handoff": null,
            "task": {
                "created_at": "<timestamp>",
                "created_by": "<id>",
                "deleted_at": null,
                "description": "",
                "due_at": null,
                "due_on": null,
                "due_tz": null,
                "duplicate_of": null,
                "external_id": null,
                "grade": null,
                "id": "<id>",
                "key": "JS-1",
                "metadata": {},
                "parent_id": null,
                "priority": 3,
                "project_id": "<id>",
                "scheduled_at": null,
                "scheduled_on": null,
                "scheduled_tz": null,
                "source": null,
                "status": "in_progress",
                "title": "one",
                "updated_at": "<timestamp>",
                "version": 2,
                "workspace_id": "<id>",
            },
        }),
    );

    let mine = json(&th, &["mine"]);
    assert_shape(&mine[0], &json!({"claim": claim_shape(), "key": "JS-1", "status": "in_progress", "title": "one"}));

    assert_shape(&json(&th, &["comment", "JS-1", "hi"]), &comment_shape("note", "hi", Value::Null));
    assert_shape(&json(&th, &["ask", "JS-1", "a or b?"]), &comment_shape("question", "a or b?", Value::Null));
    assert_shape(&json(&th, &["answer", "JS-1", "a"]), &comment_shape("question", "a or b?", json!("<timestamp>")));
}

fn with_keys(mut shape: Value, extra: Value) -> Value {
    shape.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    shape
}

#[test]
fn verdict_handoff_and_renew_keep_their_types_and_formats() {
    let th = setup();

    let bare = json(&th, &["verdict", "JS-1", "approve", "ok"]);
    assert_shape(&bare, &with_keys(comment_shape("review", "ok", Value::Null), json!({"verdict": "approve"})));

    let modelled = json(&th, &["verdict", "JS-1", "request-changes", "no", "--model", "m"]);
    assert_shape(
        &modelled,
        &with_keys(comment_shape("review", "no", Value::Null), json!({"verdict": "request_changes", "model": "m"})),
    );

    th.stdout(&["claim", "JS-1"]);
    th.stdout(&["handoff", "JS-1", "left off here"]);
    th.stdout(&["release", "JS-1"]);
    let claimed = json(&th, &["claim", "JS-1"]);
    assert_shape(&claimed["handoff"], &comment_shape("handoff", "left off here", Value::Null));

    assert_shape(&json(&th, &["renew", "JS-1"]), &claim_shape());
}
