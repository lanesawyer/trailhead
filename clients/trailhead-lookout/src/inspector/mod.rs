//! The inspector (`docs/factory/ui.md#views`): the selected task, run or agent, and the controls to steer it: release
//! a claim, cancel, move the status, answer a question, undo the last change, revert what an agent did since a time.
//! Each is a command ([`crate::commands`]) whose refusal is shown beside the control that sent it.
//!
//! A task shows in the [`TaskModal`], in the middle of the window: its title, description, subtasks, open questions
//! and PR in a main column, and its facts (project, status, priority, grade, claim, dates, dependencies, run) in a
//! sidebar. The whole task is fetched when it is selected ([`crate::detail`]), for what the mirror doesn't hold. Its
//! header has up and down arrows (and the keys up, down, K and J) to the previous and next task in its board column,
//! and a ⋯ menu of what else can be done to it: release, cancel, undo, revert. An agent still shows in the side
//! panel, which stays for anything that suits a panel better than a modal.
//!
//! There is no approve or done button. A factory task is approved by the merger once its gates pass and finished when
//! its PR merges (`docs/factory/lifecycle.md`), so the panel links the PR and shows its merge state and gates from the
//! task's `factory` metadata and leaves the merge to GitHub. Status moves on a factory task leave out `approved` and
//! `done` for the same reason.
//!
//! [`build`] is a pure function from the mirror's JSON to an [`InspectorView`], and [`plan`] turns an [`Action`] into
//! the [`Request`] it sends, so both are tested without a window. "answer" opens the answer form from
//! [`crate::author`], as the board's does; "resolve" closes a question without one.

use std::collections::{HashMap, HashSet};

use bevy::input::keyboard::KeyboardInput;
use bevy::input_focus::{FocusCause, FocusedInput, InputFocus};
use bevy::prelude::*;
use bevy::text::EditableText;
use bevy::ui::InteractionDisabled;
use bevy::ui_widgets::{Activate, ControlOrientation, RequestClose};
#[cfg(test)]
use reqwest::Method;
#[cfg(test)]
use serde_json::json;
use trailhead_client::http::{Call, NewComment, Revert, TaskUpdate, Undo};
use trailhead_client::text::Text as Wording;
use trailhead_factory::{GateKind, MergeDecision, MergedBy, RunUpdate};
use trailhead_model::{
    Agent, Claim, Comment, CommentKind, Grade, Priority, Project, Status, Task, TaskDetail, TaskKey, User,
};
use uuid::Uuid;

use crate::author::{
    Act, grade_choices, grade_index, grade_options, grade_tones, priority_options, priority_tones, status_tones,
};
use crate::board::Input;
use crate::commands::{CommandDone, Outcome, Request, Submit};
use crate::connection::{Api, Connection, ConnectionStatus};
use crate::detail::{Detail, Refetch};
use crate::factory::FactoryStatus;
use crate::mirror::{Mirror, Mirrored, Rev, rows};
use crate::project::OpenProject;
use crate::settings::OpenLogs;
use crate::sync::SyncItem;
use crate::widgets::{
    ButtonKind, DetailPanes, Icon, InlineChosen, InlineEdited, InlineField, InlineText, Keyed, Modal, ModalClosed,
    ModalRoot, ModalSlots, PanelTitle, Picked, SelectEdit, Side, TextTone, Token, badge, bubble, bubble_head, button,
    comment_icon, display, extent, grade_tone, inline_field, inline_text_sized, layer, link_button, menu_item,
    open_modal, panel, panel_header, priority_tone, row, scroll_body, scroll_frame, scrollbar, section_frame, select,
    select_iconed, size, space, spawn_detail_panes, spawn_icon_menu, status_label, text, text_dim, text_input,
    text_toned, token, tool_button,
};

mod compose;
mod edit;
mod layout;
mod modal;
mod model;
mod panel;
#[cfg(test)]
mod testing;

pub use compose::*;
pub use edit::*;
use layout::*;
pub use modal::*;
pub use model::*;
use panel::*;

/// What the inspector is looking at: a task (and its run, if it has one), or an agent. Ids, not keys.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Subject {
    Task(String),
    Agent(String),
}

#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct Selection(pub Option<Subject>);

/// The tasks before and after the selected one in its board column (ids), which the task modal's arrows go to. The
/// board keeps it ([`crate::board`]); none at a column's end or for a task the board doesn't show.
#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct Neighbours {
    pub previous: Option<String>,
    pub next: Option<String>,
}

/// How far back "revert" goes, in hours; one of [`WINDOWS`], picked from a dropdown.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevertWindow(pub i64);

impl Default for RevertWindow {
    fn default() -> Self {
        RevertWindow(1)
    }
}

const WINDOWS: [i64; 4] = [1, 6, 24, 72];

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
    (agents, users): (Query<&Mirrored<Agent>>, Query<&Mirrored<User>>),
    detail: Option<Res<Detail>>,
    api: Option<Res<Api>>,
    mut state: ResMut<InspectorState>,
) {
    let gone = removed.read().count() > 0;
    let fetched = detail.as_ref().is_some_and(|d| d.is_changed());
    if !(selection.is_changed() || latest.is_changed() || mirror.is_changed() || fetched || gone || !changed.is_empty())
    {
        return;
    }
    let next = selection.0.as_ref().and_then(|subject| {
        let (projects, tasks, claims, comments, agents, users) =
            (rows(&projects), rows(&tasks), rows(&claims), rows(&comments), rows(&agents), rows(&users));
        let input = Input {
            projects: &projects,
            tasks: &tasks,
            claims: &claims,
            comments: &comments,
            agents: &agents,
            users: &users,
            now: jiff::Timestamp::now(),
            ..Input::default()
        };
        let me = api.as_ref().map(|a| (a.0.user_id.as_str(), a.0.user.as_str()));
        let fetched = match &detail {
            Some(d) => Fetched { detail: d.data.as_ref(), error: d.error.as_deref(), me },
            None => Fetched { me, ..Fetched::default() },
        };
        build(input, &latest.0, subject, fetched)
    });
    if state.0 != next {
        state.0 = next;
    }
}

/// The task version each finished control was answered at, so [`tidy_controls`] can tell the change the command made
/// itself (its event may land before or after the reply) from a later one. `None` until a version is known.
#[derive(Resource, Default)]
struct Settled(HashMap<Entity, Option<i64>>);

fn settle(mut done: MessageReader<CommandDone>, mut controls: Query<&mut Control>, mut settled: ResMut<Settled>) {
    for d in done.read() {
        if let Ok(mut c) = controls.get_mut(d.source) {
            *c = describe(&d.outcome).map_or(Control::Done, Control::Refused);
            let version = if let Outcome::Ok { version } = d.outcome { version } else { None };
            settled.0.insert(d.source, version);
        }
    }
}

/// Keeps [`Controls`] honest: a finished control goes back to idle when its task changes again (a result is about
/// the task as it was; the change the command itself made doesn't count), and the controls of a subject that has left
/// the mirror are despawned.
fn tidy_controls(
    mut controls: ResMut<Controls>,
    mut settled: ResMut<Settled>,
    tasks: Query<&Mirrored<Task>>,
    agents: Query<&Mirrored<Agent>>,
    mut states: Query<&mut Control>,
    mut commands: Commands,
) {
    let versions: HashMap<String, i64> = tasks.iter().map(|t| (t.0.id.to_string(), t.0.version)).collect();
    for ((subject, _), entity) in &controls.0 {
        let Subject::Task(id) = subject else { continue };
        let Some(&now) = versions.get(id) else { continue };
        let Ok(mut c) = states.get_mut(*entity) else { continue };
        if !matches!(*c, Control::Done | Control::Refused(_)) {
            settled.0.remove(entity);
            continue;
        }
        // With no version from the reply, the task as the mirror has it now is the baseline.
        let at = *settled.0.entry(*entity).or_default().get_or_insert(now);
        if now > at {
            *c = Control::Idle;
            settled.0.remove(entity);
        }
    }
    let live: HashSet<String> =
        tasks.iter().map(|t| t.0.id.to_string()).chain(agents.iter().map(|a| a.0.id.to_string())).collect();
    // An empty mirror is one not yet loaded, not one with nothing in it.
    if live.is_empty() {
        return;
    }
    controls.0.retain(|(subject, _), entity| {
        let (Subject::Task(id) | Subject::Agent(id)) = subject;
        let keep = live.contains(id);
        if !keep {
            commands.entity(*entity).try_despawn();
            settled.0.remove(entity);
        }
        keep
    });
}

/// Keeps the [`Selection`] and the panel's contents from the mirror.
pub struct InspectorPlugin;

impl Plugin for InspectorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Selection>()
            .init_resource::<Neighbours>()
            .init_resource::<RevertWindow>()
            .init_resource::<Latest>()
            .init_resource::<Settled>()
            .init_resource::<InspectorState>()
            // Registering a message twice is a no-op, so these are safe beside the mirror's and commands' own.
            .add_message::<SyncItem>()
            .add_message::<CommandDone>()
            .add_message::<Submit>()
            .add_message::<Refetch>()
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
            .init_resource::<ModalUi>()
            .init_resource::<Compose>()
            .init_resource::<InputFocus>()
            .add_observer(on_modal_closed)
            .add_observer(on_open_task)
            .add_observer(on_open_project)
            .add_observer(on_open_logs)
            .add_observer(on_open_composer)
            .add_observer(on_compose_press)
            .add_observer(on_step)
            .add_observer(on_step_key)
            .add_observer(on_menu_item)
            .add_observer(on_inline_edited)
            .add_observer(on_inline_chosen)
            .add_systems(Startup, spawn_panel)
            .add_systems(Update, (answer_edits, tidy_controls.after(settle).before(sync_panel)))
            // After the view is rebuilt: a modal closed from inside must not be reopened from the view it just left.
            .add_systems(
                Update,
                (follow_compose, sync_modal, sync_panel, sync_tools, dress, gate_send, focus_composer, clear_sent)
                    .chain()
                    .after(rebuild),
            );
    }
}
