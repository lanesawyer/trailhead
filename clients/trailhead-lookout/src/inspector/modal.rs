//! The task modal: the inspector for a task, opened over the board and closed to clear the selection.

use super::*;

/// The task modal: the inspector for a task, in the middle of the window, laid out as a main column and a sidebar
/// of facts (`docs/factory/ui.md#views`). It is open while a task is selected; closing it clears the selection.
#[derive(Component, Default)]
pub struct TaskModal {
    pub(super) title: String,
}

impl Modal for TaskModal {
    fn title(&self) -> String {
        self.title.clone()
    }
    fn width(&self) -> f32 {
        extent::DETAIL_WIDTH
    }
    fn tall(&self) -> bool {
        true
    }
}

/// The open task modal and, once its body exists, its two columns.
#[derive(Resource, Default, Debug)]
pub(super) struct ModalUi {
    pub(super) root: Option<Entity>,
    pub(super) panes: Option<DetailPanes>,
    pub(super) tools: Option<Tools>,
}

/// The task modal's own header controls, before its X.
#[derive(Debug, Clone)]
pub(super) struct Tools {
    pub(super) previous: Entity,
    pub(super) next: Entity,
    /// The ⋯ menu's root, hidden while there is nothing it can do.
    pub(super) menu: Entity,
    /// Where the menu's last command stands.
    pub(super) note: Entity,
    pub(super) items: Vec<(Action, Entity)>,
}

/// On an arrow in the task modal's header.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    Previous,
    Next,
}

/// On an item of the task modal's ⋯ menu: what it does.
#[derive(Component, Debug, Clone)]
pub(super) struct MenuAct(pub(super) Action);

/// What the ⋯ menu can hold, in order; each shows while the task offers it.
pub(super) const MENU_ACTIONS: [Action; 7] = [
    Action::Release,
    Action::Cancel,
    Action::Undo,
    Action::Revert(1),
    Action::Revert(6),
    Action::Revert(24),
    Action::Revert(72),
];

/// A menu item's caption and icon, and whether it is destructive.
pub(super) fn menu_look(action: &Action) -> (String, Icon, ButtonKind) {
    match action {
        Action::Release => ("Release the claim".into(), Icon::Release, ButtonKind::Normal),
        Action::Cancel => ("Cancel the task".into(), Icon::CircleX, ButtonKind::Danger),
        Action::Undo => ("Undo the last change".into(), Icon::Undo, ButtonKind::Normal),
        Action::Revert(h) => (format!("Revert the holder's last {h}h"), Icon::History, ButtonKind::Danger),
        other => (other.label(), Icon::Circle, ButtonKind::Normal),
    }
}

/// Whether the task offers what a menu item does (a revert of any window).
pub(super) fn offers(view: &InspectorView, action: &Action) -> bool {
    match action {
        Action::Revert(_) => view.actions.iter().any(|a| matches!(a, Action::Revert(_))),
        other => view.actions.contains(other),
    }
}

/// Puts the arrows, the menu and its note at the start of the modal header's tools, before the X.
pub(super) fn spawn_tools(commands: &mut Commands, tools: Entity) -> Tools {
    let row =
        commands.spawn(Node { align_items: AlignItems::Center, column_gap: px(space::CONTROLS), ..default() }).id();
    let note = spawn_note(commands, row, &None);
    let arrow = |commands: &mut Commands, icon, hint: &str, step| {
        commands.spawn_scene(tool_button(icon, hint, ButtonKind::Plain)).insert((step, ChildOf(row))).id()
    };
    let previous = arrow(commands, Icon::ChevronUp, "Previous task in the column (K)", Step::Previous);
    let next = arrow(commands, Icon::ChevronDown, "Next task in the column (J)", Step::Next);
    let menu = spawn_icon_menu(commands, Icon::Ellipsis, "More actions");
    commands.entity(row).add_child(menu.root);
    let items = MENU_ACTIONS
        .iter()
        .map(|action| {
            let (caption, icon, kind) = menu_look(action);
            let item = commands
                .spawn_scene(menu_item(caption, Some(icon), kind))
                .insert((MenuAct(action.clone()), ChildOf(menu.popup)))
                .id();
            (action.clone(), item)
        })
        .collect();
    commands.entity(tools).insert_children(0, &[row]);
    Tools { previous, next, menu: menu.root, note, items }
}

/// Keeps the header's controls in line with the task: an arrow is off at its column's end, the menu shows only what
/// the task offers (and nothing while offline), and the note says where its last command stands.
#[allow(clippy::too_many_arguments)]
pub(super) fn sync_tools(
    state: Res<InspectorState>,
    connection: Res<State<Connection>>,
    neighbours: Res<Neighbours>,
    ui: Res<ModalUi>,
    controls: Res<Controls>,
    states: Query<&Control>,
    changed_controls: Query<(), Changed<Control>>,
    mut texts: Query<&mut Text>,
    mut nodes: Query<&mut Node>,
    mut commands: Commands,
) {
    if !(state.is_changed()
        || connection.is_changed()
        || neighbours.is_changed()
        || ui.is_changed()
        || !changed_controls.is_empty())
    {
        return;
    }
    let (Some(tools), Some(view)) = (&ui.tools, state.0.as_ref().filter(|v| v.task.is_some())) else { return };
    let live = *connection.get() == Connection::Live;
    gate_button(&mut commands, tools.previous, neighbours.previous.is_some());
    gate_button(&mut commands, tools.next, neighbours.next.is_some());
    let mut any = false;
    for (action, item) in &tools.items {
        let shown = live && offers(view, action);
        any |= shown;
        set_display(&mut nodes, *item, shown);
    }
    set_display(&mut nodes, tools.menu, any);
    let note = controls.0.get(&(view.subject.clone(), Action::Menu)).and_then(|e| states.get(*e).ok()).cloned();
    dress_note(&mut commands, &mut texts, &mut nodes, tools.note, &note);
}

pub(super) fn set_display(nodes: &mut Query<&mut Node>, entity: Entity, shown: bool) {
    let display = if shown { Display::Flex } else { Display::None };
    if let Ok(mut node) = nodes.get_mut(entity)
        && node.display != display
    {
        node.display = display;
    }
}

/// An arrow in the header was pressed: show the task before or after this one.
pub(super) fn on_step(
    activate: On<Activate>,
    steps: Query<&Step>,
    neighbours: Res<Neighbours>,
    selection: ResMut<Selection>,
) {
    if let Ok(step) = steps.get(activate.entity) {
        step_to(*step, &neighbours, selection);
    }
}

/// Up and K, down and J, step through the column while the task modal is open, unless a field has the keyboard or a
/// control in the modal took the key (a menu or a dropdown's list, which stop the arrows they use).
pub(super) fn on_step_key(
    key: On<FocusedInput<KeyboardInput>>,
    ui: Res<ModalUi>,
    windows: Query<(), With<Window>>,
    inputs: Query<(), With<EditableText>>,
    neighbours: Res<Neighbours>,
    selection: ResMut<Selection>,
) {
    let Some(root) = ui.root else { return };
    let (at, from) = (key.event_target(), key.original_event_target());
    // Once: at the modal's root, where what nothing in it took arrives, or at the window when nothing has the keyboard.
    if !(at == root || (at == from && windows.contains(at))) || inputs.contains(from) {
        return;
    }
    let pressed = &key.input;
    if !pressed.state.is_pressed() {
        return;
    }
    let step = match pressed.key_code {
        KeyCode::ArrowUp | KeyCode::KeyK => Step::Previous,
        KeyCode::ArrowDown | KeyCode::KeyJ => Step::Next,
        _ => return,
    };
    step_to(step, &neighbours, selection);
}

pub(super) fn step_to(step: Step, neighbours: &Neighbours, mut selection: ResMut<Selection>) {
    let to = match step {
        Step::Previous => &neighbours.previous,
        Step::Next => &neighbours.next,
    };
    if let Some(id) = to {
        selection.0 = Some(Subject::Task(id.clone()));
    }
}

/// An item of the ⋯ menu was pressed: send it for the task shown, with its answer beside the menu.
pub(super) fn on_menu_item(
    activate: On<Activate>,
    items: Query<&MenuAct>,
    state: Res<InspectorState>,
    mut controls: ResMut<Controls>,
    mut commands: Commands,
) {
    let Ok(MenuAct(action)) = items.get(activate.entity) else { return };
    let Some(subject) = state.0.as_ref().map(|v| v.subject.clone()) else { return };
    let source = control(&mut controls, &mut commands, &subject, &Action::Menu);
    let action = action.clone();
    commands.queue(move |world: &mut World| send(world, source, &subject, &action));
}

/// On a subtask's row: opens that task in the modal.
#[derive(Component, Clone, Debug, Default)]
pub(super) struct OpensTask(pub(super) String);

/// Opens the task modal while a task is selected and closes it when none is, and makes its columns once its body has
/// spawned. Changing to another task keeps the modal; [`sync_panel`] redraws what is in it.
pub(super) fn sync_modal(
    state: Res<InspectorState>,
    mut ui: ResMut<ModalUi>,
    modals: Query<Option<&ModalSlots>, With<TaskModal>>,
    mut commands: Commands,
) {
    let wanted = state.0.as_ref().and_then(|v| v.task.as_ref());
    if let Some(root) = ui.root
        && !modals.contains(root)
    {
        // Closed from inside (the X, Escape, a click outside): it took its columns with it.
        *ui = ModalUi::default();
    }
    match (wanted, ui.root) {
        (Some(facts), None) => {
            ui.root = Some(open_modal(&mut commands, TaskModal { title: facts.breadcrumb.clone() }));
        }
        (None, Some(root)) => {
            commands.entity(root).try_despawn();
            *ui = ModalUi::default();
        }
        _ => {}
    }
    if let (Some(root), None) = (ui.root, ui.panes)
        && let Ok(Some(slots)) = modals.get(root)
    {
        ui.panes = Some(spawn_detail_panes(&mut commands, slots.body));
        ui.tools = Some(spawn_tools(&mut commands, slots.tools));
    }
}

/// The modal was closed from inside it: nothing is selected any more.
pub(super) fn on_modal_closed(
    closed: On<ModalClosed>,
    modals: Query<(), With<TaskModal>>,
    mut selection: ResMut<Selection>,
) {
    if modals.contains(closed.modal) {
        selection.0 = None;
    }
}

/// A project's button in the modal: closes it and opens the project's page.
#[derive(Component)]
pub(super) struct OpensProject(pub(super) String);

/// The project was pressed: close the modal and show the project's page.
pub(super) fn on_open_project(
    activate: On<Activate>,
    buttons: Query<&OpensProject>,
    mut selection: ResMut<Selection>,
    mut commands: Commands,
) {
    if let Ok(OpensProject(key)) = buttons.get(activate.entity) {
        selection.0 = None;
        commands.trigger(OpenProject(key.clone()));
    }
}

/// A subtask's or the parent's row was pressed: show that task instead.
pub(super) fn on_open_task(activate: On<Activate>, rows: Query<&OpensTask>, mut selection: ResMut<Selection>) {
    if let Ok(OpensTask(id)) = rows.get(activate.entity) {
        selection.0 = Some(Subject::Task(id.clone()));
    }
}

#[cfg(test)]
mod tests {

    use super::super::testing::settle;
    use super::super::testing::*;
    use super::*;
    use crate::fixture::{agent_json, claim_json, sid, task_json};
    use crate::sync::Snapshot;

    #[test]
    fn an_update_leaves_an_open_modals_title_alone() {
        let mut app = app();
        let modal = app.world_mut().commands().spawn_scene(panel_header("Answer TRAIL-7")).id();
        settle(&mut app);
        app.world_mut().write_message(SyncItem::Event(json!({
            "id": 301, "entity": "task", "entity_id": sid("t7"), "kind": "task.updated",
            "after": task_json(json!({"id": "t7", "key": "TRAIL-7", "title": "renamed", "status": "in_progress", "priority": 1,
                      "version": 5})),
        })));
        settle(&mut app);
        assert!(texts(&mut app).iter().any(|t| t == "renamed"));
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
    fn a_selected_task_opens_one_modal_with_its_title_facts_and_subtasks() {
        let mut app = app();
        assert_eq!(modal_count(&mut app), 1);
        let shown = texts(&mut app);
        for want in
            ["do it", "Add a description", "Sub-tasks", "TRAIL-9  a part", "Status", "Priority", "Claim", "Details"]
        {
            assert!(shown.iter().any(|t| t == want), "{want} in {shown:?}");
        }
        // The modal's header names the task.
        let world = app.world_mut();
        let titles: Vec<String> =
            world.query_filtered::<&Text, With<PanelTitle>>().iter(world).map(|t| t.0.clone()).collect();
        assert!(titles.iter().any(|t| t == "TRAIL-7"), "{titles:?}");
        // The side panel stays hidden for a task.
        let panel = world.query_filtered::<&Node, With<InspectorPanel>>().single(world).unwrap();
        assert_eq!(panel.display, Display::None);
    }

    #[test]
    fn closing_the_modal_clears_the_selection_and_a_new_one_opens_it_again() {
        let mut app = app();
        let root = app.world_mut().query_filtered::<Entity, With<TaskModal>>().single(app.world()).unwrap();
        app.world_mut().trigger(RequestClose { source: root });
        settle(&mut app);
        assert_eq!(app.world().resource::<Selection>().0, None);
        assert_eq!(modal_count(&mut app), 0);
        app.world_mut().resource_mut::<Selection>().0 = Some(Subject::Task(sid("t8")));
        settle(&mut app);
        assert_eq!(modal_count(&mut app), 1);
        assert!(texts(&mut app).iter().any(|t| t == "other"));
    }

    #[test]
    fn a_subtasks_row_opens_it_in_the_same_modal() {
        let mut app = app();
        let root = app.world_mut().query_filtered::<Entity, With<TaskModal>>().single(app.world()).unwrap();
        let row = app.world_mut().query_filtered::<Entity, With<OpensTask>>().single(app.world()).unwrap();
        app.world_mut().trigger(Activate { entity: row });
        settle(&mut app);
        assert_eq!(app.world().resource::<Selection>().0, Some(Subject::Task(sid("t9"))));
        assert!(texts(&mut app).iter().any(|t| t == "a part"));
        let still = app.world_mut().query_filtered::<Entity, With<TaskModal>>().single(app.world()).unwrap();
        assert_eq!(still, root, "the same modal shows the subtask");
    }

    #[test]
    fn the_parent_opens_in_the_same_modal() {
        let mut app = app();
        app.world_mut().resource_mut::<Selection>().0 = Some(Subject::Task(sid("t9")));
        settle(&mut app);
        assert!(texts(&mut app).iter().any(|t| t == "Parent"));
        let world = app.world_mut();
        let row = world.query::<(Entity, &OpensTask)>().iter(world).find(|(_, o)| o.0 == sid("t7")).unwrap().0;
        app.world_mut().trigger(Activate { entity: row });
        settle(&mut app);
        assert_eq!(app.world().resource::<Selection>().0, Some(Subject::Task(sid("t7"))));
        assert!(texts(&mut app).iter().any(|t| t == "do it"));
    }

    #[test]
    fn the_project_closes_the_modal_and_opens_its_page() {
        let mut app = app();
        app.world_mut().write_message(SyncItem::Event(json!({
            "id": 200, "entity": "project", "entity_id": sid("p1"), "kind": "project.created",
            "after": crate::fixture::project_json(json!({"id": "p1", "key": "TRAIL"})),
        })));
        settle(&mut app);
        #[derive(Resource, Default)]
        struct Opened(Vec<String>);
        app.init_resource::<Opened>()
            .add_observer(|open: On<OpenProject>, mut opened: ResMut<Opened>| opened.0.push(open.0.clone()));
        let button = app.world_mut().query_filtered::<Entity, With<OpensProject>>().single(app.world()).unwrap();
        app.world_mut().trigger(Activate { entity: button });
        settle(&mut app);
        assert_eq!(app.world().resource::<Opened>().0, ["TRAIL"]);
        assert_eq!(app.world().resource::<Selection>().0, None);
        assert_eq!(modal_count(&mut app), 0);
    }

    /// The test app with TRAIL-7 held by agent a1, and its open question, whose event makes it undoable.
    fn claimed_app() -> App {
        let mut app = app();
        let mut t7 = task_json(
            json!({"id": "t7", "key": "TRAIL-7", "title": "do it", "status": "in_progress", "priority": 1, "version": 4}),
        );
        t7["claim"] = claim_json(json!({"task_id": "t7", "holder_id": "a1"}));
        let snapshot = Snapshot {
            at: 6,
            projects: vec![],
            tasks: vec![t7],
            agents: vec![agent_json(json!({"id": "a1"}))],
            ..Default::default()
        };
        app.world_mut().write_message(SyncItem::Snapshot(snapshot));
        app.world_mut().write_message(question("c1", "which db?"));
        settle(&mut app);
        app
    }

    fn menu_item(app: &mut App, action: Action) -> Entity {
        let world = app.world_mut();
        let mut q = world.query::<(Entity, &MenuAct)>();
        q.iter(world).find(|(_, m)| m.0 == action).map(|(e, _)| e).expect("the menu item")
    }

    fn shows(app: &App, entity: Entity) -> bool {
        app.world().get::<Node>(entity).is_some_and(|n| n.display != Display::None)
    }

    #[test]
    fn the_menu_sends_what_the_task_offers_and_its_answer_shows_beside_it() {
        let mut app = claimed_app();
        for action in MENU_ACTIONS {
            let item = menu_item(&mut app, action.clone());
            assert!(shows(&app, item), "{action:?} is offered");
        }
        let press = |app: &mut App, action: Action| {
            let item = menu_item(app, action);
            app.world_mut().trigger(Activate { entity: item });
            app.update();
            sent(app).remove(0)
        };
        let release = press(&mut app, Action::Release);
        assert_eq!(release.request.path, "/v1/tasks/TRAIL-7/release");
        assert_eq!(app.world().get::<Control>(release.source), Some(&Control::Busy));
        let cancel = press(&mut app, Action::Cancel);
        assert_eq!(cancel.request.body, Some(json!({"status": "cancelled"})));
        assert_eq!(press(&mut app, Action::Undo).request.path, "/v1/undo");
        let revert = press(&mut app, Action::Revert(6));
        assert_eq!(revert.request.path, "/v1/revert");
        assert_eq!(revert.request.body.as_ref().unwrap()["actor_id"], json!(sid("a1")));
        assert_eq!(revert.source, release.source, "the menu's items share the one note beside it");
        *app.world_mut().get_mut::<Control>(revert.source).unwrap() = Control::Refused("not yours".into());
        settle(&mut app);
        let note = app.world().resource::<ModalUi>().tools.as_ref().unwrap().note;
        assert_eq!(app.world().get::<Text>(note).map(|t| t.0.as_str()), Some("not yours"));
        assert!(shows(&app, note));
    }

    #[test]
    fn the_menu_leaves_out_what_an_unheld_task_cant_do_and_goes_offline() {
        let mut app = app();
        let item = menu_item(&mut app, Action::Release);
        assert!(!shows(&app, item));
        let item = menu_item(&mut app, Action::Revert(1));
        assert!(!shows(&app, item));
        let item = menu_item(&mut app, Action::Cancel);
        assert!(shows(&app, item));
        let menu = app.world().resource::<ModalUi>().tools.as_ref().unwrap().menu;
        assert!(shows(&app, menu));
        app.world_mut().resource_mut::<NextState<Connection>>().set(Connection::Offline);
        settle(&mut app);
        assert!(!shows(&app, menu), "nothing to do offline");
    }

    #[test]
    fn the_sidebar_has_no_actions_now_the_menu_holds_them() {
        let mut app = claimed_app();
        let acts =
            app.world_mut().query::<&Shown>().iter(app.world()).filter(|s| matches!(s.0, Block::Act { .. })).count();
        assert_eq!(acts, 0);
    }

    fn arrow(app: &mut App, step: Step) -> Entity {
        let world = app.world_mut();
        let mut q = world.query::<(Entity, &Step)>();
        q.iter(world).find(|(_, s)| **s == step).map(|(e, _)| e).unwrap()
    }

    fn next_is_t8(app: &mut App) {
        *app.world_mut().resource_mut::<Neighbours>() = Neighbours { previous: None, next: Some(sid("t8")) };
        settle(app);
    }

    #[test]
    fn the_arrows_step_through_the_column_and_are_off_at_its_ends() {
        let mut app = app();
        next_is_t8(&mut app);
        let (previous, next) = (arrow(&mut app, Step::Previous), arrow(&mut app, Step::Next));
        assert!(app.world().get::<InteractionDisabled>(previous).is_some(), "nothing before the first");
        assert!(app.world().get::<InteractionDisabled>(next).is_none());
        app.world_mut().trigger(Activate { entity: next });
        settle(&mut app);
        assert_eq!(app.world().resource::<Selection>().0, Some(Subject::Task(sid("t8"))));
        assert_eq!(modal_count(&mut app), 1, "the same modal shows the next task");
    }

    fn key(app: &mut App, from: Entity, code: KeyCode) {
        let input = KeyboardInput {
            key_code: code,
            logical_key: bevy::input::keyboard::Key::Unidentified(bevy::input::keyboard::NativeKey::Unidentified),
            state: bevy::input::ButtonState::Pressed,
            text: None,
            repeat: false,
            window: from,
        };
        // A real window: the press climbs to it last and stops there. An entity without `Window` would be handed
        // the press again forever.
        let window = app.world_mut().spawn(Window::default()).id();
        app.world_mut().trigger(FocusedInput::new(from, input, window));
        settle(app);
    }

    #[test]
    fn j_and_k_step_through_the_column_from_inside_the_modal_but_not_while_typing() {
        let mut app = app();
        next_is_t8(&mut app);
        let input = composer_input(&mut app);
        key(&mut app, input, KeyCode::KeyJ);
        key(&mut app, input, KeyCode::ArrowDown);
        assert_eq!(app.world().resource::<Selection>().0, Some(Subject::Task(sid("t7"))), "typing in the box");
        let from = arrow(&mut app, Step::Previous);
        key(&mut app, from, KeyCode::KeyK);
        assert_eq!(app.world().resource::<Selection>().0, Some(Subject::Task(sid("t7"))), "nothing before it");
        key(&mut app, from, KeyCode::KeyJ);
        assert_eq!(app.world().resource::<Selection>().0, Some(Subject::Task(sid("t8"))));
    }
}
