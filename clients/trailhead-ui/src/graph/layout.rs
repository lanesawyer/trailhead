//! Where a graph's nodes and edges go: a pure function of the nodes' keys and sizes and the edges between them, in
//! layers from left to right (Sugiyama's method, kept small).
//!
//! 1. A cycle is broken by turning round each edge that closes one, found by a depth-first walk in the nodes' order,
//!    so the same graph always breaks the same way. The edge is still drawn, against the flow.
//! 2. Each node goes one layer right of the furthest of the nodes pointing into it; nodes nothing points into are
//!    the first layer.
//! 3. An edge that spans several layers passes each one between as a point of its own, so it is routed round the
//!    nodes there rather than through them.
//! 4. Each layer is ordered by the mean place of its neighbours in the last, sweeping right then left a few times,
//!    and the order with the fewest crossings is kept.
//! 5. Each node is moved towards its neighbours' middle, as far as the order and the room between nodes allow.
//! 6. An edge leaves its node's right side and meets its target's left, turning at right angles between the layers.

use std::collections::{BTreeSet, HashMap};

use bevy::math::{Rect, Vec2};

use crate::extent;

/// A node of a graph: a key it keeps while it is in the graph, and how big its box is, in UI pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphNode {
    pub key: String,
    pub size: Vec2,
}

impl GraphNode {
    pub fn new(key: impl Into<String>, size: Vec2) -> Self {
        GraphNode { key: key.into(), size }
    }
}

/// An edge from one node to another, by their keys: `from` is laid out left of `to`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
}

impl GraphEdge {
    pub fn new(from: impl Into<String>, to: impl Into<String>) -> Self {
        GraphEdge { from: from.into(), to: to.into() }
    }
}

/// Where everything of a graph goes, in UI pixels from its top left.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphLayout {
    /// Each node's box, by key.
    pub nodes: HashMap<String, Rect>,
    /// Each edge drawn, in the order given, less those to a key that isn't a node and those from a node to itself.
    pub edges: Vec<EdgeRoute>,
    /// How big the whole is.
    pub size: Vec2,
}

/// The line an edge is drawn along: points from the edge of `from`'s box to the edge of `to`'s, each a right angle
/// from the last.
#[derive(Clone, Debug, PartialEq)]
pub struct EdgeRoute {
    pub from: String,
    pub to: String,
    pub points: Vec<Vec2>,
}

/// How many times the layers' order is swept, right and left.
const SWEEPS: usize = 12;
/// How many times the nodes are moved towards their neighbours, down the layers and back.
const SETTLES: usize = 4;

/// Lays out `nodes` and `edges`. Two nodes with one key are one (the first); edges are taken once each.
pub fn lay_out_graph(nodes: &[GraphNode], edges: &[GraphEdge]) -> GraphLayout {
    let mut index: HashMap<&str, usize> = HashMap::new();
    let mut real: Vec<&GraphNode> = Vec::new();
    for node in nodes {
        if !index.contains_key(node.key.as_str()) {
            index.insert(&node.key, real.len());
            real.push(node);
        }
    }
    let mut seen = BTreeSet::new();
    let edges: Vec<(usize, usize, &GraphEdge)> = edges
        .iter()
        .filter_map(|e| Some((*index.get(e.from.as_str())?, *index.get(e.to.as_str())?, e)))
        .filter(|(from, to, _)| from != to && seen.insert((*from, *to)))
        .collect();
    let pairs: Vec<(usize, usize)> = edges.iter().map(|(f, t, _)| (*f, *t)).collect();
    let reversed = back_edges(real.len(), &pairs);
    let flowing: Vec<(usize, usize)> =
        pairs.iter().zip(&reversed).map(|(&(f, t), &back)| if back { (t, f) } else { (f, t) }).collect();
    let mut graph = Layered::new(&real, &flowing);
    graph.order();
    graph.place();
    let routes = graph.routes();
    let nodes = real.iter().enumerate().map(|(i, n)| (n.key.clone(), graph.rect(i))).collect();
    let edges = edges
        .iter()
        .zip(&reversed)
        .enumerate()
        .map(|(at, ((_, _, edge), back))| {
            let mut points = routes[graph.chain_of[at]].clone();
            if *back {
                points.reverse();
            }
            EdgeRoute { from: edge.from.clone(), to: edge.to.clone(), points }
        })
        .collect();
    GraphLayout { nodes, edges, size: graph.size() }
}

/// For each edge, whether it closes a cycle and is turned round: the edges a depth-first walk in the nodes' order
/// finds pointing back into its path.
fn back_edges(count: usize, edges: &[(usize, usize)]) -> Vec<bool> {
    let mut out: Vec<Vec<(usize, usize)>> = vec![Vec::new(); count];
    for (at, &(from, to)) in edges.iter().enumerate() {
        out[from].push((to, at));
    }
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        New,
        OnPath,
        Done,
    }
    let mut marks = vec![Mark::New; count];
    let mut back = vec![false; edges.len()];
    for root in 0..count {
        if marks[root] != Mark::New {
            continue;
        }
        marks[root] = Mark::OnPath;
        let mut path = vec![(root, 0)];
        while let Some((node, next)) = path.last_mut() {
            let node = *node;
            let Some(&(to, edge)) = out[node].get(*next) else {
                marks[node] = Mark::Done;
                path.pop();
                continue;
            };
            *next += 1;
            match marks[to] {
                Mark::OnPath => back[edge] = true,
                Mark::New => {
                    marks[to] = Mark::OnPath;
                    path.push((to, 0));
                }
                Mark::Done => {}
            }
        }
    }
    back
}

/// The graph in layers, with a point for each layer an edge passes, and where everything goes.
struct Layered {
    /// The real nodes' sizes; the points after them have none.
    sizes: Vec<Vec2>,
    /// Each item's layer.
    layer: Vec<usize>,
    /// Each layer's items, in order.
    layers: Vec<Vec<usize>>,
    /// Each item's place in its layer.
    place: Vec<usize>,
    /// Each item's neighbours in the layer before and the layer after.
    before: Vec<Vec<usize>>,
    after: Vec<Vec<usize>>,
    /// Each edge between layers, left to right, as its items: its source, the points it passes, its target.
    chains: Vec<Vec<usize>>,
    /// For each edge given, which chain draws it.
    chain_of: Vec<usize>,
    /// Each item's top, and each layer's left and width.
    top: Vec<f32>,
    left: Vec<f32>,
    width: Vec<f32>,
}

impl Layered {
    fn new(real: &[&GraphNode], edges: &[(usize, usize)]) -> Self {
        let count = real.len();
        let layer = layers_of(count, edges);
        let mut sizes: Vec<Vec2> = real.iter().map(|n| n.size.max(Vec2::ZERO)).collect();
        let mut item_layer = layer.clone();
        let mut chains = Vec::new();
        let mut chain_at: HashMap<(usize, usize), usize> = HashMap::new();
        let mut chain_of = Vec::with_capacity(edges.len());
        for &(from, to) in edges {
            let at = *chain_at.entry((from, to)).or_insert_with(|| {
                let mut chain = vec![from];
                for passed in layer[from] + 1..layer[to] {
                    chain.push(sizes.len());
                    sizes.push(Vec2::ZERO);
                    item_layer.push(passed);
                }
                chain.push(to);
                chains.push(chain);
                chains.len() - 1
            });
            chain_of.push(at);
        }
        let items = sizes.len();
        let depth = item_layer.iter().max().map_or(0, |l| l + 1);
        let mut layers = vec![Vec::new(); depth];
        for (item, &l) in item_layer.iter().enumerate() {
            layers[l].push(item);
        }
        let mut place = vec![0; items];
        for layer in &layers {
            for (at, &item) in layer.iter().enumerate() {
                place[item] = at;
            }
        }
        let (mut before, mut after) = (vec![Vec::new(); items], vec![Vec::new(); items]);
        for chain in &chains {
            for hop in chain.windows(2) {
                after[hop[0]].push(hop[1]);
                before[hop[1]].push(hop[0]);
            }
        }
        Layered {
            sizes,
            layer: item_layer,
            layers,
            place,
            before,
            after,
            chains,
            chain_of,
            top: vec![0.0; items],
            left: vec![0.0; depth],
            width: vec![0.0; depth],
        }
    }

    /// Orders each layer to keep crossings few: sweeps by the mean place of each item's neighbours, keeping the best.
    fn order(&mut self) {
        let mut best = (self.crossings(), self.layers.clone());
        for sweep in 0..SWEEPS {
            if sweep % 2 == 0 {
                for l in 1..self.layers.len() {
                    self.reorder(l, true);
                }
            } else {
                for l in (0..self.layers.len().saturating_sub(1)).rev() {
                    self.reorder(l, false);
                }
            }
            let crossings = self.crossings();
            if crossings < best.0 {
                best = (crossings, self.layers.clone());
            }
            if crossings == 0 {
                break;
            }
        }
        self.layers = best.1;
        for layer in &self.layers {
            for (at, &item) in layer.iter().enumerate() {
                self.place[item] = at;
            }
        }
    }

    /// Sorts layer `l` by the mean place of each item's neighbours on the side looked at; one with none keeps its own.
    fn reorder(&mut self, l: usize, from_before: bool) {
        let neighbours = if from_before { &self.before } else { &self.after };
        let mut keyed: Vec<(f32, usize)> = self.layers[l]
            .iter()
            .map(|&item| {
                let near = &neighbours[item];
                let key = if near.is_empty() {
                    self.place[item] as f32
                } else {
                    near.iter().map(|n| self.place[*n] as f32).sum::<f32>() / near.len() as f32
                };
                (key, item)
            })
            .collect();
        keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
        self.layers[l] = keyed.into_iter().map(|(_, item)| item).collect();
        for (at, &item) in self.layers[l].iter().enumerate() {
            self.place[item] = at;
        }
    }

    /// How many pairs of edges cross between neighbouring layers.
    fn crossings(&self) -> usize {
        let mut place = vec![0; self.place.len()];
        for layer in &self.layers {
            for (at, &item) in layer.iter().enumerate() {
                place[item] = at;
            }
        }
        let mut total = 0;
        for layer in &self.layers {
            let mut hops: Vec<(usize, usize)> =
                layer.iter().flat_map(|&a| self.after[a].iter().map(move |&b| (a, b))).collect();
            hops.iter_mut().for_each(|h| *h = (place[h.0], place[h.1]));
            for (i, a) in hops.iter().enumerate() {
                total += hops[i + 1..].iter().filter(|b| (a.0 < b.0 && a.1 > b.1) || (a.0 > b.0 && a.1 < b.1)).count();
            }
        }
        total
    }

    fn height(&self, item: usize) -> f32 {
        self.sizes[item].y
    }

    fn middle(&self, item: usize) -> f32 {
        self.top[item] + self.height(item) / 2.0
    }

    /// Where each layer and item goes: layers side by side, and the items of each stacked in order, moved towards
    /// their neighbours.
    fn place(&mut self) {
        let mut x = 0.0;
        for (l, layer) in self.layers.iter().enumerate() {
            self.width[l] = layer.iter().map(|&i| self.sizes[i].x).fold(0.0, f32::max);
            self.left[l] = x;
            x += self.width[l] + extent::GRAPH_LAYERS_APART;
        }
        let stacked = |layer: &[usize], sizes: &[Vec2]| {
            layer.iter().map(|&i| sizes[i].y).sum::<f32>()
                + extent::GRAPH_NODES_APART * layer.len().saturating_sub(1) as f32
        };
        let tallest = self.layers.iter().map(|l| stacked(l, &self.sizes)).fold(0.0, f32::max);
        for layer in &self.layers {
            let mut y = (tallest - stacked(layer, &self.sizes)) / 2.0;
            for &item in layer {
                self.top[item] = y;
                y += self.sizes[item].y + extent::GRAPH_NODES_APART;
            }
        }
        for _ in 0..SETTLES {
            for l in 1..self.layers.len() {
                self.settle(l, true);
            }
            for l in (0..self.layers.len().saturating_sub(1)).rev() {
                self.settle(l, false);
            }
        }
        let least = self.top.iter().copied().fold(f32::INFINITY, f32::min);
        if least.is_finite() {
            self.top.iter_mut().for_each(|t| *t -= least);
        }
    }

    /// Moves layer `l`'s items towards the middle of their neighbours on one side, keeping their order and the room
    /// between them: the mean of pushing each down past the one above and pushing each up past the one below, which
    /// keeps both.
    fn settle(&mut self, l: usize, from_before: bool) {
        let layer = self.layers[l].clone();
        let neighbours = if from_before { &self.before } else { &self.after };
        let wanted: Vec<f32> = layer
            .iter()
            .map(|&item| {
                let near = &neighbours[item];
                if near.is_empty() {
                    self.top[item]
                } else {
                    let middle = near.iter().map(|n| self.middle(*n)).sum::<f32>() / near.len() as f32;
                    middle - self.height(item) / 2.0
                }
            })
            .collect();
        let apart = extent::GRAPH_NODES_APART;
        let mut down = wanted.clone();
        for at in 1..layer.len() {
            down[at] = down[at].max(down[at - 1] + self.height(layer[at - 1]) + apart);
        }
        let mut up = wanted;
        for at in (0..layer.len().saturating_sub(1)).rev() {
            up[at] = up[at].min(up[at + 1] - apart - self.height(layer[at]));
        }
        for (at, &item) in layer.iter().enumerate() {
            self.top[item] = (down[at] + up[at]) / 2.0;
        }
    }

    /// A real node's box.
    fn rect(&self, item: usize) -> Rect {
        let min = Vec2::new(self.left[self.layer[item]], self.top[item]);
        Rect::from_corners(min, min + self.sizes[item])
    }

    fn size(&self) -> Vec2 {
        let right = self.left.last().zip(self.width.last()).map_or(0.0, |(l, w)| l + w);
        let bottom = (0..self.sizes.len()).map(|i| self.top[i] + self.height(i)).fold(0.0, f32::max);
        Vec2::new(right, bottom)
    }

    /// The points each chain is drawn through, left to right.
    fn routes(&self) -> Vec<Vec<Vec2>> {
        let tracks = self.tracks();
        self.chains
            .iter()
            .map(|chain| {
                let mut points: Vec<Vec2> = Vec::new();
                for (at, hop) in chain.windows(2).enumerate() {
                    let (a, b) = (hop[0], hop[1]);
                    let (la, lb) = (self.layer[a], self.layer[b]);
                    let right = if at == 0 { self.left[la] + self.sizes[a].x } else { self.left[la] + self.width[la] };
                    let (ya, yb) = (self.middle(a), self.middle(b));
                    if at > 0 {
                        points.push(Vec2::new(self.left[la], ya));
                    }
                    points.push(Vec2::new(right, ya));
                    if let Some(x) = tracks.get(&a).filter(|_| (ya - yb).abs() > f32::EPSILON) {
                        points.push(Vec2::new(*x, ya));
                        points.push(Vec2::new(*x, yb));
                    }
                    points.push(Vec2::new(self.left[lb], yb));
                }
                points.dedup();
                points
            })
            .collect()
    }

    /// Where, across the room after its layer, the edges leaving each item turn: each item that has an edge to turn
    /// gets a line of its own, spread evenly across the room in the order of the items' heights.
    fn tracks(&self) -> HashMap<usize, f32> {
        let mut tracks = HashMap::new();
        for (l, layer) in self.layers.iter().enumerate().take(self.layers.len().saturating_sub(1)) {
            let mut turning: Vec<usize> = layer
                .iter()
                .copied()
                .filter(|&a| self.after[a].iter().any(|&b| (self.middle(a) - self.middle(b)).abs() > f32::EPSILON))
                .collect();
            turning.sort_by(|a, b| self.middle(*a).total_cmp(&self.middle(*b)));
            let start = self.left[l] + self.width[l];
            let share = extent::GRAPH_LAYERS_APART / (turning.len() + 1) as f32;
            for (at, item) in turning.into_iter().enumerate() {
                tracks.insert(item, start + share * (at + 1) as f32);
            }
        }
        tracks
    }
}

/// Each node's layer: one past the furthest of those pointing into it, in an order where every edge points forward.
fn layers_of(count: usize, edges: &[(usize, usize)]) -> Vec<usize> {
    let mut into = vec![0usize; count];
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); count];
    for &(from, to) in edges {
        into[to] += 1;
        out[from].push(to);
    }
    let mut ready: BTreeSet<usize> = (0..count).filter(|&n| into[n] == 0).collect();
    let mut layer = vec![0; count];
    while let Some(node) = ready.pop_first() {
        for &to in &out[node] {
            layer[to] = layer[to].max(layer[node] + 1);
            into[to] -= 1;
            if into[to] == 0 {
                ready.insert(to);
            }
        }
    }
    layer
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARD: Vec2 = Vec2::new(200.0, 80.0);

    fn nodes(keys: &[&str]) -> Vec<GraphNode> {
        keys.iter().map(|k| GraphNode::new(*k, CARD)).collect()
    }

    fn edges(pairs: &[(&str, &str)]) -> Vec<GraphEdge> {
        pairs.iter().map(|(f, t)| GraphEdge::new(*f, *t)).collect()
    }

    fn no_two_overlap(layout: &GraphLayout) {
        let boxes: Vec<(&String, &Rect)> = layout.nodes.iter().collect();
        for (i, (a, ra)) in boxes.iter().enumerate() {
            for (b, rb) in &boxes[i + 1..] {
                assert!(ra.intersect(**rb).is_empty(), "{a} {ra:?} overlaps {b} {rb:?}");
            }
        }
    }

    /// Every route starts on its source's box and ends on its target's, and turns only at right angles.
    fn routes_join_their_nodes(layout: &GraphLayout) {
        for route in &layout.edges {
            let (from, to) = (layout.nodes[&route.from], layout.nodes[&route.to]);
            let (first, last) = (route.points[0], *route.points.last().unwrap());
            let on = |r: Rect, p: Vec2| r.inflate(0.01).contains(p);
            assert!(on(from, first), "{route:?} leaves {from:?}");
            assert!(on(to, last), "{route:?} meets {to:?}");
            for pair in route.points.windows(2) {
                assert!(pair[0].x == pair[1].x || pair[0].y == pair[1].y, "{route:?} has a slant");
            }
        }
    }

    fn center(layout: &GraphLayout, key: &str) -> Vec2 {
        layout.nodes[key].center()
    }

    #[test]
    fn a_chain_runs_left_to_right_on_one_line() {
        let layout = lay_out_graph(&nodes(&["a", "b", "c"]), &edges(&[("a", "b"), ("b", "c")]));
        let (a, b, c) = (center(&layout, "a"), center(&layout, "b"), center(&layout, "c"));
        assert!(a.x < b.x && b.x < c.x);
        assert!(a.y == b.y && b.y == c.y, "{a} {b} {c}");
        assert_eq!(layout.edges.len(), 2);
        for route in &layout.edges {
            assert_eq!(route.points.len(), 2, "straight across: {route:?}");
        }
        routes_join_their_nodes(&layout);
        let gap = layout.nodes["b"].min.x - layout.nodes["a"].max.x;
        assert_eq!(gap, extent::GRAPH_LAYERS_APART);
        assert_eq!(layout.size, Vec2::new(CARD.x * 3.0 + gap * 2.0, CARD.y));
    }

    #[test]
    fn a_diamond_puts_the_middle_two_side_by_side_in_one_layer() {
        let layout =
            lay_out_graph(&nodes(&["a", "b", "c", "d"]), &edges(&[("a", "b"), ("a", "c"), ("b", "d"), ("c", "d")]));
        let (a, b, c, d) = (center(&layout, "a"), center(&layout, "b"), center(&layout, "c"), center(&layout, "d"));
        assert_eq!(b.x, c.x);
        assert!(a.x < b.x && c.x < d.x);
        assert!((b.y - c.y).abs() >= CARD.y + extent::GRAPH_NODES_APART - 0.01);
        // The ends sit between the two.
        assert!((a.y - (b.y + c.y) / 2.0).abs() < 0.5 && (d.y - (b.y + c.y) / 2.0).abs() < 0.5, "{a} {b} {c} {d}");
        no_two_overlap(&layout);
        routes_join_their_nodes(&layout);
        assert_eq!(layout.edges.len(), 4);
    }

    #[test]
    fn a_node_with_no_edges_is_laid_out_in_the_first_layer_apart_from_the_rest() {
        let layout = lay_out_graph(&nodes(&["a", "b", "lone"]), &edges(&[("a", "b")]));
        assert_eq!(layout.nodes["lone"].min.x, 0.0);
        no_two_overlap(&layout);
        assert_eq!(layout.nodes.len(), 3);
    }

    #[test]
    fn a_cycle_is_broken_the_same_way_every_time_and_still_drawn() {
        let given = edges(&[("a", "b"), ("b", "c"), ("c", "a")]);
        let layout = lay_out_graph(&nodes(&["a", "b", "c"]), &given);
        assert_eq!(layout, lay_out_graph(&nodes(&["a", "b", "c"]), &given));
        let xs: Vec<f32> = ["a", "b", "c"].iter().map(|k| layout.nodes[*k].min.x).collect();
        assert!(xs[0] < xs[1] && xs[1] < xs[2], "the walk from a breaks c -> a: {xs:?}");
        assert_eq!(layout.edges.len(), 3);
        routes_join_their_nodes(&layout);
        let back = layout.edges.iter().find(|r| r.from == "c").unwrap();
        assert!(back.points[0].x > back.points.last().unwrap().x, "drawn from c back leftwards to a: {back:?}");
    }

    #[test]
    fn an_edge_over_several_layers_goes_round_the_nodes_between() {
        // a -> b -> c -> d, and a -> d past b and c.
        let layout =
            lay_out_graph(&nodes(&["a", "b", "c", "d"]), &edges(&[("a", "b"), ("b", "c"), ("c", "d"), ("a", "d")]));
        no_two_overlap(&layout);
        routes_join_their_nodes(&layout);
        let long = layout.edges.iter().find(|r| r.from == "a" && r.to == "d").unwrap();
        for key in ["b", "c"] {
            let r = layout.nodes[key];
            for pair in long.points.windows(2) {
                let seg = Rect::from_corners(pair[0], pair[1]);
                assert!(seg.intersect(r.inflate(-0.5)).is_empty(), "{long:?} crosses {key} {r:?}");
            }
        }
    }

    #[test]
    fn edges_to_nodes_not_given_and_to_themselves_are_left_out() {
        let layout = lay_out_graph(&nodes(&["a", "b"]), &edges(&[("a", "x"), ("a", "a"), ("a", "b"), ("a", "b")]));
        assert_eq!(layout.edges.len(), 1);
    }

    #[test]
    fn nothing_lays_out_as_nothing() {
        assert_eq!(lay_out_graph(&[], &[]), GraphLayout::default());
    }

    #[test]
    fn crossings_are_undone_where_an_order_has_none() {
        // a1 -> b2 and a2 -> b1 cross as given; b's order is swapped.
        let layout = lay_out_graph(&nodes(&["a1", "a2", "b1", "b2"]), &edges(&[("a1", "b2"), ("a2", "b1")]));
        let above = |x: &str, y: &str| layout.nodes[x].min.y < layout.nodes[y].min.y;
        assert_eq!(above("a1", "a2"), above("b2", "b1"));
    }

    #[test]
    fn a_few_hundred_nodes_lay_out_quickly() {
        let count = 300;
        let given: Vec<GraphNode> = (0..count).map(|i| GraphNode::new(format!("n{i}"), CARD)).collect();
        // Each node points at two later ones, a little way on: a wide, deep plan.
        let mut links = Vec::new();
        for i in 0..count {
            for step in [3, 17] {
                if i + step < count {
                    links.push(GraphEdge::new(format!("n{i}"), format!("n{}", i + step)));
                }
            }
        }
        let started = std::time::Instant::now();
        let layout = lay_out_graph(&given, &links);
        let took = started.elapsed();
        assert_eq!(layout.nodes.len(), count);
        no_two_overlap(&layout);
        routes_join_their_nodes(&layout);
        assert!(took.as_millis() < 2000, "took {took:?}");
    }
}
