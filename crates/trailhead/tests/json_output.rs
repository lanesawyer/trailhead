//! Pins the exact keys of `--json` for the claim and comment commands, so moving them onto
//! typed clients can't change what scripts and agents parse.

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
const CLAIM: &[&str] = &["claimed_at", "expires_at", "holder_id", "on_behalf_of", "task_id", "workspace_id"];
const COMMENT: &[&str] = &[
    "addressed_to",
    "author_id",
    "body",
    "created_at",
    "deleted_at",
    "id",
    "kind",
    "on_behalf_of",
    "resolved_at",
    "task_id",
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

fn with(base: &[&'static str], extra: &[&'static str]) -> Vec<&'static str> {
    base.iter().chain(extra).copied().collect()
}

fn setup() -> Instance {
    let th = Instance::new();
    th.stdout(&["project", "add", "JS", "Json"]);
    th.stdout(&["add", "one", "-p", "JS"]);
    th
}

#[test]
fn claim_renew_mine_and_release_keep_their_keys() {
    let th = setup();

    let claimed = json(&th, &["claim", "JS-1"]);
    assert_keys(&claimed, &["claim", "handoff", "task"]);
    assert_eq!(claimed["handoff"], Value::Null);
    assert_keys(&claimed["claim"], CLAIM);
    assert_keys(&claimed["task"], TASK);
    assert_eq!(claimed["task"]["key"], json!("JS-1"));

    let mine = json(&th, &["mine"]);
    let mine = mine.as_array().unwrap();
    assert_eq!(mine.len(), 1);
    assert_keys(&mine[0], &["claim", "key", "status", "title"]);
    assert_keys(&mine[0]["claim"], CLAIM);

    assert_keys(&json(&th, &["renew", "JS-1"]), CLAIM);
    assert_keys(&json(&th, &["release", "JS-1"]), TASK);
}

#[test]
fn claim_prints_the_last_handoff() {
    let th = setup();
    th.stdout(&["claim", "JS-1"]);
    th.stdout(&["handoff", "JS-1", "left off here"]);
    th.stdout(&["release", "JS-1"]);
    let claimed = json(&th, &["claim", "JS-1"]);
    assert_keys(&claimed, &["claim", "handoff", "task"]);
    assert_eq!(claimed["handoff"]["body"], json!("left off here"));
    assert_keys(&claimed["handoff"], COMMENT);
}

#[test]
fn comment_ask_and_answer_keep_their_keys() {
    let th = setup();
    let note = json(&th, &["comment", "JS-1", "hi"]);
    assert_keys(&note, COMMENT);
    assert_eq!(note["kind"], json!("note"));
    assert_eq!(note["addressed_to"], Value::Null);

    let question = json(&th, &["ask", "JS-1", "a or b?"]);
    assert_keys(&question, COMMENT);
    assert_eq!(question["resolved_at"], Value::Null);

    let answered = json(&th, &["answer", "JS-1", "a"]);
    assert_keys(&answered, COMMENT);
    assert_eq!(answered["id"], question["id"]);
    assert_ne!(answered["resolved_at"], Value::Null);
}

#[test]
fn verdict_leaves_model_out_unless_given() {
    let th = setup();
    let bare = json(&th, &["verdict", "JS-1", "approve", "ok"]);
    assert_keys(&bare, &with(COMMENT, &["verdict"]));
    assert_eq!(bare["kind"], json!("review"));
    assert_eq!(bare["verdict"], json!("approve"));

    let modelled = json(&th, &["verdict", "JS-1", "request-changes", "no", "--model", "m"]);
    assert_keys(&modelled, &with(COMMENT, &["verdict", "model"]));
    assert_eq!(modelled["verdict"], json!("request_changes"));
    assert_eq!(modelled["model"], json!("m"));
}
