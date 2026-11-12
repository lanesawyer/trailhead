//! The inspector panel: its shell, its controls, and sending a control's action as a command.

use super::*;

#[derive(Component)]
pub(super) struct InspectorPanel;

/// The panel's fixed header: the title and the close button, above the scrolling body.
#[derive(Component)]
pub(super) struct InspectorHeader;

/// The panel's scrolling body, which lives as long as the panel so its scroll position survives redraws.
#[derive(Component)]
pub(super) struct InspectorBody;

/// The dropdown that moves a task to another status: `statuses[i]` is what its option `i` means, the task's own
/// status first. Its answer shows beside it on `source`.
#[derive(Component, Debug, Clone)]
pub(super) struct StatusMove {
    pub(super) source: Entity,
    pub(super) subject: Subject,
    pub(super) statuses: Vec<Status>,
}

/// The dropdown that sets the task's grade: option `i` is `GRADES[i]`. Its answer shows beside it on `source`.
#[derive(Component, Debug, Clone)]
pub(super) struct GradeChoice {
    pub(super) source: Entity,
    pub(super) subject: Subject,
}

/// The dropdown that sets how far back "revert" goes.
#[derive(Component, Debug, Clone, Copy, Default)]
pub(super) struct WindowChoice;

pub(super) fn spawn_panel(mut commands: Commands) {
    let panel = commands
        .spawn_scene(bsn! {
            @panel()
            Node {
                position_type: PositionType::Absolute,
                right: px(0),
                top: px(0),
                width: { px(extent::SIDE_PANEL_WIDTH) },
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
pub(super) fn on_close_request(
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
pub(super) fn on_escape(
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

/// What a dropdown of the panel picked: a status to move to, how far back "revert" goes, or the comment box's kind.
#[allow(clippy::too_many_arguments)]
pub(super) fn on_pick(
    picked: On<Picked>,
    moves: Query<&StatusMove>,
    grades: Query<&GradeChoice>,
    windows: Query<(), With<WindowChoice>>,
    kinds: Query<&ComposePart>,
    mut window: ResMut<RevertWindow>,
    compose: Option<ResMut<Compose>>,
    mut commands: Commands,
) {
    if kinds.get(picked.entity) == Ok(&ComposePart::Kind) {
        if let (Some(mut compose), Some(kind)) = (compose, COMPOSE_KINDS.get(picked.index)) {
            compose.kind = *kind;
        }
    } else if windows.contains(picked.entity) {
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
pub(super) fn send(world: &mut World, source: Entity, subject: &Subject, action: &Action) {
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
    // The side panel's revert is one control however far back it goes (`Revert(0)`); its window is read as the button
    // is pressed. The modal's menu names its hours.
    let action = &match action {
        Action::Revert(0) => Action::Revert(world.resource::<RevertWindow>().0),
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
pub(super) fn sends(source: Entity, subject: Subject, action: Action) -> impl Scene {
    bsn! {
        on(move |_: On<Activate>, mut commands: Commands| {
            let (subject, action) = (subject.clone(), action.clone());
            commands.queue(move |world: &mut World| send(world, source, &subject, &action));
        })
    }
}

/// The control for `action` on `subject`, made the first time it is asked for.
pub(super) fn control(controls: &mut Controls, commands: &mut Commands, subject: &Subject, action: &Action) -> Entity {
    *controls.0.entry((subject.clone(), action.clone())).or_insert_with(|| commands.spawn(Control::Idle).id())
}

/// The status dropdown's options, read as words; its [`StatusMove`] keeps the API values it sends.
pub(super) fn status_labels(statuses: &[Status]) -> Vec<String> {
    statuses.iter().map(|s| status_label(*s).to_string()).collect()
}

/// The key of the one control every status move shares, whichever status it moves to.
pub(super) const MOVE_CONTROL: Action = Action::Status(Status::Open);

/// Whether a line of [`InspectorView::lines`] is about the run rather than the task's status.
pub(super) fn is_run_line(line: &str) -> bool {
    line.starts_with("run:")
}

/// What the panel has on its entities: the sections of the body, the items in them and a PR's gates.
#[derive(Default)]
pub(super) struct Drawn {
    pub(super) subject: Option<Subject>,
    /// The modal the sections were drawn in, if they were; a different one (or none) means they went with it.
    pub(super) modal: Option<Entity>,
    pub(super) header: Option<Entity>,
    pub(super) sections: Keyed<()>,
    pub(super) items: Keyed<Block>,
    pub(super) gates: Keyed<Gate>,
}

/// Keeps the panel's entities in step with [`InspectorState`]: a task's in the task modal's two columns, an agent's
/// in the side panel. Selecting a different subject builds the sections anew, once; while the same one stays
/// selected, only what changed is set: text through its `Text`, a list's new rows appended, the rest left as the same
/// entities (and the scroll position where it was).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(super) fn sync_panel(
    state: Res<InspectorState>,
    connection: Res<State<Connection>>,
    factory: Res<FactoryStatus>,
    status: Res<ConnectionStatus>,
    window: Res<RevertWindow>,
    (ui, compose): (Res<ModalUi>, Res<Compose>),
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
        || ui.is_changed()
        || compose.is_changed()
        || factory.is_changed()
        || !changed_controls.is_empty())
    {
        return;
    }
    let (body, mut scroll) = body.into_inner();
    let drawn = &mut *drawn;
    let subject = state.0.as_ref().map(|v| v.subject.clone());
    let in_modal = state.0.as_ref().is_some_and(|v| v.task.is_some());
    let target = if in_modal { ui.root } else { None };
    if subject != drawn.subject || target != drawn.modal {
        // A different subject (or none, or another place to draw it) starts over, at the top.
        if let Some(old) = drawn.header.take() {
            commands.entity(old).try_despawn();
        }
        if drawn.modal.is_some() && drawn.modal != target {
            // That modal is gone, and its sections with it.
            drawn.sections.forget();
        } else {
            drawn.sections.clear(&mut commands);
        }
        drawn.items.forget();
        drawn.gates.forget();
        scroll.0 = Vec2::ZERO;
        drawn.subject = subject;
        drawn.modal = target;
    }
    let Some(view) = &state.0 else {
        panel_node.display = Display::None;
        return;
    };

    let live = *connection.get() == Connection::Live;
    let why = status.reason.as_ref().map_or_else(String::new, |r| format!(": {r}"));
    let offline = format!("{:?}{why}. Controls are off.", connection.get());
    let subject = &view.subject;
    let note =
        |action: &Action| controls.0.get(&(subject.clone(), action.clone())).and_then(|e| states.get(*e).ok()).cloned();
    let human = factory.human_grades_for(view.task.as_ref().and_then(|t| t.project.as_deref()));

    let (title, root, wanted) = match &view.task {
        Some(facts) => {
            panel_node.display = Display::None;
            // The modal's columns come a frame or two after it opens; this runs again when they do.
            let (Some(root), Some(panes)) = (ui.root, ui.panes) else { return };
            let answering = compose.answering.as_ref().map(|(_, q)| q.as_str());
            let (main, side) = layout_task(view, facts, live, &offline, &human, answering, &note);
            let keys = |plan: &Plan| plan.iter().map(|(k, _)| (k.clone(), ())).collect::<Vec<_>>();
            let parents = vec![(panes.main, keys(&main)), (panes.side, keys(&side))];
            (facts.breadcrumb.clone(), root, (parents, main.into_iter().chain(side).collect::<Plan>()))
        }
        None => {
            panel_node.display = Display::Flex;
            let root = *drawn.header.get_or_insert_with(|| {
                commands.spawn_scene(panel_header(view.title.clone())).insert(ChildOf(*header)).id()
            });
            let plan = layout(view, live, &offline, window.0, &human, &note);
            let keys = plan.iter().map(|(k, _)| (k.clone(), ())).collect::<Vec<_>>();
            (view.title.clone(), root, (vec![(body, keys)], plan))
        }
    };
    // Only this surface's own title: every modal's header is a `panel_header` too, with a `PanelTitle` of its own.
    for (entity, mut words) in &mut titles {
        if words.0 != title && parents.iter_ancestors(entity).any(|a| a == root) {
            words.0 = title.clone();
        }
    }

    let (sections, plan) = wanted;
    drawn.sections.sync(
        &mut commands,
        &sections,
        |commands, _, _| commands.spawn_scene(section_frame()).id(),
        |_, _, _| {},
    );
    let wanted: Vec<(Entity, Vec<(String, Block)>)> =
        plan.into_iter().filter_map(|(key, items)| Some((drawn.sections.entity(&key)?, items))).collect();
    let controls = &mut *controls;
    drawn.items.sync(
        &mut commands,
        &wanted,
        |commands, _, block| spawn_block(commands, controls, subject, block),
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
                GateState::Passed => (Icon::CircleCheck, token::STATUS_OK),
                GateState::Waiting => (Icon::Circle, token::PENDING),
            };
            commands.spawn_scene(badge(gate.name.clone(), Some(icon), Some(tone))).id()
        },
        |_, _, _| {},
    );
}

#[cfg(test)]
mod tests {

    use super::super::testing::settle;
    use super::super::testing::*;
    use super::*;
    use crate::fixture::{comment_json, sid, task_json};
    use crate::sync::Snapshot;
    use crate::widgets::ScrollDown;

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
            ..Default::default()
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
    fn the_panel_follows_the_mirror_and_an_answer_reaches_its_control() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(crate::mirror::MirrorPlugin).add_plugins(InspectorPlugin);
        let f = fixture();
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot {
            at: 5,
            projects: vec![],
            tasks: as_json(&f.tasks),
            agents: as_json(&f.agents),
            ..Default::default()
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
            ..Default::default()
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
        let v = build(f.input(), &latest(), &Subject::Task(sid("t8")), Fetched::default()).unwrap();
        assert_eq!(v.grade, None);
    }
    #[test]
    fn a_new_comment_adds_its_row_and_leaves_every_other_entity_as_it_was() {
        let mut app = app();
        let before = body(&mut app);
        assert!(texts(&mut app).iter().any(|t| t == "which db?"));

        app.world_mut().write_message(question("c2", "and the cache?"));
        settle(&mut app);
        let after = body(&mut app);
        assert!(texts(&mut app).iter().any(|t| t == "and the cache?"));
        let added: Vec<_> = after.iter().filter(|e| !before.contains(e)).collect();
        assert!(!added.is_empty(), "the new question has entities");
        // Everything that was there still is, in the same place: the first question, the comment box and the rest.
        let kept: Vec<_> = after.iter().filter(|e| before.contains(e)).copied().collect();
        assert_eq!(kept.len(), before.len());
        let world = app.world();
        let questions = |entities: &[Entity]| {
            entities
                .iter()
                .filter(|e| {
                    world
                        .get::<Shown>(**e)
                        .is_some_and(|s| matches!(&s.0, Block::Entry { entry, .. } if entry.open_question))
                })
                .count()
        };
        assert_eq!((questions(&before), questions(&after)), (1, 2));
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
        assert!(texts(&mut app).iter().any(|t| t == "renamed"));
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
    fn a_different_subject_rebuilds_the_panel() {
        let mut app = app();
        let before = body(&mut app);
        app.world_mut().resource_mut::<Selection>().0 = Some(Subject::Task(sid("t8")));
        settle(&mut app);
        let after = body(&mut app);
        assert!(after.iter().skip(2).all(|e| !before.contains(e)), "nothing is carried over");
        assert!(texts(&mut app).iter().any(|t| t == "other"));
        assert!(!texts(&mut app).iter().any(|t| t == "? which db?"));
    }

    #[test]
    fn an_agent_still_opens_the_side_panel_and_no_modal() {
        let mut app = app();
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot {
            at: 6,
            projects: vec![],
            tasks: vec![],
            agents: vec![crate::fixture::agent_json(json!({"id": "a1", "name": "factory/trail-7-a1"}))],
            ..Default::default()
        }));
        app.world_mut().resource_mut::<Selection>().0 = Some(Subject::Agent(sid("a1")));
        settle(&mut app);
        assert_eq!(modal_count(&mut app), 0);
        let world = app.world_mut();
        let panel = world.query_filtered::<&Node, With<InspectorPanel>>().single(world).unwrap();
        assert_eq!(panel.display, Display::Flex);
    }

    #[test]
    fn a_factory_event_that_leaves_the_human_grades_alone_dresses_nothing_again() {
        use crate::factory::FactoryStatus;
        let mut app = app();
        settle(&mut app);
        let stamps = |app: &mut App| {
            let world = app.world_mut();
            world.query::<(Entity, Ref<Act>)>().iter(world).map(|(e, a)| (e, a.last_changed())).collect::<Vec<_>>()
        };
        let before = stamps(&mut app);
        assert!(!before.is_empty());
        // Any factory event touches the resource; none of them changes the grades.
        app.world_mut().resource_mut::<FactoryStatus>().set_changed();
        settle(&mut app);
        assert_eq!(stamps(&mut app), before);
    }
}
