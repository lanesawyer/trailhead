//! One task's subtasks as a graph, by what depends on what (`docs/app-platform.md`, "The graph"): a second way to
//! look at work beside the board, read-only for now.
//!
//! [`OpenGraph`] shows it for a task from anywhere: a card's tool button, and the task modal's header, offer it on a
//! task with subtasks ([`OpensGraph`]). [`build_graph`] is a pure function from the mirror to the [`GraphView`]: a
//! node per direct subtask, each the board's [`Card`] with its status and grade, an edge for each `blocked_by` between
//! two of them (blocker to the left), and a "needs TRAIL-12" badge on a node for each task outside the parent it waits
//! on. [`GraphPlugin`] keeps the view from the mirror; [`GraphUiPlugin`] draws it in [`View::Graph`] on the kit's
//! graph, whose canvas pans and zooms, with the cards drawn and opened in the inspector as on the board. A change in
//! the mirror (a status, a new subtask, a dependency) updates the nodes in place and keeps the pan and zoom.
//!
//! Left for what comes next: a grade dropdown per node goes in [`PlanNode`] (it carries the grade) and the card's
//! tools row; an Approve button goes in the page's header beside Fit (`dress_header`); editing the plan (adding,
//! removing or splitting a node, adding or removing an edge) answers presses on the canvas and the nodes, and changes
//! the server, from which the mirror brings the graph back.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use bevy::ui_widgets::{Activate, RequestClose};
use jiff::Timestamp;
use trailhead_model::{Grade, Status, key_number};

use crate::board::{Badge, BadgeKind, BoardCard, Card, Filter, Input, Sources, View, build_with, next_expiry};
use crate::inspector::Selection;
use crate::widgets::{
    self, ButtonKind, COLUMN_WIDTH, Canvas, Graph, GraphEdge, GraphNode, GraphNodeBox, Icon, PanelTitle, PanelTools,
    TITLE_LINES, UiPlace, extent, set_display, set_text, size, space, spawn_graph, status_label, status_tone, stroke,
    text_dim, token, tool_button, width_of,
};

/// Shows the graph of the subtasks of the task with this id.
#[derive(Event, Debug, Clone, PartialEq, Eq)]
pub struct OpenGraph(pub String);

/// On a button that opens the graph of a task's subtasks (by id), closing the inspector if it is open.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct OpensGraph(pub String);

/// Whose subtasks the graph shows, by id.
#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct GraphPage(pub Option<String>);

/// What the graph shows.
#[derive(Resource, Default, Debug, Clone, PartialEq)]
pub struct GraphView {
    /// The parent's key and title; none when it isn't in the mirror.
    pub parent: Option<(String, String)>,
    /// A node per direct subtask, by key.
    pub nodes: Vec<PlanNode>,
    /// Each `blocked_by` between two of them, by id: blocker, then blocked.
    pub edges: Vec<(String, String)>,
}

impl GraphView {
    fn node(&self, id: &str) -> Option<&PlanNode> {
        self.nodes.iter().find(|n| n.card.id == id)
    }
}

/// A subtask in the graph: its card, what it shows, and how big its box is.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanNode {
    pub card: Card,
    pub status: Status,
    pub grade: Option<Grade>,
    /// The keys of the tasks outside the parent it waits on, with whether each is finished; "a task out of view" when
    /// the token can't see it.
    pub needs: Vec<(String, bool)>,
    pub size: Vec2,
}

/// The most "needs" badges a node shows before the rest are counted in one.
const NEEDS_SHOWN: usize = 2;
/// A node's width: a card in a board column.
const NODE_WIDTH: f32 = COLUMN_WIDTH - space::PANEL_INSET * 2.0;
/// Bevy's default line height, as a multiple of the font size.
const LINE_HEIGHT: f32 = 1.2;

/// The graph of the subtasks of the task `parent` (an id): only its direct children are nodes.
pub fn build_graph(input: Input, parent: &str) -> GraphView {
    let Some(task) = input.tasks.iter().find(|t| t.id.to_string() == parent) else { return GraphView::default() };
    let filter = Filter { parent: Some(task.key.0.clone()), closed: true, ..default() };
    let mut cards: Vec<Card> = build_with(input, &filter, false).columns.into_iter().flat_map(|c| c.cards).collect();
    cards.sort_by(|a, b| (key_number(&a.key), &a.key).cmp(&(key_number(&b.key), &b.key)));
    let children: HashSet<String> = cards.iter().map(|c| c.id.clone()).collect();
    let tasks: HashMap<String, &trailhead_model::Task> = input.tasks.iter().map(|t| (t.id.to_string(), t)).collect();
    let mut edges = Vec::new();
    let mut needs: HashMap<String, Vec<(String, bool)>> = HashMap::new();
    for d in input.dependencies {
        let (blocker, blocked) = (d.blocker_id.to_string(), d.blocked_id.to_string());
        if !children.contains(&blocked) {
            continue;
        }
        if children.contains(&blocker) {
            edges.push((blocker, blocked));
        } else {
            let need = match tasks.get(&blocker) {
                Some(t) => (t.key.0.clone(), t.status.is_closed()),
                None => ("a task out of view".to_string(), false),
            };
            needs.entry(blocked).or_default().push(need);
        }
    }
    let order = |id: &str| tasks.get(id).map(|t| (key_number(t.key.as_str()), t.key.0.clone()));
    edges.sort_by_key(|(from, to)| (order(from), order(to)));
    edges.dedup();
    let nodes = cards
        .into_iter()
        .filter_map(|card| {
            let task = tasks.get(&card.id)?;
            let mut needs = needs.remove(&card.id).unwrap_or_default();
            needs.sort_by(|a, b| (key_number(&a.0), &a.0).cmp(&(key_number(&b.0), &b.0)));
            Some(plan_node(card, task.status, task.grade, needs))
        })
        .collect();
    GraphView { parent: Some((task.key.0.clone(), task.title.clone())), nodes, edges }
}

/// A subtask's node: the board's card for it, its status first among the badges, its grade, what it needs from
/// outside, and none of the card's other badges or tools but the one that opens its own subtasks' graph.
fn plan_node(card: Card, status: Status, grade: Option<Grade>, needs: Vec<(String, bool)>) -> PlanNode {
    let mut badges = vec![Badge {
        kind: BadgeKind::Status,
        label: status_label(status).into(),
        tone: Some(status_tone(status)),
        hint: None,
    }];
    badges.extend(card.badges.iter().filter(|b| matches!(b.kind, BadgeKind::Grade | BadgeKind::NeedsGrade)).cloned());
    for (key, done) in needs.iter().take(NEEDS_SHOWN) {
        let hint = if *done {
            format!("Waits on {key}, outside this graph: finished")
        } else {
            format!("Waits on {key}, outside this graph")
        };
        let tone = (!done).then_some(token::STATUS_WARN);
        badges.push(Badge { kind: BadgeKind::Needs, label: format!("needs {key}"), tone, hint: Some(hint) });
    }
    if needs.len() > NEEDS_SHOWN {
        let rest: Vec<&str> = needs[NEEDS_SHOWN..].iter().map(|(k, _)| k.as_str()).collect();
        badges.push(Badge {
            kind: BadgeKind::Needs,
            label: format!("+{} more", rest.len()),
            tone: Some(token::STATUS_WARN),
            hint: Some(format!("Also waits on {}", rest.join(", "))),
        });
    }
    let card =
        Card { badges, actions: Vec::new(), parent: None, depth: 0, found: None, pr: None, holder: None, ..card };
    let size = Vec2::new(NODE_WIDTH, node_height(&card));
    PlanNode { card, status, grade, needs, size }
}

/// How tall a node's card comes out at most: its key, a title of two lines, its badges wrapped to its width, and its
/// tool, by the kit's estimate of a glyph's width. A card shorter than its box sits in the box's middle.
fn node_height(card: &Card) -> f32 {
    let badge_height = size::SMALL.px() * LINE_HEIGHT + space::BADGE_INSET * 2.0;
    let room = NODE_WIDTH - (space::CONTROL_INSET + stroke::HAIRLINE) * 2.0;
    let mut rows = 1.0;
    let mut used = 0.0;
    for badge in &card.badges {
        let width = width_of(badge.label.chars().count(), size::SMALL) + space::ICON_LABEL * 2.0;
        if used > 0.0 && used + space::CONTROLS + width > room {
            rows += 1.0;
            used = width;
        } else {
            used += if used > 0.0 { space::CONTROLS + width } else { width };
        }
    }
    let tools = if card.graph { extent::CONTROL_HEIGHT } else { 0.0 };
    (space::CONTROL_INSET + stroke::HAIRLINE) * 2.0
        + badge_height
        + size::CARD_TITLE.px() * LINE_HEIGHT * TITLE_LINES
        + badge_height * rows
        + space::CONTROLS * (rows - 1.0)
        + space::STACKED * 3.0
        + tools
}

/// Keeps the [`GraphView`] from the mirror. Needs the board's [`View`] state and
/// [`BoardPlugin`](crate::board::BoardPlugin), whose resources it builds from too.
pub struct GraphPlugin;

impl Plugin for GraphPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GraphPage>()
            .init_resource::<GraphView>()
            .add_observer(on_open)
            .add_observer(on_opens_graph)
            .add_systems(Update, rebuild_view);
    }
}

fn on_open(open: On<OpenGraph>, mut page: ResMut<GraphPage>, mut next: ResMut<NextState<View>>) {
    if page.0.as_deref() == Some(open.0.as_str()) {
        next.set_if_different(View::Graph);
        return;
    }
    page.0 = Some(open.0.clone());
    // Entering again draws the page anew, so another task's graph starts fitted.
    next.set(View::Graph);
}

/// A button that opens a graph was pressed: close the inspector, if it is what holds it, and show the graph.
fn on_opens_graph(
    activate: On<Activate>,
    buttons: Query<&OpensGraph>,
    mut selection: ResMut<Selection>,
    mut commands: Commands,
) {
    let Ok(OpensGraph(id)) = buttons.get(activate.entity) else { return };
    if id.is_empty() {
        return;
    }
    selection.0 = None;
    commands.trigger(OpenGraph(id.clone()));
}

fn rebuild_view(
    page: Res<GraphPage>,
    mut sources: Sources,
    mut view: ResMut<GraphView>,
    mut expiry: Local<Option<Timestamp>>,
) {
    let changed = sources.changed();
    let now = Timestamp::now();
    let expired = expiry.is_some_and(|at| at <= now);
    if !(changed || page.is_changed() || expired) {
        return;
    }
    let Some(parent) = &page.0 else { return };
    let rows = sources.rows();
    *expiry = next_expiry(&rows.claims, now);
    let next = build_graph(rows.input(&[], now), parent);
    if *view != next {
        *view = next;
    }
}

/// Draws the graph while in [`View::Graph`].
pub struct GraphUiPlugin;

impl Plugin for GraphUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(GraphPlugin)
            .add_systems(OnEnter(View::Graph), spawn_page)
            .add_systems(OnExit(View::Graph), despawn_page)
            .add_observer(on_close)
            .add_observer(on_fit)
            .add_systems(
                Update,
                ((set_graph, update_title, dress_header, show_empty).before(UiPlace), fill_nodes.after(UiPlace))
                    .after(rebuild_view)
                    .run_if(in_state(View::Graph)),
            );
    }
}

/// The page, which a press on its header's X closes back to the board.
#[derive(Component)]
pub struct GraphPanel;

/// The graph's root on the page: the kit's canvas, holding the [`Graph`].
#[derive(Component)]
pub struct PlanGraph;

/// The header's Fit button.
#[derive(Component)]
struct FitGraph;

/// The line shown in place of a graph with no nodes.
#[derive(Component)]
struct GraphEmpty;

fn spawn_page(mut commands: Commands) {
    let panel = commands.spawn((Name::new("graph"), GraphPanel, widgets::BesideSidebar, widgets::page_node())).id();
    commands.spawn_scene(widgets::panel_header("")).insert(ChildOf(panel));
    commands.spawn_scene(text_dim("No subtasks", size::BODY)).insert((
        GraphEmpty,
        Node { display: Display::None, ..default() },
        ChildOf(panel),
    ));
    let parts = spawn_graph(&mut commands);
    commands.entity(parts.root).insert((PlanGraph, ChildOf(panel)));
}

fn despawn_page(mut commands: Commands, panel: Query<Entity, With<GraphPanel>>) {
    for e in &panel {
        commands.entity(e).despawn();
    }
}

/// The header's X: back to the board.
fn on_close(close: On<RequestClose>, panels: Query<(), With<GraphPanel>>, mut next: ResMut<NextState<View>>) {
    if panels.contains(close.event_target()) {
        next.set(View::Board);
    }
}

/// The header's Fit: the whole graph into view.
fn on_fit(activate: On<Activate>, fits: Query<(), With<FitGraph>>, mut canvases: Query<&mut Canvas, With<PlanGraph>>) {
    if fits.contains(activate.entity) {
        for mut canvas in &mut canvases {
            canvas.fit();
        }
    }
}

/// Puts the Fit button before the header's X once the header has spawned.
fn dress_header(
    tools: Query<(Entity, &ChildOf), Added<PanelTools>>,
    parents: Query<&ChildOf>,
    panels: Query<(), With<GraphPanel>>,
    mut commands: Commands,
) {
    for (row, header) in &tools {
        if !parents.get(header.parent()).is_ok_and(|p| panels.contains(p.parent())) {
            continue;
        }
        let fit = commands
            .spawn_scene(tool_button(Icon::Fit, "Fit the whole graph in view", ButtonKind::Plain))
            .insert(FitGraph)
            .id();
        commands.entity(row).insert_children(0, &[fit]);
    }
}

/// "Subtasks of TRAIL-12 · its title", in place.
fn update_title(
    view: Res<GraphView>,
    panels: Query<(), With<GraphPanel>>,
    parents: Query<&ChildOf>,
    mut titles: Query<(Entity, &mut Text), With<PanelTitle>>,
) {
    let words = match &view.parent {
        Some((key, title)) => format!("Subtasks of {key} · {title}"),
        None => "Subtasks".to_string(),
    };
    for (title, text) in &mut titles {
        if parents.iter_ancestors(title).any(|e| panels.contains(e)) {
            set_text(text, &words);
        }
    }
}

/// Says so when there are no subtasks to draw.
fn show_empty(view: Res<GraphView>, empty: Query<Entity, With<GraphEmpty>>, mut nodes: Query<&mut Node>) {
    for line in &empty {
        set_display(&mut nodes, line, view.nodes.is_empty());
    }
}

/// Gives the kit's graph the nodes and edges to lay out: a change of what a node shows changes nothing here.
fn set_graph(view: Res<GraphView>, mut graphs: Query<&mut Graph, With<PlanGraph>>) {
    for mut graph in &mut graphs {
        graph.set_if_neq(Graph {
            nodes: view.nodes.iter().map(|n| GraphNode::new(n.card.id.clone(), n.size)).collect(),
            edges: view.edges.iter().map(|(from, to)| GraphEdge::new(from.clone(), to.clone())).collect(),
        });
    }
}

/// Draws each new node's card in its box, and gives a drawn card its new view model when it changed; the board's
/// card systems keep what it shows in step, in place.
fn fill_nodes(
    view: Res<GraphView>,
    boxes: Query<(Entity, &GraphNodeBox, Option<&Children>)>,
    added: Query<(), Added<GraphNodeBox>>,
    graphs: Query<(), With<PlanGraph>>,
    cards: Query<&BoardCard>,
    mut commands: Commands,
) {
    if !view.is_changed() && added.is_empty() {
        return;
    }
    for (entity, node, children) in &boxes {
        let Some(wanted) = view.node(&node.key).filter(|_| graphs.contains(node.graph)) else { continue };
        let drawn = children.and_then(|c| c.iter().find(|e| cards.contains(*e)));
        match drawn {
            Some(card) if cards.get(card).is_ok_and(|c| c.0 != wanted.card) => {
                commands.entity(card).insert(BoardCard(wanted.card.clone()));
            }
            Some(_) => {}
            None => {
                commands.spawn((BoardCard(wanted.card.clone()), ChildOf(entity)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use bevy::state::app::StatesPlugin;
    use serde_json::{Value, json};
    use trailhead_model::{Dependency, Task};
    use trailhead_shell::fixture::{project_json, sid, task_json};
    use trailhead_shell::mirror::MirrorPlugin;
    use trailhead_shell::sync::{Snapshot, SyncItem};
    use uuid::Uuid;

    use super::*;
    use crate::board::BoardPlugin;

    fn task(id: &str, key: &str, status: &str, parent: Option<&str>) -> Value {
        let mut t = json!({"id": id, "key": key, "title": format!("task {key}"), "project_id": "p1",
                           "status": status, "priority": 2});
        if let Some(p) = parent {
            t["parent_id"] = json!(sid(p));
        }
        task_json(t)
    }

    fn typed(tasks: Vec<Value>) -> Vec<Task> {
        tasks.into_iter().map(|t| serde_json::from_value(t).unwrap()).collect()
    }

    fn dep(blocker: &str, blocked: &str) -> Dependency {
        let id = |s: &str| Uuid::parse_str(&sid(s)).unwrap();
        Dependency { workspace_id: Uuid::nil(), blocker_id: id(blocker), blocked_id: id(blocked), created_at: at() }
    }

    fn at() -> Timestamp {
        "2026-11-01T00:00:00Z".parse().unwrap()
    }

    fn graph(tasks: &[Task], deps: &[Dependency], parent: &str) -> GraphView {
        build_graph(Input { tasks, dependencies: deps, now: at(), ..Default::default() }, &sid(parent))
    }

    fn keys(view: &GraphView) -> Vec<&str> {
        view.nodes.iter().map(|n| n.card.key.as_str()).collect()
    }

    fn edge_keys(view: &GraphView) -> Vec<(String, String)> {
        let key = |id: &str| view.node(id).unwrap().card.key.clone();
        view.edges.iter().map(|(a, b)| (key(a), key(b))).collect()
    }

    fn pair(a: &str, b: &str) -> (String, String) {
        (a.to_string(), b.to_string())
    }

    #[test]
    fn a_chain_and_a_diamond_under_one_parent_are_its_nodes_and_edges() {
        let tasks = typed(vec![
            task("p", "T-1", "open", None),
            task("a", "T-2", "open", Some("p")),
            task("b", "T-3", "open", Some("p")),
            task("c", "T-4", "open", Some("p")),
            task("d", "T-5", "open", Some("p")),
            task("e", "T-6", "open", Some("p")),
        ]);
        // A chain a -> b, and a diamond b -> c, b -> d, c -> e, d -> e.
        let deps = [dep("a", "b"), dep("b", "c"), dep("b", "d"), dep("c", "e"), dep("d", "e")];
        let view = graph(&tasks, &deps, "p");
        assert_eq!(view.parent, Some(("T-1".into(), "task T-1".into())));
        assert_eq!(keys(&view), ["T-2", "T-3", "T-4", "T-5", "T-6"]);
        assert_eq!(
            edge_keys(&view),
            [pair("T-2", "T-3"), pair("T-3", "T-4"), pair("T-3", "T-5"), pair("T-4", "T-6"), pair("T-5", "T-6")]
        );
        assert!(view.nodes.iter().all(|n| n.needs.is_empty()));
    }

    #[test]
    fn a_subtask_blocked_by_a_task_outside_the_parent_shows_it_as_a_need_not_a_node() {
        let tasks = typed(vec![
            task("p", "T-1", "open", None),
            task("a", "T-2", "open", Some("p")),
            task("x", "T-9", "in_progress", None),
            task("y", "T-8", "done", None),
        ]);
        let view = graph(&tasks, &[dep("x", "a"), dep("y", "a")], "p");
        assert_eq!(keys(&view), ["T-2"]);
        assert!(view.edges.is_empty());
        let node = &view.nodes[0];
        assert_eq!(node.needs, [("T-8".to_string(), true), ("T-9".to_string(), false)]);
        let needs: Vec<(&str, Option<_>)> = node
            .card
            .badges
            .iter()
            .filter(|b| b.kind == BadgeKind::Needs)
            .map(|b| (b.label.as_str(), b.tone.clone()))
            .collect();
        assert_eq!(needs, [("needs T-8", None), ("needs T-9", Some(token::STATUS_WARN))], "finished ones are dim");
    }

    #[test]
    fn a_done_subtask_is_a_node_showing_its_status_and_keeps_its_edges() {
        let mut tasks = vec![
            task("p", "T-1", "open", None),
            task("a", "T-2", "done", Some("p")),
            task("b", "T-3", "open", Some("p")),
        ];
        tasks[1]["grade"] = json!(2);
        let view = graph(&typed(tasks), &[dep("a", "b")], "p");
        assert_eq!(keys(&view), ["T-2", "T-3"]);
        let done = &view.nodes[0];
        assert_eq!((done.status, done.grade.map(|g| g.get())), (Status::Done, Some(2)));
        let first = &done.card.badges[0];
        assert_eq!((first.kind, first.label.as_str()), (BadgeKind::Status, status_label(Status::Done)));
        assert_eq!(first.tone, Some(status_tone(Status::Done)));
        assert!(done.card.badges.iter().any(|b| b.kind == BadgeKind::Grade && b.label == "G2"));
        assert!(done.card.actions.is_empty(), "a node is a small card: no edit or comment tools");
        assert_eq!(edge_keys(&view), [pair("T-2", "T-3")]);
    }

    #[test]
    fn only_direct_children_are_nodes_and_one_with_its_own_offers_its_graph() {
        let tasks = typed(vec![
            task("p", "T-1", "open", None),
            task("a", "T-2", "open", Some("p")),
            task("aa", "T-3", "open", Some("a")),
            task("b", "T-4", "open", Some("p")),
        ]);
        // A grandchild blocking a child is outside the graph, as any other task.
        let view = graph(&tasks, &[dep("aa", "b")], "p");
        assert_eq!(keys(&view), ["T-2", "T-4"]);
        assert!(view.edges.is_empty());
        assert_eq!(view.nodes[1].needs, [("T-3".to_string(), false)]);
        assert!(view.nodes[0].card.graph && !view.nodes[1].card.graph);
        assert!(view.nodes[0].size.y > view.nodes[1].size.y, "room for its tool");
    }

    #[test]
    fn a_parent_not_in_the_mirror_has_an_empty_graph() {
        let tasks = typed(vec![task("a", "T-2", "open", None)]);
        assert_eq!(graph(&tasks, &[], "p"), GraphView::default());
    }

    #[test]
    fn many_needs_are_counted_after_the_first_few() {
        let card = graph(&typed(vec![task("p", "T-1", "open", None)]), &[], "p");
        assert!(card.nodes.is_empty());
        let needs: Vec<(String, bool)> = (10..14).map(|n| (format!("T-{n}"), false)).collect();
        let base = Card {
            id: "a".into(),
            key: "T-2".into(),
            title: "a".into(),
            parent: None,
            depth: 0,
            badges: vec![],
            actions: vec![],
            found: None,
            pr: None,
            holder: None,
            graph: false,
        };
        let node = plan_node(base, Status::Open, None, needs);
        let labels: Vec<&str> = node.card.badges.iter().map(|b| b.label.as_str()).collect();
        assert_eq!(labels, [status_label(Status::Open), "needs T-10", "needs T-11", "+2 more"]);
        assert_eq!(node.card.badges[3].hint.as_deref(), Some("Also waits on T-12, T-13"));
    }

    /// The graph's logic and the kit's layout, headless: the view from the mirror, the boxes, and the cards in them.
    fn app(tasks: Vec<Value>) -> (App, Entity) {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin)).add_plugins((MirrorPlugin, BoardPlugin, GraphPlugin));
        app.add_plugins(crate::widgets::CanvasPlugin);
        app.insert_state(trailhead_shell::ActiveApp(crate::LOOKOUT)).add_sub_state::<View>();
        app.add_systems(Update, (set_graph.before(UiPlace), fill_nodes.after(UiPlace)).after(rebuild_view));
        let projects = vec![project_json(json!({"id": "p1", "key": "T", "name": "T"}))];
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot { at: 1, projects, tasks, ..Default::default() }));
        let mut queue = bevy::ecs::world::CommandQueue::default();
        let mut commands = Commands::new(&mut queue, app.world_mut());
        let parts = spawn_graph(&mut commands);
        commands.entity(parts.root).insert(PlanGraph);
        queue.apply(app.world_mut());
        app.world_mut().trigger(OpenGraph(sid("p")));
        settle(&mut app);
        (app, parts.root)
    }

    fn settle(app: &mut App) {
        for _ in 0..4 {
            app.update();
        }
    }

    /// Each node's box and the card drawn in it, by key.
    fn drawn(app: &mut App) -> HashMap<String, (Entity, Entity)> {
        let mut q = app.world_mut().query::<(Entity, &BoardCard, &ChildOf)>();
        q.iter(app.world()).map(|(card, c, parent)| (c.0.key.clone(), (parent.parent(), card))).collect()
    }

    fn update(app: &mut App, id: &str, key: &str, status: &str, version: i64) {
        app.world_mut().write_message(trailhead_shell::fixture::sync_event(json!({
            "id": 10 + version, "entity": "task", "entity_id": sid(id), "kind": "task.updated",
            "after": task_json(json!({"id": id, "key": key, "title": format!("task {key}"), "project_id": "p1",
                "status": status, "priority": 2, "parent_id": sid("p"), "version": version})),
        })));
        settle(app);
    }

    #[test]
    fn opening_the_graph_draws_a_card_in_a_box_per_subtask() {
        let (mut app, _) = app(vec![
            task("p", "T-1", "open", None),
            task("a", "T-2", "open", Some("p")),
            task("b", "T-3", "open", Some("p")),
        ]);
        assert_eq!(*app.world().resource::<State<View>>().get(), View::Graph);
        let drawn = drawn(&mut app);
        assert_eq!(drawn.len(), 2);
        for (box_entity, _) in drawn.values() {
            assert!(app.world().get::<GraphNodeBox>(*box_entity).is_some(), "each card sits in a node's box");
        }
    }

    #[test]
    fn a_change_in_the_mirror_updates_the_graph_in_place_and_keeps_the_view() {
        let (mut app, root) = app(vec![
            task("p", "T-1", "open", None),
            task("a", "T-2", "open", Some("p")),
            task("b", "T-3", "open", Some("p")),
        ]);
        app.world_mut().get_mut::<Canvas>(root).unwrap().set(Vec2::new(40.0, -12.0), 0.75);
        let before = drawn(&mut app);
        // A status changes: the same box and card, showing it.
        update(&mut app, "b", "T-3", "in_progress", 2);
        let after = drawn(&mut app);
        assert_eq!(after, before);
        let status = &app.world().get::<BoardCard>(after["T-3"].1).unwrap().0.badges[0].label;
        assert_eq!(status, status_label(Status::InProgress));
        // A new subtask: one more box, and the others are where they were.
        app.world_mut().write_message(trailhead_shell::fixture::sync_event(json!({
            "id": 30, "entity": "task", "entity_id": sid("c"), "kind": "task.created",
            "after": task("c", "T-4", "open", Some("p")),
        })));
        settle(&mut app);
        let grown = drawn(&mut app);
        assert_eq!(grown.len(), 3);
        for key in ["T-2", "T-3"] {
            assert_eq!(grown[key], before[key], "{key} kept its box and card");
        }
        let view = app.world().get::<Canvas>(root).unwrap();
        assert_eq!((view.pan(), view.zoom()), (Vec2::new(40.0, -12.0), 0.75), "the pan and zoom held");
    }
}
