//! The Projects page, opened from the sidebar's Projects heading: every project in a table, with how many of its
//! tasks are open and done and how far along it is, a search over keys and names, a toggle for archived projects,
//! and a New project button.
//!
//! [`project_rows`] turns the mirror and the [`ProjectsFilter`] into the rows shown; it is pure, so it is tested
//! without a window. [`ProjectsPageUiPlugin`] draws the page while in [`View::Projects`], spawning it once and keeping
//! its rows in step with [`Keyed`]. Pressing a row opens that project's page.

use std::collections::HashMap;

use bevy::prelude::*;
use bevy::text::EditableText;
use bevy::ui_widgets::{Activate, ControlOrientation, ValueChange};
use trailhead_model::{Project, ProjectStatus, Status, Task};
use uuid::Uuid;

use crate::board::View;
use crate::mirror::Mirrored;
use crate::project::OpenProject;
use crate::project_form::NewProject;
use crate::widgets::{
    self, BesideSidebar, ButtonKind, CellAlign, ColumnWidth, Icon, Keyed, Progress, ScrollDown, TableColumn, button,
    matches_search, progress_bar, set_text, size, space, spawn_search_field, table_head, table_row, text, text_dim,
    toggle_switch,
};

/// How wide the key column is: room for a ten-letter key.
const KEY_WIDTH: f32 = 90.0;
/// How wide the open and done columns are.
const COUNT_WIDTH: f32 = 70.0;
/// How wide the progress column is: the bar and its percentage.
const PROGRESS_WIDTH: f32 = 200.0;

/// What the page lists: the projects whose key or name has every word of `query`, and archived ones only when asked.
#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct ProjectsFilter {
    pub query: String,
    pub archived: bool,
}

/// One project's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRow {
    pub key: String,
    pub name: String,
    pub archived: bool,
    /// Tasks not yet done or cancelled.
    pub open: usize,
    pub done: usize,
}

impl ProjectRow {
    /// How much of the project is done, 0 to 1: done of done and open, cancelled left out. Nothing yet is 0.
    pub fn progress(&self) -> f32 {
        let all = self.open + self.done;
        if all == 0 { 0.0 } else { self.done as f32 / all as f32 }
    }

    /// The progress as a whole percentage.
    pub fn percent(&self) -> String {
        format!("{}%", (self.progress() * 100.0).round() as u32)
    }
}

/// The rows the page shows for `projects` and their `tasks` under `filter`, by key.
pub fn project_rows<'a>(
    projects: impl IntoIterator<Item = &'a Project>,
    tasks: impl IntoIterator<Item = &'a Task>,
    filter: &ProjectsFilter,
) -> Vec<ProjectRow> {
    let mut counts: HashMap<Uuid, (usize, usize)> = HashMap::new();
    for task in tasks {
        let (open, done) = counts.entry(task.project_id).or_default();
        if task.status == Status::Done {
            *done += 1;
        } else if !task.status.is_closed() {
            *open += 1;
        }
    }
    let mut rows: Vec<ProjectRow> = projects
        .into_iter()
        .filter(|p| filter.archived || p.status != ProjectStatus::Archived)
        .filter(|p| matches_search(&filter.query, &[&p.key, &p.name]))
        .map(|p| {
            let (open, done) = counts.get(&p.id).copied().unwrap_or_default();
            ProjectRow {
                key: p.key.clone(),
                name: p.name.clone(),
                archived: p.status == ProjectStatus::Archived,
                open,
                done,
            }
        })
        .collect();
    rows.sort_by(|a, b| a.key.cmp(&b.key));
    rows
}

/// The table's columns.
fn columns() -> Vec<TableColumn> {
    vec![
        TableColumn::new("Name", ColumnWidth::Fill, CellAlign::Start),
        TableColumn::new("Key", ColumnWidth::Fixed(KEY_WIDTH), CellAlign::Start),
        TableColumn::new("Open", ColumnWidth::Fixed(COUNT_WIDTH), CellAlign::End),
        TableColumn::new("Done", ColumnWidth::Fixed(COUNT_WIDTH), CellAlign::End),
        TableColumn::new("Progress", ColumnWidth::Fixed(PROGRESS_WIDTH), CellAlign::Start),
    ]
}

#[derive(Component)]
struct ProjectsPanel;

/// The node the rows are kept under.
#[derive(Component)]
struct ProjectsBody;

/// The page's search box, the text entity that holds what is typed.
#[derive(Component)]
struct ProjectsSearch;

/// The switch that shows or hides archived projects.
#[derive(Component, Clone, Copy, Default)]
struct ArchivedToggle;

/// "No projects" or "No projects match", while the table is empty.
#[derive(Component)]
struct EmptyNote;

/// A row, and the project it opens.
#[derive(Component, Debug, Clone)]
struct RowOf(String);

/// Which text of a row this is, for updating it in place.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
enum RowText {
    #[default]
    Name,
    /// "archived" after the name, shown only for an archived project.
    Archived,
    Key,
    Open,
    Done,
    Percent,
}

/// The rows' entities, by key.
#[derive(Resource, Default)]
struct Rows(Keyed<ProjectRow>);

/// The page's logic, without drawing.
pub struct ProjectsPagePlugin;

impl Plugin for ProjectsPagePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ProjectsFilter>().add_observer(on_toggle_archived).add_observer(on_row_press);
    }
}

/// Draws the page while in [`View::Projects`].
pub struct ProjectsPageUiPlugin;

impl Plugin for ProjectsPageUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ProjectsPagePlugin)
            .init_resource::<Rows>()
            .add_systems(OnEnter(View::Projects), spawn_page)
            .add_systems(OnExit(View::Projects), despawn_page)
            .add_systems(Update, (read_search, sync_rows).chain().run_if(in_state(View::Projects)));
    }
}

fn on_toggle_archived(
    change: On<ValueChange<bool>>,
    toggles: Query<(), With<ArchivedToggle>>,
    mut filter: ResMut<ProjectsFilter>,
) {
    if toggles.contains(change.source) && filter.archived != change.value {
        filter.archived = change.value;
    }
}

fn on_row_press(activate: On<Activate>, rows: Query<&RowOf>, mut commands: Commands) {
    if let Ok(RowOf(key)) = rows.get(activate.entity) {
        commands.trigger(OpenProject(key.clone()));
    }
}

/// The Projects page's panel; its padding leaves room for the row frame's scrollbar.
fn page_node() -> Node {
    Node {
        min_width: px(0),
        min_height: px(0),
        flex_direction: FlexDirection::Column,
        padding: UiRect::all(px(space::PANEL_INSET)),
        row_gap: px(space::GROUPS),
        ..widgets::beside_sidebar()
    }
}

fn spawn_page(mut commands: Commands, filter: Res<ProjectsFilter>, mut rows: ResMut<Rows>) {
    // The rows of a page drawn before went with it.
    rows.0 = Keyed::default();
    let panel = commands.spawn((Name::new("projects"), ProjectsPanel, BesideSidebar, page_node())).id();
    let head = commands
        .spawn((Node { align_items: AlignItems::Center, column_gap: px(space::CONTROLS), ..default() }, ChildOf(panel)))
        .id();
    commands
        .queue_spawn_scene(bsn! { @text("Projects", size::APP_TITLE) Node { flex_grow: 1.0 } })
        .insert(ChildOf(head));
    let search = spawn_search_field(&mut commands, "Search projects");
    commands.entity(search.field).insert(ProjectsSearch);
    commands.entity(search.entry).insert(ChildOf(head));
    commands
        .queue_spawn_scene(toggle_switch("Show archived", filter.archived, bsn! { ArchivedToggle }))
        .insert(ChildOf(head));
    commands
        .queue_spawn_scene(button("New project", Some(Icon::Plus), ButtonKind::Primary))
        .insert((NewProject, ChildOf(head)));

    let columns = columns();
    commands.queue_spawn_scene(table_head(&columns)).insert(ChildOf(panel));
    // The rows and their scrollbar share a row, so the bar runs the rows' height.
    let frame = commands
        .spawn((
            Node { flex_grow: 1.0, min_height: px(0), column_gap: px(space::CONTROLS), ..default() },
            ChildOf(panel),
        ))
        .id();
    let scroll = commands
        .spawn((
            Node {
                flex_grow: 1.0,
                min_width: px(0),
                min_height: px(0),
                flex_direction: FlexDirection::Column,
                overflow: Overflow::scroll_y(),
                ..default()
            },
            ScrollDown,
            ChildOf(frame),
        ))
        .id();
    commands.spawn((Node { flex_direction: FlexDirection::Column, ..default() }, ProjectsBody, ChildOf(scroll)));
    commands
        .queue_spawn_scene(bsn! { @text_dim("", size::SECONDARY) Node { margin: { UiRect::top(px(space::ROWS)) } } })
        .insert((EmptyNote, ChildOf(scroll)));
    commands.queue_spawn_scene(widgets::scrollbar(scroll, ControlOrientation::Vertical)).insert(ChildOf(frame));
}

fn despawn_page(mut commands: Commands, panels: Query<Entity, With<ProjectsPanel>>) {
    for e in &panels {
        commands.entity(e).despawn();
    }
}

/// Copies what the search box says into the filter.
fn read_search(boxes: Query<&EditableText, With<ProjectsSearch>>, mut filter: ResMut<ProjectsFilter>) {
    let Ok(text) = boxes.single() else { return };
    let typed = text.value().to_string();
    if filter.query != typed {
        filter.query = typed;
    }
}

/// A new row's cells, one per column.
fn cells(row: &ProjectRow) -> Vec<Box<dyn SceneList>> {
    let (name, key) = (row.name.clone(), row.key.clone());
    let (open, done, percent) = (row.open.to_string(), row.done.to_string(), row.percent());
    let shown = if row.archived { Display::Flex } else { Display::None };
    let progress = row.progress();
    vec![
        Box::new(bsn_list! {
            @text(name, size::BODY) RowText::Name
            --
            @text_dim("archived", size::SMALL) Node { display: { shown } } RowText::Archived
        }),
        Box::new(bsn_list! { @text_dim(key, size::SECONDARY) RowText::Key }),
        Box::new(bsn_list! { @text(open, size::BODY) RowText::Open }),
        Box::new(bsn_list! { @text(done, size::BODY) RowText::Done }),
        Box::new(bsn_list! {
            @progress_bar(progress)
            --
            @text_dim(percent, size::SECONDARY) Node { flex_shrink: 0.0 } RowText::Percent
        }),
    ]
}

/// One row per project the filter lets through, following the mirror, updated in place.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn sync_rows(
    filter: Res<ProjectsFilter>,
    projects: Query<&Mirrored<Project>>,
    tasks: Query<&Mirrored<Task>>,
    changed: Query<(), Or<(Changed<Mirrored<Project>>, Changed<Mirrored<Task>>)>>,
    (mut removed, mut removed_tasks): (RemovedComponents<Mirrored<Project>>, RemovedComponents<Mirrored<Task>>),
    body: Query<Entity, With<ProjectsBody>>,
    mut note: Query<&mut Text, (With<EmptyNote>, Without<RowText>)>,
    children: Query<&Children>,
    (parts, mut texts, mut nodes, mut bars): (
        Query<&RowText>,
        Query<&mut Text, Without<EmptyNote>>,
        Query<&mut Node>,
        Query<&mut Progress>,
    ),
    mut rows: ResMut<Rows>,
    mut commands: Commands,
    mut drawn: Local<Option<Entity>>,
) {
    let gone = removed.read().count() + removed_tasks.read().count() > 0;
    let Ok(body) = body.single() else { return };
    if *drawn == Some(body) && changed.is_empty() && !gone && !filter.is_changed() {
        return;
    }
    *drawn = Some(body);
    let wanted = project_rows(projects.iter().map(|p| &p.0), tasks.iter().map(|t| &t.0), &filter);
    let words = match (wanted.is_empty(), projects.is_empty()) {
        (false, _) => "",
        (true, true) => "No projects yet.",
        (true, false) => "No projects match.",
    };
    for text in &mut note {
        set_text(text, words);
    }
    let columns = columns();
    let wanted = [(body, wanted.into_iter().map(|r| (r.key.clone(), r)).collect::<Vec<_>>())];
    rows.0.sync(
        &mut commands,
        &wanted,
        |commands, key, row| commands.spawn_scene(table_row(&columns, cells(row))).insert(RowOf(key.to_string())).id(),
        |_, entity, row| {
            for e in children.iter_descendants(entity) {
                if let Ok(mut bar) = bars.get_mut(e) {
                    bar.set_if_neq(Progress(row.progress()));
                }
                let Ok(part) = parts.get(e) else { continue };
                if *part == RowText::Archived {
                    if let Ok(mut node) = nodes.get_mut(e) {
                        let shown = if row.archived { Display::Flex } else { Display::None };
                        if node.display != shown {
                            node.display = shown;
                        }
                    }
                    continue;
                }
                let words = match part {
                    RowText::Name => row.name.clone(),
                    RowText::Key => row.key.clone(),
                    RowText::Open => row.open.to_string(),
                    RowText::Done => row.done.to_string(),
                    RowText::Percent => row.percent(),
                    RowText::Archived => continue,
                };
                if let Ok(text) = texts.get_mut(e) {
                    set_text(text, &words);
                }
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::fixture::{project, task};

    #[test]
    fn the_page_panel_has_room_for_a_scrollbar() {
        assert!(widgets::has_scrollbar_room(&page_node()));
    }

    fn world() -> (Vec<Project>, Vec<Task>) {
        let projects = vec![
            project(json!({"id": "p1", "key": "TRAIL", "name": "Trailhead"})),
            project(json!({"id": "p2", "key": "OPS", "name": "Operations"})),
            project(json!({"id": "p3", "key": "OLD", "name": "Old things", "status": "archived"})),
        ];
        let tasks = vec![
            task(json!({"id": "t1", "project_id": "p1", "status": "open"})),
            task(json!({"id": "t2", "project_id": "p1", "status": "in_review"})),
            task(json!({"id": "t3", "project_id": "p1", "status": "done"})),
            task(json!({"id": "t4", "project_id": "p1", "status": "cancelled"})),
            task(json!({"id": "t5", "project_id": "p3", "status": "done"})),
        ];
        (projects, tasks)
    }

    fn rows(filter: &ProjectsFilter) -> Vec<ProjectRow> {
        let (projects, tasks) = world();
        project_rows(&projects, &tasks, filter)
    }

    #[test]
    fn rows_count_open_and_done_and_leave_cancelled_out_of_the_progress() {
        let all = rows(&ProjectsFilter::default());
        let keys: Vec<&str> = all.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["OPS", "TRAIL"], "by key, archived hidden");
        let trail = &all[1];
        assert_eq!((trail.open, trail.done, trail.percent()), (2, 1, "33%".to_string()));
        assert_eq!((all[0].progress(), all[0].percent()), (0.0, "0%".to_string()), "no tasks is no progress");
    }

    #[test]
    fn archived_projects_show_only_when_asked_for() {
        let shown = rows(&ProjectsFilter { archived: true, ..default() });
        let old = shown.iter().find(|r| r.key == "OLD").unwrap();
        assert!(old.archived);
        assert_eq!(old.percent(), "100%");
    }

    fn app() -> App {
        use bevy::state::app::StatesPlugin;

        use crate::fixture::{project_json, task_json};
        use crate::mirror::MirrorPlugin;
        use crate::sync::{Snapshot, SyncItem};

        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin, AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .init_state::<View>()
            .add_plugins((MirrorPlugin, ProjectsPageUiPlugin));
        let projects = vec![
            project_json(json!({"id": "p1", "key": "TRAIL", "name": "Trailhead"})),
            project_json(json!({"id": "p3", "key": "OLD", "name": "Old things", "status": "archived"})),
        ];
        let tasks = vec![task_json(json!({"id": "t1", "project_id": "p1", "status": "done"}))];
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot { at: 1, projects, tasks, ..default() }));
        app.world_mut().resource_mut::<NextState<View>>().set(View::Projects);
        settle(&mut app);
        app
    }

    fn settle(app: &mut App) {
        for _ in 0..4 {
            app.update();
        }
    }

    fn shown(app: &mut App) -> Vec<String> {
        let world = app.world_mut();
        world.query::<&RowOf>().iter(world).map(|r| r.0.clone()).collect()
    }

    #[derive(Resource, Default)]
    struct Opened(Option<String>);

    #[test]
    fn the_page_lists_projects_shows_archived_on_asking_and_a_row_opens_its_project() {
        let mut app = app();
        assert_eq!(shown(&mut app), ["TRAIL"]);
        let toggle = {
            let world = app.world_mut();
            world.query_filtered::<Entity, With<ArchivedToggle>>().single(world).unwrap()
        };
        app.world_mut().trigger(ValueChange { source: toggle, value: true, is_final: true });
        settle(&mut app);
        let mut keys = shown(&mut app);
        keys.sort();
        assert_eq!(keys, ["OLD", "TRAIL"]);
        let bars: Vec<f32> = {
            let world = app.world_mut();
            world.query::<&Progress>().iter(world).map(|p| p.0).collect()
        };
        assert!(bars.contains(&1.0), "TRAIL's one task is done: {bars:?}");

        app.init_resource::<Opened>();
        app.add_observer(|open: On<OpenProject>, mut opened: ResMut<Opened>| opened.0 = Some(open.0.clone()));
        let row = {
            let world = app.world_mut();
            world.query::<(Entity, &RowOf)>().iter(world).find(|(_, r)| r.0 == "TRAIL").unwrap().0
        };
        app.world_mut().trigger(Activate { entity: row });
        app.update();
        assert_eq!(app.world().resource::<Opened>().0.as_deref(), Some("TRAIL"));
    }

    #[test]
    fn search_matches_key_and_name_by_every_word() {
        let by = |q: &str| rows(&ProjectsFilter { query: q.into(), ..default() });
        assert_eq!(by("trail").len(), 1);
        assert_eq!(by("oper").first().map(|r| r.key.as_str()), Some("OPS"));
        assert!(by("trail ops").is_empty());
    }

    #[test]
    fn escape_in_the_search_box_empties_it_and_the_filter() {
        use bevy::input::keyboard::{Key, KeyboardInput};
        use bevy::input_focus::FocusedInput;

        let mut app = app();
        let field = {
            let world = app.world_mut();
            world.query_filtered::<Entity, With<ProjectsSearch>>().single(world).unwrap()
        };
        app.world_mut().entity_mut(field).insert(EditableText::new("trail"));
        settle(&mut app);
        assert_eq!(app.world().resource::<ProjectsFilter>().query, "trail");
        let input = KeyboardInput {
            key_code: KeyCode::Escape,
            logical_key: Key::Escape,
            state: bevy::input::ButtonState::Pressed,
            text: None,
            repeat: false,
            window: field,
        };
        let window = app.world_mut().spawn_empty().id();
        app.world_mut().trigger(FocusedInput::new(field, input, window));
        settle(&mut app);
        assert_eq!(app.world().get::<EditableText>(field).unwrap().value().to_string(), "");
        assert_eq!(app.world().resource::<ProjectsFilter>().query, "");
    }
}
