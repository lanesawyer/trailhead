//! Lookout drawn to PNGs, named `lookout-<view>-<theme>`: its main views, each set up from fixtures. `#[ignore]`d,
//! since it needs a render adapter: `mise run shots` runs every app's, and says where the pictures went.

use bevy::prelude::*;
use bevy::ui_widgets::Activate;
use serde_json::{Value, json};
use trailhead_factory::{
    ControlState, FactorySnapshot, RunPhase, RunStatus, SandboxState, SandboxStatus, TaskKey, Usd,
};
use trailhead_lookout::LOOKOUT;
use trailhead_lookout::Lookout;
use trailhead_lookout::board::View;
use trailhead_lookout::factory::{FactoryItem, FactoryUpdate};
use trailhead_lookout::inspector::{OpenComposer, Selection, Subject};
use trailhead_lookout::settings::{SettingsButton, SettingsModal};
use trailhead_shell::connection::Connection;
use trailhead_shell::fixture::{agent_json, at, claim_json, comment_json, project_json, sid, task_json, user_json};
use trailhead_shell::sync::Snapshot;
use trailhead_shell::testing::shot::{ShotApp, ShotOptions, ShotTheme, shot};
use trailhead_shell::testing::{fill, set_connection, settle};
use trailhead_shell::{ActiveApp, ShellPlugin};
use trailhead_ui::ModalRoot;
use trailhead_wire::ui::RepoSnapshot;

/// Saves the view as it is now, and fails on a frame with nothing drawn in it.
fn save(shot: &mut ShotApp, view: &str) {
    let picture = shot.picture();
    let name = format!("lookout-{view}-{}", shot.options().theme.word());
    assert_eq!(picture.size(), shot.options().pixels(), "{name} is the size asked for");
    assert!(picture.colors() > 16, "{name} has only {} colors: nothing was drawn", picture.colors());
    println!("{}", picture.save(&name).display());
}

fn task(id: &str, key: &str, title: &str, status: &str, priority: i64) -> Value {
    task_json(json!({"id": id, "key": key, "title": title, "status": status, "priority": priority}))
}

/// Two projects, and tasks in every status: subtasks, a grade, holders, a question, a handoff, a blocker and a PR.
fn snapshot() -> Snapshot {
    let projects = vec![
        project_json(json!({"id": "p1", "key": "TRAIL", "name": "Trailhead"})),
        project_json(json!({"id": "p2", "key": "OPS", "name": "Operations"})),
    ];
    let mut tasks = vec![
        task("t1", "TRAIL-1", "Draw every view to a PNG with no window", "open", 1),
        task("t2", "TRAIL-2", "Software rendering in the sandbox image", "in_progress", 2),
        task("t3", "TRAIL-3", "Attach before and after to the pull request", "in_review", 2),
        task("t4", "TRAIL-4", "Show the pictures on the task", "open", 2),
        task("t5", "TRAIL-5", "Name each view the same way in every app", "done", 3),
        task("t6", "TRAIL-6", "A shots test for the example app", "approved", 3),
        task("t7", "TRAIL-7", "A shots test for Lookout", "in_progress", 2),
        task("t8", "OPS-1", "Rotate the deploy keys", "needs_input", 4),
    ];
    tasks[1]["grade"] = json!(2);
    tasks[2]["grade"] = json!(3);
    tasks[2]["metadata"] = json!({"factory": {"attempt": 1, "sandbox": "trail-3-a1",
        "pr": {"repo": {"owner": "o", "name": "r"}, "number": 9, "url": "https://host/o/r/pull/9"}}});
    tasks[3]["blocked_by"] = json!([sid("t3")]);
    tasks[5]["parent_id"] = json!(sid("t1"));
    tasks[6]["parent_id"] = json!(sid("t1"));
    tasks[7]["project_id"] = json!(sid("p2"));
    tasks[7]["metadata"] = json!({"factory": {"attempt": 1, "sandbox": "ops-1-a1"}});
    tasks[1]["claim"] = claim_json(json!({"task_id": "t2", "holder_id": "u1"}));
    tasks[2]["claim"] = claim_json(json!({"task_id": "t3", "holder_id": "a1"}));
    tasks[6]["claim"] = claim_json(json!({"task_id": "t7", "holder_id": "a2"}));
    tasks[7]["open_questions"] = json!([comment_json(json!({"id": "c1", "task_id": "t8", "kind": "question",
        "author_id": "a2", "body": "Which keys rotate first?", "created_at": at(1)}))]);
    tasks[2]["last_handoff"] = comment_json(json!({"id": "c2", "task_id": "t3", "kind": "handoff",
        "author_id": "a1", "body": "Attached both, side by side.", "created_at": at(2)}));
    let agents = vec![
        agent_json(json!({"id": "a1", "name": "factory/trail-3-a1"})),
        agent_json(json!({"id": "a2", "name": "claude"})),
    ];
    let users = vec![user_json(json!({"id": "u1", "name": "lane"}))];
    Snapshot { at: 1, projects, agents, users, tasks }
}

/// The factory's pool: TRAIL-3's run in its sandbox, one sandbox paused and two idle.
fn factory() -> FactoryItem {
    let sandbox = |name: &str, state, task: Option<&str>, role: Option<&str>| SandboxStatus {
        name: name.into(),
        target: None,
        state,
        task: task.map(|t| TaskKey(t.into())),
        role: role.map(Into::into),
        warm: true,
        own: false,
    };
    let run = RunStatus {
        task: TaskKey("TRAIL-3".into()),
        phase: RunPhase::Checks,
        sandbox: Some("trail-3-a1".into()),
        steps: 42,
        tool_calls: 57,
        spend: Usd(1_870_000),
        last_tool: Some("Bash".into()),
        started: 0,
        phase_started: 0,
    };
    let snapshot = FactorySnapshot {
        control: ControlState::Up,
        last_error: None,
        sandboxes: vec![
            sandbox("trail-3-a1", SandboxState::Busy, Some("TRAIL-3"), Some("worker")),
            sandbox("ops-1-a1", SandboxState::Paused, Some("OPS-1"), Some("worker")),
            sandbox("pool-1", SandboxState::Idle, None, None),
            sandbox("pool-2", SandboxState::Idle, None, None),
        ],
        runs: vec![run],
        seq: 1,
        holding: None,
        targets: Vec::new(),
        human_grades: None,
        queue: Vec::new(),
        usage: Vec::new(),
    };
    FactoryItem(FactoryUpdate::Snapshot(vec![RepoSnapshot { repo: "o/r".into(), snapshot }]))
}

fn press<T: Component>(shot: &mut ShotApp) {
    let world = shot.world_mut();
    let found = world.query_filtered::<Entity, With<T>>().iter(world).next().expect("the button to press");
    world.trigger(Activate { entity: found });
    settle(shot);
}

fn close_modals(shot: &mut ShotApp) {
    let world = shot.world_mut();
    let open: Vec<Entity> = world.query_filtered::<Entity, With<ModalRoot>>().iter(world).collect();
    for modal in open {
        world.despawn(modal);
    }
    settle(shot);
}

fn go(shot: &mut ShotApp, view: View) {
    shot.world_mut().resource_mut::<NextState<View>>().set(view);
    settle(shot);
}

#[test]
#[ignore = "needs a render adapter: run with `mise run shots`"]
fn shots() {
    // Wide enough for every open column of the board.
    let board = ShotOptions { size: (1680, 900), ..ShotOptions::default() };
    let mut shot = shot(ShellPlugin::new().with(Lookout::new()), board);
    set_connection(&mut shot, Connection::Live);
    fill(&mut shot, snapshot());
    assert_eq!(shot.world().resource::<State<ActiveApp>>().get().0, LOOKOUT);

    save(&mut shot, "board");
    shot.set_theme(ShotTheme::Light);
    save(&mut shot, "board");
    shot.set_options(ShotOptions::default());

    shot.world_mut().resource_mut::<Selection>().0 = Some(Subject::Task(sid("t3")));
    settle(&mut shot);
    save(&mut shot, "inspector");
    shot.world_mut().resource_mut::<Selection>().0 = None;
    settle(&mut shot);

    // The answer form on a question the factory parked on: Send, and Set aside beside it.
    let question = Some((sid("c1"), "Which keys rotate first?".to_string()));
    shot.world_mut().trigger(OpenComposer { key: "OPS-1".into(), answer: question });
    settle(&mut shot);
    save(&mut shot, "answer");
    close_modals(&mut shot);
    shot.world_mut().resource_mut::<Selection>().0 = None;
    settle(&mut shot);

    press::<SettingsButton>(&mut shot);
    let world = shot.world_mut();
    assert_eq!(world.query::<&SettingsModal>().iter(world).count(), 1, "settings is open");
    save(&mut shot, "settings");
    close_modals(&mut shot);

    shot.world_mut().write_message(factory());
    // Wide enough for the camps and both columns of waypoints.
    shot.set_size((1440, 800));
    go(&mut shot, View::Map);
    save(&mut shot, "map");
}
