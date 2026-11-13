//! Fixtures the inspector's tests share.

use bevy::state::app::StatesPlugin;
use serde_json::Value;

use super::*;
use crate::fixture::{agent, claim, comment, task};
use crate::fixture::{at, comment_json, sid, task_json};
use crate::sync::Snapshot;

pub(super) struct Fixture {
    pub(super) projects: Vec<Project>,
    pub(super) tasks: Vec<Task>,
    pub(super) claims: Vec<Claim>,
    pub(super) comments: Vec<Comment>,
    pub(super) agents: Vec<Agent>,
}

pub(super) fn as_json<T: serde::Serialize>(items: &[T]) -> Vec<Value> {
    items.iter().map(|i| serde_json::to_value(i).unwrap()).collect()
}

impl Fixture {
    pub(super) fn input(&self) -> Input<'_> {
        Input {
            projects: &self.projects,
            tasks: &self.tasks,
            claims: &self.claims,
            comments: &self.comments,
            agents: &self.agents,
            ..Input::default()
        }
    }
}

pub(super) fn fixture() -> Fixture {
    let factory = json!({
        "sandbox": "trail-7-a1", "attempt": 2,
        "pr": {"repo": {"owner": "o", "name": "r"}, "number": 12, "url": "https://github.com/o/r/pull/12"},
        "grade": {"grade": 2},
        "merge": {"grade": 2, "head": "abc", "passed": ["checks", "ci"], "waiting": ["agent_review"],
                  "would_merge": false, "problem": null, "message": "Grade 2: waiting for agent review."},
    });
    Fixture {
        projects: vec![],
        tasks: vec![
            task(
                json!({"id": "t7", "key": "TRAIL-7", "title": "do it", "status": "in_review", "priority": 1, "version": 4,
                   "metadata": {"factory": factory}}),
            ),
            task(json!({"id": "t8", "key": "TRAIL-8", "title": "plain", "status": "open", "version": 1})),
        ],
        claims: vec![claim(json!({"task_id": "t7", "holder_id": "a1"}))],
        comments: vec![
            comment(json!({"id": "c1", "task_id": "t7", "kind": "question", "body": "which db?", "created_at": at(1)})),
            comment(
                json!({"id": "c2", "task_id": "t7", "kind": "question", "body": "old", "resolved_at": at(2), "created_at": at(1)}),
            ),
        ],
        agents: vec![agent(json!({"id": "a1", "name": "factory/trail-7-a1"}))],
    }
}

pub(super) fn question(id: &str, body: &str) -> SyncItem {
    crate::fixture::sync_event(json!({
        "id": id.trim_start_matches('c').parse::<i64>().unwrap() + 100, "entity": "comment", "entity_id": sid(id),
        "kind": "comment.created",
        "after": comment_json(json!({"id": id, "task_id": "t7", "kind": "question", "body": body,
                  "created_at": at(id.trim_start_matches('c').parse().unwrap())})),
    }))
}

pub(super) fn settle(app: &mut App) {
    for _ in 0..5 {
        app.update();
    }
}

pub(super) fn app() -> App {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, StatesPlugin, AssetPlugin::default(), bevy::scene::ScenePlugin))
        .init_asset::<bevy::text::Font>()
        .init_asset::<Image>()
        .init_state::<Connection>()
        .init_resource::<ConnectionStatus>()
        .add_plugins((
            crate::mirror::MirrorPlugin,
            InspectorUiPlugin,
            crate::widgets::ModalPlugin,
            crate::widgets::InlinePlugin,
        ));
    let tasks = vec![
        task_json(
            json!({"id": "t7", "key": "TRAIL-7", "title": "do it", "status": "in_progress", "priority": 1, "version": 4}),
        ),
        task_json(json!({"id": "t8", "key": "TRAIL-8", "title": "other", "status": "open", "version": 1})),
        task_json(json!({"id": "t9", "key": "TRAIL-9", "title": "a part", "status": "open", "parent_id": "t7"})),
    ];
    app.world_mut().write_message(SyncItem::Snapshot(Snapshot {
        at: 5,
        projects: vec![],
        tasks,
        agents: vec![],
        ..Default::default()
    }));
    app.world_mut().write_message(question("c1", "which db?"));
    app.world_mut().resource_mut::<NextState<Connection>>().set(Connection::Live);
    app.world_mut().resource_mut::<Selection>().0 = Some(Subject::Task(sid("t7")));
    settle(&mut app);
    app
}

/// Every entity in the task modal's two columns, in order.
pub(super) fn body(app: &mut App) -> Vec<Entity> {
    let world = app.world_mut();
    let main = world.query_filtered::<Entity, With<crate::widgets::DetailMain>>().single(world).unwrap();
    let side = world.query_filtered::<Entity, With<crate::widgets::DetailSide>>().single(world).unwrap();
    let mut found = vec![main, side];
    let mut at = 0;
    while at < found.len() {
        if let Some(children) = world.get::<Children>(found[at]) {
            found.extend(children.iter());
        }
        at += 1;
    }
    found
}

pub(super) fn texts(app: &mut App) -> Vec<String> {
    let world = app.world_mut();
    world.query::<&Text>().iter(world).map(|t| t.0.clone()).collect()
}

pub(super) fn latest() -> HashMap<String, i64> {
    HashMap::from([(sid("t7"), 41)])
}

pub(super) fn sent(app: &mut App) -> Vec<Submit> {
    app.world_mut().resource_mut::<Messages<Submit>>().drain().collect()
}

pub(super) fn composer_input(app: &mut App) -> Entity {
    let world = app.world_mut();
    let mut q = world.query::<(Entity, &ComposePart)>();
    q.iter(world).find(|(_, p)| **p == ComposePart::Input).map(|(e, _)| e).expect("the comment box's input")
}

pub(super) fn modal_count(app: &mut App) -> usize {
    app.world_mut().query_filtered::<(), With<TaskModal>>().iter(app.world()).count()
}
