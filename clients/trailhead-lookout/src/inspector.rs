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
use bevy::ui_widgets::{Activate, RequestClose};
use reqwest::Method;
use serde_json::{Value, json};

use crate::author::Act;
use crate::board::Input;
use crate::commands::{CommandDone, Outcome, Request, Submit};
use crate::connection::{Connection, ConnectionStatus};
use crate::mirror::{Data, IsAgent, IsClaim, IsComment, IsProject, IsTask, Mirror};
use crate::sync::SyncItem;
use crate::widgets::{
    Icon, ModalRoot, Picked, badge, button, danger_button, extent, layer, panel, panel_header, priority_tone, row,
    section, select, size, text, text_dim, text_toned, token, tool_button,
};

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
    Status(String),
    /// Resolve this question (a comment id) without an answer. Answering opens the form instead.
    Resolve(String),
    /// Undo the event that last changed the task.
    Undo,
    /// Revert what the holder (an agent id) did in the last `n` hours.
    Revert(i64),
}

impl Action {
    pub fn label(&self) -> String {
        match self {
            Action::Release => "release claim".into(),
            Action::Cancel => "cancel task".into(),
            Action::Status(s) => format!("move to {s}"),
            Action::Resolve(_) => "resolve".into(),
            Action::Undo => "undo last change".into(),
            Action::Revert(h) => format!("revert last {h}h"),
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
    pub number: Option<u64>,
    /// `open`, or `merged by a person` / `merged by the merger` with the commit.
    pub state: String,
    pub grade: Option<u64>,
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
    pub status: Option<String>,
    /// The task's priority; none for an agent.
    pub priority: Option<i64>,
    pub actions: Vec<Action>,
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or_default()
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(|g| g.as_str().map(str::to_string)).collect()
}

/// The PR panel from the `factory` metadata; none when the task has no PR yet.
pub fn pr_panel(factory: &Value) -> Option<PrPanel> {
    let merged = &factory["merged"];
    let pr = if merged["pr"].is_object() { &merged["pr"] } else { &factory["pr"] };
    let url = pr["url"].as_str()?.to_string();
    let state = if merged.is_object() {
        let by = if s(merged, "by") == "merger" { "the merger" } else { "a person" };
        let commit = merged["commit"]
            .as_str()
            .map_or(String::new(), |c| format!(" as {}", c.chars().take(8).collect::<String>()));
        format!("merged by {by}{commit}")
    } else {
        "open".to_string()
    };
    let decision = &factory["merge"];
    Some(PrPanel {
        url,
        number: pr["number"].as_u64(),
        state,
        grade: factory["grade"]["grade"].as_u64().or_else(|| decision["grade"].as_u64()),
        passed: strings(&decision["passed"]),
        waiting: strings(&decision["waiting"]),
        failing: strings(&decision["failing"]),
        would_merge: decision["would_merge"].as_bool().unwrap_or(false),
        problem: decision["problem"].as_str().or_else(|| decision["refused"].as_str()).map(str::to_string),
    })
}

/// The statuses a task can be moved to by hand from the panel.
fn status_moves(current: &str, factory: bool) -> Vec<Action> {
    let all: &[&str] = if factory {
        &["open", "in_progress", "needs_input", "in_review"]
    } else {
        &["open", "in_progress", "needs_input", "in_review", "approved", "done"]
    };
    all.iter().filter(|st| **st != current).map(|st| Action::Status(st.to_string())).collect()
}

/// The panel for `subject`, or none when it isn't in the mirror (deleted, or not synced yet). `latest` maps a task
/// id to the id of the event that last changed it.
pub fn build(input: Input, latest: &HashMap<String, i64>, subject: &Subject) -> Option<InspectorView> {
    let agent_name = |id: &str| input.agents.iter().find(|a| s(a, "id") == id).map(|a| s(a, "name").to_string());
    let task_key = |id: &str| input.tasks.iter().find(|t| s(t, "id") == id).map(|t| s(t, "key").to_string());
    match subject {
        Subject::Agent(id) => {
            let name = agent_name(id)?;
            let held: Vec<String> = input
                .claims
                .iter()
                .filter(|c| s(c, "holder_id") == id)
                .filter_map(|c| task_key(s(c, "task_id")))
                .collect();
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
                actions: vec![Action::Revert(1)],
            })
        }
        Subject::Task(id) => {
            let task = input.tasks.iter().find(|t| s(t, "id") == id)?;
            let status = s(task, "status");
            let factory = &task["metadata"]["factory"];
            let is_factory = factory.is_object();
            let holder = input.claims.iter().find(|c| s(c, "task_id") == id).map(|c| s(c, "holder_id").to_string());
            let holder_agent = holder.as_deref().and_then(|h| agent_name(h).map(|n| (h.to_string(), n)));

            let mut lines = vec![format!("status: {status}")];
            if let Some(grade) = task["grade"].as_i64() {
                lines.push(format!("grade: {grade}"));
            }
            match (&holder_agent, &holder) {
                (Some((_, name)), _) => lines.push(format!("held by {name}")),
                (None, Some(h)) => lines.push(format!("held by {}", h.chars().take(8).collect::<String>())),
                (None, None) => {}
            }
            if let Some(sandbox) = factory["sandbox"].as_str() {
                let attempt = factory["attempt"].as_u64().map_or(String::new(), |a| format!(", attempt {a}"));
                lines.push(format!("run: {sandbox}{attempt}"));
            }

            let mut questions: Vec<&Value> = input
                .comments
                .iter()
                .filter(|c| s(c, "task_id") == id && s(c, "kind") == "question" && c["resolved_at"].is_null())
                .collect();
            questions.sort_by(|a, b| s(a, "created_at").cmp(s(b, "created_at")));
            let questions: Vec<Question> = questions
                .into_iter()
                .map(|q| Question { key: s(task, "key").into(), id: s(q, "id").into(), body: s(q, "body").into() })
                .collect();

            let pr = pr_panel(factory);
            let merge_note = (is_factory && !matches!(status, "done" | "cancelled")).then(|| match &pr {
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
            if status != "cancelled" {
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
                title: format!("{} {}", s(task, "key"), s(task, "title")),
                lines,
                questions,
                pr,
                merge_note,
                status: Some(status.to_string()),
                priority: task["priority"].as_i64(),
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
        Subject::Task(id) => input.tasks.iter().find(|t| s(t, "id") == id),
        Subject::Agent(_) => None,
    };
    let key = task.map(|t| s(t, "key"));
    let version = task.and_then(|t| t["version"].as_i64());
    let path = |tail: &str| format!("/v1/tasks/{}{tail}", key.unwrap_or_default());
    Some(match action {
        Action::Release => Request::new(Method::POST, path("/release"), None, None),
        Action::Cancel => Request::new(Method::PATCH, path(""), Some(json!({"status": "cancelled"})), version),
        Action::Status(st) => Request::new(Method::PATCH, path(""), Some(json!({"status": st})), version),
        Action::Resolve(comment) => Request::new(Method::POST, format!("/v1/comments/{comment}/resolve"), None, None),
        Action::Undo => {
            let Subject::Task(id) = subject else { return None };
            Request::new(Method::POST, "/v1/undo", Some(json!({"event_id": latest.get(id)?})), None)
        }
        Action::Revert(hours) => {
            let actor = match subject {
                Subject::Agent(id) => id.clone(),
                Subject::Task(id) => {
                    input.claims.iter().find(|c| s(c, "task_id") == id).map(|c| s(c, "holder_id").to_string())?
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

fn collect<C: Component>(q: &Query<&Data, With<C>>) -> Vec<Value> {
    q.iter().map(|d| d.0.clone()).collect()
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn rebuild(
    selection: Res<Selection>,
    latest: Res<Latest>,
    mirror: Res<Mirror>,
    changed: Query<(), Or<(Added<Data>, Changed<Data>)>>,
    mut removed: RemovedComponents<Data>,
    projects: Query<&Data, With<IsProject>>,
    tasks: Query<&Data, With<IsTask>>,
    claims: Query<&Data, With<IsClaim>>,
    comments: Query<&Data, With<IsComment>>,
    agents: Query<&Data, With<IsAgent>>,
    mut state: ResMut<InspectorState>,
) {
    let gone = removed.read().count() > 0;
    if !(selection.is_changed() || latest.is_changed() || mirror.is_changed() || gone || !changed.is_empty()) {
        return;
    }
    let next = selection.0.as_ref().and_then(|subject| {
        let (projects, tasks, claims, comments, agents) =
            (collect(&projects), collect(&tasks), collect(&claims), collect(&comments), collect(&agents));
        let input = Input { projects: &projects, tasks: &tasks, claims: &claims, comments: &comments, agents: &agents };
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
            .add_systems(Startup, spawn_panel)
            .add_systems(Update, redraw);
    }
}

#[derive(Component)]
struct InspectorPanel;

/// The dropdown that moves a task to another status: `statuses[i]` is what its option `i` means, the task's own
/// status first. Its answer shows beside it on `source`.
#[derive(Component, Debug, Clone)]
struct StatusMove {
    source: Entity,
    subject: Subject,
    statuses: Vec<String>,
}

/// The dropdown that sets how far back "revert" goes.
#[derive(Component, Debug, Clone, Copy, Default)]
struct WindowChoice;

fn spawn_panel(mut commands: Commands) {
    commands
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
        .insert((Name::new("inspector"), InspectorPanel));
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
    windows: Query<(), With<WindowChoice>>,
    mut window: ResMut<RevertWindow>,
    mut commands: Commands,
) {
    if windows.contains(picked.entity) {
        if let Some(hours) = WINDOWS.get(picked.index) {
            window.0 = *hours;
        }
    } else if let Ok(StatusMove { source, subject, statuses }) = moves.get(picked.entity)
        && picked.index > 0
        && let Some(status) = statuses.get(picked.index)
    {
        let (source, subject, action) = (*source, subject.clone(), Action::Status(status.clone()));
        commands.queue(move |world: &mut World| send(world, source, &subject, &action));
    }
}

/// Builds the request when the button is pressed, from the mirror as it is then: the version for `If-Match`, the
/// event to undo and the `since` of a revert must not be those of the last redraw.
fn send(world: &mut World, source: Entity, subject: &Subject, action: &Action) {
    fn rows<C: Component>(world: &mut World) -> Vec<Value> {
        world.query_filtered::<&Data, With<C>>().iter(world).map(|d| d.0.clone()).collect()
    }
    let (projects, tasks, claims, comments, agents) = (
        rows::<IsProject>(world),
        rows::<IsTask>(world),
        rows::<IsClaim>(world),
        rows::<IsComment>(world),
        rows::<IsAgent>(world),
    );
    let input = Input { projects: &projects, tasks: &tasks, claims: &claims, comments: &comments, agents: &agents };
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

/// Says under `parent` where a control's command stands.
fn note(commands: &mut Commands, parent: Entity, state: Option<&Control>) {
    let scene: Box<dyn Scene> = match state {
        Some(Control::Busy) => Box::new(text_dim("sending…", size::SECONDARY)),
        Some(Control::Done) => Box::new(text_dim("done", size::SECONDARY)),
        Some(Control::Refused(why)) => Box::new(text_toned(why.clone(), size::SECONDARY, token::DANGER)),
        _ => return,
    };
    commands.spawn_scene(scene).insert(ChildOf(parent));
}

/// Whether a line of [`InspectorView::lines`] is about the run rather than the task's status.
fn is_run_line(line: &str) -> bool {
    line.starts_with("run:")
}

/// A section of the panel under `heading`, for the caller to fill.
fn section_in(commands: &mut Commands, panel: Entity, heading: &str) -> Entity {
    commands.spawn_scene(bsn! { @section(heading.to_string()) }).insert(ChildOf(panel)).id()
}

/// A row under `parent` whose items wrap onto more lines when they don't fit.
fn wrapping_row(commands: &mut Commands, parent: Entity) -> Entity {
    commands.spawn_scene(bsn! { @row() Node { flex_wrap: FlexWrap::Wrap } }).insert(ChildOf(parent)).id()
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn redraw(
    state: Res<InspectorState>,
    connection: Res<State<Connection>>,
    status: Res<ConnectionStatus>,
    window: Res<RevertWindow>,
    mut controls: ResMut<Controls>,
    states: Query<&Control>,
    changed_controls: Query<(), Changed<Control>>,
    panel: Single<(Entity, Option<&Children>, &mut Node), With<InspectorPanel>>,
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
    let (panel, children, mut node) = panel.into_inner();
    for child in children.into_iter().flatten() {
        commands.entity(*child).despawn();
    }
    let Some(view) = &state.0 else {
        node.display = Display::None;
        return;
    };
    node.display = Display::Flex;
    let live = *connection.get() == Connection::Live;
    let c = &mut commands;

    c.spawn_scene(panel_header(view.title.clone(), true)).insert(ChildOf(panel));

    let (run, status_lines): (Vec<&String>, Vec<&String>) = view.lines.iter().partition(|l| is_run_line(l));
    for (heading, lines) in [("Status", status_lines), ("Run", run)] {
        if lines.is_empty() {
            continue;
        }
        let section = section_in(c, panel, heading);
        if let (true, Some(p)) = (heading == "Status", view.priority) {
            c.spawn_scene(badge(format!("P{p}"), None, priority_tone(p))).insert(ChildOf(section));
        }
        for l in lines {
            c.spawn_scene(text(l.clone(), size::BODY)).insert(ChildOf(section));
        }
    }

    if !view.questions.is_empty() {
        let section = section_in(c, panel, "Questions");
        for q in &view.questions {
            let line = c.spawn_scene(bsn! { @row() }).insert(ChildOf(section)).id();
            c.spawn_scene(
                bsn! { @text_toned(format!("? {}", q.body), size::BODY, token::WARN) Node { flex_grow: 1.0, flex_shrink: 1.0 } },
            )
            .insert(ChildOf(line));
            // The same form the board opens; it saves even after the inspector moves on.
            let act = Act::Answer { key: q.key.clone(), comment_id: q.id.clone(), question: q.body.clone() };
            let mut answer = c.spawn_scene(bsn! { @tool_button(Icon::Comment, "Answer", ButtonVariant::Normal) });
            answer.insert((act, ChildOf(line)));
            if !live {
                answer.insert(InteractionDisabled);
            }
            let resolve = Action::Resolve(q.id.clone());
            let source = control(&mut controls, c, &view.subject, &resolve);
            let mut button = c.spawn_scene(bsn! {
                @tool_button(Icon::Check, "Resolve without an answer", ButtonVariant::Normal)
                @sends(source, view.subject.clone(), resolve.clone())
            });
            button.insert(ChildOf(line));
            if !live {
                button.insert(InteractionDisabled);
            }
            note(c, section, states.get(source).ok());
        }
    }

    if view.pr.is_some() || view.merge_note.is_some() {
        let section = section_in(c, panel, "Pull request");
        if let Some(pr) = &view.pr {
            let number = pr.number.map_or(String::new(), |n| format!("#{n} "));
            c.spawn_scene(text(format!("{number}{}", pr.url), size::BODY)).insert(ChildOf(section));
            let grade = pr.grade.map_or(String::new(), |g| format!(", grade {g}"));
            c.spawn_scene(text_dim(format!("{}{grade}", pr.state), size::SECONDARY)).insert(ChildOf(section));
            if !pr.passed.is_empty() || !pr.waiting.is_empty() || !pr.failing.is_empty() {
                let gates = wrapping_row(c, section);
                for (names, icon, tone) in [
                    (&pr.passed, Icon::CircleCheck, token::OK),
                    (&pr.waiting, Icon::Circle, tokens::TEXT_DIM),
                    (&pr.failing, Icon::CircleX, token::DANGER),
                ] {
                    for name in names {
                        c.spawn_scene(badge(name.clone(), Some(icon), Some(tone.clone()))).insert(ChildOf(gates));
                    }
                }
            }
            if let Some(problem) = &pr.problem {
                c.spawn_scene(text_toned(problem.clone(), size::BODY, token::WARN)).insert(ChildOf(section));
            }
        }
        if let Some(merge_note) = &view.merge_note {
            c.spawn_scene(text_dim(merge_note.clone(), size::SECONDARY)).insert(ChildOf(section));
        }
    }

    if !live {
        let why = status.reason.as_ref().map_or_else(String::new, |r| format!(": {r}"));
        c.spawn_scene(text_toned(format!("{:?}{why}. Controls are off.", connection.get()), size::BODY, token::WARN))
            .insert(ChildOf(panel));
        return;
    }

    let section = section_in(c, panel, "Actions");
    // One dropdown for every status the task can move to, the current one showing.
    let moves: Vec<String> =
        view.actions.iter().filter_map(|a| if let Action::Status(st) = a { Some(st.clone()) } else { None }).collect();
    if let (Some(current), false) = (&view.status, moves.is_empty()) {
        let line = c.spawn_scene(bsn! { @row() }).insert(ChildOf(section)).id();
        c.spawn_scene(text_dim("Move to", size::SECONDARY)).insert(ChildOf(line));
        let statuses: Vec<String> = std::iter::once(current.clone()).chain(moves).collect();
        let source = control(&mut controls, c, &view.subject, &Action::Status(String::new()));
        let max_visible = statuses.len();
        c.spawn_scene(select(statuses.clone(), Some(0), max_visible))
            .insert((StatusMove { source, subject: view.subject.clone(), statuses }, ChildOf(line)));
        note(c, line, states.get(source).ok());
    }
    for action in &view.actions {
        let action = match action {
            Action::Status(_) | Action::Resolve(_) => continue,
            Action::Revert(_) => Action::Revert(window.0),
            other => other.clone(),
        };
        let source = control(&mut controls, c, &view.subject, &action);
        let line = c.spawn_scene(bsn! { @row() }).insert(ChildOf(section)).id();
        let press = sends(source, view.subject.clone(), action.clone());
        match action {
            Action::Release => {
                c.spawn_scene(bsn! { @button(action.label(), Some(Icon::Release), ButtonVariant::Normal) @{press} })
            }
            Action::Undo => {
                c.spawn_scene(bsn! { @button(action.label(), Some(Icon::Undo), ButtonVariant::Normal) @{press} })
            }
            Action::Cancel => c.spawn_scene(bsn! { @danger_button(action.label(), Some(Icon::Stop)) @{press} }),
            _ => c.spawn_scene(bsn! { @danger_button("revert since", Some(Icon::History)) @{press} }),
        }
        .insert(ChildOf(line));
        if matches!(action, Action::Revert(_)) {
            let windows = WINDOWS.iter().map(|h| format!("{h}h ago")).collect();
            let selected = WINDOWS.iter().position(|h| *h == window.0);
            c.spawn_scene(select(windows, selected, WINDOWS.len())).insert((WindowChoice, ChildOf(line)));
        }
        note(c, line, states.get(source).ok());
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::sync::Snapshot;

    struct Fixture {
        projects: Vec<Value>,
        tasks: Vec<Value>,
        claims: Vec<Value>,
        comments: Vec<Value>,
        agents: Vec<Value>,
    }

    impl Fixture {
        fn input(&self) -> Input<'_> {
            Input {
                projects: &self.projects,
                tasks: &self.tasks,
                claims: &self.claims,
                comments: &self.comments,
                agents: &self.agents,
            }
        }
    }

    fn fixture() -> Fixture {
        let factory = json!({
            "sandbox": "trail-7-a1", "attempt": 2,
            "pr": {"number": 12, "url": "https://github.com/o/r/pull/12"},
            "grade": {"grade": 2},
            "merge": {"grade": 2, "passed": ["checks", "ci"], "waiting": ["agent_review"], "failing": [],
                      "would_merge": false, "problem": null},
        });
        Fixture {
            projects: vec![],
            tasks: vec![
                json!({"id": "t7", "key": "TRAIL-7", "title": "do it", "status": "in_review", "priority": 1, "version": 4,
                       "metadata": {"factory": factory}}),
                json!({"id": "t8", "key": "TRAIL-8", "title": "plain", "status": "open", "version": 1}),
            ],
            claims: vec![json!({"task_id": "t7", "holder_id": "a1"})],
            comments: vec![
                json!({"id": "c1", "task_id": "t7", "kind": "question", "body": "which db?", "resolved_at": null, "created_at": "1"}),
                json!({"id": "c2", "task_id": "t7", "kind": "question", "body": "old", "resolved_at": "2", "created_at": "1"}),
            ],
            agents: vec![json!({"id": "a1", "name": "factory/trail-7-a1"})],
        }
    }

    fn latest() -> HashMap<String, i64> {
        HashMap::from([("t7".to_string(), 41)])
    }

    #[test]
    fn a_task_view_carries_its_priority_and_an_agent_has_none() {
        let f = fixture();
        let v = build(f.input(), &latest(), &Subject::Task("t7".into())).unwrap();
        assert_eq!(v.priority, Some(1));
        let a = build(f.input(), &latest(), &Subject::Agent("a1".into())).unwrap();
        assert_eq!(a.priority, None);
    }

    #[test]
    fn a_factory_task_shows_its_pr_and_gates_and_has_no_path_to_done() {
        let f = fixture();
        let v = build(f.input(), &latest(), &Subject::Task("t7".into())).unwrap();
        assert_eq!(v.title, "TRAIL-7 do it");
        assert!(v.lines.contains(&"held by factory/trail-7-a1".to_string()));
        assert!(v.lines.contains(&"run: trail-7-a1, attempt 2".to_string()));
        let pr = v.pr.unwrap();
        assert_eq!(
            (pr.url.as_str(), pr.number, pr.state.as_str()),
            ("https://github.com/o/r/pull/12", Some(12), "open")
        );
        assert_eq!(
            (pr.passed, pr.waiting, pr.grade),
            (vec!["checks".into(), "ci".into()], vec!["agent_review".into()], Some(2))
        );
        assert!(v.merge_note.unwrap().contains("Merge on GitHub"));
        for a in &v.actions {
            assert!(!matches!(a, Action::Status(s) if s == "done" || s == "approved"), "{a:?}");
        }
        assert_eq!(v.questions.len(), 1);
        for want in [Action::Release, Action::Cancel, Action::Undo, Action::Revert(1), Action::Resolve("c1".into())] {
            assert!(v.actions.contains(&want), "{want:?}");
        }
        assert!(!v.actions.contains(&Action::Status("in_review".into())));
    }

    #[test]
    fn a_plain_task_has_no_claim_controls_and_no_merge_note() {
        let f = fixture();
        let v = build(f.input(), &HashMap::new(), &Subject::Task("t8".into())).unwrap();
        assert!(v.pr.is_none() && v.merge_note.is_none());
        assert!(!v.actions.contains(&Action::Release) && !v.actions.contains(&Action::Undo));
        assert!(v.actions.contains(&Action::Status("done".into())));
    }

    #[test]
    fn a_merged_pr_says_who_merged_it() {
        let factory = json!({"merged": {"by": "human", "commit": "abcdef0123",
                                         "pr": {"number": 3, "url": "https://x/pull/3"}}});
        assert_eq!(pr_panel(&factory).unwrap().state, "merged by a person as abcdef01");
        assert!(pr_panel(&json!({})).is_none());
    }

    #[test]
    fn an_agent_offers_revert() {
        let f = fixture();
        let v = build(f.input(), &latest(), &Subject::Agent("a1".into())).unwrap();
        assert_eq!((v.title.as_str(), v.lines[0].as_str()), ("factory/trail-7-a1", "holds TRAIL-7"));
        assert_eq!(v.actions, [Action::Revert(1)]);
        assert!(build(f.input(), &latest(), &Subject::Agent("nope".into())).is_none());
    }

    #[test]
    fn actions_become_the_requests_the_api_takes() {
        let f = fixture();
        let t = Subject::Task("t7".into());
        let p = |a: &Action| plan(a, f.input(), &latest(), &t, 1_700_000_000).unwrap();
        let r = p(&Action::Release);
        assert_eq!((r.method, r.path.as_str(), r.if_match), (Method::POST, "/v1/tasks/TRAIL-7/release", None));
        let r = p(&Action::Cancel);
        assert_eq!((r.method, r.path.as_str(), r.if_match), (Method::PATCH, "/v1/tasks/TRAIL-7", Some(4)));
        assert_eq!(r.body, Some(json!({"status": "cancelled"})));
        assert_eq!(p(&Action::Status("open".into())).body, Some(json!({"status": "open"})));
        let r = p(&Action::Resolve("c1".into()));
        assert_eq!((r.path.as_str(), r.body), ("/v1/comments/c1/resolve", None));
        assert_eq!(p(&Action::Undo).body, Some(json!({"event_id": 41})));
        let r = p(&Action::Revert(1));
        assert_eq!(
            (r.path.as_str(), r.body),
            ("/v1/revert", Some(json!({"actor_id": "a1", "since": "2023-11-14T21:13:20Z"})))
        );
        // From the agent itself.
        let r = plan(&Action::Revert(24), f.input(), &latest(), &Subject::Agent("a1".into()), 1_700_000_000).unwrap();
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
            tasks: f.tasks,
            agents: vec![],
        }));
        app.update();
        let source = app.world_mut().spawn(Control::Idle).id();
        let statuses = vec!["open".to_string(), "in_progress".to_string()];
        let select = app.world_mut().spawn(StatusMove { source, subject: Subject::Task("t8".into()), statuses }).id();
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
        let tasks = f.tasks.clone();
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot {
            at: 5,
            projects: vec![],
            tasks,
            agents: f.agents,
        }));
        app.world_mut().write_message(SyncItem::Event(json!({
            "id": 6, "entity": "comment", "entity_id": "c1", "kind": "comment.created",
            "after": {"id": "c1", "task_id": "t7", "kind": "question", "body": "q", "resolved_at": null},
        })));
        app.world_mut().resource_mut::<Selection>().0 = Some(Subject::Task("t7".into()));
        app.update();
        app.update();
        assert_eq!(app.world().resource::<Latest>().0.get("t7"), Some(&6));
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
        app.world_mut().resource_mut::<Selection>().0 = Some(Subject::Task("t7".into()));
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
}
