//! The trail map (`docs/factory/ui.md#views`): campsites are the pooled sandboxes (idle ones too), waypoints are the
//! tasks being worked, and agents walk between them. Everything comes from the two mirrors, Trailhead's
//! ([`crate::mirror`]) and the factory's ([`crate::factory`]).
//!
//! What moves, and what only marks:
//! - An agent is whoever holds a claim. It stands at its task's waypoint until the task's run has a sandbox, then
//!   walks to that campsite; when the run ends, or the sandbox is released, it walks back. So a claim is an agent
//!   appearing at a waypoint, a run is the walk to a camp, and a handoff is the walk back.
//! - Markers sit on the waypoint: `?` for open questions (the task is parked), the last handoff, `review` and
//!   `approved` for the verdict so far, and the task's spend while a run has it.
//! - A campsite shows its state (idle, busy, paused, broken), its role, whether it is warm, and the task and phase
//!   it holds.
//!
//! [`build`] is a pure function over the mirrors' data and gives positions in map pixels, so the layout and what
//! moves are tested without a window. [`MapPlugin`] keeps a [`TrailMap`] from the mirrors; [`MapUiPlugin`] draws it
//! with UI nodes in [`View::Map`], and agents glide to a new spot instead of jumping.

use std::collections::HashMap;

use bevy::feathers::controls::FeathersButton;
use bevy::feathers::display::caption;
use bevy::prelude::*;
use bevy::ui_widgets::Activate;
use trailhead_factory::{RunPhase, SandboxState};
use trailhead_model::{Agent, Claim, Comment, CommentKind, Status, Task};
use uuid::Uuid;

use crate::board::View;
use crate::factory::{Campsite, FactoryStatus, Run};
use crate::mirror::{Mirrored, Rev, rows};
use crate::widgets::{space, status_label};

/// Campsites sit on a grid on the left, this many to a row.
const CAMP_COLUMNS: usize = 3;
const CAMP_CELL: Vec2 = Vec2::new(190.0, 100.0);
const CAMP_SIZE: Vec2 = Vec2::new(170.0, 80.0);
/// Waypoints sit on a grid to the right of the camps.
const WAY_COLUMNS: usize = 2;
const WAY_CELL: Vec2 = Vec2::new(230.0, 76.0);
const WAY_SIZE: Vec2 = Vec2::new(210.0, 60.0);
const ORIGIN: Vec2 = Vec2::new(16.0, 56.0);
/// A walker stands this far from the corner of its spot, below the text.
const STAND: Vec2 = Vec2::new(6.0, 52.0);

/// Whether a task in `status` gets a waypoint: work under way or a decision waiting.
fn active(status: Status) -> bool {
    !status.is_closed() && status != Status::Open
}

#[derive(Debug, Clone, PartialEq)]
pub struct Camp {
    pub repo: String,
    pub name: String,
    pub state: SandboxState,
    pub role: Option<String>,
    pub warm: bool,
    /// The task it is held for and the phase its run is in.
    pub task: Option<String>,
    pub phase: Option<String>,
    /// Top left, in map pixels.
    pub pos: Vec2,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Marker {
    /// Held by this agent or user.
    Claim(String),
    /// Open questions: the task is waiting for an answer.
    Questions(usize),
    /// The first line of the last handoff.
    Handoff(String),
    /// A reviewer is on it.
    Reviewing,
    /// Waiting for a verdict or a merge.
    InReview,
    Approved,
    /// Dollars the task has cost so far.
    Spend(f64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Waypoint {
    pub key: String,
    pub title: String,
    pub status: Status,
    pub markers: Vec<Marker>,
    pub pos: Vec2,
}

/// Where an agent is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spot {
    /// A campsite, by repo and name.
    Camp(String, String),
    /// A waypoint, by task key.
    Waypoint(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Walker {
    /// The claim holder's id, which stays put when the name is filled in later.
    pub id: String,
    pub name: String,
    pub task: String,
    pub at: Spot,
    pub pos: Vec2,
}

#[derive(Resource, Default, Debug, Clone, PartialEq)]
pub struct MapModel {
    pub camps: Vec<Camp>,
    pub waypoints: Vec<Waypoint>,
    pub walkers: Vec<Walker>,
}

/// Everything [`build`] reads, as the mirrors hold it.
#[derive(Default, Clone, Copy)]
pub struct Input<'a> {
    pub tasks: &'a [Task],
    pub claims: &'a [Claim],
    pub comments: &'a [Comment],
    pub agents: &'a [Agent],
    pub campsites: &'a [Campsite],
    pub runs: &'a [Run],
}

fn key_number(key: &str) -> i64 {
    key.rsplit('-').next().and_then(|n| n.parse().ok()).unwrap_or(0)
}

fn grid(index: usize, columns: usize, cell: Vec2, origin: Vec2) -> Vec2 {
    origin + Vec2::new((index % columns) as f32 * cell.x, (index / columns) as f32 * cell.y)
}

/// Where the waypoints start: right of the camps, however many there are.
fn way_origin() -> Vec2 {
    ORIGIN + Vec2::new(CAMP_COLUMNS as f32 * CAMP_CELL.x + 40.0, 0.0)
}

/// Lays out the camps and the active tasks, and puts each claim holder where its run is.
pub fn build(input: Input) -> MapModel {
    let mut camps: Vec<&Campsite> = input.campsites.iter().collect();
    camps.sort_by(|a, b| (&a.repo, &a.status.name).cmp(&(&b.repo, &b.status.name)));
    // A task key names one task, so one run per key; if two repos have one, the later entry wins: the run that has a
    // sandbox, then the later repo.
    let mut runs: Vec<&Run> = input.runs.iter().collect();
    runs.sort_by(|a, b| (a.status.sandbox.is_some(), &a.repo).cmp(&(b.status.sandbox.is_some(), &b.repo)));
    let run_of: HashMap<&str, &Run> = runs.into_iter().map(|r| (r.status.task.0.as_str(), r)).collect();
    let camps: Vec<Camp> = camps
        .into_iter()
        .enumerate()
        .map(|(i, c)| Camp {
            repo: c.repo.clone(),
            name: c.status.name.clone(),
            state: c.status.state,
            role: c.status.role.clone(),
            warm: c.status.warm,
            task: c.status.task.as_ref().map(|t| t.0.clone()),
            phase: c.status.task.as_ref().and_then(|t| run_of.get(t.0.as_str())).map(|r| r.status.phase.label().into()),
            pos: grid(i, CAMP_COLUMNS, CAMP_CELL, ORIGIN),
        })
        .collect();

    let agent_name: HashMap<Uuid, &str> = input.agents.iter().map(|a| (a.id, a.name.as_str())).collect();
    let holder_of: HashMap<Uuid, Uuid> = input.claims.iter().map(|c| (c.task_id, c.holder_id)).collect();
    let on_behalf_of: HashMap<Uuid, Uuid> = input.claims.iter().map(|c| (c.task_id, c.on_behalf_of)).collect();
    // The agent list only changes with a sync snapshot, so a sub-agent made for a run after it isn't known yet. A
    // holder of a task that has a run is a factory agent all the same: name it for the task until the list has it.
    let holder_name = |holder: Uuid, key: &str| -> Option<String> {
        match agent_name.get(&holder) {
            Some(n) => Some(n.to_string()),
            None => run_of.contains_key(key).then(|| format!("factory/{}", key.to_lowercase())),
        }
    };
    let mut questions: HashMap<Uuid, usize> = HashMap::new();
    let mut handoffs: HashMap<Uuid, &Comment> = HashMap::new();
    for c in input.comments {
        match c.kind {
            CommentKind::Question if c.resolved_at.is_none() => *questions.entry(c.task_id).or_default() += 1,
            CommentKind::Handoff if handoffs.get(&c.task_id).is_none_or(|h| h.created_at <= c.created_at) => {
                handoffs.insert(c.task_id, c);
            }
            _ => {}
        }
    }

    let mut tasks: Vec<&Task> =
        input.tasks.iter().filter(|t| active(t.status) || run_of.contains_key(t.key.as_str())).collect();
    tasks.sort_by_key(|t| (key_number(&t.key), t.key.clone()));
    let origin = way_origin();
    let mut waypoints = Vec::new();
    for (i, t) in tasks.iter().enumerate() {
        let (key, status) = (t.key.as_str(), t.status);
        let run = run_of.get(key);
        let mut markers = Vec::new();
        if let Some(holder) = holder_of.get(&t.id) {
            // The snapshot carries no users, so a holder that isn't a known agent gets a label, not just part of an id.
            // A person claims for themselves (holder == on_behalf_of); anyone else is an agent the stale list lacks.
            let label = || {
                let id = holder.to_string().chars().take(8).collect::<String>();
                if on_behalf_of.get(&t.id) == Some(holder) {
                    format!("a person ({id})")
                } else {
                    format!("an agent ({id})")
                }
            };
            markers.push(Marker::Claim(holder_name(*holder, key).unwrap_or_else(label)));
        }
        if let Some(n) = questions.get(&t.id) {
            markers.push(Marker::Questions(*n));
        }
        if let Some(h) = handoffs.get(&t.id) {
            markers.push(Marker::Handoff(h.body.lines().next().unwrap_or_default().to_string()));
        }
        match status {
            Status::InReview => markers.push(Marker::InReview),
            Status::Approved => markers.push(Marker::Approved),
            _ => {}
        }
        if let Some(run) = run {
            if run.status.phase == RunPhase::Reviewing {
                markers.push(Marker::Reviewing);
            }
            markers.push(Marker::Spend(run.status.spend.as_dollars()));
        }
        waypoints.push(Waypoint {
            key: key.to_string(),
            title: t.title.clone(),
            status,
            markers,
            pos: grid(i, WAY_COLUMNS, WAY_CELL, origin),
        });
    }

    // Each claim holder that is an agent, at its run's camp if the run has one that is on the map, else at its task.
    let mut walkers = Vec::new();
    let mut claimed: Vec<(Uuid, Uuid)> = holder_of.iter().map(|(t, h)| (*t, *h)).collect();
    claimed.sort();
    for (task_id, holder) in claimed {
        let Some(wp) = waypoints.iter().zip(&tasks).find(|(_, t)| t.id == task_id).map(|(w, _)| w) else {
            continue;
        };
        let Some(name) = holder_name(holder, &wp.key) else { continue };
        let camp = run_of
            .get(wp.key.as_str())
            .and_then(|r| r.status.sandbox.as_deref().map(|sb| (&r.repo, sb)))
            .and_then(|(repo, sb)| camps.iter().find(|c| c.repo == *repo && c.name == sb));
        let (at, corner) = match camp {
            Some(c) => (Spot::Camp(c.repo.clone(), c.name.clone()), c.pos),
            None => (Spot::Waypoint(wp.key.clone()), wp.pos),
        };
        let pos = corner + STAND;
        walkers.push(Walker { id: holder.to_string(), name, task: wp.key.clone(), at, pos });
    }
    walkers.sort_by(|a, b| a.name.cmp(&b.name));
    MapModel { camps, waypoints, walkers }
}

fn state_color(state: SandboxState) -> Color {
    match state {
        SandboxState::Idle => Color::srgb(0.22, 0.30, 0.24),
        SandboxState::Busy => Color::srgb(0.20, 0.34, 0.50),
        SandboxState::Paused => Color::srgb(0.46, 0.38, 0.16),
        SandboxState::Broken => Color::srgb(0.50, 0.18, 0.18),
    }
}

fn marker_line(m: &Marker) -> (String, Color) {
    let warn = Color::srgb(0.95, 0.75, 0.3);
    let dim = Color::srgb(0.7, 0.7, 0.7);
    match m {
        Marker::Claim(who) => (format!("held by {who}"), dim),
        Marker::Questions(n) => (format!("? {n} open question{}", if *n == 1 { "" } else { "s" }), warn),
        Marker::Handoff(h) => (format!("handoff: {h}"), dim),
        Marker::Reviewing => ("reviewing".into(), Color::srgb(0.6, 0.8, 1.0)),
        Marker::InReview => (status_label(Status::InReview).into(), Color::srgb(0.6, 0.8, 1.0)),
        Marker::Approved => (status_label(Status::Approved).into(), Color::srgb(0.5, 0.9, 0.5)),
        Marker::Spend(d) => (format!("${d:.2}"), dim),
    }
}

fn camp_lines(c: &Camp) -> Vec<String> {
    let mut head = format!("{} · {}", c.name, c.state.as_str());
    if c.warm {
        head += " · warm";
    }
    let mut lines = vec![head];
    let held = [c.role.clone(), c.task.clone(), c.phase.clone()].into_iter().flatten().collect::<Vec<_>>();
    if !held.is_empty() {
        lines.push(held.join(" · "));
    }
    lines
}

#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn rebuild_map(
    changed: Query<(), Or<(Changed<Rev>, Changed<Campsite>, Changed<Run>)>>,
    mut removed_data: RemovedComponents<Rev>,
    mut removed_camps: RemovedComponents<Campsite>,
    mut removed_runs: RemovedComponents<Run>,
    tasks: Query<&Mirrored<Task>>,
    claims: Query<&Mirrored<Claim>>,
    comments: Query<&Mirrored<Comment>>,
    agents: Query<&Mirrored<Agent>>,
    campsites: Query<&Campsite>,
    runs: Query<&Run>,
    mut map: ResMut<TrailMap>,
) {
    let gone = removed_data.read().count() + removed_camps.read().count() + removed_runs.read().count() > 0;
    if !(gone || !changed.is_empty() || map.is_added()) {
        return;
    }
    let (tasks, claims, comments, agents) = (rows(&tasks), rows(&claims), rows(&comments), rows(&agents));
    let (campsites, runs): (Vec<_>, Vec<_>) = (campsites.iter().cloned().collect(), runs.iter().cloned().collect());
    let next = build(Input {
        tasks: &tasks,
        claims: &claims,
        comments: &comments,
        agents: &agents,
        campsites: &campsites,
        runs: &runs,
    });
    if map.0 != next {
        map.0 = next;
    }
}

/// The model the map draws.
#[derive(Resource, Default, Debug)]
pub struct TrailMap(pub MapModel);

/// Keeps [`TrailMap`] from the mirrors.
pub struct MapPlugin;

impl Plugin for MapPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TrailMap>().add_systems(Update, rebuild_map);
    }
}

/// Draws the map while in [`View::Map`].
pub struct MapUiPlugin;

impl Plugin for MapUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MapPlugin)
            .add_systems(OnEnter(View::Map), spawn_panel)
            .add_systems(OnExit(View::Map), despawn_panel)
            .add_systems(Update, (redraw, glide).chain().run_if(in_state(View::Map)));
    }
}

#[derive(Component)]
struct MapPanel;

/// Camps, waypoints and their text: despawned and drawn again whenever the model changes.
#[derive(Component)]
struct Terrain;

/// An agent's token, which outlives redraws so it can walk to where the model now puts it.
#[derive(Component)]
struct Token {
    id: String,
    target: Vec2,
}

fn spawn_panel(mut commands: Commands) {
    commands.spawn((Name::new("trail map"), MapPanel, Node { width: percent(100), height: percent(100), ..default() }));
}

fn despawn_panel(mut commands: Commands, panel: Query<Entity, With<MapPanel>>) {
    for e in &panel {
        commands.entity(e).despawn();
    }
}

/// A box at a map position, absolute inside the panel.
fn plot(pos: Vec2, size: Vec2, color: Color) -> impl Bundle {
    (
        Node {
            position_type: PositionType::Absolute,
            left: px(pos.x),
            top: px(pos.y),
            width: px(size.x),
            height: px(size.y),
            flex_direction: FlexDirection::Column,
            padding: UiRect::all(px(space::LABEL_INSET)),
            overflow: Overflow::clip(),
            ..default()
        },
        BackgroundColor(color),
    )
}

fn line(commands: &mut Commands, parent: Entity, text: String, color: Color) {
    commands.spawn((Text::new(text), TextColor(color), TextFont::from_font_size(12.0), ChildOf(parent)));
}

fn board_button() -> impl Scene {
    bsn! {
        @FeathersButton
        on(|_: On<Activate>, mut next: ResMut<NextState<View>>| next.set(View::Board))
        Children [ @caption("board") ]
    }
}

/// Tokens sit above the terrain, which is respawned (as later siblings) on every redraw.
const TOKEN_Z: i32 = 1;

/// Redraws the terrain when the model or the factory's link changes, and moves tokens' targets; [`glide`] walks them.
#[allow(clippy::too_many_arguments)]
fn redraw(
    map: Res<TrailMap>,
    factory: Res<FactoryStatus>,
    panel: Single<(Entity, Option<&Children>), With<MapPanel>>,
    mut tokens: Query<(Entity, &mut Token)>,
    terrain: Query<Entity, With<Terrain>>,
    mut commands: Commands,
    mut drawn: Local<Option<Entity>>,
    mut link: Local<Option<crate::factory::FactoryLink>>,
) {
    let (panel, _) = *panel;
    let fresh = *drawn != Some(panel);
    // The factory's status changes with every tool call; only its link is drawn.
    if !(fresh || map.is_changed() || *link != Some(factory.link.clone())) {
        return;
    }
    *drawn = Some(panel);
    *link = Some(factory.link.clone());
    for e in &terrain {
        commands.entity(e).despawn();
    }
    let ink = Color::WHITE;
    let stopped = factory.link != crate::factory::FactoryLink::Running;
    let bar = commands
        .spawn((
            Terrain,
            Node {
                position_type: PositionType::Absolute,
                left: px(ORIGIN.x),
                top: px(8),
                column_gap: px(space::ROWS),
                ..default()
            },
            ChildOf(panel),
        ))
        .id();
    commands.queue_spawn_scene(board_button()).insert(ChildOf(bar));
    let note = if stopped { format!("{}: showing the last known state", factory.label()) } else { factory.label() };
    let color = if stopped { Color::srgb(0.9, 0.7, 0.3) } else { Color::srgb(0.7, 0.7, 0.7) };
    commands.spawn((Text::new(note), TextColor(color), ChildOf(bar)));

    for c in &map.0.camps {
        let tint = if stopped { Color::srgb(0.2, 0.2, 0.2) } else { state_color(c.state) };
        let node = commands.spawn((Terrain, plot(c.pos, CAMP_SIZE, tint), ChildOf(panel))).id();
        for l in camp_lines(c) {
            line(&mut commands, node, l, ink);
        }
    }
    for w in &map.0.waypoints {
        let node = commands.spawn((Terrain, plot(w.pos, WAY_SIZE, Color::srgb(0.16, 0.16, 0.18)), ChildOf(panel))).id();
        line(&mut commands, node, format!("{} {}", w.key, w.title), ink);
        for (l, c) in w.markers.iter().map(marker_line) {
            line(&mut commands, node, l, c);
        }
    }

    // Tokens: move the ones the model still has, drop the rest, add the new at their spot.
    let mut have: HashMap<String, Entity> = HashMap::new();
    for (e, mut t) in &mut tokens {
        match map.0.walkers.iter().find(|w| w.id == t.id) {
            Some(w) => {
                t.target = w.pos;
                have.insert(t.id.clone(), e);
            }
            None => commands.entity(e).despawn(),
        }
    }
    for w in &map.0.walkers {
        if have.contains_key(&w.id) {
            continue;
        }
        let name = Name::new(format!("agent {}", w.name));
        let node =
            commands.spawn((Token { id: w.id.clone(), target: w.pos }, name, ZIndex(TOKEN_Z), ChildOf(panel))).id();
        commands.entity(node).insert(plot(w.pos, Vec2::new(20.0, 20.0), Color::srgb(0.95, 0.75, 0.3)));
        let initial = w.name.rsplit(['/', '-']).next().unwrap_or("a").to_string();
        commands.spawn((Text::new(initial), TextColor(Color::BLACK), TextFont::from_font_size(11.0), ChildOf(node)));
    }
}

/// Eases each token toward its target.
fn glide(time: Res<Time>, mut tokens: Query<(&Token, &mut Node)>) {
    let k = 1.0 - (-8.0 * time.delta_secs()).exp();
    for (token, mut node) in &mut tokens {
        let at = |v: Val, or: f32| if let Val::Px(p) = v { p } else { or };
        let (x, y) = (at(node.left, token.target.x), at(node.top, token.target.y));
        let (nx, ny) = (x + (token.target.x - x) * k, y + (token.target.y - y) * k);
        node.left = px(nx);
        node.top = px(ny);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use trailhead_factory::{RunStatus, SandboxStatus, TaskKey, Usd};

    use super::*;
    use crate::fixture::{self, at, claim_json, sid};

    fn camp(name: &str, state: SandboxState, task: Option<&str>) -> Campsite {
        let status = SandboxStatus {
            name: name.into(),
            target: None,
            state,
            task: task.map(|t| TaskKey(t.into())),
            role: task.map(|_| "worker".into()),
            warm: true,
            own: false,
        };
        Campsite { repo: "o/r".into(), status }
    }

    fn run(task: &str, sandbox: Option<&str>, phase: RunPhase, spend: f64) -> Run {
        Run {
            repo: "o/r".into(),
            status: RunStatus {
                task: TaskKey(task.into()),
                phase,
                sandbox: sandbox.map(Into::into),
                steps: 1,
                tool_calls: 1,
                spend: Usd::dollars(spend),
                last_tool: None,
                started: 0,
            },
        }
    }

    fn task(id: &str, key: &str, status: &str) -> Task {
        fixture::task(json!({"id": id, "key": key, "title": format!("title {key}"), "status": status}))
    }

    struct World {
        tasks: Vec<Task>,
        claims: Vec<Claim>,
        comments: Vec<Comment>,
        agents: Vec<Agent>,
        campsites: Vec<Campsite>,
        runs: Vec<Run>,
    }

    impl World {
        fn model(&self) -> MapModel {
            build(Input {
                tasks: &self.tasks,
                claims: &self.claims,
                comments: &self.comments,
                agents: &self.agents,
                campsites: &self.campsites,
                runs: &self.runs,
            })
        }
    }

    fn world() -> World {
        World {
            tasks: vec![
                task("t42", "TRAIL-42", "in_progress"),
                task("t43", "TRAIL-43", "needs_input"),
                task("t44", "TRAIL-44", "open"),
                task("t45", "TRAIL-45", "done"),
                task("t9", "TRAIL-9", "in_review"),
            ],
            claims: vec![
                fixture::claim(json!({"task_id": "t42", "holder_id": "a1"})),
                fixture::claim(json!({"task_id": "t43", "holder_id": "a2"})),
                fixture::claim(json!({"task_id": "t44", "holder_id": "u1", "on_behalf_of": "u1"})),
            ],
            comments: vec![
                fixture::comment(json!({"id": "c1", "task_id": "t43", "kind": "question", "created_at": at(1)})),
                fixture::comment(
                    json!({"id": "c2", "task_id": "t43", "kind": "question", "resolved_at": at(2), "created_at": at(1)}),
                ),
                fixture::comment(
                    json!({"id": "c3", "task_id": "t42", "kind": "handoff", "body": "old", "created_at": at(1)}),
                ),
                fixture::comment(
                    json!({"id": "c4", "task_id": "t42", "kind": "handoff", "body": "new\nmore", "created_at": at(2)}),
                ),
            ],
            agents: vec![
                fixture::agent(json!({"id": "a1", "name": "factory/trail-42-a1"})),
                fixture::agent(json!({"id": "a2", "name": "factory/trail-43-a1"})),
            ],
            campsites: vec![
                camp("b", SandboxState::Idle, None),
                camp("a", SandboxState::Busy, Some("TRAIL-42")),
                camp("c", SandboxState::Paused, Some("TRAIL-43")),
            ],
            runs: vec![run("TRAIL-42", Some("a"), RunPhase::Agent, 1.5)],
        }
    }

    #[test]
    fn campsites_include_idle_ones_in_a_stable_grid() {
        let model = world().model();
        let names: Vec<_> = model.camps.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);
        assert_eq!(model.camps[0].pos, ORIGIN);
        assert_eq!(model.camps[1].pos, ORIGIN + Vec2::new(CAMP_CELL.x, 0.0));
        assert_eq!(model.camps[1].state, SandboxState::Idle);
        assert_eq!(model.camps[0].phase.as_deref(), Some("agent"));
        // Nothing overlaps the waypoints.
        assert!(model.camps.iter().all(|c| c.pos.x + CAMP_SIZE.x < way_origin().x));
    }

    #[test]
    fn a_holder_that_is_no_agent_and_has_no_run_is_labelled_a_person() {
        let mut w = world();
        w.tasks[2] = task("t44", "TRAIL-44", "in_progress");
        let model = w.model();
        let markers = &model.waypoints.iter().find(|w| w.key == "TRAIL-44").unwrap().markers;
        let id = sid("u1");
        assert_eq!(markers[0], Marker::Claim(format!("a person ({})", &id[..8])));
    }

    #[test]
    fn an_unknown_holder_acting_for_someone_else_is_not_called_a_person() {
        let mut w = world();
        w.tasks[2] = task("t44", "TRAIL-44", "in_progress");
        w.claims[2] = fixture::claim(json!({"task_id": "t44", "holder_id": "u1", "on_behalf_of": "u2"}));
        let model = w.model();
        let markers = &model.waypoints.iter().find(|w| w.key == "TRAIL-44").unwrap().markers;
        assert_eq!(markers[0], Marker::Claim(format!("an agent ({})", &sid("u1")[..8])));
    }

    #[test]
    fn only_tasks_in_play_get_waypoints() {
        let keys: Vec<_> = world().model().waypoints.iter().map(|w| w.key.clone()).collect();
        // Open and done tasks don't, unless a run has them.
        assert_eq!(keys, ["TRAIL-9", "TRAIL-42", "TRAIL-43"]);
        let mut w = world();
        w.runs.push(run("TRAIL-44", None, RunPhase::Preparing { cold: true }, 0.0));
        assert!(w.model().waypoints.iter().any(|w| w.key == "TRAIL-44"));
    }

    #[test]
    fn markers_show_claims_questions_handoffs_verdicts_and_spend() {
        let model = world().model();
        let by = |key: &str| model.waypoints.iter().find(|w| w.key == key).unwrap().markers.clone();
        assert_eq!(
            by("TRAIL-42"),
            [Marker::Claim("factory/trail-42-a1".into()), Marker::Handoff("new".into()), Marker::Spend(1.5)]
        );
        // The resolved question doesn't count.
        assert_eq!(by("TRAIL-43"), [Marker::Claim("factory/trail-43-a1".into()), Marker::Questions(1)]);
        assert_eq!(by("TRAIL-9"), [Marker::InReview]);
        let mut w = world();
        w.runs[0] = run("TRAIL-42", Some("a"), RunPhase::Reviewing, 2.0);
        w.tasks[0] = task("t42", "TRAIL-42", "approved");
        let model = w.model();
        let markers = &model.waypoints.iter().find(|w| w.key == "TRAIL-42").unwrap().markers;
        assert!(markers.contains(&Marker::Reviewing) && markers.contains(&Marker::Approved));
    }

    #[test]
    fn agents_stand_at_their_task_until_the_run_has_a_camp_then_walk_there_and_back() {
        let mut w = world();
        let at = |w: &World, name: &str| w.model().walkers.into_iter().find(|k| k.name == name).unwrap();
        // The run is in a sandbox: the agent is at the camp.
        let walker = at(&w, "factory/trail-42-a1");
        assert_eq!(walker.at, Spot::Camp("o/r".into(), "a".into()));
        assert_eq!(walker.pos, ORIGIN + STAND);
        // The parked task's agent, with no run, stays at its waypoint; humans don't walk.
        assert_eq!(at(&w, "factory/trail-43-a1").at, Spot::Waypoint("TRAIL-43".into()));
        assert_eq!(w.model().walkers.len(), 2);
        // The run ends: the agent is back at the waypoint.
        w.runs.clear();
        let walker = at(&w, "factory/trail-42-a1");
        assert_eq!(walker.at, Spot::Waypoint("TRAIL-42".into()));
        let wp = w.model().waypoints.into_iter().find(|p| p.key == "TRAIL-42").unwrap();
        assert_eq!(walker.pos, wp.pos + STAND);
    }

    #[test]
    fn a_run_in_a_sandbox_the_map_lacks_leaves_the_agent_at_its_task() {
        let mut w = world();
        w.runs[0] = run("TRAIL-42", Some("gone"), RunPhase::Agent, 1.0);
        let walker = w.model().walkers.into_iter().find(|k| k.name == "factory/trail-42-a1").unwrap();
        assert_eq!(walker.at, Spot::Waypoint("TRAIL-42".into()));
    }

    #[test]
    fn an_agent_made_after_the_snapshot_still_walks_to_its_camp() {
        use crate::factory::{FactoryItem, FactoryPlugin, FactoryUpdate};
        use crate::mirror::MirrorPlugin;
        use crate::sync::{Snapshot, SyncItem};
        use trailhead_daemon::ui::RepoSnapshot;
        use trailhead_factory::{ControlState, FactorySnapshot};

        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins((MirrorPlugin, FactoryPlugin, MapPlugin));
        let w = world();
        // No claims and no agents at the snapshot: the run's sub-agent doesn't exist yet.
        let tasks: Vec<Value> = w
            .tasks
            .iter()
            .map(|t| {
                let mut t = serde_json::to_value(t).unwrap();
                t["claim"] = Value::Null;
                t["open_questions"] = json!([]);
                t
            })
            .collect();
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot { at: 1, tasks, ..Default::default() }));
        let snapshot = FactorySnapshot {
            control: ControlState::Up,
            last_error: None,
            sandboxes: w.campsites.iter().map(|c| c.status.clone()).collect(),
            runs: w.runs.iter().map(|r| r.status.clone()).collect(),
            seq: 1,
            holding: None,
            targets: Vec::new(),
            human_grades: Vec::new(),
            queue: Vec::new(),
        };
        app.world_mut()
            .write_message(FactoryItem(FactoryUpdate::Snapshot(vec![RepoSnapshot { repo: "o/r".into(), snapshot }])));
        for _ in 0..3 {
            app.update();
        }
        assert!(app.world().resource::<TrailMap>().0.walkers.is_empty());
        // The claim arrives as an event; its holder is in no agent list.
        app.world_mut().write_message(SyncItem::Event(json!({
            "id": 2, "entity": "claim", "entity_id": sid("t42"), "kind": "claim.created",
            "after": claim_json(json!({"task_id": "t42", "holder_id": "a-new"})),
        })));
        for _ in 0..3 {
            app.update();
        }
        let map = &app.world().resource::<TrailMap>().0;
        let walker = map.walkers.iter().find(|k| k.id == sid("a-new")).expect("a walker for the new holder");
        assert_eq!(walker.name, "factory/trail-42");
        assert_eq!(walker.at, Spot::Camp("o/r".into(), "a".into()));
        let wp = map.waypoints.iter().find(|p| p.key == "TRAIL-42").unwrap();
        assert!(wp.markers.contains(&Marker::Claim("factory/trail-42".into())));
    }

    #[test]
    fn tokens_are_drawn_above_the_terrain_after_a_redraw() {
        use crate::factory::FactoryStatus;

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_resource::<FactoryStatus>()
            .init_asset::<bevy::text::Font>()
            .add_systems(Update, redraw);
        app.world_mut().spawn(MapPanel);
        let w = world();
        let walkers = w.model().walkers;
        assert!(!walkers.is_empty());
        app.insert_resource(TrailMap(w.model()));
        for _ in 0..3 {
            app.update();
        }
        // A later change redraws the terrain after the tokens; they must still win.
        app.world_mut().resource_mut::<TrailMap>().set_changed();
        app.update();
        let mut q = app.world_mut().query::<(&Token, &ZIndex)>();
        let zs: Vec<i32> = q.iter(app.world()).map(|(_, z)| z.0).collect();
        assert_eq!(zs.len(), walkers.len());
        assert!(zs.iter().all(|z| *z > 0));
    }

    #[test]
    fn the_map_follows_the_mirrors() {
        use crate::factory::{FactoryItem, FactoryPlugin, FactoryUpdate};
        use crate::mirror::MirrorPlugin;
        use crate::sync::{Snapshot, SyncItem};
        use trailhead_daemon::ui::RepoSnapshot;
        use trailhead_factory::{ControlState, FactorySnapshot};

        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins((MirrorPlugin, FactoryPlugin, MapPlugin));
        let w = world();
        let mut tasks: Vec<Value> = w.tasks.iter().map(|t| serde_json::to_value(t).unwrap()).collect();
        tasks[0]["claim"] = claim_json(json!({"task_id": "t42", "holder_id": "a1"}));
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot {
            at: 1,
            tasks,
            agents: w.agents.iter().map(|a| serde_json::to_value(a).unwrap()).collect(),
            ..Default::default()
        }));
        let snapshot = FactorySnapshot {
            control: ControlState::Up,
            last_error: None,
            sandboxes: w.campsites.iter().map(|c| c.status.clone()).collect(),
            runs: w.runs.iter().map(|r| r.status.clone()).collect(),
            seq: 1,
            holding: None,
            targets: Vec::new(),
            human_grades: Vec::new(),
            queue: Vec::new(),
        };
        app.world_mut()
            .write_message(FactoryItem(FactoryUpdate::Snapshot(vec![RepoSnapshot { repo: "o/r".into(), snapshot }])));
        for _ in 0..3 {
            app.update();
        }
        let map = &app.world().resource::<TrailMap>().0;
        assert_eq!(map.camps.len(), 3);
        assert_eq!(map.waypoints.len(), 3);
        let walker = map.walkers.iter().find(|k| k.name == "factory/trail-42-a1").unwrap();
        assert_eq!(walker.at, Spot::Camp("o/r".into(), "a".into()));
    }
}
