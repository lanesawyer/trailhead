//! The sidebar down the left of the window, the same in every view: Lookout's name, the search box, the places to go
//! (Switchbacks, which is the board, the trail map, then each project's page), and a footer with the connection chip
//! and settings.
//!
//! It is spawned once and updated in place: the projects follow the mirror through [`Keyed`], the page shown is marked
//! on its [`NavItem`], and the chip is edited with [`ChipEdit`]. Searching shows the results on the board.

use bevy::input::keyboard::KeyboardInput;
use bevy::input_focus::FocusedInput;
use bevy::prelude::*;
use bevy::text::EditableText;
use bevy::ui_widgets::Activate;
use trailhead_factory::FactoryState;
use trailhead_model::Project;

use crate::board::{Filter, View};
use crate::connection::{Connection, ConnectionStatus};
use crate::factory::{FactoryLink, FactoryStatus};
use crate::mirror::Mirrored;
use crate::project::{OpenProject, ProjectPage};
use crate::settings::SettingsButton;
use crate::widgets::{
    ButtonKind, ChipEdit, Icon, Keyed, NavItem, Token, chip, nav_heading, nav_item, nav_item_noted, set_text, sidebar,
    sidebar_footer, size, spawn_search_field_filling, text, token, tool_button,
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

/// The node the projects' entries are kept under.
#[derive(Component)]
struct ProjectList;

/// The connection chip in the footer.
#[derive(Component)]
pub struct ConnectionChip;

/// A project's entry: its name, with its key after it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    key: String,
    name: String,
}

impl Entry {
    /// The caption and the note: the name with the key, or the key alone for a project without a name.
    fn words(&self) -> (String, String) {
        if self.name.is_empty() { (self.key.clone(), String::new()) } else { (self.name.clone(), self.key.clone()) }
    }
}

#[derive(Resource, Default)]
struct Entries(Keyed<Entry>);

/// Spawns the sidebar and keeps it up to date. Needs the board's [`View`] state and [`Filter`], and the project page.
pub struct SidebarUiPlugin;

impl Plugin for SidebarUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Entries>()
            .add_systems(Startup, spawn_sidebar)
            .add_observer(on_nav)
            .add_observer(clear_search_on_escape)
            .add_systems(Update, (read_search_box, sync_projects, mark_current, update_chip).chain());
    }
}

fn spawn_sidebar(mut commands: Commands) {
    let root = commands.spawn_scene(sidebar()).insert((Name::new("sidebar"), Sidebar)).id();
    commands.queue_spawn_scene(text("Lookout", size::APP_TITLE)).insert(ChildOf(root));
    let search = spawn_search_field_filling(&mut commands, "Search tasks");
    commands.entity(search.field).insert(SearchBox);
    commands.entity(search.entry).insert(ChildOf(root));
    for (caption, icon, to) in [("Switchbacks", Icon::Board, NavTo::Board), ("Trail map", Icon::Map, NavTo::Map)] {
        commands.queue_spawn_scene(nav_item(caption, Some(icon))).insert((to, ChildOf(root)));
    }
    commands.queue_spawn_scene(nav_heading("Projects")).insert(ChildOf(root));
    commands.spawn((
        Node { flex_direction: FlexDirection::Column, min_height: px(0), overflow: Overflow::scroll_y(), ..default() },
        ProjectList,
        ChildOf(root),
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
        Ok(NavTo::Board) => next.set(View::Board),
        Ok(NavTo::Map) => next.set(View::Map),
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

/// One entry per project, by key, following the mirror.
#[allow(clippy::too_many_arguments)]
fn sync_projects(
    projects: Query<&Mirrored<Project>>,
    changed: Query<(), Changed<Mirrored<Project>>>,
    mut removed: RemovedComponents<Mirrored<Project>>,
    list: Query<Entity, With<ProjectList>>,
    children: Query<&Children>,
    mut texts: Query<&mut Text>,
    mut entries: ResMut<Entries>,
    mut commands: Commands,
    mut drawn: Local<Option<Entity>>,
) {
    let gone = removed.read().count() > 0;
    let Ok(list) = list.single() else { return };
    if *drawn == Some(list) && changed.is_empty() && !gone {
        return;
    }
    *drawn = Some(list);
    let mut wanted: Vec<Entry> =
        projects.iter().map(|p| Entry { key: p.0.key.clone(), name: p.0.name.clone() }).collect();
    wanted.sort_by(|a, b| a.key.cmp(&b.key));
    let wanted = [(list, wanted.into_iter().map(|e| (e.key.clone(), e)).collect::<Vec<_>>())];
    entries.0.sync(
        &mut commands,
        &wanted,
        |commands, _, entry| {
            let (caption, note) = entry.words();
            commands.spawn_scene(nav_item_noted(caption, note)).insert(NavTo::Project(entry.key.clone())).id()
        },
        |_, entity, entry| {
            // The caption, then the note.
            let (caption, note) = entry.words();
            let mut words = [caption, note].into_iter();
            for e in children.iter_descendants(entity) {
                if let Ok(text) = texts.get_mut(e)
                    && let Some(wanted) = words.next()
                {
                    set_text(text, &wanted);
                }
            }
        },
    );
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
            (token::WARN, "Syncing", reason.unwrap_or_else(|| "Catching up with the Trailhead server".into()))
        }
        Connection::Offline => {
            (token::DANGER, "Offline", reason.unwrap_or_else(|| "Not connected to the Trailhead server".into()))
        }
        Connection::Live => match factory {
            FactoryLink::Running => (token::OK, "Live", "Connected to the Trailhead server and the factory".into()),
            FactoryLink::Unknown => {
                (token::WARN, "Factory connecting", "Server connected; the factory hasn't been heard from yet".into())
            }
            FactoryLink::Control(state) => (
                token::WARN,
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
                token::WARN,
                "Factory down",
                "Server connected; the factory can't be reached, so the map shows its last known state".into(),
            ),
            FactoryLink::TooOld(why) => {
                (token::WARN, "Factory too old", format!("Server connected; the factory is too old: {why}"))
            }
            FactoryLink::Refused(why) => {
                (token::WARN, "Factory refused", format!("Server connected; the factory refused Lookout: {why}"))
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
    use crate::fixture::{project_json, sid};
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
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot { at: 1, projects, ..Default::default() }));
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

    #[test]
    fn an_entry_shows_the_name_then_the_key_and_follows_a_rename_in_place() {
        let mut app = app();
        let words = |app: &mut App| {
            let world = app.world_mut();
            world.query::<&Text>().iter(world).map(|t| t.0.clone()).collect::<Vec<_>>()
        };
        let now = words(&mut app);
        assert!(now.contains(&"Trailhead".to_string()) && now.contains(&"TRAIL".to_string()), "{now:?}");
        let before = tree(&mut app);
        app.world_mut().write_message(SyncItem::Event(json!({
            "id": 2, "entity": "project", "entity_id": sid("p1"), "kind": "project.updated",
            "after": project_json(json!({"id": "p1", "key": "TRAIL", "name": "Trailhead engine", "version": 2})),
        })));
        settle(&mut app);
        assert!(words(&mut app).contains(&"Trailhead engine".to_string()));
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
