//! The inspector (`docs/factory/ui.md#views`): a panel for the selected task, run or agent, from which to steer it:
//! release a claim, cancel, move the status, answer a question, undo the last change, revert what an agent did since a
//! time. Each is a command ([`crate::commands`]) whose refusal is shown beside the control that sent it.
//!
//! There is no approve or done button. A factory task is approved by the merger once its gates pass and finished when
//! its PR merges (`docs/factory/lifecycle.md`), so the panel links the PR and shows its merge state and gates from the
//! task's `factory` metadata and leaves the merge to GitHub. Status moves on a factory task leave out `approved` and
//! `done` for the same reason.
//!
//! [`build`] is a pure function from the mirror's JSON to an [`InspectorView`], and [`plan`] turns an [`Action`] into
//! the [`Request`] it sends, so both are tested without a window. "answer" opens the answer form from
//! [`crate::author`], as the board's does; "resolve" closes a question without one.

use std::collections::HashMap;

use bevy::feathers::controls::ButtonVariant;
use bevy::feathers::tokens;
use bevy::input::keyboard::KeyboardInput;
use bevy::input_focus::FocusedInput;
use bevy::prelude::*;
use bevy::text::EditableText;
use bevy::ui::InteractionDisabled;
use bevy::ui_widgets::{Activate, ControlOrientation, RequestClose};
use reqwest::Method;
use serde_json::{Value, json};
use trailhead_factory::{GateKind, METADATA_KEY, MergeDecision, MergedBy, RunUpdate};
use trailhead_model::{Agent, Claim, Comment, CommentKind, Grade, Priority, Project, Status, Task};
use uuid::Uuid;

use crate::author::{Act, grade_choices, grade_index, grade_options};
use crate::board::Input;
use crate::commands::{CommandDone, Outcome, Request, Submit};
use crate::connection::{Connection, ConnectionStatus};
use crate::factory::FactoryStatus;
use crate::mirror::{Mirror, Mirrored, Rev, rows};
use crate::sync::SyncItem;
use crate::widgets::{
    Icon, Keyed, ModalRoot, PanelTitle, Picked, SelectEdit, badge, button, danger_button, extent, layer, panel,
    panel_header, priority_tone, row, scroll_body, scroll_frame, scrollbar, section_frame, select, size, space,
    status_label, text, text_dim, text_toned, token, tool_button,
};
use bevy::feathers::theme::{ThemeTextColor, ThemeToken};

/// What the inspector is looking at: a task (and its run, if it has one), or an agent. Ids, not keys.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Subject {
    Task(String),
    Agent(String),
}

#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct Selection(pub Option<Subject>);

/// How far back "revert" goes, in hours; one of [`WINDOWS`], picked from a dropdown.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevertWindow(pub i64);

impl Default for RevertWindow {
    fn default() -> Self {
        RevertWindow(1)
    }
}

const WINDOWS: [i64; 4] = [1, 6, 24, 72];

/// One thing the inspector can do.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Action {
    Release,
    Cancel,
    /// Move the task to this status.
    Status(Status),
    /// Resolve this question (a comment id) without an answer. Answering opens the form instead.
    Resolve(String),
    /// Undo the event that last changed the task.
    Undo,
    /// Revert what the holder (an agent id) did in the last `n` hours.
    Revert(i64),
    /// Set the task's grade; none clears it.
    Grade(Option<Grade>),
}

impl Action {
    pub fn label(&self) -> String {
        match self {
            Action::Release => "release claim".into(),
            Action::Cancel => "cancel task".into(),
            Action::Status(s) => format!("move to {}", status_label(*s)),
            Action::Resolve(_) => "resolve".into(),
            Action::Undo => "undo last change".into(),
            Action::Revert(h) => format!("revert last {h}h"),
            Action::Grade(Some(g)) => format!("grade {g}"),
            Action::Grade(None) => "clear grade".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    /// The task's key, which the answer form needs.
    pub key: String,
    pub id: String,
    pub body: String,
}

/// A task's PR as the `factory` metadata describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrPanel {
    pub url: String,
    pub number: u64,
    /// `open`, or `merged by a person` / `merged by the merger` with the commit.
    pub state: String,
    pub grade: Option<Grade>,
    pub passed: Vec<String>,
    pub waiting: Vec<String>,
    pub failing: Vec<String>,
    pub would_merge: bool,
    /// Why the merger couldn't act or the host refused to merge.
    pub problem: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectorView {
    pub subject: Subject,
    pub title: String,
    /// Plain lines under the title: status, holder, run.
    pub lines: Vec<String>,
    pub questions: Vec<Question>,
    pub pr: Option<PrPanel>,
    /// Shown instead of a merge control: who finishes the task.
    pub merge_note: Option<String>,
    /// The task's own status; none for an agent.
    pub status: Option<Status>,
    /// The task's priority; none for an agent.
    pub priority: Option<Priority>,
    /// The task's risk grade; none for an agent, or a task with none.
    pub grade: Option<Grade>,
    pub actions: Vec<Action>,
}

/// The PR panel from the task's run; none when it has no PR yet.
pub fn pr_panel(run: &RunUpdate) -> Option<PrPanel> {
    let merged = run.merged.as_ref();
    let pr = merged.map(|m| &m.pr).or(run.pr.as_ref())?;
    let state = match merged {
        Some(m) => {
            let by = match m.by {
                MergedBy::Merger => "the merger",
                MergedBy::Human => "a person",
            };
            let commit =
                m.commit.as_ref().map_or(String::new(), |c| format!(" as {}", c.chars().take(8).collect::<String>()));
            format!("merged by {by}{commit}")
        }
        None => "open".to_string(),
    };
    let decision = run.merge.as_ref();
    let gates = |of: fn(&MergeDecision) -> &Vec<GateKind>| {
        decision.map(|d| of(d).iter().map(|g| g.as_str().to_string()).collect()).unwrap_or_default()
    };
    Some(PrPanel {
        url: pr.url.clone(),
        number: pr.number,
        state,
        grade: run.grade.as_ref().map(|g| g.grade).or(decision.map(|d| d.grade)),
        passed: gates(|d| &d.passed),
        waiting: gates(|d| &d.waiting),
        failing: decision.map(|d| d.failing.clone()).unwrap_or_default(),
        would_merge: decision.is_some_and(|d| d.would_merge),
        problem: decision.and_then(|d| d.problem.clone().or_else(|| d.refused.clone())),
    })
}

/// The statuses a task can be moved to by hand from the panel: any the task can go to next, but not cancelled (its
/// own action) and, on the factory's tasks, not approved or done (the factory finishes those).
fn status_moves(current: Status, factory: bool) -> Vec<Action> {
    Status::ALL
        .into_iter()
        .filter(|to| *to != current && *to != Status::Cancelled && current.can_transition_to(*to))
        .filter(|to| !factory || !matches!(to, Status::Approved | Status::Done))
        .map(Action::Status)
        .collect()
}

/// The panel for `subject`, or none when it isn't in the mirror (deleted, or not synced yet). `latest` maps a task
/// id to the id of the event that last changed it.
pub fn build(input: Input, latest: &HashMap<String, i64>, subject: &Subject) -> Option<InspectorView> {
    let agent_name = |id: Uuid| input.agents.iter().find(|a| a.id == id).map(|a| a.name.clone());
    let task_key = |id: Uuid| input.tasks.iter().find(|t| t.id == id).map(|t| t.key.clone());
    match subject {
        Subject::Agent(id) => {
            let id = Uuid::parse_str(id).ok()?;
            let name = agent_name(id)?;
            let held: Vec<String> =
                input.claims.iter().filter(|c| c.holder_id == id).filter_map(|c| task_key(c.task_id)).collect();
            let lines =
                vec![if held.is_empty() { "holds nothing".into() } else { format!("holds {}", held.join(", ")) }];
            Some(InspectorView {
                subject: subject.clone(),
                title: name,
                lines,
                questions: Vec::new(),
                pr: None,
                merge_note: None,
                status: None,
                priority: None,
                grade: None,
                actions: vec![Action::Revert(1)],
            })
        }
        Subject::Task(id) => {
            let task = input.tasks.iter().find(|t| t.id.to_string() == *id)?;
            let status = task.status;
            let is_factory = task.metadata.get(METADATA_KEY).is_some_and(Value::is_object);
            let run = RunUpdate::of(task).ok();
            let holder = input.claims.iter().find(|c| c.task_id == task.id).map(|c| c.holder_id);
            let holder_agent = holder.and_then(agent_name);

            let mut lines = vec![format!("status: {}", status_label(status))];
            match (&holder_agent, holder) {
                (Some(name), _) => lines.push(format!("held by {name}")),
                (None, Some(h)) => lines.push(format!("held by {}", h.to_string().chars().take(8).collect::<String>())),
                (None, None) => {}
            }
            if let Some(run) = &run
                && let Some(sandbox) = &run.sandbox
            {
                let attempt = run.attempt.map_or(String::new(), |a| format!(", attempt {a}"));
                lines.push(format!("run: {sandbox}{attempt}"));
            }

            let mut questions: Vec<&Comment> = input
                .comments
                .iter()
                .filter(|c| c.task_id == task.id && c.kind == CommentKind::Question && c.resolved_at.is_none())
                .collect();
            questions.sort_by_key(|c| c.created_at);
            let questions: Vec<Question> = questions
                .into_iter()
                .map(|q| Question { key: task.key.clone(), id: q.id.to_string(), body: q.body.clone() })
                .collect();

            let pr = run.as_ref().and_then(pr_panel);
            let merge_note = (is_factory && !status.is_closed()).then(|| match &pr {
                Some(p) if p.state == "open" => {
                    "Merge on GitHub (or let the merger): the factory approves it and closes the task once the PR merges.".to_string()
                }
                Some(_) => "The PR is merged; the factory is closing the task.".to_string(),
                None => "No PR yet; the factory finishes the task when its PR merges.".to_string(),
            });

            let mut actions = Vec::new();
            if holder.is_some() {
                actions.push(Action::Release);
            }
            if status != Status::Cancelled && status.can_transition_to(Status::Cancelled) {
                actions.push(Action::Cancel);
            }
            actions.extend(status_moves(status, is_factory));
            actions.extend(questions.iter().map(|q| Action::Resolve(q.id.clone())));
            if latest.contains_key(id) {
                actions.push(Action::Undo);
            }
            if holder_agent.is_some() {
                actions.push(Action::Revert(1));
            }
            Some(InspectorView {
                subject: subject.clone(),
                title: format!("{} {}", task.key, task.title),
                lines,
                questions,
                pr,
                merge_note,
                status: Some(status),
                priority: Some(task.priority),
                grade: task.grade,
                actions,
            })
        }
    }
}

/// The request an action sends for the subject, or none when what it needs isn't there. `now` is unix seconds.
pub fn plan(
    action: &Action,
    input: Input,
    latest: &HashMap<String, i64>,
    subject: &Subject,
    now: i64,
) -> Option<Request> {
    let task = match subject {
        Subject::Task(id) => input.tasks.iter().find(|t| t.id.to_string() == *id),
        Subject::Agent(_) => None,
    };
    let key = task.map(|t| t.key.as_str());
    let version = task.map(|t| t.version);
    let path = |tail: &str| format!("/v1/tasks/{}{tail}", key.unwrap_or_default());
    Some(match action {
        Action::Release => Request::new(Method::POST, path("/release"), None, None),
        Action::Cancel => Request::new(Method::PATCH, path(""), Some(json!({"status": Status::Cancelled})), version),
        Action::Status(st) => Request::new(Method::PATCH, path(""), Some(json!({"status": st})), version),
        Action::Grade(g) => Request::new(Method::PATCH, path(""), Some(json!({"grade": g})), version),
        Action::Resolve(comment) => Request::new(Method::POST, format!("/v1/comments/{comment}/resolve"), None, None),
        Action::Undo => {
            let Subject::Task(id) = subject else { return None };
            Request::new(Method::POST, "/v1/undo", Some(json!({"event_id": latest.get(id)?})), None)
        }
        Action::Revert(hours) => {
            let actor = match subject {
                Subject::Agent(id) => id.clone(),
                Subject::Task(id) => {
                    input.claims.iter().find(|c| c.task_id.to_string() == *id).map(|c| c.holder_id.to_string())?
                }
            };
            let body = json!({"actor_id": actor, "since": rfc3339(now - hours * 3600)});
            Request::new(Method::POST, "/v1/revert", Some(body), None)
        }
    })
}

/// A unix time as RFC 3339 in UTC.
pub fn rfc3339(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// What to say beside a control for the server's answer; none for success.
pub fn describe(outcome: &Outcome) -> Option<String> {
    match outcome {
        Outcome::Ok { .. } => None,
        Outcome::Conflict { message, .. } => Some(format!("changed since you looked: {message}")),
        Outcome::Forbidden(m) => Some(format!("not allowed: {m}")),
        Outcome::RateLimited { message, retry_after } => Some(match retry_after {
            Some(after) => format!("rate limited, try again in {}s: {message}", after.as_secs()),
            None => format!("rate limited: {message}"),
        }),
        Outcome::Failed(m) => Some(m.clone()),
    }
}

/// Where a control's command stands. Lives on the control's own entity, which outlives redraws.
#[derive(Component, Debug, Clone, PartialEq, Eq, Default)]
pub enum Control {
    #[default]
    Idle,
    Busy,
    Done,
    Refused(String),
}

/// One persistent entity per subject and action, so the answer finds its control however often the panel is redrawn,
/// and one subject's result never shows on another's.
#[derive(Resource, Default)]
struct Controls(HashMap<(Subject, Action), Entity>);

/// The id of the event that last changed each task, for undo.
#[derive(Resource, Default, Debug)]
pub struct Latest(pub HashMap<String, i64>);

#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct InspectorState(pub Option<InspectorView>);

fn track_events(mut items: MessageReader<SyncItem>, mut latest: ResMut<Latest>) {
    for item in items.read() {
        let SyncItem::Event(e) = item else { continue };
        let Some(id) = e["id"].as_i64() else { continue };
        let task = match e["entity"].as_str() {
            Some("task") => e["entity_id"].as_str(),
            Some("comment" | "claim") => e["after"]["task_id"].as_str().or_else(|| e["before"]["task_id"].as_str()),
            _ => None,
        };
        if let Some(task) = task {
            latest.0.insert(task.to_string(), id);
        }
    }
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn rebuild(
    selection: Res<Selection>,
    latest: Res<Latest>,
    mirror: Res<Mirror>,
    changed: Query<(), Changed<Rev>>,
    mut removed: RemovedComponents<Rev>,
    projects: Query<&Mirrored<Project>>,
    tasks: Query<&Mirrored<Task>>,
    claims: Query<&Mirrored<Claim>>,
    comments: Query<&Mirrored<Comment>>,
    agents: Query<&Mirrored<Agent>>,
    mut state: ResMut<InspectorState>,
) {
    let gone = removed.read().count() > 0;
    if !(selection.is_changed() || latest.is_changed() || mirror.is_changed() || gone || !changed.is_empty()) {
        return;
    }
    let next = selection.0.as_ref().and_then(|subject| {
        let (projects, tasks, claims, comments, agents) =
            (rows(&projects), rows(&tasks), rows(&claims), rows(&comments), rows(&agents));
        let input = Input {
            projects: &projects,
            tasks: &tasks,
            claims: &claims,
            comments: &comments,
            agents: &agents,
            ..Input::default()
        };
        build(input, &latest.0, subject)
    });
    if state.0 != next {
        state.0 = next;
    }
}

fn settle(mut done: MessageReader<CommandDone>, mut controls: Query<&mut Control>) {
    for d in done.read() {
        if let Ok(mut c) = controls.get_mut(d.source) {
            *c = describe(&d.outcome).map_or(Control::Done, Control::Refused);
        }
    }
}

/// Keeps the [`Selection`] and the panel's contents from the mirror.
pub struct InspectorPlugin;

impl Plugin for InspectorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Selection>()
            .init_resource::<RevertWindow>()
            .init_resource::<Latest>()
            .init_resource::<InspectorState>()
            // Registering a message twice is a no-op, so these are safe beside the mirror's and commands' own.
            .add_message::<SyncItem>()
            .add_message::<CommandDone>()
            .add_observer(on_pick)
            .add_observer(on_close_request)
            .add_observer(on_escape)
            .add_systems(Update, (track_events, rebuild, settle).chain());
    }
}

/// Draws the inspector over whichever view is showing: it is not tied to a [`crate::board::View`].
pub struct InspectorUiPlugin;

impl Plugin for InspectorUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(InspectorPlugin)
            .init_resource::<Controls>()
            .init_resource::<FactoryStatus>()
            .add_systems(Startup, spawn_panel)
            .add_systems(Update, (sync_panel, dress).chain());
    }
}

#[derive(Component)]
struct InspectorPanel;

/// The panel's fixed header: the title and the close button, above the scrolling body.
#[derive(Component)]
struct InspectorHeader;

/// The panel's scrolling body, which lives as long as the panel so its scroll position survives redraws.
#[derive(Component)]
struct InspectorBody;

/// The dropdown that moves a task to another status: `statuses[i]` is what its option `i` means, the task's own
/// status first. Its answer shows beside it on `source`.
#[derive(Component, Debug, Clone)]
struct StatusMove {
    source: Entity,
    subject: Subject,
    statuses: Vec<Status>,
}

/// The dropdown that sets the task's grade: option `i` is `GRADES[i]`. Its answer shows beside it on `source`.
#[derive(Component, Debug, Clone)]
struct GradeChoice {
    source: Entity,
    subject: Subject,
}

/// The dropdown that sets how far back "revert" goes.
#[derive(Component, Debug, Clone, Copy, Default)]
struct WindowChoice;

fn spawn_panel(mut commands: Commands) {
    let panel = commands
        .spawn_scene(bsn! {
            @panel()
            Node {
                position_type: PositionType::Absolute,
                right: px(0),
                top: px(0),
                width: { px(extent::INSPECTOR_WIDTH) },
                height: percent(100),
                display: Display::None,
            }
            // Above the board, which fills the window and is spawned later, so it would take every click here; below
            // the form that "answer" opens.
            GlobalZIndex({ layer::INSPECTOR })
        })
        .insert((Name::new("inspector"), InspectorPanel))
        .id();
    // The header stays put; the body under it scrolls, held to the window's height by the panel and its frame.
    commands
        .spawn(Node { flex_direction: FlexDirection::Column, flex_shrink: 0.0, ..default() })
        .insert((InspectorHeader, ChildOf(panel)));
    let frame = commands.spawn(scroll_frame()).insert(ChildOf(panel)).id();
    let body = commands.spawn((scroll_body(), InspectorBody)).insert(ChildOf(frame)).id();
    commands.queue_spawn_scene(scrollbar(body, ControlOrientation::Vertical)).insert(ChildOf(frame));
}

/// The X in the panel's header (or anything else inside it) asked to close it.
fn on_close_request(
    close: On<RequestClose>,
    panels: Query<(), With<InspectorPanel>>,
    mut selection: ResMut<Selection>,
) {
    if panels.contains(close.event_target()) {
        selection.0 = None;
    }
}

/// Escape closes the inspector, unless a modal is open (its own Escape closes it, and only it) or the press came from
/// a text input (whose Escape just leaves the field).
fn on_escape(
    key: On<FocusedInput<KeyboardInput>>,
    modals: Query<(), With<ModalRoot>>,
    inputs: Query<(), With<EditableText>>,
    panels: Query<Entity, With<InspectorPanel>>,
    mut commands: Commands,
) {
    // The press bubbles up from the focused entity; act once, where it starts.
    if key.event_target() != key.original_event_target() {
        return;
    }
    let pressed = &key.input;
    if pressed.key_code != KeyCode::Escape || !pressed.state.is_pressed() || pressed.repeat {
        return;
    }
    if !modals.is_empty() || inputs.contains(key.original_event_target()) {
        return;
    }
    for panel in &panels {
        commands.trigger(RequestClose { source: panel });
    }
}

/// What a dropdown of the panel picked: a status to move to, or how far back "revert" goes.
fn on_pick(
    picked: On<Picked>,
    moves: Query<&StatusMove>,
    grades: Query<&GradeChoice>,
    windows: Query<(), With<WindowChoice>>,
    mut window: ResMut<RevertWindow>,
    mut commands: Commands,
) {
    if windows.contains(picked.entity) {
        if let Some(hours) = WINDOWS.get(picked.index) {
            window.0 = *hours;
        }
    } else if let Ok(GradeChoice { source, subject }) = grades.get(picked.entity) {
        if let Some(grade) = grade_choices().nth(picked.index) {
            let (source, subject, action) = (*source, subject.clone(), Action::Grade(grade));
            commands.queue(move |world: &mut World| send(world, source, &subject, &action));
        }
    } else if let Ok(StatusMove { source, subject, statuses }) = moves.get(picked.entity)
        && picked.index > 0
        && let Some(status) = statuses.get(picked.index)
    {
        let (source, subject, action) = (*source, subject.clone(), Action::Status(*status));
        commands.queue(move |world: &mut World| send(world, source, &subject, &action));
    }
}

/// Builds the request when the button is pressed, from the mirror as it is then: the version for `If-Match`, the
/// event to undo and the `since` of a revert must not be those of the last redraw.
fn send(world: &mut World, source: Entity, subject: &Subject, action: &Action) {
    fn all<T: Clone + Send + Sync + 'static>(world: &mut World) -> Vec<T> {
        world.query::<&Mirrored<T>>().iter(world).map(|m| m.0.clone()).collect()
    }
    let (projects, tasks, claims, comments, agents) = (all(world), all(world), all(world), all(world), all(world));
    let input = Input {
        projects: &projects,
        tasks: &tasks,
        claims: &claims,
        comments: &comments,
        agents: &agents,
        ..Input::default()
    };
    // A revert's control is one however far back it goes; the window is read as the button is pressed.
    let action = &match action {
        Action::Revert(_) => Action::Revert(world.resource::<RevertWindow>().0),
        other => other.clone(),
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let request = plan(action, input, &world.resource::<Latest>().0, subject, now);
    let outcome = match request {
        Some(request) => {
            world.resource_mut::<Messages<Submit>>().write(Submit { source, request });
            Control::Busy
        }
        None => Control::Refused("no longer possible: the task changed".into()),
    };
    if let Some(mut c) = world.get_mut::<Control>(source) {
        *c = outcome;
    }
}

/// Sends `action` for `subject` when the button it is on is pressed.
fn sends(source: Entity, subject: Subject, action: Action) -> impl Scene {
    bsn! {
        on(move |_: On<Activate>, mut commands: Commands| {
            let (subject, action) = (subject.clone(), action.clone());
            commands.queue(move |world: &mut World| send(world, source, &subject, &action));
        })
    }
}

/// The control for `action` on `subject`, made the first time it is asked for.
fn control(controls: &mut Controls, commands: &mut Commands, subject: &Subject, action: &Action) -> Entity {
    *controls.0.entry((subject.clone(), action.clone())).or_insert_with(|| commands.spawn(Control::Idle).id())
}

/// The status dropdown's options, read as words; its [`StatusMove`] keeps the API values it sends.
fn status_labels(statuses: &[Status]) -> Vec<String> {
    statuses.iter().map(|s| status_label(*s).to_string()).collect()
}

/// The key of the one control every status move shares, whichever status it moves to.
const MOVE_CONTROL: Action = Action::Status(Status::Open);

/// Whether a line of [`InspectorView::lines`] is about the run rather than the task's status.
fn is_run_line(line: &str) -> bool {
    line.starts_with("run:")
}

/// How a [`Block::Text`] is colored; fixed for the item that holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    Body,
    Dim,
    Warn,
}

/// One thing in a section of the panel. Items are keyed (`status/line/0`, `q/<comment id>`, `act/Release`), so a
/// change in the task changes the entities of what changed and nothing else.
#[derive(Debug, Clone, PartialEq)]
enum Block {
    /// A line of text; only the words change while the key stays.
    Text { text: String, size: f32, tone: Tone },
    /// The `P1` badge. Its key holds the priority, so a new priority is a new badge.
    Priority(Priority),
    /// The wrapping row of a PR's gates, which are [`Gate`]s keyed under it.
    Gates,
    /// An open question, with its answer and resolve buttons and what became of the last resolve.
    Question { key: String, id: String, body: String, live: bool, note: Option<Control> },
    /// The grade dropdown (live only); the offline grade is a [`Block::Text`].
    Grade { grade: Option<Grade>, note: Option<Control> },
    /// The status dropdown: the task's own status first.
    Move { statuses: Vec<Status>, note: Option<Control> },
    /// A button for `action` (a revert has its window's dropdown, `window` hours).
    Act { action: Action, window: Option<i64>, note: Option<Control> },
}

/// One of a PR's gates: its key holds the state, so a gate that changes state is a new badge.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Gate {
    name: String,
    state: GateState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateState {
    Passed,
    Waiting,
    Failing,
}

/// A block on its entity, which [`dress`] brings in line with it when it changes.
#[derive(Component, Debug, Clone)]
struct Shown(Block);

/// The parts of a block's entity that are set in place.
#[derive(Component, Debug, Clone, Copy, Default)]
struct Parts {
    text: Option<Entity>,
    answer: Option<Entity>,
    resolve: Option<Entity>,
    select: Option<Entity>,
    note: Option<Entity>,
}

type Plan = Vec<(String, Vec<(String, Block)>)>;

fn text_block(text: impl Into<String>, size: f32, tone: Tone) -> Block {
    Block::Text { text: text.into(), size, tone }
}

/// The panel's sections and their items, in order. `note` is where a control's command stands, by action.
fn layout(
    view: &InspectorView,
    live: bool,
    offline: &str,
    window: i64,
    note: &dyn Fn(&Action) -> Option<Control>,
) -> Plan {
    let mut plan: Plan = Vec::new();
    let mut section = |key: &str, heading: &str, mut items: Vec<(String, Block)>| {
        if !heading.is_empty() {
            items.insert(0, (format!("{key}/heading"), text_block(heading, size::SECONDARY, Tone::Dim)));
        }
        plan.push((key.to_string(), items));
    };

    let (run, status): (Vec<&String>, Vec<&String>) = view.lines.iter().partition(|l| is_run_line(l));
    for (key, heading, lines) in [("status", "Status", status), ("run", "Run", run)] {
        if lines.is_empty() {
            continue;
        }
        let mut items = Vec::new();
        if let (true, Some(p)) = (key == "status", view.priority) {
            items.push((format!("{key}/priority/{p}"), Block::Priority(p)));
        }
        items.extend(
            lines
                .into_iter()
                .enumerate()
                .map(|(i, l)| (format!("{key}/line/{i}"), text_block(l, size::BODY, Tone::Body))),
        );
        if key == "status" && view.status.is_some() {
            if live {
                let note = note(&Action::Grade(None));
                items.push(("status/grade".to_string(), Block::Grade { grade: view.grade, note }));
            } else {
                let grade = view.grade.map_or_else(|| "none".to_string(), |g| g.to_string());
                items.push((
                    "status/grade/off".to_string(),
                    text_block(format!("grade: {grade}"), size::BODY, Tone::Body),
                ));
            }
        }
        section(key, heading, items);
    }

    if !view.questions.is_empty() {
        let items = view
            .questions
            .iter()
            .map(|q| {
                let block = Block::Question {
                    key: q.key.clone(),
                    id: q.id.clone(),
                    body: q.body.clone(),
                    live,
                    note: note(&Action::Resolve(q.id.clone())),
                };
                (format!("q/{}", q.id), block)
            })
            .collect();
        section("questions", "Questions", items);
    }

    if view.pr.is_some() || view.merge_note.is_some() {
        let mut items = Vec::new();
        if let Some(pr) = &view.pr {
            items
                .push(("pr/url".to_string(), text_block(format!("#{} {}", pr.number, pr.url), size::BODY, Tone::Body)));
            let grade = pr.grade.map_or(String::new(), |g| format!(", grade {g}"));
            items
                .push(("pr/state".to_string(), text_block(format!("{}{grade}", pr.state), size::SECONDARY, Tone::Dim)));
            if !gates(pr).is_empty() {
                items.push(("pr/gates".to_string(), Block::Gates));
            }
            if let Some(problem) = &pr.problem {
                items.push(("pr/problem".to_string(), text_block(problem, size::BODY, Tone::Warn)));
            }
        }
        if let Some(merge_note) = &view.merge_note {
            items.push(("pr/note".to_string(), text_block(merge_note, size::SECONDARY, Tone::Dim)));
        }
        section("pr", "Pull request", items);
    }

    if !live {
        let items = vec![("offline/text".to_string(), text_block(offline, size::BODY, Tone::Warn))];
        section("offline", "", items);
        return plan;
    }

    let mut items = Vec::new();
    // One dropdown for every status the task can move to, the current one showing.
    let moves: Vec<Status> =
        view.actions.iter().filter_map(|a| if let Action::Status(st) = a { Some(*st) } else { None }).collect();
    if let (Some(current), false) = (view.status, moves.is_empty()) {
        let statuses = std::iter::once(current).chain(moves).collect();
        items.push(("move".to_string(), Block::Move { statuses, note: note(&MOVE_CONTROL) }));
    }
    for action in &view.actions {
        let action = match action {
            Action::Status(_) | Action::Resolve(_) => continue,
            // One control however far back, so the answer finds it when the window changes.
            Action::Revert(_) => Action::Revert(0),
            other => other.clone(),
        };
        let window = matches!(action, Action::Revert(_)).then_some(window);
        let block = Block::Act { note: note(&action), window, action: action.clone() };
        items.push((format!("act/{action:?}"), block));
    }
    section("actions", "Actions", items);
    plan
}

/// A PR's gates, in the order the panel shows them.
fn gates(pr: &PrPanel) -> Vec<(String, Gate)> {
    [
        (&pr.passed, GateState::Passed, "passed"),
        (&pr.waiting, GateState::Waiting, "waiting"),
        (&pr.failing, GateState::Failing, "failing"),
    ]
    .into_iter()
    .flat_map(|(names, state, word)| {
        names.iter().map(move |name| (format!("{word}/{name}"), Gate { name: name.clone(), state }))
    })
    .collect()
}

fn window_labels() -> Vec<String> {
    WINDOWS.iter().map(|h| format!("{h}h ago")).collect()
}

/// The words beside a control for where its command stands, and their color; no words for none.
fn note_look(note: &Option<Control>) -> (String, ThemeToken) {
    match note {
        Some(Control::Busy) => ("sending…".into(), tokens::TEXT_DIM),
        // The command went through: not the task's status, so not `status_label`'s.
        Some(Control::Done) => (String::from("done"), tokens::TEXT_DIM),
        Some(Control::Refused(why)) => (why.clone(), token::DANGER),
        _ => (String::new(), tokens::TEXT_DIM),
    }
}

fn spawn_note(commands: &mut Commands, parent: Entity, note: &Option<Control>) -> Entity {
    let (words, tone) = note_look(note);
    let display = if words.is_empty() { Display::None } else { Display::Flex };
    commands
        .spawn_scene(bsn! { @text_toned(words, size::SECONDARY, tone) Node { display: { display } } })
        .insert(ChildOf(parent))
        .id()
}

/// Disables a button while the connection is not live.
fn gate_button(commands: &mut Commands, button: Entity, live: bool) {
    if live {
        commands.entity(button).remove::<InteractionDisabled>();
    } else {
        commands.entity(button).insert(InteractionDisabled);
    }
}

/// Makes the entity for a new block, not yet parented.
fn spawn_block(
    commands: &mut Commands,
    controls: &mut Controls,
    subject: &Subject,
    block: &Block,
    human: &[Grade],
) -> Entity {
    let item = match block {
        Block::Text { text: words, size, tone } => {
            let scene: Box<dyn Scene> = match tone {
                Tone::Body => Box::new(text(words.clone(), *size)),
                Tone::Dim => Box::new(text_dim(words.clone(), *size)),
                Tone::Warn => Box::new(text_toned(words.clone(), *size, token::WARN)),
            };
            commands.spawn_scene(scene).id()
        }
        Block::Priority(p) => commands.spawn_scene(badge(format!("P{p}"), None, Some(priority_tone(*p)))).id(),
        Block::Gates => commands.spawn_scene(bsn! { @row() Node { flex_wrap: FlexWrap::Wrap } }).id(),
        Block::Question { key, id, body, live, note } => {
            let item = commands
                .spawn(Node { flex_direction: FlexDirection::Column, row_gap: px(space::ROWS), ..default() })
                .id();
            let line = commands.spawn_scene(bsn! { @row() }).insert(ChildOf(item)).id();
            let words = commands
                .spawn_scene(
                    bsn! { @text_toned(format!("? {body}"), size::BODY, token::WARN) Node { flex_grow: 1.0, flex_shrink: 1.0, min_width: px(0) } },
                )
                .insert(ChildOf(line))
                .id();
            // The same form the board opens; it saves even after the inspector moves on.
            let act = Act::Answer { key: key.clone(), comment_id: id.clone(), question: body.clone() };
            let answer = commands
                .spawn_scene(bsn! { @tool_button(Icon::Comment, "Answer", ButtonVariant::Normal) })
                .insert((act, ChildOf(line)))
                .id();
            let resolve_action = Action::Resolve(id.clone());
            let source = control(controls, commands, subject, &resolve_action);
            let resolve = commands
                .spawn_scene(bsn! {
                    @tool_button(Icon::Check, "Resolve without an answer", ButtonVariant::Normal)
                    @sends(source, subject.clone(), resolve_action.clone())
                })
                .insert(ChildOf(line))
                .id();
            gate_button(commands, answer, *live);
            gate_button(commands, resolve, *live);
            let note = spawn_note(commands, item, note);
            commands.entity(item).insert(Parts {
                text: Some(words),
                answer: Some(answer),
                resolve: Some(resolve),
                note: Some(note),
                ..default()
            });
            item
        }
        Block::Grade { grade, note } => {
            let line = commands.spawn_scene(bsn! { @row() }).id();
            commands.spawn_scene(text_dim("Grade", size::SECONDARY)).insert(ChildOf(line));
            let source = control(controls, commands, subject, &Action::Grade(None));
            let options = grade_options(human);
            let max_visible = options.len();
            let picker = commands
                .spawn_scene(select(options, grade_index(*grade), max_visible))
                .insert((GradeChoice { source, subject: subject.clone() }, ChildOf(line)))
                .id();
            let note = spawn_note(commands, line, note);
            commands.entity(line).insert(Parts { select: Some(picker), note: Some(note), ..default() });
            line
        }
        Block::Move { statuses, note } => {
            let line = commands.spawn_scene(bsn! { @row() }).id();
            commands.spawn_scene(text_dim("Move to", size::SECONDARY)).insert(ChildOf(line));
            let source = control(controls, commands, subject, &MOVE_CONTROL);
            let max_visible = statuses.len();
            let picker = commands
                .spawn_scene(select(status_labels(statuses), Some(0), max_visible))
                .insert((StatusMove { source, subject: subject.clone(), statuses: statuses.clone() }, ChildOf(line)))
                .id();
            let note = spawn_note(commands, line, note);
            commands.entity(line).insert(Parts { select: Some(picker), note: Some(note), ..default() });
            line
        }
        Block::Act { action, window, note } => {
            let line = commands.spawn_scene(bsn! { @row() }).id();
            let source = control(controls, commands, subject, action);
            let press = sends(source, subject.clone(), action.clone());
            match action {
                Action::Release => commands
                    .spawn_scene(bsn! { @button(action.label(), Some(Icon::Release), ButtonVariant::Normal) @{press} }),
                Action::Undo => commands
                    .spawn_scene(bsn! { @button(action.label(), Some(Icon::Undo), ButtonVariant::Normal) @{press} }),
                Action::Cancel => {
                    commands.spawn_scene(bsn! { @danger_button(action.label(), Some(Icon::Stop)) @{press} })
                }
                _ => commands.spawn_scene(bsn! { @danger_button("revert since", Some(Icon::History)) @{press} }),
            }
            .insert(ChildOf(line));
            let mut parts = Parts::default();
            if let Some(hours) = window {
                let selected = WINDOWS.iter().position(|h| h == hours);
                parts.select = Some(
                    commands
                        .spawn_scene(select(window_labels(), selected, WINDOWS.len()))
                        .insert((WindowChoice, ChildOf(line)))
                        .id(),
                );
            }
            parts.note = Some(spawn_note(commands, line, note));
            commands.entity(line).insert(parts);
            line
        }
    };
    commands.entity(item).insert(Shown(block.clone()));
    item
}

/// Sets a note's words, color and whether it takes room.
fn dress_note(
    commands: &mut Commands,
    texts: &mut Query<&mut Text>,
    nodes: &mut Query<&mut Node>,
    note: Entity,
    state: &Option<Control>,
) {
    let (words, tone) = note_look(state);
    let display = if words.is_empty() { Display::None } else { Display::Flex };
    if let Ok(mut t) = texts.get_mut(note)
        && t.0 != words
    {
        t.0 = words;
    }
    if let Ok(mut node) = nodes.get_mut(note)
        && node.display != display
    {
        node.display = display;
    }
    commands.entity(note).insert(ThemeTextColor(tone));
}

/// Brings the parts of changed blocks in line with them: words, notes, disabled buttons, dropdown rows.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn dress(
    shown: Query<(&Shown, Option<&Parts>)>,
    every: Query<Entity, With<Shown>>,
    factory: Res<FactoryStatus>,
    changed: Query<Entity, Changed<Shown>>,
    mut texts: Query<&mut Text>,
    mut nodes: Query<&mut Node>,
    mut moves: Query<&mut StatusMove>,
    mut edit: SelectEdit,
    mut again: Local<Vec<Entity>>,
    mut commands: Commands,
) {
    let mut todo: Vec<Entity> = std::mem::take(&mut *again);
    todo.extend(changed.iter());
    if factory.is_changed() {
        todo.extend(every.iter());
    }
    todo.dedup();
    for entity in todo {
        let Ok((Shown(block), parts)) = shown.get(entity) else { continue };
        if let Block::Text { text: words, .. } = block {
            if let Ok(mut t) = texts.get_mut(entity)
                && t.0 != *words
            {
                t.0 = words.clone();
            }
            continue;
        }
        let Some(parts) = parts else { continue };
        match block {
            Block::Question { key, id, body, live, note } => {
                let words = format!("? {body}");
                if let Some(Ok(mut t)) = parts.text.map(|e| texts.get_mut(e))
                    && t.0 != words
                {
                    t.0 = words;
                }
                for button in [parts.answer, parts.resolve].into_iter().flatten() {
                    gate_button(&mut commands, button, *live);
                }
                if let Some(answer) = parts.answer {
                    let act = Act::Answer { key: key.clone(), comment_id: id.clone(), question: body.clone() };
                    commands.entity(answer).insert(act);
                }
                if let Some(n) = parts.note {
                    dress_note(&mut commands, &mut texts, &mut nodes, n, note);
                }
            }
            Block::Grade { grade, note } => {
                if let Some(picker) = parts.select
                    && !edit.set(picker, &grade_options(&factory.human_grades()), grade_index(*grade))
                {
                    again.push(entity);
                }
                if let Some(n) = parts.note {
                    dress_note(&mut commands, &mut texts, &mut nodes, n, note);
                }
            }
            Block::Move { statuses, note } => {
                if let Some(picker) = parts.select {
                    if let Ok(mut m) = moves.get_mut(picker)
                        && m.statuses != *statuses
                    {
                        m.statuses = statuses.clone();
                    }
                    if !edit.set(picker, &status_labels(statuses), Some(0)) {
                        again.push(entity);
                    }
                }
                if let Some(n) = parts.note {
                    dress_note(&mut commands, &mut texts, &mut nodes, n, note);
                }
            }
            Block::Act { window, note, .. } => {
                if let (Some(picker), Some(hours)) = (parts.select, window)
                    && !edit.set(picker, &window_labels(), WINDOWS.iter().position(|h| h == hours))
                {
                    again.push(entity);
                }
                if let Some(n) = parts.note {
                    dress_note(&mut commands, &mut texts, &mut nodes, n, note);
                }
            }
            Block::Text { .. } | Block::Priority(_) | Block::Gates => {}
        }
    }
}

/// What the panel has on its entities: the sections of the body, the items in them and a PR's gates.
#[derive(Default)]
struct Drawn {
    subject: Option<Subject>,
    header: Option<Entity>,
    sections: Keyed<()>,
    items: Keyed<Block>,
    gates: Keyed<Gate>,
}

/// Keeps the panel's entities in step with [`InspectorState`]. Selecting a different subject builds the header and
/// sections anew, once; while the same one stays selected, only what changed is set: text through its `Text`, a list's
/// new rows appended, the rest left as the same entities (and the body's scroll position where it was).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn sync_panel(
    state: Res<InspectorState>,
    connection: Res<State<Connection>>,
    factory: Res<FactoryStatus>,
    status: Res<ConnectionStatus>,
    window: Res<RevertWindow>,
    mut controls: ResMut<Controls>,
    states: Query<&Control>,
    changed_controls: Query<(), Changed<Control>>,
    mut panel_node: Single<&mut Node, With<InspectorPanel>>,
    header: Single<Entity, With<InspectorHeader>>,
    body: Single<(Entity, &mut ScrollPosition), With<InspectorBody>>,
    mut titles: Query<(Entity, &mut Text), With<PanelTitle>>,
    parents: Query<&ChildOf>,
    mut drawn: Local<Drawn>,
    mut commands: Commands,
) {
    if !(state.is_changed()
        || connection.is_changed()
        || status.is_changed()
        || window.is_changed()
        || !changed_controls.is_empty())
    {
        return;
    }
    let (body, mut scroll) = body.into_inner();
    let drawn = &mut *drawn;
    let subject = state.0.as_ref().map(|v| v.subject.clone());
    if subject != drawn.subject {
        // A different subject (or none) starts over, at the top.
        if let Some(old) = drawn.header.take() {
            commands.entity(old).try_despawn();
        }
        drawn.sections.clear(&mut commands);
        drawn.items.forget();
        drawn.gates.forget();
        scroll.0 = Vec2::ZERO;
        drawn.subject = subject;
    }
    let Some(view) = &state.0 else {
        panel_node.display = Display::None;
        return;
    };
    panel_node.display = Display::Flex;
    let root = *drawn.header.get_or_insert_with(|| {
        commands.spawn_scene(panel_header(view.title.clone(), true)).insert(ChildOf(*header)).id()
    });
    // Only the inspector's own title: every modal's header is a `panel_header` too, with a `PanelTitle` of its own.
    for (entity, mut title) in &mut titles {
        if title.0 != view.title && parents.iter_ancestors(entity).any(|a| a == root) {
            title.0 = view.title.clone();
        }
    }

    let live = *connection.get() == Connection::Live;
    let why = status.reason.as_ref().map_or_else(String::new, |r| format!(": {r}"));
    let offline = format!("{:?}{why}. Controls are off.", connection.get());
    let subject = &view.subject;
    let note =
        |action: &Action| controls.0.get(&(subject.clone(), action.clone())).and_then(|e| states.get(*e).ok()).cloned();
    let plan = layout(view, live, &offline, window.0, &note);

    let wanted: Vec<(String, ())> = plan.iter().map(|(key, _)| (key.clone(), ())).collect();
    drawn.sections.sync(
        &mut commands,
        &[(body, wanted)],
        |commands, _, _| commands.spawn_scene(section_frame()).id(),
        |_, _, _| {},
    );
    let wanted: Vec<(Entity, Vec<(String, Block)>)> =
        plan.into_iter().filter_map(|(key, items)| Some((drawn.sections.entity(&key)?, items))).collect();
    let controls = &mut *controls;
    let human = factory.human_grades();
    drawn.items.sync(
        &mut commands,
        &wanted,
        |commands, _, block| spawn_block(commands, controls, subject, block, &human),
        |commands, entity, block| {
            commands.entity(entity).insert(Shown(block.clone()));
        },
    );
    let rows = view.pr.as_ref().map(gates).unwrap_or_default();
    let wanted: Vec<(Entity, Vec<(String, Gate)>)> =
        drawn.items.entity("pr/gates").map(|row| (row, rows)).into_iter().collect();
    drawn.gates.sync(
        &mut commands,
        &wanted,
        |commands, _, gate| {
            let (icon, tone) = match gate.state {
                GateState::Passed => (Icon::CircleCheck, token::OK),
                GateState::Waiting => (Icon::Circle, tokens::TEXT_DIM),
                GateState::Failing => (Icon::CircleX, token::DANGER),
            };
            commands.spawn_scene(badge(gate.name.clone(), Some(icon), Some(tone))).id()
        },
        |_, _, _| {},
    );
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::fixture::{agent, at, claim, comment, comment_json, sid, task};
    use crate::sync::Snapshot;
    use crate::widgets::ScrollDown;

    struct Fixture {
        projects: Vec<Project>,
        tasks: Vec<Task>,
        claims: Vec<Claim>,
        comments: Vec<Comment>,
        agents: Vec<Agent>,
    }

    fn as_json<T: serde::Serialize>(items: &[T]) -> Vec<Value> {
        items.iter().map(|i| serde_json::to_value(i).unwrap()).collect()
    }

    impl Fixture {
        fn input(&self) -> Input<'_> {
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

    fn fixture() -> Fixture {
        let factory = json!({
            "sandbox": "trail-7-a1", "attempt": 2,
            "pr": {"repo": {"owner": "o", "name": "r"}, "number": 12, "url": "https://github.com/o/r/pull/12"},
            "grade": {"grade": 2},
            "merge": {"grade": 2, "head": "abc", "passed": ["checks", "ci"], "waiting": ["agent_review"], "failing": [],
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
                comment(
                    json!({"id": "c1", "task_id": "t7", "kind": "question", "body": "which db?", "created_at": at(1)}),
                ),
                comment(
                    json!({"id": "c2", "task_id": "t7", "kind": "question", "body": "old", "resolved_at": at(2), "created_at": at(1)}),
                ),
            ],
            agents: vec![agent(json!({"id": "a1", "name": "factory/trail-7-a1"}))],
        }
    }

    fn latest() -> HashMap<String, i64> {
        HashMap::from([(sid("t7"), 41)])
    }

    #[test]
    fn a_task_view_carries_its_priority_and_an_agent_has_none() {
        let f = fixture();
        let v = build(f.input(), &latest(), &Subject::Task(sid("t7"))).unwrap();
        assert_eq!(v.priority, Priority::new(1));
        let a = build(f.input(), &latest(), &Subject::Agent(sid("a1"))).unwrap();
        assert_eq!(a.priority, None);
    }

    #[test]
    fn a_factory_task_shows_its_pr_and_gates_and_has_no_path_to_done() {
        let f = fixture();
        let v = build(f.input(), &latest(), &Subject::Task(sid("t7"))).unwrap();
        assert_eq!(v.title, "TRAIL-7 do it");
        assert!(v.lines.contains(&"held by factory/trail-7-a1".to_string()));
        assert!(v.lines.contains(&"run: trail-7-a1, attempt 2".to_string()));
        let pr = v.pr.unwrap();
        assert_eq!((pr.url.as_str(), pr.number, pr.state.as_str()), ("https://github.com/o/r/pull/12", 12, "open"));
        assert_eq!(
            (pr.passed, pr.waiting, pr.grade),
            (vec!["checks".into(), "ci".into()], vec!["agent_review".into()], Grade::new(2))
        );
        assert!(v.merge_note.unwrap().contains("Merge on GitHub"));
        for a in &v.actions {
            assert!(!matches!(a, Action::Status(Status::Done | Status::Approved)), "{a:?}");
        }
        assert_eq!(v.questions.len(), 1);
        for want in [Action::Release, Action::Cancel, Action::Undo, Action::Revert(1), Action::Resolve(sid("c1"))] {
            assert!(v.actions.contains(&want), "{want:?}");
        }
        assert!(!v.actions.contains(&Action::Status(Status::InReview)));
    }

    #[test]
    fn a_plain_task_has_no_claim_controls_and_no_merge_note() {
        let f = fixture();
        let v = build(f.input(), &HashMap::new(), &Subject::Task(sid("t8"))).unwrap();
        assert!(v.pr.is_none() && v.merge_note.is_none());
        assert!(!v.actions.contains(&Action::Release) && !v.actions.contains(&Action::Undo));
        assert!(v.actions.contains(&Action::Status(Status::Done)));
    }

    #[test]
    fn a_done_task_offers_no_cancel() {
        let mut f = fixture();
        f.tasks.push(task(json!({"id": "t9", "key": "TRAIL-9", "title": "finished", "status": "done", "version": 1})));
        let v = build(f.input(), &HashMap::new(), &Subject::Task(sid("t9"))).unwrap();
        assert!(!v.actions.contains(&Action::Cancel));
    }

    #[test]
    fn a_cancelled_task_offers_no_cancel() {
        let mut f = fixture();
        f.tasks.push(task(
            json!({"id": "t10", "key": "TRAIL-10", "title": "dropped", "status": "cancelled", "version": 1}),
        ));
        let v = build(f.input(), &HashMap::new(), &Subject::Task(sid("t10"))).unwrap();
        assert!(!v.actions.contains(&Action::Cancel));
    }

    #[test]
    fn a_merged_pr_says_who_merged_it() {
        let factory = json!({"merged": {"by": "human", "commit": "abcdef0123",
                                         "pr": {"repo": {"owner": "o", "name": "r"}, "number": 3, "url": "https://x/pull/3"}}});
        let run: RunUpdate = serde_json::from_value(factory).unwrap();
        assert_eq!(pr_panel(&run).unwrap().state, "merged by a person as abcdef01");
        assert!(pr_panel(&RunUpdate::default()).is_none());
    }

    #[test]
    fn an_agent_offers_revert() {
        let f = fixture();
        let v = build(f.input(), &latest(), &Subject::Agent(sid("a1"))).unwrap();
        assert_eq!((v.title.as_str(), v.lines[0].as_str()), ("factory/trail-7-a1", "holds TRAIL-7"));
        assert_eq!(v.actions, [Action::Revert(1)]);
        assert!(build(f.input(), &latest(), &Subject::Agent(sid("nope"))).is_none());
    }

    #[test]
    fn actions_become_the_requests_the_api_takes() {
        let f = fixture();
        let t = Subject::Task(sid("t7"));
        let p = |a: &Action| plan(a, f.input(), &latest(), &t, 1_700_000_000).unwrap();
        let r = p(&Action::Release);
        assert_eq!((r.method, r.path.as_str(), r.if_match), (Method::POST, "/v1/tasks/TRAIL-7/release", None));
        let r = p(&Action::Cancel);
        assert_eq!((r.method, r.path.as_str(), r.if_match), (Method::PATCH, "/v1/tasks/TRAIL-7", Some(4)));
        assert_eq!(r.body, Some(json!({"status": "cancelled"})));
        assert_eq!(p(&Action::Status(Status::Open)).body, Some(json!({"status": "open"})));
        let r = p(&Action::Resolve(sid("c1")));
        assert_eq!((r.path, r.body), (format!("/v1/comments/{}/resolve", sid("c1")), None));
        assert_eq!(p(&Action::Undo).body, Some(json!({"event_id": 41})));
        let r = p(&Action::Revert(1));
        assert_eq!(
            (r.path.as_str(), r.body),
            ("/v1/revert", Some(json!({"actor_id": sid("a1"), "since": "2023-11-14T21:13:20Z"})))
        );
        // From the agent itself.
        let r = plan(&Action::Revert(24), f.input(), &latest(), &Subject::Agent(sid("a1")), 1_700_000_000).unwrap();
        assert_eq!(r.body.unwrap()["since"], "2023-11-13T22:13:20Z");
        // No event to undo.
        assert!(plan(&Action::Undo, f.input(), &HashMap::new(), &t, 0).is_none());
    }

    #[test]
    fn times() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn the_window_dropdown_sets_how_far_back_revert_goes() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(crate::mirror::MirrorPlugin).add_plugins(InspectorPlugin);
        let select = app.world_mut().spawn(WindowChoice).id();
        app.world_mut().trigger(Picked { entity: select, index: 2 });
        app.update();
        assert_eq!(app.world().resource::<RevertWindow>().0, 24);
        // A row past the end changes nothing.
        app.world_mut().trigger(Picked { entity: select, index: 9 });
        app.update();
        assert_eq!(app.world().resource::<RevertWindow>().0, 24);
    }

    #[test]
    fn the_status_dropdown_sends_the_move_and_ignores_the_current_status() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(crate::mirror::MirrorPlugin).add_plugins(InspectorPlugin);
        app.add_message::<Submit>();
        let f = fixture();
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot {
            at: 5,
            projects: vec![],
            tasks: as_json(&f.tasks),
            agents: vec![],
        }));
        app.update();
        let source = app.world_mut().spawn(Control::Idle).id();
        let statuses = vec![Status::Open, Status::InProgress];
        let select = app.world_mut().spawn(StatusMove { source, subject: Subject::Task(sid("t8")), statuses }).id();
        app.world_mut().trigger(Picked { entity: select, index: 0 });
        app.update();
        assert_eq!(app.world().get::<Control>(source), Some(&Control::Idle));
        app.world_mut().trigger(Picked { entity: select, index: 1 });
        app.update();
        assert_eq!(app.world().get::<Control>(source), Some(&Control::Busy));
        let sent: Vec<_> = app.world_mut().resource_mut::<Messages<Submit>>().drain().collect();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].request.path, "/v1/tasks/TRAIL-8");
        assert_eq!(sent[0].request.body, Some(json!({"status": "in_progress"})));
    }

    #[test]
    fn the_status_dropdown_shows_words_for_the_api_values() {
        let statuses = vec![Status::NeedsInput, Status::InProgress];
        assert_eq!(status_labels(&statuses), ["Needs Input", "In Progress"]);
    }

    #[test]
    fn refusals_are_described_for_the_control() {
        assert_eq!(describe(&Outcome::Ok { version: Some(2) }), None);
        assert!(
            describe(&Outcome::Conflict { current_version: Some(3), message: "stale".into() })
                .unwrap()
                .contains("stale")
        );
        assert!(describe(&Outcome::Forbidden("humans only".into())).unwrap().contains("humans only"));
        let limited = Outcome::RateLimited { retry_after: Some(Duration::from_secs(3)), message: "slow".into() };
        assert!(describe(&limited).unwrap().contains("3s"));
        assert_eq!(describe(&Outcome::Failed("no".into())).as_deref(), Some("no"));
    }

    #[test]
    fn the_panel_follows_the_mirror_and_an_answer_reaches_its_control() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(crate::mirror::MirrorPlugin).add_plugins(InspectorPlugin);
        let f = fixture();
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot {
            at: 5,
            projects: vec![],
            tasks: as_json(&f.tasks),
            agents: as_json(&f.agents),
        }));
        app.world_mut().write_message(SyncItem::Event(json!({
            "id": 6, "entity": "comment", "entity_id": sid("c1"), "kind": "comment.created",
            "after": comment_json(json!({"id": "c1", "task_id": "t7", "kind": "question", "body": "q", "resolved_at": null})),
        })));
        app.world_mut().resource_mut::<Selection>().0 = Some(Subject::Task(sid("t7")));
        app.update();
        app.update();
        assert_eq!(app.world().resource::<Latest>().0.get(&sid("t7")), Some(&6));
        let view = app.world().resource::<InspectorState>().0.clone().unwrap();
        assert_eq!(view.title, "TRAIL-7 do it");
        assert!(view.actions.contains(&Action::Undo));

        // A refusal lands on the control that sent it.
        let control = app.world_mut().spawn(Control::Busy).id();
        app.world_mut().write_message(CommandDone {
            source: control,
            id: crate::commands::CommandId(1),
            outcome: Outcome::Forbidden("humans only".into()),
        });
        app.update();
        assert_eq!(app.world().get::<Control>(control), Some(&Control::Refused("not allowed: humans only".into())));

        // Deselecting empties the panel.
        app.world_mut().resource_mut::<Selection>().0 = None;
        app.update();
        assert!(app.world().resource::<InspectorState>().0.is_none());
    }

    fn escape(app: &mut App, from: Entity) {
        let input = KeyboardInput {
            key_code: KeyCode::Escape,
            logical_key: bevy::input::keyboard::Key::Escape,
            state: bevy::input::ButtonState::Pressed,
            text: None,
            repeat: false,
            window: from,
        };
        let window = app.world_mut().spawn_empty().id();
        app.world_mut().trigger(FocusedInput::new(from, input, window));
        app.update();
    }

    fn open_inspector(app: &mut App) -> Entity {
        app.world_mut().resource_mut::<Selection>().0 = Some(Subject::Task(sid("t7")));
        app.world_mut().spawn(InspectorPanel).id()
    }

    fn closing_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(crate::mirror::MirrorPlugin).add_plugins(InspectorPlugin);
        app
    }

    #[test]
    fn the_x_closes_the_inspector() {
        let mut app = closing_app();
        let panel = open_inspector(&mut app);
        let x = app.world_mut().spawn(ChildOf(panel)).id();
        app.world_mut().trigger(RequestClose { source: x });
        app.update();
        assert_eq!(app.world().resource::<Selection>().0, None);
    }

    #[test]
    fn escape_closes_the_inspector() {
        let mut app = closing_app();
        open_inspector(&mut app);
        let target = app.world_mut().spawn_empty().id();
        escape(&mut app, target);
        assert_eq!(app.world().resource::<Selection>().0, None);
    }

    #[test]
    fn escape_with_a_modal_open_leaves_the_inspector() {
        let mut app = closing_app();
        open_inspector(&mut app);
        app.world_mut().spawn(ModalRoot);
        let target = app.world_mut().spawn_empty().id();
        escape(&mut app, target);
        assert!(app.world().resource::<Selection>().0.is_some());
    }

    #[test]
    fn escape_in_a_text_input_leaves_the_inspector() {
        let mut app = closing_app();
        open_inspector(&mut app);
        let field = app.world_mut().spawn(EditableText::new("x")).id();
        escape(&mut app, field);
        assert!(app.world().resource::<Selection>().0.is_some());
    }

    #[test]
    fn the_body_is_a_scroll_area_held_to_the_window_under_a_fixed_header() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), bevy::scene::ScenePlugin))
            .add_systems(Update, spawn_panel);
        app.update();
        let world = app.world_mut();
        let panel = world.query_filtered::<Entity, With<InspectorPanel>>().single(world).unwrap();
        assert_eq!(world.get::<Node>(panel).unwrap().height, percent(100));
        let header = world.query_filtered::<Entity, With<InspectorHeader>>().single(world).unwrap();
        let (body, node) = world.query_filtered::<(Entity, &Node), With<InspectorBody>>().single(world).unwrap();
        assert_eq!((node.overflow, node.min_height), (Overflow::scroll_y(), px(0)));
        assert!(world.get::<ScrollDown>(body).is_some());
        // The body's frame is the panel's child beside the header, and may shrink below its content.
        let frame = world.get::<ChildOf>(body).unwrap().parent();
        assert_eq!(world.get::<ChildOf>(frame).unwrap().parent(), panel);
        assert_eq!(world.get::<Node>(frame).unwrap().min_height, px(0));
        assert_eq!(world.get::<ChildOf>(header).unwrap().parent(), panel);
    }

    #[test]
    fn picking_a_grade_sends_the_patch_with_the_version_and_a_refusal_shows_beside_it() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(crate::mirror::MirrorPlugin).add_plugins(InspectorPlugin);
        app.add_message::<Submit>();
        let f = fixture();
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot {
            at: 5,
            projects: vec![],
            tasks: as_json(&f.tasks),
            agents: as_json(&f.agents),
        }));
        app.update();
        let subject = Subject::Task(sid("t8"));
        let source = app.world_mut().spawn(Control::Idle).id();
        let select = app.world_mut().spawn(GradeChoice { source, subject }).id();
        // Row 3 of the dropdown is grade 3 (none, 1, 2, 3, 4).
        app.world_mut().trigger(Picked { entity: select, index: 3 });
        app.update();
        let sent: Vec<Submit> = app.world_mut().resource_mut::<Messages<Submit>>().drain().collect();
        assert_eq!(sent.len(), 1);
        let r = &sent[0].request;
        assert_eq!((r.method.clone(), r.path.as_str(), r.if_match), (Method::PATCH, "/v1/tasks/TRAIL-8", Some(1)));
        assert_eq!(r.body, Some(json!({"grade": 3})));
        assert_eq!(app.world().get::<Control>(source), Some(&Control::Busy));

        // Clearing sends null.
        app.world_mut().trigger(Picked { entity: select, index: 0 });
        app.update();
        let sent: Vec<Submit> = app.world_mut().resource_mut::<Messages<Submit>>().drain().collect();
        assert_eq!(sent[0].request.body, Some(json!({"grade": null})));

        // The server's refusal is kept on the control; the task's grade, and so the dropdown, stays as it was.
        app.world_mut().write_message(CommandDone {
            source,
            id: crate::commands::CommandId(1),
            outcome: Outcome::Forbidden("only a human can set the first grade".into()),
        });
        app.update();
        assert_eq!(
            app.world().get::<Control>(source),
            Some(&Control::Refused("not allowed: only a human can set the first grade".into()))
        );
        let f = fixture();
        let v = build(f.input(), &latest(), &Subject::Task(sid("t8"))).unwrap();
        assert_eq!(v.grade, None);
    }
}

#[cfg(test)]
mod in_place_tests {
    use bevy::state::app::StatesPlugin;

    use super::*;
    use crate::fixture::{at, comment_json, sid, task_json};
    use crate::sync::Snapshot;

    fn question(id: &str, body: &str) -> SyncItem {
        SyncItem::Event(json!({
            "id": id.trim_start_matches('c').parse::<i64>().unwrap() + 100, "entity": "comment", "entity_id": sid(id),
            "kind": "comment.created",
            "after": comment_json(json!({"id": id, "task_id": "t7", "kind": "question", "body": body,
                      "created_at": at(id.trim_start_matches('c').parse().unwrap())})),
        }))
    }

    fn settle(app: &mut App) {
        for _ in 0..5 {
            app.update();
        }
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin, AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .init_state::<Connection>()
            .init_resource::<ConnectionStatus>()
            .add_plugins((crate::mirror::MirrorPlugin, InspectorUiPlugin));
        let tasks = vec![
            task_json(
                json!({"id": "t7", "key": "TRAIL-7", "title": "do it", "status": "in_progress", "priority": 1, "version": 4}),
            ),
            task_json(json!({"id": "t8", "key": "TRAIL-8", "title": "other", "status": "open", "version": 1})),
        ];
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot { at: 5, projects: vec![], tasks, agents: vec![] }));
        app.world_mut().write_message(question("c1", "which db?"));
        app.world_mut().resource_mut::<NextState<Connection>>().set(Connection::Live);
        app.world_mut().resource_mut::<Selection>().0 = Some(Subject::Task(sid("t7")));
        settle(&mut app);
        app
    }

    /// Every entity under the panel's body, in order.
    fn body(app: &mut App) -> Vec<Entity> {
        let world = app.world_mut();
        let body = world.query_filtered::<Entity, With<InspectorBody>>().single(world).unwrap();
        let mut found = vec![body];
        let mut at = 0;
        while at < found.len() {
            if let Some(children) = world.get::<Children>(found[at]) {
                found.extend(children.iter());
            }
            at += 1;
        }
        found
    }

    fn texts(app: &mut App) -> Vec<String> {
        let world = app.world_mut();
        world.query::<&Text>().iter(world).map(|t| t.0.clone()).collect()
    }

    #[test]
    fn a_new_comment_adds_its_row_and_leaves_every_other_entity_as_it_was() {
        let mut app = app();
        let before = body(&mut app);
        assert!(texts(&mut app).iter().any(|t| t == "? which db?"));

        app.world_mut().write_message(question("c2", "and the cache?"));
        settle(&mut app);
        let after = body(&mut app);
        assert!(texts(&mut app).iter().any(|t| t == "? and the cache?"));
        let added: Vec<_> = after.iter().filter(|e| !before.contains(e)).collect();
        assert!(!added.is_empty(), "the new question has entities");
        // Everything that was there still is, in the same place.
        let kept: Vec<_> = after.iter().filter(|e| before.contains(e)).copied().collect();
        assert_eq!(kept.len(), before.len());
        // Each entity added hangs off the new question's row or the Questions section it joined.
        let world = app.world();
        let old_questions =
            before.iter().filter(|e| world.get::<Shown>(**e).is_some_and(|s| matches!(s.0, Block::Question { .. })));
        assert_eq!(old_questions.count(), 1);
        let new_questions =
            after.iter().filter(|e| world.get::<Shown>(**e).is_some_and(|s| matches!(s.0, Block::Question { .. })));
        assert_eq!(new_questions.count(), 2);
    }

    #[test]
    fn a_changed_title_and_a_pressed_control_leave_the_panel_as_it_was() {
        let mut app = app();
        let before = body(&mut app);
        app.world_mut().write_message(SyncItem::Event(json!({
            "id": 300, "entity": "task", "entity_id": sid("t7"), "kind": "task.updated",
            "after": task_json(json!({"id": "t7", "key": "TRAIL-7", "title": "renamed", "status": "in_progress", "priority": 1,
                      "version": 5})),
        })));
        settle(&mut app);
        assert!(texts(&mut app).iter().any(|t| t == "TRAIL-7 renamed"));
        let controls: Vec<Entity> = {
            let world = app.world_mut();
            world.query_filtered::<Entity, With<Control>>().iter(world).collect()
        };
        assert!(!controls.is_empty());
        for c in controls {
            *app.world_mut().get_mut::<Control>(c).unwrap() = Control::Refused("nope".into());
        }
        settle(&mut app);
        assert_eq!(body(&mut app), before);
        assert!(texts(&mut app).iter().any(|t| t == "nope"));
    }

    #[test]
    fn an_update_leaves_an_open_modals_title_alone() {
        let mut app = app();
        let modal = app.world_mut().commands().spawn_scene(panel_header("Answer TRAIL-7", true)).id();
        settle(&mut app);
        app.world_mut().write_message(SyncItem::Event(json!({
            "id": 301, "entity": "task", "entity_id": sid("t7"), "kind": "task.updated",
            "after": task_json(json!({"id": "t7", "key": "TRAIL-7", "title": "renamed", "status": "in_progress", "priority": 1,
                      "version": 5})),
        })));
        settle(&mut app);
        assert!(texts(&mut app).iter().any(|t| t == "TRAIL-7 renamed"));
        let world = app.world_mut();
        let titles: Vec<String> = world
            .query_filtered::<(Entity, &Text), With<PanelTitle>>()
            .iter(world)
            .filter(|(e, _)| world.get::<ChildOf>(*e).is_some_and(|c| c.parent() == modal))
            .map(|(_, t)| t.0.clone())
            .collect();
        assert_eq!(titles, ["Answer TRAIL-7"]);
    }

    #[test]
    fn a_different_subject_rebuilds_the_panel() {
        let mut app = app();
        let before = body(&mut app);
        app.world_mut().resource_mut::<Selection>().0 = Some(Subject::Task(sid("t8")));
        settle(&mut app);
        let after = body(&mut app);
        assert!(after.iter().skip(1).all(|e| !before.contains(e)), "nothing is carried over");
        assert!(texts(&mut app).iter().any(|t| t == "TRAIL-8 other"));
        assert!(!texts(&mut app).iter().any(|t| t == "? which db?"));
    }
}
