//! Approving a spec task's plan on its graph (`docs/app-platform.md`, "Planning an ask", Approval): each task the
//! planner made, open and ungraded, shows a grade dropdown preset to the grade it proposes, with why in a hint, and
//! Approve in the page's header sends the grades the person chose for exactly those tasks.
//!
//! The choices are view state ([`GradeChoices`]), kept while the graph updates in place and dropped for a task that
//! leaves the plan. [`approval`] is the pure function saying what Approve would send, or why it can't yet.

use std::collections::{BTreeMap, HashMap};

use bevy::prelude::*;
use bevy::ui::InteractionDisabled;
use bevy::ui_widgets::Activate;
use trailhead_client::http::Call;
use trailhead_model::{Grade, Status, TaskKey};
use trailhead_shell::commands::{CommandDone, Request, Submit};
use trailhead_shell::connection::{Connection, ConnectionStatus};

use super::{GraphPage, GraphView, PlanNode};
use crate::author::{grade_options, grade_tones};
use crate::inspector::describe;
use crate::widgets::{
    ButtonKind, Hint, Icon, Notice, Picked, SelectEdit, SelectHint, Tone, button, notice, row, select_toned,
    set_display, set_text, size, text_dim,
};

/// The grade the person chose for each task in the plan, by id, until the plan is approved.
#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct GradeChoices(pub HashMap<String, Grade>);

/// The grade a node's dropdown shows: the person's choice, else the planner's proposal.
pub fn chosen(node: &PlanNode, choices: &GradeChoices) -> Option<Grade> {
    choices.0.get(&node.card.id).copied().or(node.proposed.as_ref().map(|p| p.grade))
}

/// What Approve sends for the plan in `view`, the grade for each task to grade by key, or why it can't be pressed;
/// none when there is no plan to approve (not a spec task, or approved already), and Approve isn't shown.
pub fn approval(view: &GraphView, choices: &GradeChoices) -> Option<Result<BTreeMap<String, Grade>, String>> {
    let plan = view.plan.as_ref().filter(|p| !p.approved)?;
    let refusal = match plan.status {
        Status::Open | Status::NeedsInput => None,
        Status::InProgress => Some("The planner is working; approve once it has answered.".to_string()),
        status => Some(format!("{}: its plan can't be approved.", crate::widgets::status_label(status))),
    };
    if let Some(why) = refusal {
        return Some(Err(why));
    }
    let planned: Vec<&PlanNode> = view.nodes.iter().filter(|n| n.to_grade).collect();
    if planned.is_empty() {
        return Some(Err("No tasks to approve yet.".into()));
    }
    let missing: Vec<&str> =
        planned.iter().filter(|n| chosen(n, choices).is_none()).map(|n| n.card.key.as_str()).collect();
    if !missing.is_empty() {
        return Some(Err(format!("Choose a grade for {}.", missing.join(", "))));
    }
    Some(Ok(planned.iter().filter_map(|n| Some((n.card.key.clone(), chosen(n, choices)?))).collect()))
}

/// The request Approve sends for the spec task `key`.
pub fn approve_request(key: &str, grades: BTreeMap<String, Grade>) -> Request {
    Request::new(Call::ApprovePlan { key: TaskKey(key.into()), grades }, None)
}

/// The grade dropdown's rows: each grade and what it is like, as the inspector's grade dropdown says them.
fn options() -> (Vec<String>, Vec<Option<crate::widgets::Token>>) {
    (grade_options(&[]).split_off(1), grade_tones().split_off(1))
}

/// A node's hint on its dropdown: the planner's proposal and why.
fn why(node: &PlanNode) -> String {
    match &node.proposed {
        Some(p) if !p.why.trim().is_empty() => format!("The planner proposes grade {}: {}", p.grade, p.why.trim()),
        Some(p) => format!("The planner proposes grade {}", p.grade),
        None => "The planner proposes no grade: choose one".into(),
    }
}

/// The row of controls at the foot of a node's card, for the task (an id) it shows.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct NodeBar(pub String);

/// A node's grade dropdown, for the task (an id).
#[derive(Component, Debug, Clone, PartialEq, Eq, Default)]
pub struct GradePick(pub String);

/// The header's Approve button, which the answer goes back to.
#[derive(Component)]
pub struct ApproveButton;

/// Why Approve can't be pressed, beside it.
#[derive(Component)]
pub(super) struct ApproveWhy;

/// How the last approval went, beside Approve.
#[derive(Component)]
pub(super) struct ApproveNote;

/// An approval in flight, which can't be sent twice.
#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct Approving(pub bool);

/// Spawns a node's bar: its grade dropdown, hidden until the node is one to grade.
pub(super) fn spawn_bar(commands: &mut Commands, card: Entity, id: &str) -> Entity {
    let bar = commands.spawn_scene(row()).insert((NodeBar(id.to_string()), ChildOf(card))).id();
    let (names, tones) = options();
    let id = id.to_string();
    let pick = bsn! {
        @select_toned(names, tones, None, 4)
        Node { display: Display::None, flex_grow: 1.0, min_width: { px(0) } }
        SelectHint({ "Grade…".to_string() })
        GradePick({ id.clone() })
    };
    commands.spawn_scene(pick).insert(ChildOf(bar));
    bar
}

/// Puts Approve, and why it can't be pressed, before the header's Fit.
pub(super) fn spawn_approve(commands: &mut Commands) -> [Entity; 3] {
    let note = commands.spawn_scene(notice()).insert(ApproveNote).id();
    let why = commands.spawn_scene(text_dim("", size::SECONDARY)).insert(ApproveWhy).id();
    let approve = commands
        .spawn_scene(
            bsn! { @button("Approve", Some(Icon::Check), ButtonKind::Primary) Node { display: Display::None } },
        )
        .insert((ApproveButton, InteractionDisabled))
        .id();
    [note, why, approve]
}

/// A pick on a node's dropdown is the person's grade for its task.
pub(super) fn on_pick(pick: On<Picked>, picks: Query<&GradePick>, mut choices: ResMut<GradeChoices>) {
    let Ok(GradePick(id)) = picks.get(pick.entity) else { return };
    if let Some(&grade) = Grade::ALL.get(pick.index) {
        choices.0.insert(id.clone(), grade);
    }
}

/// Another page starts with no choices; a task that left the plan takes its choice with it.
pub(super) fn tidy_choices(page: Res<GraphPage>, view: Res<GraphView>, mut choices: ResMut<GradeChoices>) {
    if page.is_changed() && !choices.0.is_empty() {
        choices.0.clear();
    }
    if view.is_changed() {
        let keep = |id: &String| view.nodes.iter().any(|n| n.to_grade && n.card.id == *id);
        if choices.0.keys().any(|id| !keep(id)) {
            choices.0.retain(|id, _| keep(id));
        }
    }
}

/// Keeps each node's dropdown in step: shown on a task to grade, set to its chosen grade, with the planner's why.
#[allow(clippy::too_many_arguments)]
pub(super) fn sync_picks(
    view: Res<GraphView>,
    choices: Res<GradeChoices>,
    picks: Query<(Entity, &GradePick, Option<&Hint>)>,
    added: Query<(), Added<GradePick>>,
    mut nodes: Query<&mut Node>,
    mut edit: SelectEdit,
    mut again: Local<bool>,
    mut commands: Commands,
) {
    if !(view.is_changed() || choices.is_changed() || !added.is_empty() || *again) {
        return;
    }
    *again = false;
    let (names, tones) = options();
    for (pick, GradePick(id), hint) in &picks {
        let Some(node) = view.nodes.iter().find(|n| n.card.id == *id) else { continue };
        set_display(&mut nodes, pick, node.to_grade);
        if !node.to_grade {
            continue;
        }
        let at = chosen(node, &choices).and_then(|g| Grade::ALL.iter().position(|a| *a == g));
        if !edit.set_toned(pick, &names, &tones, at) {
            *again = true;
        }
        let words = why(node);
        if hint.is_none_or(|h| h.0 != words) {
            commands.entity(pick).insert(Hint(words));
        }
    }
}

/// Shows Approve while there is a plan to approve, enabled when it can be sent, with why not beside it.
#[allow(clippy::too_many_arguments)]
pub(super) fn gate_approve(
    view: Res<GraphView>,
    choices: Res<GradeChoices>,
    connection: Res<State<Connection>>,
    status: Option<Res<ConnectionStatus>>,
    approving: Res<Approving>,
    buttons: Query<(Entity, Has<InteractionDisabled>), With<ApproveButton>>,
    whys: Query<Entity, With<ApproveWhy>>,
    mut texts: Query<&mut Text>,
    mut nodes: Query<&mut Node>,
    mut commands: Commands,
) {
    let wanted = approval(&view, &choices);
    let live = *connection.get() == Connection::Live;
    let why = match &wanted {
        None => String::new(),
        Some(Err(why)) => why.clone(),
        Some(Ok(_)) if !live => {
            status.and_then(|s| s.reason.as_ref().map(ToString::to_string)).unwrap_or_else(|| "Not connected.".into())
        }
        Some(Ok(_)) if approving.0 => "Approving…".into(),
        Some(Ok(_)) => String::new(),
    };
    let on = matches!(wanted, Some(Ok(_))) && why.is_empty();
    for (button, off) in &buttons {
        set_display(&mut nodes, button, wanted.is_some());
        if off == on {
            if on {
                commands.entity(button).remove::<InteractionDisabled>();
            } else {
                commands.entity(button).insert(InteractionDisabled);
            }
        }
    }
    for line in &whys {
        set_display(&mut nodes, line, !why.is_empty());
        if let Ok(text) = texts.get_mut(line) {
            set_text(text, &why);
        }
    }
}

/// Approve was pressed: send the grades.
pub(super) fn on_approve(
    activate: On<Activate>,
    buttons: Query<(), (With<ApproveButton>, Without<InteractionDisabled>)>,
    view: Res<GraphView>,
    choices: Res<GradeChoices>,
    mut approving: ResMut<Approving>,
    mut submit: MessageWriter<Submit>,
) {
    if !buttons.contains(activate.entity) || approving.0 {
        return;
    }
    let (Some(plan), Some(Ok(grades))) = (&view.plan, approval(&view, &choices)) else { return };
    submit.write(Submit { source: activate.entity, request: approve_request(&plan.key, grades) });
    approving.0 = true;
}

/// Approve's answer: on `Ok` the stream brings the graded tasks and the approved plan, so the dropdowns and Approve
/// go; anything else is said beside Approve.
pub(super) fn finish_approve(
    mut done: MessageReader<CommandDone>,
    buttons: Query<(), With<ApproveButton>>,
    notes: Query<Entity, With<ApproveNote>>,
    mut approving: ResMut<Approving>,
    mut commands: Commands,
) {
    for d in done.read() {
        if !buttons.contains(d.source) {
            continue;
        }
        approving.0 = false;
        let says = match describe(&d.outcome) {
            None => Notice::default(),
            Some(why) => Notice::new(Tone::Error, format!("Not approved: {why}")),
        };
        for note in &notes {
            commands.entity(note).insert(says.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use bevy::state::app::StatesPlugin;
    use serde_json::{Value, json};
    use trailhead_model::Task;
    use trailhead_shell::fixture::{sid, sync_event, task_json};
    use trailhead_shell::mirror::MirrorPlugin;
    use trailhead_shell::sync::{Snapshot, SyncItem};

    use super::*;
    use crate::board::{BoardCard, Input};
    use crate::graph::{GraphUiPlugin, OpenGraph, Thread, build_graph};
    use crate::inspector::Selection;

    fn spec(status: &str, approved: bool) -> Value {
        let spec = if approved { json!({"approved": 1_790_000_000}) } else { json!({}) };
        task_json(
            json!({"id": "s", "key": "T-1", "title": "plan", "status": status, "metadata": {"plan": {"spec": spec}}}),
        )
    }

    fn part(id: &str, key: &str, status: &str, grade: Option<u8>, proposed: Option<u8>) -> Value {
        let mut t = task_json(json!({"id": id, "key": key, "title": key, "status": status, "parent_id": "s",
                                     "grade": grade}));
        if let Some(g) = proposed {
            t["metadata"] = json!({"plan": {"proposed": {"grade": g, "why": "one crate"}}});
        }
        t
    }

    fn view(tasks: Vec<Value>) -> GraphView {
        let tasks: Vec<Task> = tasks.into_iter().map(|t| serde_json::from_value(t).unwrap()).collect();
        build_graph(Input { tasks: &tasks, ..Input::default() }, &sid("s"), Thread::default())
    }

    fn grade(n: u8) -> Grade {
        Grade::new(n).unwrap()
    }

    #[test]
    fn only_open_ungraded_tasks_of_a_plan_not_yet_approved_get_a_dropdown_preset_to_the_proposal() {
        let v = view(vec![
            spec("needs_input", false),
            part("a", "T-2", "open", None, Some(2)),
            part("b", "T-3", "open", Some(1), None),
            part("c", "T-4", "in_progress", None, None),
            part("d", "T-5", "open", None, None),
        ]);
        let to_grade: Vec<&str> = v.nodes.iter().filter(|n| n.to_grade).map(|n| n.card.key.as_str()).collect();
        assert_eq!(to_grade, ["T-2", "T-5"]);
        let none = GradeChoices::default();
        assert_eq!(chosen(&v.nodes[0], &none), Some(grade(2)), "the planner's proposal");
        assert_eq!(chosen(&v.nodes[3], &none), None, "no proposal starts unset");
        assert!(v.nodes[0].size.y > v.nodes[1].size.y, "room for the dropdown");
        assert_eq!(why(&v.nodes[0]), "The planner proposes grade 2: one crate");
        let plain = view(vec![task_json(json!({"id": "s", "key": "T-1"})), part("a", "T-2", "open", None, Some(2))]);
        assert!(!plain.nodes[0].to_grade, "a parent that is no spec task has no plan to approve");
        assert_eq!(approval(&plain, &none), None);
    }

    #[test]
    fn approve_sends_a_grade_for_every_task_to_grade_and_says_why_it_cannot() {
        let tasks = vec![
            spec("needs_input", false),
            part("a", "T-2", "open", None, Some(2)),
            part("b", "T-3", "open", Some(1), None),
            part("d", "T-5", "open", None, None),
        ];
        let v = view(tasks.clone());
        let mut choices = GradeChoices::default();
        assert_eq!(approval(&v, &choices), Some(Err("Choose a grade for T-5.".into())));
        choices.0.insert(sid("d"), grade(3));
        choices.0.insert(sid("a"), grade(1));
        let sent = BTreeMap::from([("T-2".to_string(), grade(1)), ("T-5".to_string(), grade(3))]);
        assert_eq!(approval(&v, &choices), Some(Ok(sent.clone())), "the person's choice over the proposal");
        let r = approve_request("T-1", sent);
        assert_eq!(r.call.path(), "/v1/tasks/T-1/plan/approve");
        assert_eq!(r.call.body(), Some(json!({"grades": {"T-2": 1, "T-5": 3}})));

        let mut working = tasks.clone();
        working[0] = spec("in_progress", false);
        assert!(approval(&view(working), &choices).unwrap().unwrap_err().contains("working"));
        assert_eq!(approval(&view(vec![spec("open", false)]), &choices), Some(Err("No tasks to approve yet.".into())));
        let mut approved = tasks;
        approved[0] = spec("open", true);
        assert_eq!(approval(&view(approved), &choices), None, "approved: no Approve");
    }

    /// The graph page drawn headless on the spec task `s`, its plan two tasks to grade.
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
            .add_plugins((MirrorPlugin, crate::board::BoardUiPlugin, GraphUiPlugin, crate::widgets::CanvasPlugin));
        app.insert_state(trailhead_shell::ActiveApp(crate::LOOKOUT));
        let tasks = vec![
            spec("needs_input", false),
            part("a", "T-2", "open", None, Some(2)),
            part("d", "T-5", "open", None, None),
        ];
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot { at: 1, tasks, ..Default::default() }));
        app.world_mut().resource_mut::<NextState<Connection>>().set(Connection::Live);
        app.world_mut().trigger(OpenGraph(sid("s")));
        settle(&mut app);
        app
    }

    fn settle(app: &mut App) {
        for _ in 0..6 {
            app.update();
        }
    }

    fn picks(app: &mut App) -> Vec<(String, Entity, bool)> {
        let world = app.world_mut();
        let mut found: Vec<_> = world
            .query::<(Entity, &GradePick, &Node)>()
            .iter(world)
            .map(|(e, p, n)| (p.0.clone(), e, n.display != Display::None))
            .collect();
        found.sort();
        found
    }

    fn approve_button(app: &mut App) -> Entity {
        app.world_mut().query_filtered::<Entity, With<ApproveButton>>().single(app.world()).unwrap()
    }

    fn everything(app: &mut App) -> Vec<Entity> {
        let mut all: Vec<Entity> = app.world_mut().query::<Entity>().iter(app.world()).collect();
        all.sort();
        all
    }

    #[test]
    fn grades_chosen_on_the_nodes_survive_an_update_and_approve_sends_them() {
        let mut app = app();
        let shown = picks(&mut app);
        assert_eq!(shown.len(), 2, "a dropdown on each node");
        assert!(shown.iter().all(|(_, _, visible)| *visible));
        let approve = approve_button(&mut app);
        assert!(app.world().get::<InteractionDisabled>(approve).is_some(), "T-5 has no grade yet");
        let unset = shown.iter().find(|(id, ..)| *id == sid("d")).unwrap().1;
        app.world_mut().trigger(Picked { entity: unset, index: 2 });
        settle(&mut app);
        assert!(app.world().get::<InteractionDisabled>(approve).is_none());
        // Another task's change lands: nothing respawned, the choice held.
        let before = everything(&mut app);
        let mut retitled = part("a", "T-2", "open", None, Some(2));
        retitled["title"] = json!("T-2, retitled");
        retitled["version"] = json!(2);
        app.world_mut().write_message(sync_event(
            json!({"id": 30, "entity": "task", "entity_id": sid("a"), "kind": "task.updated", "after": retitled}),
        ));
        settle(&mut app);
        assert_eq!(everything(&mut app), before);
        assert_eq!(app.world().resource::<GradeChoices>().0.get(&sid("d")), Some(&grade(3)));

        app.world_mut().trigger(Activate { entity: approve });
        let sent: Vec<Submit> =
            app.world_mut().resource_mut::<bevy::ecs::message::Messages<Submit>>().drain().collect();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].request.call.body(), Some(json!({"grades": {"T-2": 2, "T-5": 3}})));

        // Approved: the tasks graded and the plan marked; the dropdowns and Approve go.
        let mut graded = |n: i64, after: Value| {
            let id = after["id"].clone();
            app.world_mut().write_message(sync_event(
                json!({"id": n, "entity": "task", "entity_id": id, "kind": "task.updated", "after": after}),
            ));
        };
        let mut a = part("a", "T-2", "open", Some(2), Some(2));
        a["version"] = json!(3);
        a["title"] = json!("T-2, retitled");
        let mut d = part("d", "T-5", "open", Some(3), None);
        d["version"] = json!(2);
        let mut s = spec("open", true);
        s["version"] = json!(2);
        graded(31, a);
        graded(32, d);
        graded(33, s);
        settle(&mut app);
        assert!(picks(&mut app).iter().all(|(_, _, visible)| !*visible));
        assert_eq!(app.world().get::<Node>(approve).unwrap().display, Display::None);
        let cards = app.world_mut().query::<&BoardCard>().iter(app.world()).count();
        assert_eq!(cards, 2);
    }
}
