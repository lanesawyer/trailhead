//! Lookout drawn to PNGs, named `lookout-<view>-<theme>`: its main views, each set up from fixtures, and a task's
//! pictures in the inspector and the large view (`pictures`), from PNGs drawn in memory. `#[ignore]`d,
//! since it needs a render adapter: `mise run shots` runs every app's, and says where the pictures went.

use bevy::prelude::*;
use bevy::ui_widgets::Activate;
use serde_json::{Value, json};
use trailhead_factory::logs::{LogLevel, LogLine};
use trailhead_factory::{
    ControlState, FactorySnapshot, RunPhase, RunStatus, SandboxState, SandboxStatus, TaskKey, Usd,
};
use trailhead_lookout::LOOKOUT;
use trailhead_lookout::Lookout;
use trailhead_lookout::board::View;
use trailhead_lookout::factory::{FactoryItem, FactoryUpdate};
use trailhead_lookout::inspector::{OpenComposer, Selection, Subject};
use trailhead_lookout::logs::{LogItem, LogUpdate};
use trailhead_lookout::peak::MapMode;
use trailhead_lookout::peak::orbit::Orbit;
use trailhead_lookout::peak::place;
use trailhead_lookout::peak::terrain::{Peak, SEED, Trail};
use trailhead_lookout::pictures::{OpenFolds, PictureBytes, WantsPicture};
use trailhead_lookout::settings::{FACTORY_SETTINGS, LOGS_SETTINGS};
use trailhead_shell::connection::Connection;
use trailhead_shell::fixture::{agent_json, at, claim_json, comment_json, project_json, sid, task_json, user_json};
use trailhead_shell::settings::{OpenSettings, PageId, SettingsModal};
use trailhead_shell::sync::Snapshot;
use trailhead_shell::testing::shot::{ShotApp, ShotOptions, ShotTheme, shot};
use trailhead_shell::testing::{fill, set_connection, settle};
use trailhead_shell::{ActiveApp, ShellPlugin};
use trailhead_ui::{ButtonGroup, ModalRoot, TextSize};
use trailhead_wire::ui::RepoSnapshot;

/// Saves the view as it is now, and fails on a frame with nothing drawn in it.
fn save(shot: &mut ShotApp, view: &str) {
    let picture = shot.picture();
    let name = shot.options().name("lookout", view);
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

/// Runs in every phase, so the hikers stand at five heights on the peak: the snapshot's tasks, and OPS-1 in a second
/// repo.
fn climbing() -> FactoryItem {
    let run = |task: &str, phase| RunStatus {
        task: TaskKey(task.into()),
        phase,
        sandbox: None,
        steps: 12,
        tool_calls: 20,
        spend: Usd(640_000),
        last_tool: None,
        started: 0,
        phase_started: 0,
    };
    let snapshot = |runs| FactorySnapshot {
        control: ControlState::Up,
        last_error: None,
        sandboxes: Vec::new(),
        runs,
        seq: 2,
        holding: None,
        targets: Vec::new(),
        human_grades: None,
        queue: Vec::new(),
        usage: Vec::new(),
    };
    let trail = vec![
        run("TRAIL-2", RunPhase::Preparing { cold: false }),
        run("TRAIL-7", RunPhase::Agent),
        run("TRAIL-3", RunPhase::Checks),
        run("TRAIL-6", RunPhase::Reviewing),
    ];
    let ops = vec![run("OPS-1", RunPhase::Publishing)];
    FactoryItem(FactoryUpdate::Snapshot(vec![
        RepoSnapshot { repo: "o/r".into(), snapshot: snapshot(trail) },
        RepoSnapshot { repo: "o/ops".into(), snapshot: snapshot(ops) },
    ]))
}

/// The peak from a fixed place on its orbit, with the clock it turns by stopped, so the picture is the same each run.
fn look_from(shot: &mut ShotApp, orbit: Orbit) {
    shot.world_mut().resource_mut::<Time<Virtual>>().pause();
    *shot.world_mut().resource_mut::<Orbit>() = orbit;
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

fn open_settings(shot: &mut ShotApp, page: PageId) {
    shot.world_mut().trigger(OpenSettings { page: Some(page) });
    settle(shot);
    let world = shot.world_mut();
    assert_eq!(world.query::<&SettingsModal>().iter(world).count(), 1, "settings is open");
}

/// A few of the control plane's lines about TRAIL-3's run, a warning and an error among them.
fn log_lines() -> Vec<LogLine> {
    let line = |n: i64, level, task: Option<&str>, message: &str| LogLine {
        ts_ms: 1_767_000_000_000 + n * 7_000,
        level,
        target: "trailhead_control".into(),
        repo: Some("o/r".into()),
        task: task.map(String::from),
        message: message.into(),
    };
    vec![
        line(0, LogLevel::Info, None, "control plane up; 2 sandboxes warm"),
        line(1, LogLevel::Info, Some("TRAIL-3"), "claimed; starting a run in trail-3-a1"),
        line(2, LogLevel::Info, Some("TRAIL-3"), "harness started: claude-code"),
        line(3, LogLevel::Warn, Some("OPS-1"), "paused: waiting on an answer to a question"),
        line(4, LogLevel::Info, Some("TRAIL-3"), "checks: cargo test -p trailhead-shell"),
        line(5, LogLevel::Error, Some("TRAIL-2"), "push refused: the branch moved; retrying after a fetch"),
        line(6, LogLevel::Info, Some("TRAIL-3"), "checks passed; opening a pull request"),
    ]
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
    // At 150%, as Ctrl and = leave it, in the same window: fewer columns fit, and the board scrolls across to them.
    shot.set_options(ShotOptions { text_size: TextSize::nearest(150), ..board });
    save(&mut shot, "board");
    shot.set_options(ShotOptions::default());

    shot.world_mut().resource_mut::<Selection>().0 = Some(Subject::Task(sid("t3")));
    settle(&mut shot);
    save(&mut shot, "inspector");
    shot.set_text_size(TextSize::nearest(150));
    save(&mut shot, "inspector");
    shot.set_text_size(TextSize::DEFAULT);
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

    shot.world_mut().write_message(factory());
    settle(&mut shot);
    // Lookout's pages in the shell's settings: the factory's state, and its log following a run.
    open_settings(&mut shot, FACTORY_SETTINGS);
    save(&mut shot, "settings-factory");
    open_settings(&mut shot, LOGS_SETTINGS);
    shot.world_mut().write_message(LogItem(LogUpdate::Backlog(log_lines())));
    settle(&mut shot);
    save(&mut shot, "settings-logs");
    close_modals(&mut shot);

    // Wide enough for the camps and both columns of waypoints.
    shot.set_size((1440, 800));
    go(&mut shot, View::Map);
    shot.world_mut().resource_mut::<NextState<MapMode>>().set(MapMode::Flat);
    settle(&mut shot);
    save(&mut shot, "map");
    shot.set_text_size(TextSize::nearest(150));
    save(&mut shot, "map");
    shot.set_text_size(TextSize::DEFAULT);

    // The peak, with a hiker for each run, by night and by day, and from higher up round the far side, where the
    // peak hides some of them and their labels.
    shot.world_mut().write_message(climbing());
    shot.world_mut().resource_mut::<NextState<MapMode>>().set(MapMode::Peak);
    look_from(&mut shot, Orbit::new(0.92, 0.42, 27.0));
    save(&mut shot, "peak");
    // At 150% the labels still sit by their hikers: the scene keeps its size, the labels are placed in UI pixels.
    shot.set_text_size(TextSize::nearest(150));
    save(&mut shot, "peak");
    shot.set_text_size(TextSize::DEFAULT);
    shot.set_theme(ShotTheme::Light);
    save(&mut shot, "peak");
    shot.set_theme(ShotTheme::Dark);
    look_from(&mut shot, Orbit::new(3.6, 0.7, 22.0));
    save(&mut shot, "peak-above");
}

/// Presses the mouse at `at` (logical pixels in the shot's image), moves it by `drag`, and lets go.
fn press_and_release(shot: &mut ShotApp, at: Vec2, drag: Vec2) {
    use bevy::picking::pointer::{Location, PointerAction, PointerButton, PointerId, PointerInput};
    let world = shot.world_mut();
    let target = world.query_filtered::<&bevy::camera::RenderTarget, With<Camera2d>>().single(world).unwrap();
    let target = target.normalize(None).expect("the shot's image");
    let input = |position: Vec2, action| {
        PointerInput::new(PointerId::Mouse, Location { target: target.clone(), position }, action)
    };
    let steps = [
        input(at, PointerAction::Move { delta: Vec2::ZERO }),
        input(at, PointerAction::Press(PointerButton::Primary)),
        input(at + drag, PointerAction::Move { delta: drag }),
        input(at + drag, PointerAction::Release(PointerButton::Primary)),
    ];
    for step in steps {
        shot.world_mut().write_message(step);
        settle(shot);
    }
}

/// Where a run in `phase` stands on screen, through the peak's camera.
fn on_screen(shot: &mut ShotApp, phase: RunPhase) -> Vec2 {
    let trail = Trail::new(&Peak::new(SEED));
    let hiker = trail.at(place::distance(phase)) + Vec3::Y * 0.3;
    let world = shot.world_mut();
    let (camera, eye) =
        world.query_filtered::<(&Camera, &GlobalTransform), With<Camera3d>>().single(world).expect("the peak's camera");
    camera.world_to_viewport(eye, hiker).expect("the hiker is in view")
}

/// Clicks on the peak go through the UI over it to the meshes, with Bevy's mesh picking: a hiker opens its task, and
/// a drag that ends on one turns the camera instead.
#[test]
#[ignore = "needs a render adapter: run with `mise run shots`"]
fn clicking_a_hiker_opens_its_task_and_dragging_turns_the_camera() {
    let mut shot = shot(ShellPlugin::new().with(Lookout::new()), ShotOptions::default());
    // Bevy's pointer rays are made only where there is a primary window, though the cameras draw into the shot's
    // image; one that is never opened stands in for it.
    shot.world_mut().spawn((Window::default(), bevy::window::PrimaryWindow));
    set_connection(&mut shot, Connection::Live);
    fill(&mut shot, snapshot());
    shot.world_mut().write_message(climbing());
    go(&mut shot, View::Map);
    look_from(&mut shot, Orbit::new(0.92, 0.42, 27.0));
    let hiker = on_screen(&mut shot, RunPhase::Checks);

    press_and_release(&mut shot, hiker, Vec2::new(0.0, 60.0));
    assert_eq!(shot.world().resource::<Selection>().0, None, "a drag opens nothing");
    assert_ne!(shot.world().resource::<Orbit>().pitch, 0.42, "and tips the camera");

    look_from(&mut shot, Orbit::new(0.92, 0.42, 27.0));
    press_and_release(&mut shot, hiker, Vec2::ZERO);
    assert_eq!(shot.world().resource::<Selection>().0, Some(Subject::Task(sid("t3"))), "TRAIL-3 is open");

    // Its modal now covers the peak: a click on another hiker lands on the modal, not on the hiker under it.
    let other = on_screen(&mut shot, RunPhase::Agent);
    press_and_release(&mut shot, other, Vec2::ZERO);
    assert_ne!(shot.world().resource::<Selection>().0, Some(Subject::Task(sid("t7"))), "the UI on top wins");
    shot.world_mut().resource_mut::<Selection>().0 = None;
    settle(&mut shot);
    press_and_release(&mut shot, other, Vec2::ZERO);
    assert_eq!(shot.world().resource::<Selection>().0, Some(Subject::Task(sid("t7"))), "with the modal shut it opens");
}

/// A small window's screenshot, drawn in memory: a header bar, a sidebar and rows, and in the after a new row in the
/// accent and a wider sidebar, so the two differ where a person would look.
fn screen(after: bool, tint: [u8; 3]) -> Vec<u8> {
    let (w, h) = (480u32, 300u32);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    let sidebar = if after { 140 } else { 110 };
    for y in 0..h {
        for x in 0..w {
            let px = if y < 36 {
                tint
            } else if x < sidebar {
                [44, 48, 56]
            } else if (60..280).contains(&y) && (y - 60) % 44 < 30 && x > sidebar + 20 && x < w - 20 {
                let row = (y - 60) / 44;
                if after && row == 2 { [232, 140, 60] } else { [70, 76, 88] }
            } else {
                [30, 32, 38]
            };
            rgba.extend([px[0], px[1], px[2], 255]);
        }
    }
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header().and_then(|mut wr| wr.write_image_data(&rgba)).expect("encoding into memory");
    bytes
}

fn picture_json(id: &str, name: &str, role: Option<&str>, set: &str, sha: &str, at_n: u8) -> Value {
    trailhead_shell::fixture::attachment_json(json!({
        "id": id, "task_id": "t4", "name": name, "role": role, "set": set, "sha256": sha,
        "width": 480, "height": 300, "created_at": at(at_n),
    }))
}

/// What the server answers for a picture's hash.
type Served = (&'static str, Result<Vec<u8>, String>);

/// TRAIL-4's pictures: a run with a changed view, a new one, a removed one and an unchanged one, and an older run whose
/// two pictures are one still loading and one that failed (the kit's other two states).
fn with_pictures(mut snapshot: Snapshot) -> (Snapshot, Vec<Served>) {
    let set = "run-0193";
    snapshot.tasks[3]["attachments"] = json!([
        picture_json("f1", "board", Some("before"), set, "board-before", 10),
        picture_json("f2", "board", Some("after"), set, "board-after", 11),
        picture_json("f3", "inbox", Some("after"), set, "inbox-after", 11),
        picture_json("f4", "ask", Some("before"), set, "ask-before", 10),
        picture_json("f5", "map", Some("before"), set, "map", 10),
        picture_json("f6", "map", Some("after"), set, "map", 11),
        picture_json("f7", "board", Some("after"), "run-0188", "old-loading", 2),
        picture_json("f8", "inbox", Some("after"), "run-0188", "old-failed", 2),
    ]);
    let bytes = vec![
        ("board-before", Ok(screen(false, [60, 90, 160]))),
        ("board-after", Ok(screen(true, [60, 90, 160]))),
        ("inbox-after", Ok(screen(true, [40, 130, 90]))),
        ("ask-before", Ok(screen(false, [150, 70, 120]))),
        ("map", Ok(screen(false, [120, 110, 60]))),
        ("old-failed", Err("404 not_found".into())),
    ];
    (snapshot, bytes)
}

/// Answers every read of a picture the shot has asked for, as the server would, until none is left but the one kept
/// loading.
fn answer_pictures(shot: &mut ShotApp, bytes: &[Served]) {
    for _ in 0..6 {
        for (sha, result) in bytes {
            let reply = PictureBytes { sha256: sha.to_string(), result: result.clone() };
            shot.world_mut().write_message(reply);
        }
        settle(shot);
    }
}

/// The thumbnail showing the picture with this hash.
fn thumbnail(shot: &mut ShotApp, sha: &str) -> Entity {
    let world = shot.world_mut();
    let frame = world
        .query::<(Entity, &WantsPicture)>()
        .iter(world)
        .find(|(_, w)| w.0.sha256 == sha)
        .map(|(e, _)| e)
        .expect("a thumbnail for it");
    world.get::<ChildOf>(frame).expect("a thumbnail's frame is in it").parent()
}

#[test]
#[ignore = "needs a render adapter: run with `mise run shots`"]
fn pictures() {
    // Tall enough for the older run under the newer one.
    let tall = ShotOptions { size: (1280, 1300), ..ShotOptions::default() };
    let mut shot = shot(ShellPlugin::new().with(Lookout::new()), tall);
    set_connection(&mut shot, Connection::Live);
    let (snapshot, bytes) = with_pictures(snapshot());
    fill(&mut shot, snapshot);
    // TRAIL-4's card says it has pictures.
    save(&mut shot, "pictures-card");
    shot.world_mut().resource_mut::<Selection>().0 = Some(Subject::Task(sid("t4")));
    settle(&mut shot);
    answer_pictures(&mut shot, &bytes);
    save(&mut shot, "pictures");
    shot.set_theme(ShotTheme::Light);
    save(&mut shot, "pictures");
    shot.set_theme(ShotTheme::Dark);

    shot.world_mut().resource_mut::<OpenFolds>().0.insert("run-0193".into());
    settle(&mut shot);
    answer_pictures(&mut shot, &bytes);
    save(&mut shot, "pictures-unchanged");

    // The large view of the changed pair: side by side, then each alone in the same place.
    let board = thumbnail(&mut shot, "board-after");
    shot.world_mut().trigger(Activate { entity: board });
    settle(&mut shot);
    save(&mut shot, "picture-view");
    let world = shot.world_mut();
    let groups: Vec<(Entity, usize)> =
        world.query::<(Entity, &ButtonGroup, &Children)>().iter(world).map(|(e, _, c)| (e, c.len())).collect();
    let (shown, _) = *groups.iter().find(|(_, n)| *n == 3).expect("the pair's group: both, before, after");
    for (index, view) in [(1, "picture-view-before"), (2, "picture-view-after")] {
        // Pressed as a person presses it, so the group shows the choice too.
        let option = shot.world().get::<Children>(shown).expect("the group's options")[index];
        shot.world_mut().trigger(Activate { entity: option });
        settle(&mut shot);
        save(&mut shot, view);
    }
    shot.set_text_size(TextSize::nearest(150));
    save(&mut shot, "picture-view-after");
    shot.set_text_size(TextSize::DEFAULT);
    let (fit, _) = *groups.iter().find(|(_, n)| *n == 2).expect("the group of Fit and Actual size");
    let actual = shot.world().get::<Children>(fit).expect("the group's options")[1];
    shot.world_mut().trigger(Activate { entity: actual });
    settle(&mut shot);
    save(&mut shot, "picture-view-actual");
}
