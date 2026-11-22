//! Changing a plan by hand on the graph of any parent task (`docs/app-platform.md`, "The graph"): add a task under
//! the parent, and on each node, from its ⋯ menu, make it depend on another task of the plan, drop one of those
//! dependencies, split it in two, or remove it (cancel it).
//!
//! Each is one ordinary write (create a task with its parent and dependencies, add or remove a blocker, cancel), so
//! undo covers it, and the mirror brings the change back to the graph, which updates in place and keeps its pan and
//! zoom. The pure parts are which tasks a node can depend on without a cycle ([`dependable`]) and the request each
//! change sends. A change that needs a choice or a confirmation opens a small modal ([`PlanEditModal`]).

use bevy::prelude::*;
use bevy::text::EditableText;
use bevy::ui_widgets::Activate;
use trailhead_client::http::{Call, NewTask, TaskUpdate};
use trailhead_model::{Status, TaskKey};
use trailhead_shell::commands::{CommandDone, Outcome, Request, Submit};
use trailhead_shell::connection::Connection;

use super::{GraphView, PlanNode};
use crate::form::{FormState, lock_while_saving, on_modal_closed, update_message};
use crate::inspector::describe;
use crate::widgets::{
    ButtonKind, Icon, Modal, ModalFocus, ModalSlots, Notice, SelectEdit, Tone, button, field, menu_item, notice,
    open_modal, select, set_display, size, spawn_icon_menu, text, text_dim, text_input,
};

/// The tasks of the plan `id` (a node) can be made to depend on: every other node it doesn't wait on yet and that
/// doesn't already wait on it, however indirectly, which would make a cycle.
pub fn dependable<'a>(view: &'a GraphView, id: &str) -> Vec<&'a PlanNode> {
    let mut after = vec![id.to_string()];
    let mut at = 0;
    while at < after.len() {
        let from = after[at].clone();
        for (_, to) in view.edges.iter().filter(|(blocker, _)| *blocker == from) {
            if !after.contains(to) {
                after.push(to.clone());
            }
        }
        at += 1;
    }
    let waits_on = |other: &str| view.edges.iter().any(|(b, d)| b == other && d == id);
    view.nodes.iter().filter(|n| !after.contains(&n.card.id) && !waits_on(&n.card.id)).collect()
}

/// The tasks of the plan `id` waits on.
pub fn plan_blockers<'a>(view: &'a GraphView, id: &str) -> Vec<&'a PlanNode> {
    let waits: Vec<&String> = view.edges.iter().filter(|(_, d)| d == id).map(|(b, _)| b).collect();
    view.nodes.iter().filter(|n| waits.contains(&&n.card.id)).collect()
}

/// A new, ungraded task under the graph's parent.
pub fn add_request(view: &GraphView, title: &str, description: &str) -> Result<Request, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("Give it a title.".into());
    }
    let (Some(project), Some((parent, _))) = (&view.project, &view.parent) else {
        return Err("The parent task isn't loaded yet.".into());
    };
    let new = NewTask {
        project: project.clone(),
        title: title.to_string(),
        description: description.trim().to_string(),
        parent: Some(parent.clone()),
        ..NewTask::default()
    };
    Ok(Request::new(Call::CreateTask(new), None))
}

/// A second task beside `node`: the same parent, waiting on what it waits on and waited on by what waits on it,
/// titled "<title> (2)" with its description, for the person to divide the work between the two.
pub fn split_request(view: &GraphView, node: &PlanNode) -> Result<Request, String> {
    let (Some(project), Some((parent, _))) = (&view.project, &view.parent) else {
        return Err("The parent task isn't loaded yet.".into());
    };
    let new = NewTask {
        project: project.clone(),
        title: format!("{} (2)", node.card.title),
        description: node.description.clone(),
        parent: Some(parent.clone()),
        blocked_by: node.blocked_by.clone(),
        blocks: node.blocks.clone(),
        ..NewTask::default()
    };
    Ok(Request::new(Call::CreateTask(new), None))
}

/// `blocked` waits on `blocker` from now on.
pub fn depend_request(blocked: &str, blocker: &str) -> Request {
    Request::new(Call::AddBlocker { blocked: TaskKey(blocked.into()), blocker: TaskKey(blocker.into()) }, None)
}

/// `blocked` no longer waits on `blocker`.
pub fn undepend_request(blocked: &str, blocker: &str) -> Request {
    Request::new(Call::RemoveBlocker { blocked: TaskKey(blocked.into()), blocker: TaskKey(blocker.into()) }, None)
}

/// Cancels `node`, at the version shown.
pub fn remove_request(node: &PlanNode) -> Request {
    let update = TaskUpdate { status: Some(Status::Cancelled), ..TaskUpdate::default() };
    Request::new(Call::UpdateTask { key: TaskKey(node.card.key.clone()), update }, Some(node.version))
}

/// What a node's menu offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeEdit {
    DependsOn,
    NoLongerDependsOn,
    Split,
    Remove,
}

impl NodeEdit {
    const ALL: [NodeEdit; 4] = [NodeEdit::DependsOn, NodeEdit::NoLongerDependsOn, NodeEdit::Split, NodeEdit::Remove];

    fn look(self) -> (&'static str, Icon, ButtonKind) {
        match self {
            NodeEdit::DependsOn => ("Depends on…", Icon::Plus, ButtonKind::Normal),
            NodeEdit::NoLongerDependsOn => ("No longer depends on…", Icon::X, ButtonKind::Normal),
            NodeEdit::Split => ("Split", Icon::Copy, ButtonKind::Normal),
            NodeEdit::Remove => ("Remove", Icon::Stop, ButtonKind::Danger),
        }
    }

    /// Whether `node` offers it.
    pub fn offered(self, view: &GraphView, node: &PlanNode) -> bool {
        match self {
            NodeEdit::DependsOn => !dependable(view, &node.card.id).is_empty(),
            NodeEdit::NoLongerDependsOn => !plan_blockers(view, &node.card.id).is_empty(),
            NodeEdit::Split => true,
            NodeEdit::Remove => !node.status.is_closed(),
        }
    }
}

/// An item of a node's menu: what it does, to the task (an id).
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct NodeAct {
    pub id: String,
    pub edit: NodeEdit,
}

/// The header's button that adds a task to the plan.
#[derive(Component)]
pub struct AddToPlan;

/// The change a [`PlanEditModal`] makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanEdit {
    Add,
    /// Make the task (an id) depend on one of the others, chosen in the modal.
    DependsOn(String),
    /// Drop one of the task's dependencies, chosen in the modal.
    NoLongerDependsOn(String),
    /// Cancel the task, once confirmed.
    Remove(String),
}

/// The plan edit being made, its save in flight and what it says.
pub type PlanEditing = FormState<PlanEdit>;

/// The modal of a change to the plan that needs a choice or a confirmation.
#[derive(Component)]
pub struct PlanEditModal(String);

impl Modal for PlanEditModal {
    fn title(&self) -> String {
        self.0.clone()
    }
}

/// A text field of the add form.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EditField {
    #[default]
    Title,
    Description,
}

/// The modal's dropdown of tasks.
#[derive(Component, Clone, Default)]
pub struct EditPick;

/// The modal's button that makes the change.
#[derive(Component, Clone, Default)]
pub struct EditSave;

/// The modal's line saying how the change went.
#[derive(Component, Clone, Default)]
pub struct EditMessage;

/// The header's line saying how a split went.
#[derive(Component)]
pub(super) struct EditNote;

/// Spawns a node's ⋯ menu at the end of its bar, every item it may offer in it.
pub(super) fn spawn_menu(commands: &mut Commands, bar: Entity, id: &str) {
    let menu = spawn_icon_menu(commands, Icon::Ellipsis, "Change the plan");
    commands.entity(bar).add_child(menu.root);
    for edit in NodeEdit::ALL {
        let (caption, icon, kind) = edit.look();
        commands
            .spawn_scene(menu_item(caption, Some(icon), kind))
            .insert((NodeAct { id: id.to_string(), edit }, ChildOf(menu.popup)));
    }
}

/// The header's Add, and the line saying how a split went.
pub(super) fn spawn_add(commands: &mut Commands) -> [Entity; 2] {
    let note = commands.spawn_scene(notice()).insert(EditNote).id();
    let add = commands
        .spawn_scene(crate::widgets::tool_button(Icon::Plus, "Add a task to this plan", ButtonKind::Plain))
        .insert(AddToPlan)
        .id();
    [note, add]
}

/// Shows each menu item only where it applies.
pub(super) fn sync_menus(view: Res<GraphView>, items: Query<(Entity, &NodeAct)>, mut nodes: Query<&mut Node>) {
    for (item, act) in &items {
        let offered = view.node(&act.id).is_some_and(|n| act.edit.offered(&view, n));
        set_display(&mut nodes, item, offered);
    }
}

fn open(form: &mut PlanEditing, commands: &mut Commands, edit: PlanEdit, title: String) {
    form.open(edit);
    open_modal(commands, PlanEditModal(title));
}

/// The header's Add opens the add form.
pub(super) fn on_add(
    activate: On<Activate>,
    adds: Query<(), With<AddToPlan>>,
    mut form: ResMut<PlanEditing>,
    mut commands: Commands,
) {
    if adds.contains(activate.entity) {
        open(&mut form, &mut commands, PlanEdit::Add, "Add a task to the plan".into());
    }
}

/// A node's menu item: a split is sent at once, the rest open their modal.
#[allow(clippy::too_many_arguments)]
pub(super) fn on_node_act(
    activate: On<Activate>,
    acts: Query<&NodeAct>,
    view: Res<GraphView>,
    connection: Res<State<Connection>>,
    mut form: ResMut<PlanEditing>,
    notes: Query<Entity, With<EditNote>>,
    mut submit: MessageWriter<Submit>,
    mut commands: Commands,
) {
    let Ok(act) = acts.get(activate.entity) else { return };
    let Some(node) = view.node(&act.id) else { return };
    let key = node.card.key.clone();
    match act.edit {
        NodeEdit::Split => {
            let says = match split_request(&view, node) {
                Ok(_) if *connection.get() != Connection::Live => Some("Not connected: nothing was split.".into()),
                Ok(request) => {
                    submit.write(Submit { source: activate.entity, request });
                    None
                }
                Err(why) => Some(why),
            };
            if let Some(why) = says {
                for note in &notes {
                    commands.entity(note).insert(Notice::new(Tone::Error, why.clone()));
                }
            }
        }
        NodeEdit::DependsOn => {
            open(&mut form, &mut commands, PlanEdit::DependsOn(act.id.clone()), format!("{key} depends on…"))
        }
        NodeEdit::NoLongerDependsOn => open(
            &mut form,
            &mut commands,
            PlanEdit::NoLongerDependsOn(act.id.clone()),
            format!("{key} no longer depends on…"),
        ),
        NodeEdit::Remove => open(&mut form, &mut commands, PlanEdit::Remove(act.id.clone()), format!("Remove {key}?")),
    }
}

/// The tasks a modal's dropdown offers for `edit`.
fn choices<'a>(view: &'a GraphView, edit: &PlanEdit) -> Vec<&'a PlanNode> {
    match edit {
        PlanEdit::DependsOn(id) => dependable(view, id),
        PlanEdit::NoLongerDependsOn(id) => plan_blockers(view, id),
        PlanEdit::Add | PlanEdit::Remove(_) => Vec::new(),
    }
}

/// Fills a new modal: the fields or the dropdown or the question, then the message and the button.
pub(super) fn fill_modal(
    new: Query<&ModalSlots, (With<PlanEditModal>, Added<ModalSlots>)>,
    form: Res<PlanEditing>,
    view: Res<GraphView>,
    mut commands: Commands,
) {
    let Some(edit) = &form.open else { return };
    for slots in &new {
        let body = slots.body;
        let save = match edit {
            PlanEdit::Add => {
                let title = bsn_list! { @text_input(1.0, bsn! { EditField::Title ModalFocus }) };
                commands.spawn_scene(field("Title", title)).insert(ChildOf(body));
                let description = bsn_list! { @text_input(4.0, bsn! { EditField::Description }) };
                commands.spawn_scene(field("Description (optional)", description)).insert(ChildOf(body));
                ("Add", ButtonKind::Primary)
            }
            PlanEdit::DependsOn(_) | PlanEdit::NoLongerDependsOn(_) => {
                let names: Vec<String> =
                    choices(&view, edit).iter().map(|n| format!("{} · {}", n.card.key, n.card.title)).collect();
                let label =
                    if matches!(edit, PlanEdit::DependsOn(_)) { "It waits on" } else { "It no longer waits on" };
                let pick = bsn_list! { @select(names, Some(0), 8) EditPick };
                commands.spawn_scene(field(label, pick)).insert(ChildOf(body));
                ("Save", ButtonKind::Primary)
            }
            PlanEdit::Remove(id) => {
                let title = view.node(id).map(|n| n.card.title.clone()).unwrap_or_default();
                let words = format!("Cancel “{title}”? It leaves the plan; Undo brings it back.");
                commands.spawn_scene(text(words, size::BODY)).insert(ChildOf(body));
                ("Remove", ButtonKind::Danger)
            }
        };
        let (caption, kind) = save;
        commands.spawn_scene(bsn! { @button(caption, None, kind) EditSave }).insert(ChildOf(slots.footer));
        commands
            .spawn_scene(text_dim(form.message.clone(), size::SECONDARY))
            .insert((EditMessage, ChildOf(slots.footer)));
    }
}

/// The modal's button: the request for its change, from what is typed or chosen.
#[allow(clippy::too_many_arguments)]
pub(super) fn on_save(
    activate: On<Activate>,
    saves: Query<(), With<EditSave>>,
    fields: Query<(&EditField, &EditableText)>,
    picks: Query<Entity, With<EditPick>>,
    edit: SelectEdit,
    view: Res<GraphView>,
    connection: Res<State<Connection>>,
    mut form: ResMut<PlanEditing>,
    mut submit: MessageWriter<Submit>,
) {
    if !saves.contains(activate.entity) {
        return;
    }
    let Some(open) = form.open.clone() else { return };
    let typed = |which: EditField| {
        fields.iter().find(|(f, _)| **f == which).map(|(_, t)| t.value().to_string()).unwrap_or_default()
    };
    let picked = picks.iter().next().and_then(|p| edit.selected_rows(p).first().copied());
    let node = |id: &str| view.node(id).ok_or_else(|| "That task has left the plan.".to_string());
    let request = match &open {
        PlanEdit::Add => add_request(&view, &typed(EditField::Title), &typed(EditField::Description)),
        PlanEdit::DependsOn(id) | PlanEdit::NoLongerDependsOn(id) => node(id).and_then(|n| {
            let other = picked.and_then(|at| choices(&view, &open).get(at).copied());
            let other = other.ok_or_else(|| "Choose a task.".to_string())?;
            Ok(match open {
                PlanEdit::DependsOn(_) => depend_request(&n.card.key, &other.card.key),
                _ => undepend_request(&n.card.key, &other.card.key),
            })
        }),
        PlanEdit::Remove(id) => node(id).map(remove_request),
    };
    let live = *connection.get() == Connection::Live;
    form.send(request, "the change", live, Some(activate.entity), &mut submit);
}

/// The answer to a change: the modal closes on `Ok` and says why on anything else; a split says so in the header.
pub(super) fn finish_edit(
    mut done: MessageReader<CommandDone>,
    saves: Query<(), With<EditSave>>,
    acts: Query<(), With<NodeAct>>,
    modals: Query<Entity, With<PlanEditModal>>,
    notes: Query<Entity, With<EditNote>>,
    mut form: ResMut<PlanEditing>,
    mut commands: Commands,
) {
    for d in done.read() {
        if saves.contains(d.source) {
            if matches!(d.outcome, Outcome::Ok { .. }) {
                form.close();
                for modal in &modals {
                    commands.entity(modal).despawn();
                }
            } else {
                form.refuse(describe(&d.outcome).unwrap_or_default());
            }
        } else if acts.contains(d.source) {
            let says = match describe(&d.outcome) {
                None => Notice::default(),
                Some(why) => Notice::new(Tone::Error, format!("Not split: {why}")),
            };
            for note in &notes {
                commands.entity(note).insert(says.clone());
            }
        }
    }
}

/// The form's systems: its message, its lock while saving, and closing with its modal.
pub(super) fn add_edit(app: &mut App) {
    app.init_resource::<PlanEditing>()
        .add_observer(on_add)
        .add_observer(on_node_act)
        .add_observer(on_save)
        .add_observer(on_modal_closed::<PlanEdit, PlanEditModal>)
        .add_systems(
            Update,
            (
                fill_modal,
                finish_edit,
                lock_while_saving::<PlanEdit, PlanEditModal>,
                update_message::<PlanEdit, EditMessage>,
            ),
        );
}

#[cfg(test)]
mod tests {
    use bevy::state::app::StatesPlugin;
    use serde_json::{Value, json};
    use trailhead_model::{Dependency, Project, Task};
    use trailhead_shell::commands::Method;
    use trailhead_shell::connection::ConnectionStatus;
    use trailhead_shell::fixture::{project, project_json, sid, sync_event, task_json};
    use trailhead_shell::mirror::MirrorPlugin;
    use trailhead_shell::sync::{Snapshot, SyncItem};
    use uuid::Uuid;

    use super::*;
    use crate::board::{BoardCard, Input};
    use crate::graph::{GraphUiPlugin, OpenGraph, Thread, build_graph};
    use crate::inspector::Selection;
    use crate::widgets::GraphNodeBox;

    fn part(id: &str, key: &str, blocked_by: &[&str]) -> Value {
        let mut t = task_json(json!({"id": id, "key": key, "title": format!("task {key}"), "parent_id": "p",
                                     "description": "the work", "version": 4}));
        t["blocked_by"] = json!(blocked_by.iter().map(|b| sid(b)).collect::<Vec<_>>());
        t
    }

    fn parent() -> Value {
        task_json(json!({"id": "p", "key": "T-1", "title": "an epic"}))
    }

    fn dep(blocker: &str, blocked: &str) -> Dependency {
        let id = |s: &str| Uuid::parse_str(&sid(s)).unwrap();
        Dependency {
            workspace_id: Uuid::nil(),
            blocker_id: id(blocker),
            blocked_id: id(blocked),
            created_at: "2026-11-01T00:00:00Z".parse().unwrap(),
        }
    }

    /// A chain a → b → c under `p`, a fourth task d on its own, and b also waiting on x outside the plan.
    fn view() -> GraphView {
        let tasks: Vec<Task> = [
            parent(),
            part("a", "T-2", &[]),
            part("b", "T-3", &[]),
            part("c", "T-4", &[]),
            part("d", "T-5", &[]),
            task_json(json!({"id": "x", "key": "T-9"})),
        ]
        .into_iter()
        .map(|t| serde_json::from_value(t).unwrap())
        .collect();
        let deps = [dep("a", "b"), dep("b", "c"), dep("x", "b")];
        let projects: Vec<Project> = vec![project(json!({"id": "p1", "key": "T"}))];
        let input = Input { projects: &projects, tasks: &tasks, dependencies: &deps, ..Input::default() };
        build_graph(input, &sid("p"), Thread::default())
    }

    fn keys(nodes: &[&PlanNode]) -> Vec<String> {
        nodes.iter().map(|n| n.card.key.clone()).collect()
    }

    #[test]
    fn a_task_can_depend_only_on_one_that_would_not_make_a_cycle_and_it_does_not_wait_on_yet() {
        let v = view();
        assert_eq!(keys(&dependable(&v, &sid("a"))), ["T-5"], "b and c wait on a already");
        assert_eq!(keys(&dependable(&v, &sid("b"))), ["T-5"], "a it waits on, c waits on it");
        assert_eq!(keys(&dependable(&v, &sid("c"))), ["T-2", "T-5"], "a only through b: a direct edge is no cycle");
        assert_eq!(keys(&dependable(&v, &sid("d"))), ["T-2", "T-3", "T-4"]);
        assert_eq!(keys(&plan_blockers(&v, &sid("b"))), ["T-2"], "x is outside the plan");
        let node = |id: &str| v.node(&sid(id)).unwrap();
        assert!(!NodeEdit::NoLongerDependsOn.offered(&v, node("a")));
        assert!(NodeEdit::NoLongerDependsOn.offered(&v, node("c")));
    }

    #[test]
    fn each_change_is_one_ordinary_write() {
        let v = view();
        let b = v.node(&sid("b")).unwrap();
        let split = split_request(&v, b).unwrap();
        assert_eq!((split.method(), split.call.path().as_str()), (Method::Post, "/v1/tasks"));
        assert_eq!(
            split.call.body().unwrap(),
            json!({"project": "T", "title": "task T-3 (2)", "description": "the work", "parent": "T-1",
                   "blocked_by": ["T-2", "T-9"], "blocks": ["T-4"]})
        );
        let add = add_request(&v, "  Shots  ", "").unwrap();
        assert_eq!(
            add.call.body().unwrap(),
            json!({"project": "T", "title": "Shots", "description": "", "parent": "T-1"})
        );
        assert_eq!(add_request(&v, " ", "x").unwrap_err(), "Give it a title.");
        let on = depend_request("T-5", "T-2");
        assert_eq!((on.method(), on.call.path().as_str()), (Method::Post, "/v1/tasks/T-5/blockers"));
        let off = undepend_request("T-3", "T-2");
        assert_eq!((off.method(), off.call.path().as_str()), (Method::Delete, "/v1/tasks/T-3/blockers/T-2"));
        let gone = remove_request(b);
        assert_eq!((gone.method(), gone.if_match), (Method::Patch, Some(4)));
        assert_eq!(gone.call.body().unwrap(), json!({"status": "cancelled"}));
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin, AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .init_state::<Connection>()
            .init_resource::<ConnectionStatus>()
            .init_resource::<Selection>()
            .add_message::<Submit>()
            .add_message::<CommandDone>()
            .add_plugins((MirrorPlugin, crate::board::BoardUiPlugin, GraphUiPlugin, crate::widgets::CanvasPlugin))
            .add_plugins(crate::widgets::ModalPlugin);
        app.insert_state(trailhead_shell::ActiveApp(crate::LOOKOUT));
        let projects = vec![project_json(json!({"id": "p1", "key": "T"}))];
        let tasks = vec![parent(), part("a", "T-2", &[]), part("b", "T-3", &["a"]), part("d", "T-5", &[])];
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot { at: 1, projects, tasks, ..Default::default() }));
        app.world_mut().resource_mut::<NextState<Connection>>().set(Connection::Live);
        app.world_mut().trigger(OpenGraph(sid("p")));
        settle(&mut app);
        app
    }

    fn settle(app: &mut App) {
        for _ in 0..6 {
            app.update();
        }
    }

    fn item(app: &mut App, id: &str, edit: NodeEdit) -> (Entity, bool) {
        let world = app.world_mut();
        let mut q = world.query::<(Entity, &NodeAct, &Node)>();
        let found = q.iter(world).find(|(_, a, _)| a.id == sid(id) && a.edit == edit).unwrap();
        (found.0, found.2.display != Display::None)
    }

    fn sent(app: &mut App) -> Vec<Submit> {
        app.world_mut().resource_mut::<bevy::ecs::message::Messages<Submit>>().drain().collect()
    }

    #[test]
    fn a_nodes_menu_offers_what_applies_and_a_new_dependency_lands_in_place() {
        let mut app = app();
        assert!(!item(&mut app, "a", NodeEdit::NoLongerDependsOn).1, "a waits on nothing");
        assert!(item(&mut app, "b", NodeEdit::NoLongerDependsOn).1);
        let (split, shown) = item(&mut app, "b", NodeEdit::Split);
        assert!(shown);
        app.world_mut().trigger(Activate { entity: split });
        app.world_mut().flush();
        let out = sent(&mut app);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].request.call.body().unwrap()["title"], "task T-3 (2)");

        let (depends, _) = item(&mut app, "d", NodeEdit::DependsOn);
        app.world_mut().trigger(Activate { entity: depends });
        settle(&mut app);
        assert!(app.world().resource::<PlanEditing>().open == Some(PlanEdit::DependsOn(sid("d"))));
        let save = app.world_mut().query_filtered::<Entity, With<EditSave>>().single(app.world()).unwrap();
        app.world_mut().trigger(Activate { entity: save });
        app.world_mut().flush();
        let out = sent(&mut app);
        assert_eq!(out[0].request.call.path(), "/v1/tasks/T-5/blockers");
        assert_eq!(out[0].request.call.body(), Some(json!({"blocker": "T-2"})));
        app.world_mut().write_message(CommandDone {
            source: save,
            id: trailhead_shell::commands::CommandId(1),
            outcome: Outcome::Ok { version: None },
        });
        settle(&mut app);
        assert_eq!(app.world_mut().query::<&PlanEditModal>().iter(app.world()).count(), 0, "done, so closed");

        // The new edge arrives: every node keeps its box, card and bar.
        let kept = |app: &mut App| {
            let world = app.world_mut();
            let mut q = world.query_filtered::<Entity, Or<(With<GraphNodeBox>, With<BoardCard>, With<NodeAct>)>>();
            let mut found: Vec<Entity> = q.iter(world).collect();
            found.sort();
            found
        };
        let before = kept(&mut app);
        let edge = json!({"workspace_id": sid("w"), "blocker_id": sid("a"), "blocked_id": sid("d"),
                          "created_at": "2026-11-01T00:00:00Z"});
        let id = format!("{}:{}", sid("a"), sid("d"));
        app.world_mut().write_message(sync_event(
            json!({"id": 40, "entity": "dependency", "entity_id": id, "kind": "dependency.added", "after": edge}),
        ));
        settle(&mut app);
        assert_eq!(kept(&mut app), before);
        let view = app.world().resource::<GraphView>();
        assert!(view.edges.contains(&(sid("a"), sid("d"))));
    }
}
