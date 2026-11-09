//! The sidebar down the left of the window, the same in every view: Lookout's name, New task, the search box, the places to go
//! (Switchbacks, which is the board, the trail map, then each project's page), and a footer with the connection chip
//! and settings. The places scroll between the search box and the footer, which stay put.
//!
//! It is spawned once and updated in place: the projects follow the mirror through [`Keyed`], the page shown is marked
//! on its [`NavItem`], and the chip is edited with [`ChipEdit`]. Searching shows the results on the board.

use std::collections::HashMap;

use bevy::input::keyboard::KeyboardInput;
use bevy::input_focus::FocusedInput;
use bevy::prelude::*;
use bevy::text::EditableText;
use bevy::ui::InteractionDisabled;
use bevy::ui_widgets::{Activate, ControlOrientation};
use trailhead_factory::FactoryState;
use trailhead_model::{Project, ProjectStatus, Task};
use uuid::Uuid;

use crate::author::Act;
use crate::board::{Filter, View};
use crate::connection::{Connection, ConnectionStatus};
use crate::factory::{FactoryLink, FactoryStatus};
use crate::mirror::Mirrored;
use crate::project::{OpenProject, ProjectPage};
use crate::project_form::NewProject;
use crate::settings::SettingsButton;
use crate::widgets::{
    ButtonKind, ChipEdit, Icon, Keyed, NavItem, NavPart, Token, Truncated, button, chip, nav_heading_action, nav_item,
    nav_item_noted, scroll_body, scroll_frame, scrollbar, set_text, sidebar, sidebar_footer, size,
    spawn_search_field_filling, text, token, tool_button,
};

/// Where a sidebar entry goes.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub enum NavTo {
    Board,
    Map,
    Project(String),
}

/// The sidebar's root.
#[derive(Component)]
pub struct Sidebar;

/// The search field, the text entity that holds what is typed.
#[derive(Component)]
pub struct SearchBox;

/// The part that scrolls: the places and the projects, between the search box and the footer.
#[derive(Component)]
struct Places;

/// The node the projects' entries are kept under.
#[derive(Component)]
struct ProjectList;

/// The "Archived" heading over the archived projects, shown only while there are any.
#[derive(Component)]
struct ArchivedHeading;

/// The heading's button that shows or hides the archived projects.
#[derive(Component, Clone, Copy, Default)]
struct ArchivedToggle;

/// The node the archived projects' entries are kept under, shown only when asked for.
#[derive(Component)]
struct ArchivedList;

/// Whether the archived projects are shown. They start hidden: archived is out of the way.
#[derive(Resource, Default)]
struct ShowArchived(bool);

/// The New task button under the title, off while the connection isn't live.
#[derive(Component)]
pub struct NewTask;

/// The connection chip in the footer.
#[derive(Component)]
pub struct ConnectionChip;

/// A project's entry: its name, its key after it, and how many of its tasks are open at the end.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    key: String,
    name: String,
    open: usize,
}

impl Entry {
    /// The caption, the note and the end: the name, the key and the open count, or the key alone for a project
    /// without a name.
    fn words(&self) -> [String; 3] {
        let open = self.open.to_string();
        if self.name.is_empty() {
            [self.key.clone(), String::new(), open]
        } else {
            [self.name.clone(), self.key.clone(), open]
        }
    }
}

#[derive(Resource, Default)]
struct Entries(Keyed<Entry>);

/// Spawns the sidebar and keeps it up to date. Needs the board's [`View`] state and [`Filter`], and the project page.
pub struct SidebarUiPlugin;

impl Plugin for SidebarUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Entries>()
            .init_resource::<ShowArchived>()
            .add_observer(on_archived_toggle)
            .add_systems(Startup, spawn_sidebar)
            .add_observer(on_nav)
            .add_observer(clear_search_on_escape)
            .add_systems(
                Update,
                (read_search_box, sync_projects, show_archived, mark_current, update_chip, gate_new_task).chain(),
            );
    }
}

fn spawn_sidebar(mut commands: Commands) {
    let root = commands.spawn_scene(sidebar()).insert((Name::new("sidebar"), Sidebar)).id();
    commands.queue_spawn_scene(text("Lookout", size::APP_TITLE)).insert(ChildOf(root));
    commands
        .queue_spawn_scene(bsn! {
            @button("New task", Some(Icon::Plus), ButtonKind::Primary)
            Node { flex_shrink: { 0.0_f32 } }
        })
        .insert((Act::New, NewTask, InteractionDisabled, ChildOf(root)));
    let search = spawn_search_field_filling(&mut commands, "Search tasks");
    commands.entity(search.field).insert(SearchBox);
    commands.entity(search.entry).insert(ChildOf(root));
    let frame = commands.spawn(scroll_frame()).insert(ChildOf(root)).id();
    let places = commands.spawn((scroll_body(), Places)).insert(ChildOf(frame)).id();
    commands.queue_spawn_scene(scrollbar(places, ControlOrientation::Vertical)).insert(ChildOf(frame));
    for (caption, icon, to) in [("Switchbacks", Icon::Board, NavTo::Board), ("Trail map", Icon::Map, NavTo::Map)] {
        commands.queue_spawn_scene(nav_item(caption, Some(icon))).insert((to, ChildOf(places)));
    }
    commands
        .queue_spawn_scene(nav_heading_action("Projects", Icon::Plus, "New project", bsn! { NewProject }))
        .insert(ChildOf(places));
    commands.spawn((
        Node { flex_direction: FlexDirection::Column, flex_shrink: 0.0, ..default() },
        ProjectList,
        ChildOf(places),
    ));
    commands
        .queue_spawn_scene(nav_heading_action(
            "Archived",
            Icon::ChevronRight,
            "Show or hide archived projects",
            bsn! { ArchivedToggle },
        ))
        .insert((ArchivedHeading, Node { display: Display::None, ..default() }, ChildOf(places)));
    commands.spawn((
        Node { display: Display::None, flex_direction: FlexDirection::Column, flex_shrink: 0.0, ..default() },
        ArchivedList,
        ChildOf(places),
    ));
    let footer = commands.queue_spawn_scene(sidebar_footer()).insert(ChildOf(root)).id();
    let (tone, words, hint) =
        connection_chip(Connection::default(), &ConnectionStatus::default(), &FactoryLink::Unknown);
    commands.queue_spawn_scene(chip(tone, words, hint)).insert((ConnectionChip, ChildOf(footer)));
    commands
        .queue_spawn_scene(tool_button(Icon::Settings, "Settings", ButtonKind::Plain))
        .insert((SettingsButton, ChildOf(footer)));
}

fn on_nav(activate: On<Activate>, to: Query<&NavTo>, mut next: ResMut<NextState<View>>, mut commands: Commands) {
    match to.get(activate.entity) {
        Ok(NavTo::Board) => next.set_if_different(View::Board),
        Ok(NavTo::Map) => next.set_if_different(View::Map),
        Ok(NavTo::Project(key)) => commands.trigger(OpenProject(key.clone())),
        Err(_) => {}
    }
}

/// Copies what the search box says into the [`Filter`], and shows the board, where the results are, on a new search.
fn read_search_box(
    boxes: Query<&EditableText, With<SearchBox>>,
    mut filter: ResMut<Filter>,
    view: Res<State<View>>,
    mut next: ResMut<NextState<View>>,
) {
    let Ok(text) = boxes.single() else { return };
    let typed = text.value().to_string();
    if filter.query == typed {
        return;
    }
    filter.query = typed;
    if !filter.query.trim().is_empty() && *view.get() != View::Board {
        next.set(View::Board);
    }
}

/// Escape in the search box empties it.
fn clear_search_on_escape(key: On<FocusedInput<KeyboardInput>>, mut boxes: Query<&mut EditableText, With<SearchBox>>) {
    let pressed = &key.input;
    if pressed.key_code != KeyCode::Escape || !pressed.state.is_pressed() {
        return;
    }
    if let Ok(mut text) = boxes.get_mut(key.original_event_target()) {
        text.clear();
    }
}

/// One entry per project, by key, following the mirror's projects and their tasks.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn sync_projects(
    projects: Query<&Mirrored<Project>>,
    tasks: Query<&Mirrored<Task>>,
    changed: Query<(), Or<(Changed<Mirrored<Project>>, Changed<Mirrored<Task>>)>>,
    (mut removed, mut removed_tasks): (RemovedComponents<Mirrored<Project>>, RemovedComponents<Mirrored<Task>>),
    list: Query<Entity, With<ProjectList>>,
    archived_list: Query<Entity, With<ArchivedList>>,
    children: Query<&Children>,
    (mut texts, mut truncated, parts): (Query<&mut Text>, Query<&mut Truncated>, Query<&NavPart>),
    mut entries: ResMut<Entries>,
    mut commands: Commands,
    mut drawn: Local<Option<Entity>>,
) {
    let gone = removed.read().count() + removed_tasks.read().count() > 0;
    let (Ok(list), Ok(archived_list)) = (list.single(), archived_list.single()) else { return };
    if *drawn == Some(list) && changed.is_empty() && !gone {
        return;
    }
    *drawn = Some(list);
    let mut open: HashMap<Uuid, usize> = HashMap::new();
    for task in tasks.iter().filter(|t| !t.0.status.is_closed()) {
        *open.entry(task.0.project_id).or_default() += 1;
    }
    let mut sorted: Vec<&Project> = projects.iter().map(|p| &p.0).collect();
    sorted.sort_by(|a, b| a.key.cmp(&b.key));
    let entries_of = |archived: bool| {
        sorted
            .iter()
            .filter(|p| (p.status == ProjectStatus::Archived) == archived)
            .map(|p| {
                let entry = Entry {
                    key: p.key.clone(),
                    name: p.name.clone(),
                    open: open.get(&p.id).copied().unwrap_or_default(),
                };
                (entry.key.clone(), entry)
            })
            .collect::<Vec<_>>()
    };
    // An archived project's entry moves between the two lists rather than being drawn again.
    let wanted = [(list, entries_of(false)), (archived_list, entries_of(true))];
    entries.0.sync(
        &mut commands,
        &wanted,
        |commands, _, entry| {
            let [caption, note, end] = entry.words();
            commands.spawn_scene(nav_item_noted(caption, note, end)).insert(NavTo::Project(entry.key.clone())).id()
        },
        |_, entity, entry| {
            let [caption, note, end] = entry.words();
            for e in children.iter_descendants(entity) {
                match parts.get(e) {
                    // Cut to fit, so it changes through its `Truncated`.
                    Ok(NavPart::Caption) => {
                        if let Ok(mut shown) = truncated.get_mut(e)
                            && shown.full != caption
                        {
                            shown.full = caption.clone();
                        }
                    }
                    Ok(NavPart::Note) => {
                        if let Ok(text) = texts.get_mut(e) {
                            set_text(text, &note);
                        }
                    }
                    Ok(NavPart::End) => {
                        if let Ok(text) = texts.get_mut(e) {
                            set_text(text, &end);
                        }
                    }
                    Err(_) => {}
                }
            }
        },
    );
}

fn on_archived_toggle(
    activate: On<Activate>,
    toggles: Query<(), With<ArchivedToggle>>,
    mut show: ResMut<ShowArchived>,
) {
    if toggles.contains(activate.entity) {
        show.0 = !show.0;
    }
}

/// The "Archived" heading while there are archived projects, their list while asked for, and the heading's chevron
/// pointing down while it is open.
#[allow(clippy::type_complexity)]
fn show_archived(
    show: Res<ShowArchived>,
    projects: Query<&Mirrored<Project>>,
    mut heading: Query<&mut Node, (With<ArchivedHeading>, Without<ArchivedList>)>,
    mut list: Query<&mut Node, (With<ArchivedList>, Without<ArchivedHeading>)>,
    toggles: Query<Entity, With<ArchivedToggle>>,
    children: Query<&Children>,
    mut texts: Query<&mut Text>,
) {
    let any = projects.iter().any(|p| p.0.status == ProjectStatus::Archived);
    let display = |shown: bool| if shown { Display::Flex } else { Display::None };
    for mut node in &mut heading {
        if node.display != display(any) {
            node.display = display(any);
        }
    }
    for mut node in &mut list {
        if node.display != display(any && show.0) {
            node.display = display(any && show.0);
        }
    }
    let chevron = if show.0 { Icon::ChevronDown } else { Icon::ChevronRight }.glyph();
    for toggle in &toggles {
        for e in children.iter_descendants(toggle) {
            if let Ok(text) = texts.get_mut(e) {
                set_text(text, chevron);
            }
        }
    }
}

/// Marks the entry for the page shown.
fn mark_current(view: Res<State<View>>, page: Res<ProjectPage>, mut items: Query<(&NavTo, Mut<NavItem>)>) {
    let moved = view.is_changed() || page.is_changed();
    for (to, mut item) in &mut items {
        // An entry spawned from a queued scene gets its `NavItem` after its `NavTo`.
        if !(moved || item.is_added()) {
            continue;
        }
        let current = match (to, view.get()) {
            (NavTo::Board, View::Board) | (NavTo::Map, View::Map) => true,
            (NavTo::Project(key), View::Project) => page.0.as_ref() == Some(key),
            _ => false,
        };
        item.set_if_neq(NavItem { current });
    }
}

/// New task is on only while the connection is live.
fn gate_new_task(
    connection: Res<State<Connection>>,
    new: Query<(Entity, Has<InteractionDisabled>), With<NewTask>>,
    mut commands: Commands,
) {
    let live = *connection.get() == Connection::Live;
    for (entity, disabled) in &new {
        if live && disabled {
            commands.entity(entity).remove::<InteractionDisabled>();
        } else if !live && !disabled {
            commands.entity(entity).insert(InteractionDisabled);
        }
    }
}

/// Keeps the chip in step with the connection and the factory's link, in place.
fn update_chip(
    connection: Res<State<Connection>>,
    status: Res<ConnectionStatus>,
    factory: Res<FactoryStatus>,
    chips: Query<Entity, With<ConnectionChip>>,
    added: Query<(), Added<ConnectionChip>>,
    mut edit: ChipEdit,
) {
    if !(connection.is_changed() || status.is_changed() || factory.is_changed() || !added.is_empty()) {
        return;
    }
    let (tone, words, hint) = connection_chip(*connection.get(), &status, &factory.link);
    for chip in &chips {
        edit.set(chip, tone.clone(), words, &hint);
    }
}

/// The connection chip's tone, words and hint. It shows two links: the Trailhead server, which wins when it isn't
/// live, and the factory's control plane, which must be running for the chip to be green.
pub fn connection_chip(
    connection: Connection,
    status: &ConnectionStatus,
    factory: &FactoryLink,
) -> (Token, &'static str, String) {
    let reason = status.reason.as_ref().map(|r| r.to_string());
    match connection {
        Connection::Syncing => {
            (token::STATUS_WARN, "Syncing", reason.unwrap_or_else(|| "Catching up with the Trailhead server".into()))
        }
        Connection::Offline => {
            (token::STATUS_DANGER, "Offline", reason.unwrap_or_else(|| "Not connected to the Trailhead server".into()))
        }
        Connection::Live => match factory {
            FactoryLink::Running => {
                (token::STATUS_OK, "Live", "Connected to the Trailhead server and the factory".into())
            }
            FactoryLink::Unknown => (
                token::STATUS_WARN,
                "Factory connecting",
                "Server connected; the factory hasn't been heard from yet".into(),
            ),
            FactoryLink::Control(state) => (
                token::STATUS_WARN,
                match state {
                    FactoryState::Off => "Factory off",
                    FactoryState::Disabled => "Factory disabled",
                    FactoryState::Starting { .. } => "Factory starting",
                    FactoryState::Running => "Factory running",
                    FactoryState::Stopped => "Factory stopped",
                },
                format!("Server connected; the factory is {}", state.label()),
            ),
            FactoryLink::Unreachable => (
                token::STATUS_WARN,
                "Factory down",
                "Server connected; the factory can't be reached, so the map shows its last known state".into(),
            ),
            FactoryLink::TooOld(why) => {
                (token::STATUS_WARN, "Factory too old", format!("Server connected; the factory is too old: {why}"))
            }
            FactoryLink::Refused(why) => {
                (token::STATUS_WARN, "Factory refused", format!("Server connected; the factory refused Lookout: {why}"))
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use bevy::state::app::StatesPlugin;
    use serde_json::json;

    use super::*;
    use crate::board::BoardPlugin;
    use crate::fixture::{project_json, sid, task_json};
    use crate::mirror::MirrorPlugin;
    use crate::project::ProjectPlugin;
    use crate::sync::{Snapshot, SyncItem};

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin, AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .init_state::<Connection>()
            .init_state::<View>()
            .init_resource::<ConnectionStatus>()
            .init_resource::<FactoryStatus>()
            .add_plugins((MirrorPlugin, BoardPlugin, ProjectPlugin, SidebarUiPlugin));
        let projects = vec![
            project_json(json!({"id": "p2", "key": "OPS", "name": "Operations"})),
            project_json(json!({"id": "p1", "key": "TRAIL", "name": "Trailhead"})),
        ];
        let tasks = vec![
            task_json(json!({"id": "t1", "key": "TRAIL-1", "status": "open"})),
            task_json(json!({"id": "t2", "key": "TRAIL-2", "status": "in_review"})),
            task_json(json!({"id": "t3", "key": "TRAIL-3", "status": "done"})),
        ];
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot { at: 1, projects, tasks, ..Default::default() }));
        settle(&mut app);
        app
    }

    fn settle(app: &mut App) {
        for _ in 0..4 {
            app.update();
        }
    }

    /// Every entity under the sidebar, in order.
    fn tree(app: &mut App) -> Vec<Entity> {
        let world = app.world_mut();
        let root = world.query_filtered::<Entity, With<Sidebar>>().single(world).unwrap();
        let mut found = vec![root];
        let mut at = 0;
        while at < found.len() {
            if let Some(children) = world.get::<Children>(found[at]) {
                found.extend(children.iter());
            }
            at += 1;
        }
        found
    }

    fn one<C: Component>(app: &mut App) -> Entity {
        let world = app.world_mut();
        world.query_filtered::<Entity, With<C>>().single(world).unwrap()
    }

    /// The entries in order, with whether each is marked as the page shown.
    fn entries(app: &mut App) -> Vec<(NavTo, bool)> {
        let order = tree(app);
        let world = app.world_mut();
        let mut q = world.query::<(Entity, &NavTo, &NavItem)>();
        let mut found: Vec<(usize, NavTo, bool)> = q
            .iter(world)
            .map(|(e, to, v)| (order.iter().position(|o| *o == e).unwrap(), to.clone(), v.current))
            .collect();
        found.sort_by_key(|(at, _, _)| *at);
        found.into_iter().map(|(_, to, current)| (to, current)).collect()
    }

    fn press(app: &mut App, to: NavTo) {
        let world = app.world_mut();
        let entity = world.query::<(Entity, &NavTo)>().iter(world).find(|(_, t)| **t == to).unwrap().0;
        world.trigger(Activate { entity });
        settle(app);
    }

    #[test]
    fn the_sidebar_has_its_places_projects_by_key_and_footer() {
        let mut app = app();
        one::<SearchBox>(&mut app);
        one::<ConnectionChip>(&mut app);
        one::<SettingsButton>(&mut app);
        assert_eq!(
            entries(&mut app),
            [
                (NavTo::Board, true),
                (NavTo::Map, false),
                (NavTo::Project("OPS".into()), false),
                (NavTo::Project("TRAIL".into()), false),
            ]
        );
    }

    #[test]
    fn the_places_scroll_between_the_search_box_and_the_footer() {
        let mut app = app();
        let places = one::<Places>(&mut app);
        let root = one::<Sidebar>(&mut app);
        let world = app.world_mut();
        assert!(world.get::<crate::widgets::ScrollDown>(places).is_some());
        assert_eq!(world.get::<Node>(places).unwrap().overflow, Overflow::scroll_y());
        let frame = world.get::<ChildOf>(places).unwrap().parent();
        assert_eq!(world.get::<Node>(frame).unwrap().min_height, px(0), "the frame may shrink so the places scroll");
        // The title and the search box above it, the footer below.
        let top: Vec<Entity> = world.get::<Children>(root).unwrap().iter().collect();
        assert_eq!(top.len(), 6, "the drag edge, title, New task, search, places, footer");
        assert!(world.get::<crate::widgets::SidebarEdge>(top[0]).is_some());
        assert_eq!(top[4], frame);
        let mut entries = world.query_filtered::<Entity, With<NavTo>>();
        let entries: Vec<Entity> = entries.iter(world).collect();
        assert_eq!(entries.len(), 4);
        for entry in entries {
            let mut at = entry;
            while let Some(parent) = world.get::<ChildOf>(at) {
                at = parent.parent();
                if at == places {
                    break;
                }
            }
            assert_eq!(at, places, "every place scrolls");
        }
        // In a short window the places scroll rather than squash, and the footer keeps its height.
        let mut shrinks = world.query_filtered::<&Node, With<NavItem>>();
        assert!(shrinks.iter(world).all(|n| n.flex_shrink == 0.0));
        assert_eq!(world.get::<Node>(top[5]).unwrap().flex_shrink, 0.0);
    }

    #[test]
    fn new_task_sits_under_the_title_and_is_on_only_while_live() {
        let mut app = app();
        let new = one::<NewTask>(&mut app);
        let root = one::<Sidebar>(&mut app);
        assert_eq!(app.world().get::<Children>(root).unwrap()[2], new);
        assert_eq!(app.world().get::<Act>(new), Some(&Act::New));
        let disabled = |app: &App| app.world().get::<InteractionDisabled>(new).is_some();
        let set = |app: &mut App, to| {
            app.world_mut().resource_mut::<NextState<Connection>>().set(to);
            settle(app);
        };
        set(&mut app, Connection::Offline);
        assert!(disabled(&app));
        set(&mut app, Connection::Live);
        assert!(!disabled(&app));
        set(&mut app, Connection::Syncing);
        assert!(disabled(&app));
        assert_eq!(one::<NewTask>(&mut app), new, "changed in place");
    }

    #[test]
    fn going_somewhere_marks_it_and_leaves_the_sidebar_as_it_was() {
        let mut app = app();
        let before = tree(&mut app);
        press(&mut app, NavTo::Project("TRAIL".into()));
        assert_eq!(*app.world().resource::<State<View>>().get(), View::Project);
        assert_eq!(app.world().resource::<ProjectPage>().0.as_deref(), Some("TRAIL"));
        let current: Vec<NavTo> = entries(&mut app).into_iter().filter(|(_, c)| *c).map(|(t, _)| t).collect();
        assert_eq!(current, [NavTo::Project("TRAIL".into())]);
        press(&mut app, NavTo::Map);
        assert_eq!(*app.world().resource::<State<View>>().get(), View::Map);
        let current: Vec<NavTo> = entries(&mut app).into_iter().filter(|(_, c)| *c).map(|(t, _)| t).collect();
        assert_eq!(current, [NavTo::Map]);
        assert_eq!(tree(&mut app), before);
    }

    #[derive(Resource, Default)]
    struct Entered(usize);

    #[test]
    fn going_to_the_page_shown_leaves_it_as_it_was() {
        let mut app = app();
        app.init_resource::<Entered>();
        for view in [View::Board, View::Map, View::Project] {
            app.add_systems(OnEnter(view), |mut entered: ResMut<Entered>| entered.0 += 1);
        }
        let entered = |app: &App| app.world().resource::<Entered>().0;
        press(&mut app, NavTo::Board);
        assert_eq!(entered(&app), 0);
        press(&mut app, NavTo::Map);
        press(&mut app, NavTo::Map);
        assert_eq!(entered(&app), 1);
        press(&mut app, NavTo::Project("TRAIL".into()));
        press(&mut app, NavTo::Project("TRAIL".into()));
        assert_eq!(entered(&app), 2);
        press(&mut app, NavTo::Project("OPS".into()));
        assert_eq!(entered(&app), 3, "another project's page is drawn fresh");
    }

    #[test]
    fn a_new_project_adds_its_entry_in_order_and_keeps_the_rest() {
        let mut app = app();
        let before = tree(&mut app);
        app.world_mut().write_message(SyncItem::Event(json!({
            "id": 2, "entity": "project", "entity_id": sid("p3"), "kind": "project.created",
            "after": project_json(json!({"id": "p3", "key": "BDE", "name": "Data explorer"})),
        })));
        settle(&mut app);
        let after = tree(&mut app);
        assert!(before.iter().all(|e| after.contains(e)), "nothing respawned");
        let projects: Vec<NavTo> =
            entries(&mut app).into_iter().map(|(t, _)| t).filter(|t| matches!(t, NavTo::Project(_))).collect();
        assert_eq!(
            projects,
            [NavTo::Project("BDE".into()), NavTo::Project("OPS".into()), NavTo::Project("TRAIL".into())]
        );
    }

    /// An entry's words: the caption (whole, before it is cut to fit), the note, the end.
    fn entry_words(app: &mut App, key: &str) -> Vec<String> {
        let world = app.world_mut();
        let to = NavTo::Project(key.into());
        let entry = world.query::<(Entity, &NavTo)>().iter(world).find(|(_, t)| **t == to).unwrap().0;
        let mut kids = world.query::<&Children>();
        let mut found = vec![entry];
        let mut at = 0;
        while at < found.len() {
            if let Ok(children) = kids.get(world, found[at]) {
                found.extend(children.iter());
            }
            at += 1;
        }
        let word = |part: NavPart| {
            let at = found.iter().find(|e| world.get::<NavPart>(**e) == Some(&part)).unwrap();
            match world.get::<Truncated>(*at) {
                Some(caption) => caption.full.clone(),
                None => world.get::<Text>(*at).unwrap().0.clone(),
            }
        };
        vec![word(NavPart::Caption), word(NavPart::Note), word(NavPart::End)]
    }

    #[test]
    fn an_entry_counts_its_projects_open_tasks_and_follows_one_closing_in_place() {
        let mut app = app();
        assert_eq!(entry_words(&mut app, "TRAIL"), ["Trailhead", "TRAIL", "2"], "done isn't open");
        assert_eq!(entry_words(&mut app, "OPS"), ["Operations", "OPS", "0"]);
        let before = tree(&mut app);
        app.world_mut().write_message(SyncItem::Event(json!({
            "id": 2, "entity": "task", "entity_id": sid("t2"), "kind": "task.updated",
            "after": task_json(json!({"id": "t2", "key": "TRAIL-2", "status": "done", "version": 2})),
        })));
        settle(&mut app);
        assert_eq!(entry_words(&mut app, "TRAIL"), ["Trailhead", "TRAIL", "1"]);
        assert_eq!(tree(&mut app), before);
    }

    #[test]
    fn an_archived_project_moves_under_archived_which_opens_and_closes() {
        let mut app = app();
        let heading = one::<ArchivedHeading>(&mut app);
        let list = one::<ArchivedList>(&mut app);
        let display = |app: &App, e| app.world().get::<Node>(e).unwrap().display;
        assert_eq!(display(&app, heading), Display::None, "no archived projects, no heading");
        let ops = |app: &mut App| {
            let world = app.world_mut();
            let to = NavTo::Project("OPS".into());
            world.query::<(Entity, &NavTo)>().iter(world).find(|(_, t)| **t == to).unwrap().0
        };
        let entry = ops(&mut app);
        app.world_mut().write_message(SyncItem::Event(json!({
            "id": 2, "entity": "project", "entity_id": sid("p2"), "kind": "project.updated",
            "after": project_json(json!({"id": "p2", "key": "OPS", "name": "Operations", "status": "archived",
                                         "version": 2})),
        })));
        settle(&mut app);
        assert_eq!(ops(&mut app), entry, "the entry moves rather than being drawn again");
        assert_eq!(app.world().get::<ChildOf>(entry).unwrap().parent(), list);
        assert_eq!(display(&app, heading), Display::Flex);
        assert_eq!(display(&app, list), Display::None, "archived starts out of the way");
        let toggle = one::<ArchivedToggle>(&mut app);
        app.world_mut().trigger(Activate { entity: toggle });
        settle(&mut app);
        assert_eq!(display(&app, list), Display::Flex);
        app.world_mut().trigger(Activate { entity: toggle });
        settle(&mut app);
        assert_eq!(display(&app, list), Display::None);
    }

    #[test]
    fn an_entry_shows_the_name_then_the_key_and_follows_a_rename_in_place() {
        let mut app = app();
        assert_eq!(entry_words(&mut app, "TRAIL")[..2], ["Trailhead", "TRAIL"]);
        let before = tree(&mut app);
        app.world_mut().write_message(SyncItem::Event(json!({
            "id": 2, "entity": "project", "entity_id": sid("p1"), "kind": "project.updated",
            "after": project_json(json!({"id": "p1", "key": "TRAIL", "name": "Trailhead engine", "version": 2})),
        })));
        settle(&mut app);
        assert_eq!(entry_words(&mut app, "TRAIL")[..2], ["Trailhead engine", "TRAIL"]);
        assert_eq!(tree(&mut app), before);
    }

    #[test]
    fn searching_feeds_the_filter_and_shows_the_board() {
        let mut app = app();
        press(&mut app, NavTo::Map);
        let before = tree(&mut app);
        let field = one::<SearchBox>(&mut app);
        app.world_mut().entity_mut(field).insert(EditableText::new("trail-8"));
        settle(&mut app);
        assert_eq!(app.world().resource::<Filter>().query, "trail-8");
        assert_eq!(*app.world().resource::<State<View>>().get(), View::Board);
        assert_eq!(tree(&mut app), before);
    }

    #[test]
    fn the_chip_is_green_only_while_both_links_are_up() {
        let status = ConnectionStatus::default();
        let words = |c, f: FactoryLink| connection_chip(c, &status, &f).1;
        assert_eq!(words(Connection::Live, FactoryLink::Running), "Live");
        assert_eq!(words(Connection::Live, FactoryLink::Unreachable), "Factory down");
        assert_eq!(words(Connection::Live, FactoryLink::Control(FactoryState::Disabled)), "Factory disabled");
        let starting = FactoryLink::Control(FactoryState::Starting { reason: Some("sprites down".into()) });
        assert!(connection_chip(Connection::Live, &status, &starting).2.contains("sprites down"));
        assert_eq!(words(Connection::Live, FactoryLink::Unknown), "Factory connecting");
        assert_eq!(words(Connection::Live, FactoryLink::TooOld("x".into())), "Factory too old");
        assert_eq!(words(Connection::Live, FactoryLink::Refused("x".into())), "Factory refused");
        assert_eq!(words(Connection::Syncing, FactoryLink::Unreachable), "Syncing");
        assert_eq!(words(Connection::Offline, FactoryLink::Running), "Offline");
        let tone = |f| connection_chip(Connection::Live, &status, &f).0;
        assert_ne!(tone(FactoryLink::Running), tone(FactoryLink::Unreachable));
    }

    #[test]
    fn the_chip_follows_the_factory_link_in_place() {
        let mut app = app();
        app.world_mut().resource_mut::<NextState<Connection>>().set(Connection::Live);
        app.world_mut().resource_mut::<FactoryStatus>().link = FactoryLink::Running;
        settle(&mut app);
        let before = tree(&mut app);
        let words = |app: &mut App| {
            let world = app.world_mut();
            world.query::<&Text>().iter(world).map(|t| t.0.clone()).collect::<Vec<_>>()
        };
        assert!(words(&mut app).contains(&"Live".to_string()));
        app.world_mut().resource_mut::<FactoryStatus>().link = FactoryLink::Unreachable;
        settle(&mut app);
        let now = words(&mut app);
        assert!(now.contains(&"Factory down".to_string()) && !now.contains(&"Live".to_string()), "{now:?}");
        assert_eq!(tree(&mut app), before);
    }
}
