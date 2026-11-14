//! A project's page: every task in one project, closed ones included, as the board's cards in a grid.
//!
//! [`OpenProject`] shows the page for a project from anywhere. [`ProjectPlugin`] keeps a [`ProjectView`] from the
//! mirror and the grid's entities in step with it, one per card, the way the board keeps its columns; the cards are
//! [`BoardCard`]s, drawn and opened in the inspector as on the board. [`ProjectUiPlugin`] draws the page while in
//! [`View::Project`].

use bevy::prelude::*;
use bevy::ui_widgets::{Activate, ControlOrientation};
use jiff::Timestamp;
use trailhead_model::ProjectStatus;

use crate::board::{BoardCard, Card, PAGE, Sources, View, grid, next_expiry};
use crate::project_form::EditProject;
use crate::widgets::{
    self, ButtonKind, COLUMN_WIDTH, Icon, Keyed, ScrollDown, button, set_text, size, space, text, text_dim,
};

/// Shows the page of the project with this key (`TRAIL`).
#[derive(Event, Debug, Clone, PartialEq, Eq)]
pub struct OpenProject(pub String);

/// Which project the page shows, by key.
#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct ProjectPage(pub Option<String>);

/// What the page shows.
#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct ProjectView {
    pub key: String,
    pub name: String,
    pub archived: bool,
    pub cards: Vec<Card>,
}

/// The node the page's cards are kept under.
#[derive(Component)]
pub struct ProjectGrid;

/// The "No tasks" line of a project without any.
#[derive(Component)]
pub struct ProjectEmpty;

/// The "Show more" button after the last card built, with how many one press adds.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct ProjectMore(pub usize);

#[derive(Debug, Clone, PartialEq, Eq)]
enum Slot {
    Card(Box<Card>),
    Empty,
    More(usize),
}

/// The entities the grid has made, so a change touches only what it changes.
#[derive(Resource, Default)]
struct Tree {
    /// The [`ProjectGrid`] these belong to; a new one (the page was opened again) starts them over.
    root: Option<Entity>,
    slots: Keyed<Slot>,
}

/// How many cards the grid builds: [`PAGE`], and another page for each "Show more".
#[derive(Resource)]
struct Shown(usize);

impl Default for Shown {
    fn default() -> Self {
        Shown(PAGE)
    }
}

/// Keeps the [`ProjectView`] and the grid's entities from the mirror. Needs the board's [`View`] state and
/// [`BoardPlugin`](crate::board::BoardPlugin), whose resources the page builds from too.
pub struct ProjectPlugin;

impl Plugin for ProjectPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ProjectPage>()
            .init_resource::<ProjectView>()
            .init_resource::<Shown>()
            .init_resource::<Tree>()
            .add_observer(on_open)
            .add_observer(on_more_press)
            .add_systems(Update, (rebuild_view, sync_grid).chain());
    }
}

fn on_open(
    open: On<OpenProject>,
    mut page: ResMut<ProjectPage>,
    mut shown: ResMut<Shown>,
    mut next: ResMut<NextState<View>>,
) {
    if page.0.as_deref() == Some(open.0.as_str()) {
        next.set_if_different(View::Project);
        return;
    }
    page.0 = Some(open.0.clone());
    shown.0 = PAGE;
    // Re-entering redraws the page, so another project's starts at the top.
    next.set(View::Project);
}

fn on_more_press(activate: On<Activate>, more: Query<&ProjectMore>, mut shown: ResMut<Shown>) {
    if more.contains(activate.entity) {
        shown.0 += PAGE;
    }
}

fn rebuild_view(
    page: Res<ProjectPage>,
    mut sources: Sources,
    mut view: ResMut<ProjectView>,
    mut expiry: Local<Option<Timestamp>>,
) {
    let changed = sources.changed();
    let now = Timestamp::now();
    let expired = expiry.is_some_and(|at| at <= now);
    if !(changed || page.is_changed() || expired) {
        return;
    }
    let Some(key) = &page.0 else { return };
    let rows = sources.rows();
    *expiry = next_expiry(&rows.claims, now);
    let project = rows.projects.iter().find(|p| &p.key == key);
    let name = project.map(|p| p.name.clone()).unwrap_or_default();
    let archived = project.is_some_and(|p| p.status == ProjectStatus::Archived);
    let next = ProjectView { key: key.clone(), name, archived, cards: grid(rows.input(&[], now), key) };
    if *view != next {
        *view = next;
    }
}

/// Brings the cards under [`ProjectGrid`] to [`ProjectView`]: a card that left is despawned, a new one spawned, one
/// whose view model changed is given the new model, and the rest keep their entities.
fn sync_grid(
    view: Res<ProjectView>,
    shown: Res<Shown>,
    root: Query<Entity, With<ProjectGrid>>,
    mut tree: ResMut<Tree>,
    mut commands: Commands,
) {
    let root = root.single().ok();
    let Tree { root: drawn, slots } = &mut *tree;
    if *drawn != root {
        // The old nodes went with their page.
        *drawn = root;
        slots.forget();
    } else if !(view.is_changed() || shown.is_changed()) {
        return;
    }
    let Some(root) = root else { return };
    let (cards, hidden) = view.cards.split_at(shown.0.min(view.cards.len()));
    let mut items: Vec<(String, Slot)> =
        cards.iter().map(|c| (c.id.clone(), Slot::Card(Box::new(c.clone())))).collect();
    if view.cards.is_empty() {
        items.push(("empty".into(), Slot::Empty));
    }
    if !hidden.is_empty() {
        items.push(("more".into(), Slot::More(hidden.len().min(PAGE))));
    }
    slots.sync(
        &mut commands,
        &[(root, items)],
        |commands, _, slot| match slot {
            Slot::Card(card) => commands.spawn(BoardCard((**card).clone())).id(),
            Slot::Empty => commands.spawn(ProjectEmpty).id(),
            Slot::More(next) => commands.spawn(ProjectMore(*next)).id(),
        },
        |commands, entity, slot| match slot {
            Slot::Card(card) => {
                commands.entity(entity).insert(BoardCard((**card).clone()));
            }
            Slot::More(next) => {
                commands.entity(entity).insert(ProjectMore(*next));
            }
            Slot::Empty => {}
        },
    );
}

/// Draws a project's page while in [`View::Project`].
pub struct ProjectUiPlugin;

impl Plugin for ProjectUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ProjectPlugin)
            .add_systems(OnEnter(View::Project), spawn_page)
            .add_systems(OnExit(View::Project), despawn_page)
            .add_systems(Update, (update_title, dress_slots).after(sync_grid).run_if(in_state(View::Project)));
    }
}

#[derive(Component)]
struct ProjectPanel;

/// The project's key and name at the top of the page.
#[derive(Component)]
struct ProjectTitle;

fn spawn_page(mut commands: Commands) {
    let panel = commands.spawn((Name::new("project"), ProjectPanel, widgets::BesideSidebar, widgets::page_node())).id();
    // The title, and the button that edits the project, at the far end of its row.
    let head = commands
        .spawn((
            Node { align_items: AlignItems::Center, justify_content: JustifyContent::SpaceBetween, ..default() },
            ChildOf(panel),
        ))
        .id();
    commands.queue_spawn_scene(text("", size::APP_TITLE)).insert((ProjectTitle, ChildOf(head)));
    commands
        .queue_spawn_scene(bsn! { @button("Edit", Some(Icon::Pencil), ButtonKind::Normal) })
        .insert((EditProject, ChildOf(head)));
    // The body and its scrollbar share a row, so the bar runs the body's height.
    let frame = commands
        .spawn((
            Node { flex_grow: 1.0, min_height: px(0), column_gap: px(space::CONTROLS), ..default() },
            ChildOf(panel),
        ))
        .id();
    let body = commands
        .spawn((
            Node { flex_grow: 1.0, min_width: px(0), min_height: px(0), overflow: Overflow::scroll_y(), ..default() },
            ScrollDown,
            ChildOf(frame),
        ))
        .id();
    commands.spawn((
        Node {
            display: Display::Grid,
            width: percent(100),
            align_self: AlignSelf::FlexStart,
            // As many columns of at least a board column's width as fit, sharing what is left over.
            grid_template_columns: vec![RepeatedGridTrack::minmax(
                GridTrackRepetition::AutoFill,
                MinTrackSizingFunction::Px(COLUMN_WIDTH),
                MaxTrackSizingFunction::Fraction(1.0),
            )],
            align_items: AlignItems::Start,
            row_gap: px(space::CARDS),
            column_gap: px(space::CARDS),
            ..default()
        },
        ProjectGrid,
        ChildOf(body),
    ));
    commands.queue_spawn_scene(widgets::scrollbar(body, ControlOrientation::Vertical)).insert(ChildOf(frame));
}

fn despawn_page(mut commands: Commands, panel: Query<Entity, With<ProjectPanel>>) {
    for e in &panel {
        commands.entity(e).despawn();
    }
}

/// "TRAIL · Trailhead", with "(archived)" after an archived one, in place.
fn update_title(view: Res<ProjectView>, mut titles: Query<&mut Text, With<ProjectTitle>>) {
    let mut words = if view.name.is_empty() { view.key.clone() } else { format!("{} · {}", view.key, view.name) };
    if view.archived {
        words.push_str(" (archived)");
    }
    for mut title in &mut titles {
        if title.0 != words {
            title.0.clone_from(&words);
        }
    }
}

/// Draws the "No tasks" line and the "Show more" button when they appear, and changes the button's caption in place
/// when its count does.
fn dress_slots(
    empty: Query<Entity, Added<ProjectEmpty>>,
    more: Query<(Entity, &ProjectMore, Ref<ProjectMore>)>,
    children: Query<&Children>,
    mut texts: Query<&mut Text>,
    mut commands: Commands,
) {
    for entity in &empty {
        commands.entity(entity).queue_apply_scene(text_dim("No tasks in this project", size::SECONDARY));
    }
    for (entity, ProjectMore(next), changed) in &more {
        let label = format!("Show {next} more");
        if changed.is_added() {
            commands.entity(entity).queue_apply_scene(button(label, None, ButtonKind::Normal));
        } else if changed.is_changed() {
            for e in children.iter_descendants(entity) {
                if let Ok(text) = texts.get_mut(e) {
                    set_text(text, &label);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use bevy::state::app::StatesPlugin;
    use serde_json::{Value, json};

    use super::*;
    use crate::board::BoardPlugin;
    use crate::fixture::{project_json, sid, task_json};
    use crate::mirror::MirrorPlugin;
    use crate::sync::{Snapshot, SyncItem};

    fn task(id: &str, project: &str, status: &str, parent: Option<&str>) -> Value {
        let mut t = json!({"id": id, "key": id.to_uppercase(), "title": id, "project_id": project, "status": status,
                           "priority": 2});
        if let Some(p) = parent {
            t["parent_id"] = json!(sid(p));
        }
        task_json(t)
    }

    fn app(tasks: Vec<Value>) -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin)).add_plugins((MirrorPlugin, BoardPlugin, ProjectPlugin));
        app.init_state::<View>();
        let projects = vec![
            project_json(json!({"id": "p1", "key": "TRAIL", "name": "Trailhead"})),
            project_json(json!({"id": "p2", "key": "OPS"})),
        ];
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot { at: 1, projects, tasks, ..Default::default() }));
        app.world_mut().spawn(ProjectGrid);
        app.world_mut().trigger(OpenProject("TRAIL".into()));
        settle(&mut app);
        app
    }

    fn settle(app: &mut App) {
        for _ in 0..3 {
            app.update();
        }
    }

    fn cards(app: &mut App) -> HashMap<String, Entity> {
        let mut q = app.world_mut().query::<(Entity, &BoardCard)>();
        q.iter(app.world()).map(|(e, c)| (c.0.key.clone(), e)).collect()
    }

    #[test]
    fn opening_a_project_shows_its_page_with_all_its_tasks_flat() {
        let mut app = app(vec![
            task("t1", "p1", "open", None),
            task("t2", "p1", "open", Some("t1")),
            task("t3", "p1", "done", None),
            task("t4", "p2", "open", None),
        ]);
        assert_eq!(*app.world().resource::<State<View>>().get(), View::Project);
        let view = app.world().resource::<ProjectView>();
        assert_eq!((view.key.as_str(), view.name.as_str()), ("TRAIL", "Trailhead"));
        let keys: Vec<&str> = view.cards.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(keys, ["T1", "T2", "T3"], "closed tasks too, and nothing from OPS");
        let sub = &view.cards[1];
        assert_eq!((sub.depth, sub.parent.as_deref()), (0, Some("T1")), "a subtask is its own card naming its parent");
        assert_eq!(cards(&mut app).len(), 3);
    }

    #[test]
    fn changing_one_task_leaves_every_other_card_as_it_was() {
        let mut app = app(vec![task("t1", "p1", "open", None), task("t2", "p1", "in_progress", None)]);
        let before = cards(&mut app);
        app.world_mut().write_message(crate::fixture::sync_event(json!({
            "id": 9, "entity": "task", "entity_id": sid("t1"), "kind": "task.updated",
            "after": task_json(json!({"id": "t1", "key": "T1", "title": "renamed", "project_id": "p1", "status": "open",
                      "priority": 2, "version": 2})),
        })));
        settle(&mut app);
        let after = cards(&mut app);
        assert_eq!(before, after, "the same entities, one of them redrawn");
        assert_eq!(app.world().get::<BoardCard>(after["T1"]).unwrap().0.title, "renamed");
    }

    #[test]
    fn a_big_project_builds_a_page_and_shows_more_on_request() {
        let tasks = (0..PAGE + 5).map(|i| task(&format!("t{i}"), "p1", "open", None)).collect();
        let mut app = app(tasks);
        assert_eq!(cards(&mut app).len(), PAGE);
        let more = app.world_mut().query_filtered::<Entity, With<ProjectMore>>().single(app.world()).unwrap();
        assert_eq!(app.world().get::<ProjectMore>(more), Some(&ProjectMore(5)));
        app.world_mut().trigger(Activate { entity: more });
        settle(&mut app);
        assert_eq!(cards(&mut app).len(), PAGE + 5);
        assert_eq!(app.world_mut().query::<&ProjectMore>().iter(app.world()).count(), 0);
    }

    #[test]
    fn a_project_without_tasks_says_so() {
        let mut app = app(vec![task("t4", "p2", "open", None)]);
        assert_eq!(cards(&mut app).len(), 0);
        assert_eq!(app.world_mut().query::<&ProjectEmpty>().iter(app.world()).count(), 1);
    }
}
