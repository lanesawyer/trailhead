//! A directed graph laid out on a [`canvas`](crate::spawn_canvas): nodes in layers by what points at what, left to
//! right, with each edge drawn as lines between the boxes, turning at right angles.
//!
//! [`spawn_graph`] makes the canvas and two layers on it, the edges under the nodes. The caller says what the graph
//! holds with a [`Graph`] on its root: each node's key and size, and the edges between keys. The kit lays it out
//! ([`lay_out_graph`], a pure function, below) whenever the `Graph` changes, and keeps one box per key, a
//! [`GraphNodeBox`]: the caller draws its own content in each (an app's card), as a child that fills the box's width
//! and is centered down it. A box keeps its `Entity` for as long as its key is in the graph, whatever else changes,
//! so what the caller drew in it stays; a change of what a node shows is the caller's to make in place, and needs no
//! change to the `Graph` unless its size changes. Edges are thin nodes in [`token::BORDER`], [`stroke::EDGE`] thick,
//! with a small square where each meets the node it points to; they are kept in step the same way.
//!
//! The pan and zoom are the canvas's ([`Canvas`](crate::Canvas)), apart from the graph, so they hold through every change. The
//! canvas fits the graph when it first has nodes; a caller's "fit all" calls [`Canvas::fit`](crate::Canvas::fit) on
//! the root.

use bevy::picking::Pickable;
use bevy::prelude::*;

use crate::canvas::{CanvasExtent, CanvasParts, spawn_canvas};
use crate::{BackgroundTone, Keyed, extent, stroke, token};

mod layout;

pub use layout::{EdgeRoute, GraphEdge, GraphLayout, GraphNode, lay_out_graph};

/// What a graph holds: its nodes, by key and size, and the edges between them. Set it on the graph's root (with
/// `set_if_neq`, so an unchanged graph isn't laid out again); the order of the nodes breaks ties in the layout, so
/// keep it steady (by key, say) rather than by something that changes often.
#[derive(Component, Clone, Debug, Default, PartialEq)]
pub struct Graph {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

/// The box of one node of a graph, placed and sized by the kit, for the caller to draw the node in: its content goes
/// in as a child, fills its width and sits in the middle of its height.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct GraphNodeBox {
    /// The graph's root.
    pub graph: Entity,
    /// The node's key.
    pub key: String,
}

/// A line or end mark of a graph's edge, which the kit keeps; for a test to find.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct GraphEdgeLine;

/// A graph's parts, from [`spawn_graph`].
#[derive(Clone, Copy, Debug)]
pub struct GraphParts {
    /// The canvas's box, holding [`Graph`] and [`Canvas`](crate::Canvas): put it where it goes and size it.
    pub root: Entity,
    /// The layer the node boxes are kept in, which holds nothing else.
    pub nodes: Entity,
    /// The layer the edges are kept in, under the nodes.
    pub edges: Entity,
}

/// The entities a graph keeps, by key.
#[derive(Component)]
pub(crate) struct GraphDrawn {
    nodes: Keyed<Rect>,
    lines: Keyed<Rect>,
    node_layer: Entity,
    edge_layer: Entity,
}

/// A graph on a canvas of its own, empty until its [`Graph`] is set, spawned unparented.
pub fn spawn_graph(commands: &mut Commands) -> GraphParts {
    let CanvasParts { root, content } = spawn_canvas(commands);
    let layer = |name: &'static str| {
        (
            Name::new(name),
            Node { position_type: PositionType::Absolute, ..default() },
            Pickable::IGNORE,
            ChildOf(content),
        )
    };
    let edges = commands.spawn(layer("graph edges")).id();
    let nodes = commands.spawn(layer("graph nodes")).id();
    commands.entity(root).insert((
        Name::new("graph"),
        Graph::default(),
        GraphDrawn { nodes: Keyed::default(), lines: Keyed::default(), node_layer: nodes, edge_layer: edges },
    ));
    GraphParts { root, nodes, edges }
}

/// A node at `rect`, from its content's origin.
fn placed(rect: Rect) -> Node {
    Node {
        position_type: PositionType::Absolute,
        left: px(rect.min.x),
        top: px(rect.min.y),
        width: px(rect.width()),
        height: px(rect.height()),
        flex_direction: FlexDirection::Column,
        justify_content: JustifyContent::Center,
        ..default()
    }
}

/// The lines an edge is drawn with, by key: one per stretch between two of its points, [`stroke::EDGE`] thick and
/// reaching over the corners so the turns join, and a square at its end.
fn lines_of(route: &EdgeRoute) -> Vec<(String, Rect)> {
    let half = stroke::EDGE / 2.0;
    let key = |part: &str| format!("{}\u{0}{}\u{0}{part}", route.from, route.to);
    let mut lines: Vec<(String, Rect)> = route
        .points
        .windows(2)
        .enumerate()
        .map(|(at, pair)| (key(&at.to_string()), Rect::from_corners(pair[0], pair[1]).inflate(half)))
        .collect();
    if let [.., before, end] = route.points[..] {
        let toward = (end - before).normalize_or_zero();
        let size = extent::GRAPH_EDGE_END;
        lines.push((key("end"), Rect::from_center_size(end - toward * size / 2.0, Vec2::splat(size))));
    }
    lines
}

/// Lays out each graph whose [`Graph`] changed and brings its boxes and lines to the layout in place.
pub(crate) fn lay_out_graphs(
    mut graphs: Query<(Entity, &Graph, &mut GraphDrawn, &mut CanvasExtent), Changed<Graph>>,
    mut commands: Commands,
) {
    for (root, graph, mut drawn, mut extent) in &mut graphs {
        let layout = lay_out_graph(&graph.nodes, &graph.edges);
        let boxes: Vec<(String, Rect)> = graph
            .nodes
            .iter()
            .filter_map(|n| layout.nodes.get(&n.key).map(|r| (n.key.clone(), *r)))
            .fold(Vec::new(), |mut kept, (key, rect)| {
                if !kept.iter().any(|(k, _)| *k == key) {
                    kept.push((key, rect));
                }
                kept
            });
        let lines: Vec<(String, Rect)> = layout.edges.iter().flat_map(lines_of).collect();
        let GraphDrawn { nodes, lines: kept, node_layer, edge_layer } = &mut *drawn;
        nodes.sync(
            &mut commands,
            &[(*node_layer, boxes)],
            |commands, key, rect| {
                commands
                    .spawn((Name::new("graph node"), GraphNodeBox { graph: root, key: key.into() }, placed(*rect)))
                    .id()
            },
            |commands, entity, rect| {
                commands.entity(entity).insert(placed(*rect));
            },
        );
        kept.sync(
            &mut commands,
            &[(*edge_layer, lines)],
            |commands, _, rect| {
                let line = (GraphEdgeLine, placed(*rect), BackgroundTone(token::BORDER), Pickable::IGNORE);
                commands.spawn(line).id()
            },
            |commands, entity, rect| {
                commands.entity(entity).insert(placed(*rect));
            },
        );
        extent.set_if_neq(CanvasExtent(layout.size));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARD: Vec2 = Vec2::new(120.0, 60.0);

    fn app() -> (App, GraphParts) {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_systems(Update, lay_out_graphs);
        let mut queue = bevy::ecs::world::CommandQueue::default();
        let mut commands = Commands::new(&mut queue, app.world_mut());
        let parts = spawn_graph(&mut commands);
        queue.apply(app.world_mut());
        (app, parts)
    }

    fn set(app: &mut App, root: Entity, keys: &[&str], pairs: &[(&str, &str)]) {
        let graph = Graph {
            nodes: keys.iter().map(|k| GraphNode::new(*k, CARD)).collect(),
            edges: pairs.iter().map(|(f, t)| GraphEdge::new(*f, *t)).collect(),
        };
        app.world_mut().get_mut::<Graph>(root).unwrap().set_if_neq(graph);
        app.update();
    }

    fn boxes(app: &mut App) -> std::collections::HashMap<String, Entity> {
        let mut q = app.world_mut().query::<(Entity, &GraphNodeBox)>();
        q.iter(app.world()).map(|(e, b)| (b.key.clone(), e)).collect()
    }

    fn lines(app: &mut App) -> Vec<Entity> {
        let mut q = app.world_mut().query_filtered::<Entity, With<GraphEdgeLine>>();
        let mut all: Vec<Entity> = q.iter(app.world()).collect();
        all.sort();
        all
    }

    #[test]
    fn a_box_per_node_and_lines_per_edge_and_the_canvas_knows_the_size() {
        let (mut app, parts) = app();
        set(&mut app, parts.root, &["a", "b", "c"], &[("a", "b"), ("a", "c")]);
        assert_eq!(boxes(&mut app).len(), 3);
        let children = app.world().get::<Children>(parts.nodes).unwrap().len();
        assert_eq!(children, 3, "the boxes are in the node layer");
        assert!(!lines(&mut app).is_empty());
        let size = app.world().get::<CanvasExtent>(parts.root).unwrap().0;
        assert!(size.x > CARD.x * 2.0 && size.y >= CARD.y * 2.0, "{size}");
    }

    #[test]
    fn an_update_keeps_every_other_node_and_what_was_drawn_in_it() {
        let (mut app, parts) = app();
        set(&mut app, parts.root, &["a", "b", "c"], &[("a", "b"), ("b", "c")]);
        let before = boxes(&mut app);
        let drawn = app.world_mut().spawn((Text::new("card"), ChildOf(before["a"]))).id();
        let first_line = lines(&mut app)[0];
        // A node is added with an edge into it: the rest move, if they must, as the same entities.
        set(&mut app, parts.root, &["a", "b", "c", "d"], &[("a", "b"), ("b", "c"), ("a", "d")]);
        let after = boxes(&mut app);
        for key in ["a", "b", "c"] {
            assert_eq!(after[key], before[key], "{key} kept its box");
        }
        assert_eq!(app.world().get::<ChildOf>(drawn).map(ChildOf::parent), Some(before["a"]));
        assert!(lines(&mut app).contains(&first_line), "a line that is still drawn kept its entity");
        // A node is taken away with its edge: only its box goes.
        set(&mut app, parts.root, &["a", "b", "c"], &[("a", "b"), ("b", "c")]);
        let last = boxes(&mut app);
        assert_eq!(last.len(), 3);
        assert!(app.world().get_entity(after["d"]).is_err());
        for key in ["a", "b", "c"] {
            assert_eq!(last[key], before[key]);
        }
    }

    #[test]
    fn an_edge_ends_in_a_square_beside_the_node_it_points_to() {
        let route = EdgeRoute {
            from: "a".into(),
            to: "b".into(),
            points: vec![Vec2::new(0.0, 10.0), Vec2::new(20.0, 10.0), Vec2::new(20.0, 40.0), Vec2::new(50.0, 40.0)],
        };
        let lines = lines_of(&route);
        assert_eq!(lines.len(), 4);
        let end = lines.last().unwrap().1;
        assert_eq!(end.max.x, 50.0, "it touches the node");
        assert_eq!(end.center().y, 40.0);
        // Each stretch reaches half a stroke past its points, so the turns join.
        assert_eq!(lines[0].1.max.x, 20.0 + stroke::EDGE / 2.0);
    }
}
