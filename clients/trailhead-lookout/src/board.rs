//! The task board (`docs/factory/ui.md#views`), read-only: tasks by status with subtasks under their parent, each
//! a card with badges (priority, grade, holder, open questions) and icon actions, filtered by project, status,
//! parent and holder.
//!
//! [`build`] is a pure function over the mirror's JSON, so the queries are tested without a window. [`BoardPlugin`]
//! keeps a [`BoardView`] from the mirror and the [`Filter`]; [`BoardUiPlugin`] draws it with feathers, greyed out
//! with the reason whenever the connection isn't [`Connection::Live`].

use std::collections::{BTreeSet, HashMap, HashSet};

use bevy::feathers::controls::{ButtonVariant, FeathersButton};
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
use crate::widgets::{Icon, badge, card as card_shell, card_row, card_title, card_under, token, tool_button};

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

/// What a badge on a card says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeKind {
    Priority,
    Grade,
    Holder,
    /// Open questions, with their count.
    Questions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Badge {
    pub kind: BadgeKind,
    pub label: String,
}

/// A tool button on a card. What each does is [`CardAction::act`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardAction {
    Edit,
    Comment,
    /// Answers the oldest open question; the inspector lists the rest.
    Answer {
        comment_id: String,
        question: String,
    },
}

impl CardAction {
    fn act(&self, key: &str) -> Act {
        let key = key.to_string();
        match self {
            CardAction::Edit => Act::Edit(key),
            CardAction::Comment => Act::Comment(key),
            CardAction::Answer { comment_id, question } => {
                Act::Answer { key, comment_id: comment_id.clone(), question: question.clone() }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub id: String,
    pub key: String,
    pub title: String,
    /// `TRAIL-102` when this task is a subtask whose parent isn't in the same column (a nested card says it by
    /// sitting under its parent).
    pub parent: Option<String>,
    /// 0 for a top-level card, 1 for a subtask shown under its parent in the same column.
    pub depth: usize,
    /// Priority, grade, holder and open questions, in that order, leaving out what the task doesn't have.
    pub badges: Vec<Badge>,
    /// Edit and comment, and answer when a question is open.
    pub actions: Vec<CardAction>,
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
    for c in input.comments {
        if s(c, "kind") == "question" && c["resolved_at"].is_null() {
            questions.entry(s(c, "task_id")).or_default().push(c);
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
        let priority = format!("P{}", t["priority"].as_i64().unwrap_or(0));
        let mut badges = vec![Badge { kind: BadgeKind::Priority, label: priority }];
        if let Some(grade) = t["grade"].as_i64() {
            badges.push(Badge { kind: BadgeKind::Grade, label: format!("G{grade}") });
        }
        if let Some(holder) = holder_of.get(id) {
            badges.push(Badge { kind: BadgeKind::Holder, label: holder_badge(&holder_label(holder)) });
        }
        if !open.is_empty() {
            badges.push(Badge { kind: BadgeKind::Questions, label: format!("? {}", open.len()) });
        }
        let mut actions = vec![CardAction::Edit, CardAction::Comment];
        if let Some(q) = open.first() {
            actions.push(CardAction::Answer { comment_id: s(q, "id").to_string(), question: s(q, "body").to_string() });
        }
        Card {
            id: id.to_string(),
            key: s(t, "key").to_string(),
            title: s(t, "title").to_string(),
            parent: t["parent_id"]
                .as_str()
                .and_then(|p| task_key.get(p))
                .filter(|_| with_parent)
                .map(|k| k.to_string()),
            depth,
            badges,
            actions,
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

/// How many characters of a holder's name a badge shows.
const HOLDER_BADGE_CHARS: usize = 12;

/// An agent's name for a badge: without its parent (`factory/trail-103-a1` is `trail-103-a1`), cut with an ellipsis
/// when still long.
fn holder_badge(name: &str) -> String {
    let own = name.rsplit('/').next().unwrap_or(name);
    if own.chars().count() <= HOLDER_BADGE_CHARS {
        return own.to_string();
    }
    own.chars().take(HOLDER_BADGE_CHARS - 1).chain(['…']).collect()
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
    let ink = if live { Color::WHITE } else { Color::srgb(0.45, 0.45, 0.45) };

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
            let shell = commands.queue_spawn_scene(card_shell(card.depth)).insert(ChildOf(col)).id();
            let subject = Subject::Task(card.id.clone());
            commands.entity(shell).observe(move |_: On<Activate>, mut selection: ResMut<Selection>| {
                selection.0 = Some(subject.clone());
            });
            commands.queue_spawn_scene(badge(card.key.clone(), None)).insert(ChildOf(shell));
            commands.queue_spawn_scene(card_title(card.title.clone())).insert(ChildOf(shell));
            if let Some(parent) = &card.parent {
                commands.queue_spawn_scene(card_under(parent.clone())).insert(ChildOf(shell));
            }
            let badges = commands.queue_spawn_scene(card_row()).insert(ChildOf(shell)).id();
            for b in &card.badges {
                let tone = (b.kind == BadgeKind::Questions).then_some(token::WARN);
                commands.queue_spawn_scene(badge(b.label.clone(), tone)).insert(ChildOf(badges));
            }
            let tools = commands.queue_spawn_scene(card_row()).insert(ChildOf(shell)).id();
            for action in &card.actions {
                let (icon, hint) = match action {
                    CardAction::Edit => (Icon::Pencil, "Edit"),
                    CardAction::Comment => (Icon::Comment, "Comment"),
                    CardAction::Answer { .. } => (Icon::Help, "Answer the open question"),
                };
                let mut tool = commands.queue_spawn_scene(tool_button(icon, hint, ButtonVariant::Plain));
                tool.insert((action.act(&card.key), ChildOf(tools)));
                if !live {
                    tool.insert(InteractionDisabled);
                }
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
        let mut tasks = vec![
            task("t102", "TRAIL-102", "p1", "open", None, 2),
            task("t103", "TRAIL-103", "p1", "in_progress", Some("t102"), 2),
            task("t104", "TRAIL-104", "p1", "in_progress", Some("t103"), 1),
            task("t105", "TRAIL-105", "p1", "open", Some("t102"), 3),
            task("t200", "OPS-1", "p2", "in_progress", None, 2),
        ];
        tasks[1]["grade"] = json!(3);
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

    fn card<'a>(view: &'a BoardView, key: &str) -> &'a Card {
        view.columns.iter().flat_map(|c| &c.cards).find(|c| c.key == key).unwrap()
    }

    fn badges(card: &Card) -> Vec<(BadgeKind, &str)> {
        card.badges.iter().map(|b| (b.kind, b.label.as_str())).collect()
    }

    #[test]
    fn badges_show_priority_grade_holder_and_the_open_question_count() {
        let view = run(&Filter::default());
        assert_eq!(
            badges(card(&view, "TRAIL-103")),
            [
                (BadgeKind::Priority, "P2"),
                (BadgeKind::Grade, "G3"),
                (BadgeKind::Holder, "trail-103-a1"),
                (BadgeKind::Questions, "? 1"),
            ]
        );
        // No grade, no holder, no question: only the priority. A holder that isn't an agent shows as a short id.
        assert_eq!(badges(card(&view, "TRAIL-102")), [(BadgeKind::Priority, "P2")]);
        assert_eq!(badges(card(&view, "OPS-1")), [(BadgeKind::Priority, "P2"), (BadgeKind::Holder, "u1")]);
    }

    #[test]
    fn a_long_holder_name_is_cut_short() {
        assert_eq!(holder_badge("factory/trail-103-a1"), "trail-103-a1");
        assert_eq!(holder_badge("a-very-long-agent-name"), "a-very-long…");
        assert_eq!(holder_badge("claude"), "claude");
    }

    #[test]
    fn every_card_can_be_edited_and_commented_and_only_one_with_a_question_can_be_answered() {
        let view = run(&Filter::default());
        assert_eq!(card(&view, "TRAIL-102").actions, [CardAction::Edit, CardAction::Comment]);
        let asked = card(&view, "TRAIL-103");
        let answer = CardAction::Answer { comment_id: "c1".into(), question: "which db?".into() };
        assert_eq!(asked.actions, [CardAction::Edit, CardAction::Comment, answer.clone()]);
        // The buttons send the commands the forms already handle.
        assert_eq!(asked.actions[0].act(&asked.key), Act::Edit("TRAIL-103".into()));
        assert_eq!(asked.actions[1].act(&asked.key), Act::Comment("TRAIL-103".into()));
        assert_eq!(
            answer.act(&asked.key),
            Act::Answer { key: "TRAIL-103".into(), comment_id: "c1".into(), question: "which db?".into() }
        );
    }

    #[test]
    fn a_card_carries_no_handoff_or_question_body_but_names_its_parent() {
        let view = run(&Filter::default());
        let card = card(&view, "TRAIL-103");
        assert_eq!(card.parent.as_deref(), Some("TRAIL-102"));
        assert!(card.badges.iter().all(|b| !b.label.contains("which db?") && !b.label.contains("second")));
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
