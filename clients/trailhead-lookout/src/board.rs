//! The task board (`docs/factory/ui.md#views`), read-only: tasks by status with subtasks under their parent, each
//! with its claim holder, open questions and last handoff, filtered by project, status, parent and holder.
//!
//! [`build`] is a pure function over the mirror's JSON, so the queries are tested without a window. [`BoardPlugin`]
//! keeps a [`BoardView`] from the mirror and the [`Filter`]; [`BoardUiPlugin`] draws it with feathers, greyed out
//! with the reason whenever the connection isn't [`Connection::Live`].

use std::collections::{BTreeSet, HashMap, HashSet};

use bevy::feathers::controls::FeathersButton;
use bevy::feathers::display::caption;
use bevy::prelude::*;
use bevy::ui::InteractionDisabled;
use bevy::ui_widgets::Activate;
use serde_json::Value;

use crate::author::{Act, act_button};
use crate::connection::{Connection, ConnectionStatus, LocalEndpoint};
use crate::inspector::{Selection, Subject};
use crate::mirror::{Data, IsAgent, IsClaim, IsComment, IsProject, IsTask, Mirror};
use crate::widgets::ScrollBoth;

/// The views of the app; the trail map joins the board later.
#[derive(States, Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum View {
    #[default]
    Board,
}

/// Status columns in lifecycle order.
pub const STATUSES: [&str; 7] = ["open", "in_progress", "needs_input", "in_review", "approved", "done", "cancelled"];

/// What the board shows. `None` means all. `project` and `parent` are keys (`TRAIL`, `TRAIL-102`); `holder` is
/// the id of whoever holds the claim.
#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    pub project: Option<String>,
    pub status: Option<String>,
    pub parent: Option<String>,
    pub holder: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterKey {
    Project,
    Status,
    Parent,
    Holder,
}

impl FilterKey {
    const ALL: [FilterKey; 4] = [FilterKey::Project, FilterKey::Status, FilterKey::Parent, FilterKey::Holder];

    fn name(self) -> &'static str {
        match self {
            FilterKey::Project => "project",
            FilterKey::Status => "status",
            FilterKey::Parent => "parent",
            FilterKey::Holder => "holder",
        }
    }
}

impl Filter {
    pub fn get(&self, key: FilterKey) -> &Option<String> {
        match key {
            FilterKey::Project => &self.project,
            FilterKey::Status => &self.status,
            FilterKey::Parent => &self.parent,
            FilterKey::Holder => &self.holder,
        }
    }

    fn get_mut(&mut self, key: FilterKey) -> &mut Option<String> {
        match key {
            FilterKey::Project => &mut self.project,
            FilterKey::Status => &mut self.status,
            FilterKey::Parent => &mut self.parent,
            FilterKey::Holder => &mut self.holder,
        }
    }
}

/// A choice for a filter: the value to match and what to show for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub value: String,
    pub label: String,
}

/// The values each filter can take now, for its button to step through.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    pub projects: Vec<Choice>,
    pub statuses: Vec<Choice>,
    pub parents: Vec<Choice>,
    pub holders: Vec<Choice>,
}

impl Options {
    pub fn of(&self, key: FilterKey) -> &[Choice] {
        match key {
            FilterKey::Project => &self.projects,
            FilterKey::Status => &self.statuses,
            FilterKey::Parent => &self.parents,
            FilterKey::Holder => &self.holders,
        }
    }
}

/// The next choice after `current`, wrapping to "all" (`None`) after the last. A current value that is no longer
/// offered starts the cycle again.
pub fn cycle(current: &Option<String>, choices: &[Choice]) -> Option<String> {
    let next = match current {
        None => 0,
        Some(v) => choices.iter().position(|c| &c.value == v).map_or(0, |i| i + 1),
    };
    choices.get(next).map(|c| c.value.clone())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub id: String,
    pub key: String,
    pub title: String,
    pub priority: i64,
    /// `TRAIL-102` when this task is a subtask whose parent isn't in the same column (a nested card says it by
    /// sitting under its parent).
    pub parent: Option<String>,
    /// 0 for a top-level card, 1 for a subtask shown under its parent in the same column.
    pub depth: usize,
    pub holder: Option<String>,
    /// The id of the claim holder.
    pub holder_id: Option<String>,
    pub open_questions: Vec<String>,
    /// The comment ids of `open_questions`, in the same order, for answering them.
    pub question_ids: Vec<String>,
    pub handoff: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub status: String,
    pub cards: Vec<Card>,
}

#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct BoardView {
    pub columns: Vec<Column>,
    pub options: Options,
}

/// Everything [`build`] reads, as the mirror holds it.
#[derive(Default, Clone, Copy)]
pub struct Input<'a> {
    pub projects: &'a [Value],
    pub tasks: &'a [Value],
    pub claims: &'a [Value],
    pub comments: &'a [Value],
    pub agents: &'a [Value],
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or_default()
}

/// Groups the tasks passing `filter` into one column per status (empty ones included, unless a status is
/// chosen), with subtasks under their parent, and works out what each filter can offer.
pub fn build(input: Input, filter: &Filter) -> BoardView {
    let project_key: HashMap<&str, &str> = input.projects.iter().map(|p| (s(p, "id"), s(p, "key"))).collect();
    let task_key: HashMap<&str, &str> = input.tasks.iter().map(|t| (s(t, "id"), s(t, "key"))).collect();
    let agent_name: HashMap<&str, &str> = input.agents.iter().map(|a| (s(a, "id"), s(a, "name"))).collect();
    // The claim is keyed by its task.
    let holder_of: HashMap<&str, &str> = input.claims.iter().map(|c| (s(c, "task_id"), s(c, "holder_id"))).collect();
    let holder_label = |id: &str| agent_name.get(id).map_or_else(|| short(id), |n| n.to_string());

    let mut questions: HashMap<&str, Vec<&Value>> = HashMap::new();
    let mut handoffs: HashMap<&str, &Value> = HashMap::new();
    for c in input.comments {
        let task = s(c, "task_id");
        match s(c, "kind") {
            "question" if c["resolved_at"].is_null() => questions.entry(task).or_default().push(c),
            "handoff" => {
                let newer = handoffs.get(task).is_none_or(|h| s(h, "created_at") <= s(c, "created_at"));
                if newer {
                    handoffs.insert(task, c);
                }
            }
            _ => {}
        }
    }

    let passes = |t: &Value| {
        let id = s(t, "id");
        filter.project.as_deref().is_none_or(|k| project_key.get(s(t, "project_id")) == Some(&k))
            && filter.status.as_deref().is_none_or(|st| s(t, "status") == st)
            && filter
                .parent
                .as_deref()
                .is_none_or(|k| t["parent_id"].as_str().and_then(|p| task_key.get(p)) == Some(&k))
            && filter.holder.as_deref().is_none_or(|h| holder_of.get(id) == Some(&h))
    };

    let card = |t: &Value, depth: usize, with_parent: bool| {
        let id = s(t, "id");
        let mut open: Vec<&Value> = questions.get(id).cloned().unwrap_or_default();
        open.sort_by(|a, b| s(a, "created_at").cmp(s(b, "created_at")));
        Card {
            id: id.to_string(),
            key: s(t, "key").to_string(),
            title: s(t, "title").to_string(),
            priority: t["priority"].as_i64().unwrap_or(0),
            parent: t["parent_id"]
                .as_str()
                .and_then(|p| task_key.get(p))
                .filter(|_| with_parent)
                .map(|k| k.to_string()),
            depth,
            holder: holder_of.get(id).map(|h| holder_label(h)),
            holder_id: holder_of.get(id).map(|h| h.to_string()),
            open_questions: open.iter().map(|q| s(q, "body").to_string()).collect(),
            question_ids: open.iter().map(|q| s(q, "id").to_string()).collect(),
            handoff: handoffs.get(id).map(|h| s(h, "body").to_string()),
        }
    };

    let shown: Vec<&Value> = input.tasks.iter().filter(|t| passes(t)).collect();
    let mut columns = Vec::new();
    for status in STATUSES {
        if filter.status.as_deref().is_some_and(|f| f != status) {
            continue;
        }
        let mut here: Vec<&Value> = shown.iter().copied().filter(|t| s(t, "status") == status).collect();
        // Urgent first, then by key number, so the order doesn't shuffle with every update.
        here.sort_by_key(|t| (t["priority"].as_i64().unwrap_or(0), key_number(s(t, "key")), s(t, "key").to_string()));
        let ids: HashSet<&str> = here.iter().map(|t| s(t, "id")).collect();
        let nested = |t: &Value| t["parent_id"].as_str().is_some_and(|p| ids.contains(p));
        let mut cards = Vec::new();
        for t in here.iter().filter(|t| !nested(t)) {
            cards.push(card(t, 0, true));
            let id = s(t, "id");
            cards.extend(here.iter().filter(|c| c["parent_id"].as_str() == Some(id)).map(|c| card(c, 1, false)));
        }
        columns.push(Column { status: status.to_string(), cards });
    }

    let choice = |value: &str, label: String| Choice { value: value.to_string(), label };
    let projects = {
        let mut keys: Vec<&str> = project_key.values().copied().collect();
        keys.sort();
        keys.into_iter().map(|k| choice(k, k.to_string())).collect()
    };
    let parents = {
        let ids: BTreeSet<&str> = input.tasks.iter().filter_map(|t| t["parent_id"].as_str()).collect();
        let mut keys: Vec<&str> = ids.iter().filter_map(|id| task_key.get(id).copied()).collect();
        keys.sort_by_key(|k| (key_number(k), k.to_string()));
        keys.into_iter().map(|k| choice(k, k.to_string())).collect()
    };
    let holders = {
        let ids: BTreeSet<&str> = holder_of.values().copied().collect();
        let mut v: Vec<Choice> = ids.into_iter().map(|id| choice(id, holder_label(id))).collect();
        v.sort_by(|a, b| a.label.cmp(&b.label));
        v
    };
    let statuses = STATUSES.iter().map(|st| choice(st, st.to_string())).collect();
    BoardView { columns, options: Options { projects, statuses, parents, holders } }
}

fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

fn key_number(key: &str) -> i64 {
    key.rsplit('-').next().and_then(|n| n.parse().ok()).unwrap_or(0)
}

fn collect<C: Component>(q: &Query<&Data, With<C>>) -> Vec<Value> {
    q.iter().map(|d| d.0.clone()).collect()
}

#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn rebuild_view(
    filter: Res<Filter>,
    mirror: Res<Mirror>,
    changed: Query<(), Or<(Added<Data>, Changed<Data>)>>,
    mut removed: RemovedComponents<Data>,
    projects: Query<&Data, With<IsProject>>,
    tasks: Query<&Data, With<IsTask>>,
    claims: Query<&Data, With<IsClaim>>,
    comments: Query<&Data, With<IsComment>>,
    agents: Query<&Data, With<IsAgent>>,
    mut view: ResMut<BoardView>,
) {
    let gone = removed.read().count() > 0;
    if !(filter.is_changed() || mirror.is_changed() || gone || !changed.is_empty()) {
        return;
    }
    let (projects, tasks, claims, comments, agents) =
        (collect(&projects), collect(&tasks), collect(&claims), collect(&comments), collect(&agents));
    let next = build(
        Input { projects: &projects, tasks: &tasks, claims: &claims, comments: &comments, agents: &agents },
        &filter,
    );
    if *view != next {
        *view = next;
    }
}

/// Keeps [`Filter`] and [`BoardView`] from the mirror.
pub struct BoardPlugin;

impl Plugin for BoardPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Filter>().init_resource::<BoardView>().add_systems(Update, rebuild_view);
    }
}

/// Draws the board while in [`View::Board`].
pub struct BoardUiPlugin;

impl Plugin for BoardUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(BoardPlugin)
            .init_state::<View>()
            .add_systems(OnEnter(View::Board), spawn_panel)
            .add_systems(OnExit(View::Board), despawn_panel)
            .init_resource::<BoardScroll>()
            .add_systems(Update, (remember_scroll, redraw).chain().run_if(in_state(View::Board)));
    }
}

#[derive(Component)]
struct BoardPanel;

fn spawn_panel(mut commands: Commands) {
    commands.spawn((
        Name::new("board"),
        BoardPanel,
        Node {
            width: percent(100),
            height: percent(100),
            flex_direction: FlexDirection::Column,
            padding: UiRect::all(px(8)),
            row_gap: px(6),
            ..default()
        },
    ));
}

fn despawn_panel(mut commands: Commands, panel: Query<Entity, With<BoardPanel>>) {
    for e in &panel {
        commands.entity(e).despawn();
    }
}

fn text(commands: &mut Commands, parent: Entity, line: String, color: Color, indent: usize) {
    commands.spawn((
        Text::new(line),
        TextColor(color),
        Node { margin: UiRect::left(px(16 * indent)), ..default() },
        ChildOf(parent),
    ));
}

fn filter_button(key: FilterKey, label: String) -> impl Scene {
    bsn! {
        @FeathersButton
        on(move |_: On<Activate>, mut filter: ResMut<Filter>, view: Res<BoardView>| {
            let next = cycle(filter.get(key), view.options.of(key));
            *filter.get_mut(key) = next;
        })
        Children [ @caption(label) ]
    }
}

fn inspect_button(label: String, subject: Subject) -> impl Scene {
    bsn! {
        @FeathersButton
        on(move |_: On<Activate>, mut selection: ResMut<Selection>| {
            selection.0 = Some(subject.clone());
        })
        Children [ @caption(label) ]
    }
}

/// Where the board's columns are scrolled to, kept across redraws: each redraw builds a new scrolling node, which
/// would start back at the left edge.
#[derive(Resource, Default)]
struct BoardScroll(Vec2);

/// The node holding the columns, which scrolls both ways.
#[derive(Component)]
struct BoardColumns;

fn remember_scroll(
    columns: Query<&ScrollPosition, (With<BoardColumns>, Changed<ScrollPosition>)>,
    mut kept: ResMut<BoardScroll>,
) {
    if let Some(position) = columns.iter().next() {
        kept.0 = position.0;
    }
}

/// Redraws the whole panel when the view, filter or connection changes: the board is small enough that diffing it
/// isn't worth the code.
#[allow(clippy::too_many_arguments)]
fn redraw(
    view: Res<BoardView>,
    filter: Res<Filter>,
    connection: Res<State<Connection>>,
    status: Res<ConnectionStatus>,
    endpoint: Res<LocalEndpoint>,
    scroll: Res<BoardScroll>,
    panel: Single<(Entity, Option<&Children>), With<BoardPanel>>,
    mut commands: Commands,
    mut drawn: Local<Option<Entity>>,
) {
    let (panel, children) = *panel;
    let fresh = *drawn != Some(panel);
    if !(fresh
        || view.is_changed()
        || filter.is_changed()
        || connection.is_changed()
        || status.is_changed()
        || endpoint.is_changed())
    {
        return;
    }
    *drawn = Some(panel);
    for child in children.into_iter().flatten() {
        commands.entity(*child).despawn();
    }
    let live = *connection.get() == Connection::Live;
    let (ink, dim) = if live {
        (Color::WHITE, Color::srgb(0.7, 0.7, 0.7))
    } else {
        (Color::srgb(0.45, 0.45, 0.45), Color::srgb(0.35, 0.35, 0.35))
    };

    if !live {
        let why = status.reason.as_ref().map_or_else(String::new, |r| format!(": {r}"));
        text(
            &mut commands,
            panel,
            format!(
                "{:?}{why}. Showing the last known state.{}",
                connection.get(),
                endpoint.0.as_ref().map_or(String::new(), |id| format!(" This Lookout: {id}"))
            ),
            Color::srgb(0.9, 0.7, 0.3),
            0,
        );
    }

    let bar = commands.spawn((Node { column_gap: px(6), ..default() }, ChildOf(panel))).id();
    for key in FilterKey::ALL {
        let value = filter.get(key).as_ref().map_or("all", |v| {
            view.options.of(key).iter().find(|c| &c.value == v).map_or(v.as_str(), |c| c.label.as_str())
        });
        commands.queue_spawn_scene(filter_button(key, format!("{}: {value}", key.name()))).insert(ChildOf(bar));
    }
    let mut new = commands.queue_spawn_scene(act_button("+ new task".into(), Act::New));
    new.insert(ChildOf(bar));
    if !live {
        new.insert(InteractionDisabled);
    }

    // Columns past the window's edge are reached by scrolling sideways (Shift + wheel, or a touchpad swipe), and a
    // long column by scrolling down. TRAIL-799 gives each column its own vertical scroll.
    let columns = commands
        .spawn((
            Node {
                column_gap: px(12),
                align_items: AlignItems::FlexStart,
                flex_grow: 1.0,
                min_height: px(0),
                overflow: Overflow::scroll(),
                ..default()
            },
            BoardColumns,
            ScrollBoth,
            ScrollPosition(scroll.0),
            ChildOf(panel),
        ))
        .id();
    for column in &view.columns {
        let col = commands
            .spawn((
                Node { flex_direction: FlexDirection::Column, width: px(260), row_gap: px(4), ..default() },
                ChildOf(columns),
            ))
            .id();
        text(&mut commands, col, format!("{} ({})", column.status, column.cards.len()), ink, 0);
        for card in &column.cards {
            let mut head = format!("{} {}", card.key, card.title);
            if let Some(parent) = &card.parent {
                head += &format!(" (under {parent})");
            }
            text(&mut commands, col, head, ink, card.depth);
            let tools = commands
                .spawn((
                    Node { column_gap: px(4), margin: UiRect::left(px(16 * card.depth)), ..default() },
                    ChildOf(col),
                ))
                .id();
            let mut buttons = vec![
                ("edit".to_string(), Act::Edit(card.key.clone())),
                ("comment".to_string(), Act::Comment(card.key.clone())),
            ];
            for (n, (q, id)) in card.open_questions.iter().zip(&card.question_ids).enumerate() {
                let act = Act::Answer { key: card.key.clone(), comment_id: id.clone(), question: q.clone() };
                buttons.push((if n == 0 { "answer".to_string() } else { format!("answer {}", n + 1) }, act));
            }
            for (label, act) in buttons {
                let mut button = commands.queue_spawn_scene(act_button(label, act));
                button.insert(ChildOf(tools));
                if !live {
                    button.insert(InteractionDisabled);
                }
            }
            commands
                .queue_spawn_scene(inspect_button(format!("inspect {}", card.key), Subject::Task(card.id.clone())))
                .insert(ChildOf(col));
            if let Some(holder) = &card.holder {
                text(&mut commands, col, format!("held by {holder}"), dim, card.depth + 1);
                if let Some(id) = &card.holder_id {
                    commands
                        .queue_spawn_scene(inspect_button(format!("inspect {holder}"), Subject::Agent(id.clone())))
                        .insert(ChildOf(col));
                }
            }
            for q in &card.open_questions {
                text(&mut commands, col, format!("? {q}"), Color::srgb(0.9, 0.7, 0.3), card.depth + 1);
            }
            if let Some(h) = &card.handoff {
                text(
                    &mut commands,
                    col,
                    format!("handoff: {}", h.lines().next().unwrap_or_default()),
                    dim,
                    card.depth + 1,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::sync::{Snapshot, SyncItem};

    #[allow(clippy::type_complexity)]
    fn input() -> (Vec<Value>, Vec<Value>, Vec<Value>, Vec<Value>, Vec<Value>) {
        let projects = vec![json!({"id": "p1", "key": "TRAIL"}), json!({"id": "p2", "key": "OPS"})];
        let task = |id: &str, key: &str, project: &str, status: &str, parent: Option<&str>, prio: i64| {
            json!({"id": id, "key": key, "title": format!("title {key}"), "project_id": project, "status": status,
                   "parent_id": parent, "priority": prio})
        };
        let tasks = vec![
            task("t102", "TRAIL-102", "p1", "open", None, 2),
            task("t103", "TRAIL-103", "p1", "in_progress", Some("t102"), 2),
            task("t104", "TRAIL-104", "p1", "in_progress", Some("t103"), 1),
            task("t105", "TRAIL-105", "p1", "open", Some("t102"), 3),
            task("t200", "OPS-1", "p2", "in_progress", None, 2),
        ];
        let claims = vec![json!({"task_id": "t103", "holder_id": "a1"}), json!({"task_id": "t200", "holder_id": "u1"})];
        let comments = vec![
            json!({"id": "c1", "task_id": "t103", "kind": "question", "body": "which db?", "resolved_at": null, "created_at": "1"}),
            json!({"id": "c2", "task_id": "t103", "kind": "question", "body": "old", "resolved_at": "2", "created_at": "1"}),
            json!({"id": "c3", "task_id": "t103", "kind": "handoff", "body": "first", "created_at": "1"}),
            json!({"id": "c4", "task_id": "t103", "kind": "handoff", "body": "second\nmore", "created_at": "2"}),
        ];
        let agents = vec![json!({"id": "a1", "name": "factory/trail-103-a1"})];
        (projects, tasks, claims, comments, agents)
    }

    fn run(filter: &Filter) -> BoardView {
        let (projects, tasks, claims, comments, agents) = input();
        build(
            Input { projects: &projects, tasks: &tasks, claims: &claims, comments: &comments, agents: &agents },
            filter,
        )
    }

    fn keys(view: &BoardView, status: &str) -> Vec<(String, usize)> {
        let col = view.columns.iter().find(|c| c.status == status).unwrap();
        col.cards.iter().map(|c| (c.key.clone(), c.depth)).collect()
    }

    #[test]
    fn groups_by_status_with_subtasks_under_their_parent() {
        let view = run(&Filter::default());
        assert_eq!(view.columns.len(), STATUSES.len());
        // TRAIL-105 is a subtask of TRAIL-102 in the same column, so it nests under it.
        assert_eq!(keys(&view, "open"), [("TRAIL-102".into(), 0), ("TRAIL-105".into(), 1)]);
        // TRAIL-104 nests under TRAIL-103; TRAIL-103's parent is in another column, so it names it instead.
        assert_eq!(keys(&view, "in_progress"), [("OPS-1".into(), 0), ("TRAIL-103".into(), 0), ("TRAIL-104".into(), 1)]);
        let col = view.columns.iter().find(|c| c.status == "in_progress").unwrap();
        let parents: Vec<_> = col.cards.iter().map(|c| c.parent.as_deref()).collect();
        assert_eq!(parents, [None, Some("TRAIL-102"), None]);
    }

    #[test]
    fn cards_carry_holder_open_questions_and_the_last_handoff() {
        let view = run(&Filter::default());
        let col = view.columns.iter().find(|c| c.status == "in_progress").unwrap();
        let card = col.cards.iter().find(|c| c.key == "TRAIL-103").unwrap();
        assert_eq!(card.holder.as_deref(), Some("factory/trail-103-a1"));
        assert_eq!(card.open_questions, ["which db?"]);
        assert_eq!(card.handoff.as_deref(), Some("second\nmore"));
        // A holder that isn't an agent shows as a short id.
        assert_eq!(col.cards.iter().find(|c| c.key == "OPS-1").unwrap().holder.as_deref(), Some("u1"));
    }

    #[test]
    fn filters_narrow_the_board() {
        let by = |f: Filter| -> Vec<String> {
            run(&f).columns.iter().flat_map(|c| c.cards.iter().map(|c| c.key.clone())).collect()
        };
        assert_eq!(by(Filter { project: Some("OPS".into()), ..Default::default() }), ["OPS-1"]);
        assert_eq!(by(Filter { status: Some("open".into()), ..Default::default() }), ["TRAIL-102", "TRAIL-105"]);
        assert_eq!(by(Filter { parent: Some("TRAIL-102".into()), ..Default::default() }), ["TRAIL-105", "TRAIL-103"]);
        assert_eq!(by(Filter { holder: Some("a1".into()), ..Default::default() }), ["TRAIL-103"]);
        let none = Filter { project: Some("OPS".into()), holder: Some("a1".into()), ..Default::default() };
        assert!(by(none).is_empty());
        // Choosing a status leaves one column.
        assert_eq!(run(&Filter { status: Some("done".into()), ..Default::default() }).columns.len(), 1);
    }

    #[test]
    fn options_list_what_the_data_has_and_cycle_wraps_to_all() {
        let view = run(&Filter::default());
        let labels = |c: &[Choice]| c.iter().map(|c| c.label.clone()).collect::<Vec<_>>();
        assert_eq!(labels(&view.options.projects), ["OPS", "TRAIL"]);
        assert_eq!(labels(&view.options.parents), ["TRAIL-102", "TRAIL-103"]);
        assert_eq!(labels(&view.options.holders), ["factory/trail-103-a1", "u1"]);
        let projects = &view.options.projects;
        let one = cycle(&None, projects);
        assert_eq!(one.as_deref(), Some("OPS"));
        let two = cycle(&one, projects);
        assert_eq!(two.as_deref(), Some("TRAIL"));
        assert_eq!(cycle(&two, projects), None);
        assert_eq!(cycle(&Some("gone".into()), projects).as_deref(), Some("OPS"));
        assert_eq!(cycle(&None, &[]), None);
    }

    #[test]
    fn the_view_follows_the_mirror() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(crate::mirror::MirrorPlugin).add_plugins(BoardPlugin);
        let (projects, tasks, claims, comments, agents) = input();
        let snapshot = Snapshot { at: 5, projects, tasks, agents };
        let _ = (claims, comments);
        app.world_mut().write_message(SyncItem::Snapshot(snapshot));
        app.update();
        app.update();
        let view = app.world().resource::<BoardView>();
        assert_eq!(view.columns.iter().map(|c| c.cards.len()).sum::<usize>(), 5);

        app.world_mut().resource_mut::<Filter>().project = Some("OPS".into());
        app.update();
        let view = app.world().resource::<BoardView>();
        assert_eq!(view.columns.iter().map(|c| c.cards.len()).sum::<usize>(), 1);

        // An event moves the task to another column.
        app.world_mut().write_message(SyncItem::Event(json!({
            "id": 6, "entity": "task", "entity_id": "t200", "kind": "task.updated",
            "after": {"id": "t200", "key": "OPS-1", "project_id": "p2", "status": "done", "priority": 2, "version": 2},
        })));
        app.update();
        app.update();
        let view = app.world().resource::<BoardView>();
        assert_eq!(keys(view, "done"), [("OPS-1".into(), 0)]);
    }
}
