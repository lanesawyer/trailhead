//! Fixtures the board's tests share.

use serde_json::{Value, json};

use super::*;
use crate::fixture::{agent_json, at, claim_json, comment_json, project_json, task_json};

/// The board's fixture as the API's JSON.
#[allow(clippy::type_complexity)]
pub(super) fn input_json() -> (Vec<Value>, Vec<Value>, Vec<Value>, Vec<Value>, Vec<Value>) {
    let projects =
        vec![project_json(json!({"id": "p1", "key": "TRAIL"})), project_json(json!({"id": "p2", "key": "OPS"}))];
    let task = |id: &str, key: &str, project: &str, status: &str, parent: Option<&str>, prio: i64| {
        task_json(json!({"id": id, "key": key, "title": format!("title {key}"), "project_id": project,
                         "status": status, "parent_id": parent, "priority": prio}))
    };
    let mut tasks = vec![
        task("t102", "TRAIL-102", "p1", "open", None, 2),
        task("t103", "TRAIL-103", "p1", "in_progress", Some("t102"), 2),
        task("t104", "TRAIL-104", "p1", "in_progress", Some("t103"), 1),
        task("t105", "TRAIL-105", "p1", "open", Some("t102"), 3),
        task("t200", "OPS-1", "p2", "in_progress", None, 2),
    ];
    tasks[1]["grade"] = json!(3);
    tasks[2]["metadata"] = json!({"factory": {"attempts": 1}});
    let claims = vec![
        claim_json(json!({"task_id": "t103", "holder_id": "a1"})),
        claim_json(json!({"task_id": "t200", "holder_id": "u1"})),
    ];
    let comments = vec![
        comment_json(
            json!({"id": "c1", "task_id": "t103", "kind": "question", "body": "which db?", "created_at": at(1)}),
        ),
        comment_json(
            json!({"id": "c2", "task_id": "t103", "kind": "question", "body": "old", "resolved_at": at(2), "created_at": at(1)}),
        ),
        comment_json(json!({"id": "c3", "task_id": "t103", "kind": "handoff", "body": "first", "created_at": at(1)})),
        comment_json(
            json!({"id": "c4", "task_id": "t103", "kind": "handoff", "body": "second\nmore", "created_at": at(2)}),
        ),
    ];
    let agents = vec![agent_json(json!({"id": "a1", "name": "factory/trail-103-a1"}))];
    (projects, tasks, claims, comments, agents)
}

pub(super) fn typed<T: serde::de::DeserializeOwned>(values: Vec<Value>) -> Vec<T> {
    values.into_iter().map(|v| serde_json::from_value(v).unwrap()).collect()
}

pub(super) fn at_now() -> Timestamp {
    "2026-10-15T00:00:00Z".parse().unwrap()
}

pub(super) fn run(filter: &Filter) -> BoardView {
    run_with(filter, &[])
}

pub(super) fn run_with(filter: &Filter, runs: &[Run]) -> BoardView {
    let (projects, tasks, claims, comments, agents) = input_json();
    let (projects, tasks, claims, comments, agents) = (
        typed::<Project>(projects),
        typed::<Task>(tasks),
        typed::<Claim>(claims),
        typed::<Comment>(comments),
        typed::<Agent>(agents),
    );
    build(
        Input {
            projects: &projects,
            tasks: &tasks,
            claims: &claims,
            dependencies: &[],
            comments: &comments,
            agents: &agents,
            users: &[],
            runs,
            queues: &["TRAIL-102".to_string()],
            deep: &[],
            now: at_now(),
        },
        filter,
    )
}

pub(super) fn keys(view: &BoardView, status: Status) -> Vec<(String, usize)> {
    let col = view.columns.iter().find(|c| c.status == status).unwrap();
    col.cards.iter().map(|c| (c.key.clone(), c.depth)).collect()
}

pub(super) fn card<'a>(view: &'a BoardView, key: &str) -> &'a Card {
    view.columns.iter().flat_map(|c| &c.cards).find(|c| c.key == key).unwrap()
}

pub(super) fn badges(card: &Card) -> Vec<(BadgeKind, &str)> {
    card.badges.iter().map(|b| (b.kind, b.label.as_str())).collect()
}
