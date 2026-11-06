//! The task board (`docs/factory/ui.md#views`), read-only: tasks by status with subtasks under their parent, each
//! a card with badges (priority, grade, holder, open questions) and icon actions, filtered by project,
//! parent and holder, with the closed columns behind a toggle.
//!
//! [`build`] is a pure function over the mirror's JSON, so the queries are tested without a window. [`BoardPlugin`]
//! keeps a [`BoardView`] from the mirror and the [`Filter`]; [`BoardUiPlugin`] draws it with feathers, greyed out
//! with the reason whenever the connection isn't [`Connection::Live`].

use std::collections::{BTreeSet, HashMap, HashSet};

use bevy::feathers::controls::{ButtonVariant, FeathersMenuPopup};
use bevy::prelude::*;
use bevy::ui::InteractionDisabled;
use bevy::ui_widgets::{Activate, ControlOrientation};
use serde_json::Value;

use crate::author::Act;
use crate::connection::{Connection, ConnectionStatus};
use crate::inspector::{Selection, Subject};
use crate::mirror::{Data, IsAgent, IsClaim, IsComment, IsProject, IsTask, Mirror};
use crate::widgets;
use crate::widgets::{
    Icon, Keyed, Picked, ScrollBoth, badge, button, card as card_shell, card_row, card_title, card_under, chip, field,
    select, size, space, text as label_text, token, tool_button,
};

/// The views of the app: the task board and the trail map.
#[derive(States, Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum View {
    #[default]
    Board,
    Map,
}

/// Status columns in lifecycle order.
pub const STATUSES: [&str; 7] = ["open", "in_progress", "needs_input", "in_review", "approved", "done", "cancelled"];

/// The statuses nothing more happens in; their columns are hidden unless asked for.
pub const CLOSED: [&str; 2] = ["done", "cancelled"];

/// How many cards a column builds at first, and how many more each "show more" adds. A column holds hundreds
/// (`open` held 366 on 2026-11-05) and every card is a dozen entities, so building them all would make each redraw
/// cost in proportion to the backlog.
pub const PAGE: usize = 40;

/// What the board shows. `None` means all. `project` and `parent` are keys (`TRAIL`, `TRAIL-102`); `holder` is
/// the id of whoever holds the claim.
#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    pub project: Option<String>,
    pub parent: Option<String>,
    pub holder: Option<String>,
    /// Show the closed columns (done, cancelled) too. They are left off unless this is set.
    pub closed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FilterKey {
    #[default]
    Project,
    Parent,
    Holder,
}

impl FilterKey {
    const ALL: [FilterKey; 3] = [FilterKey::Project, FilterKey::Parent, FilterKey::Holder];

    fn name(self) -> &'static str {
        match self {
            FilterKey::Project => "project",
            FilterKey::Parent => "parent",
            FilterKey::Holder => "holder",
        }
    }
}

impl Filter {
    pub fn get(&self, key: FilterKey) -> &Option<String> {
        match key {
            FilterKey::Project => &self.project,
            FilterKey::Parent => &self.parent,
            FilterKey::Holder => &self.holder,
        }
    }

    fn get_mut(&mut self, key: FilterKey) -> &mut Option<String> {
        match key {
            FilterKey::Project => &mut self.project,
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

/// The values each filter can take now, for its dropdown to list after "all".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    pub projects: Vec<Choice>,
    pub parents: Vec<Choice>,
    pub holders: Vec<Choice>,
}

impl Options {
    pub fn of(&self, key: FilterKey) -> &[Choice] {
        match key {
            FilterKey::Project => &self.projects,
            FilterKey::Parent => &self.parents,
            FilterKey::Holder => &self.holders,
        }
    }
}

/// Sets `key`'s filter from a dropdown's pick: row 0 is "all", the rest are `choices` in order. A row that isn't
/// there leaves the filter alone.
pub fn choose(filter: &mut Filter, key: FilterKey, choices: &[Choice], index: usize) {
    match index {
        0 => *filter.get_mut(key) = None,
        i => {
            if let Some(choice) = choices.get(i - 1) {
                *filter.get_mut(key) = Some(choice.value.clone());
            }
        }
    }
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

/// Groups the tasks passing `filter` into one column per status (empty ones included; done and cancelled
/// only when `closed`), with subtasks under their parent, and works out
/// what each filter can offer.
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
        if !filter.closed && CLOSED.contains(&status) {
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
        let title: HashMap<&str, &str> = input.tasks.iter().map(|t| (s(t, "id"), s(t, "title"))).collect();
        let mut keys: Vec<(&str, &str)> = ids
            .iter()
            .filter_map(|id| Some((*task_key.get(id)?, title.get(id).copied().unwrap_or_default())))
            .collect();
        keys.sort_by_key(|(k, _)| (key_number(k), k.to_string()));
        keys.into_iter().map(|(k, t)| choice(k, parent_label(k, t))).collect()
    };
    let holders = {
        let ids: BTreeSet<&str> = holder_of.values().copied().collect();
        let mut v: Vec<Choice> = ids.into_iter().map(|id| choice(id, holder_label(id))).collect();
        v.sort_by(|a, b| a.label.cmp(&b.label));
        v
    };
    BoardView { columns, options: Options { projects, parents, holders } }
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

/// How many characters of a parent's title its filter option shows.
const PARENT_TITLE_CHARS: usize = 40;

/// A parent's filter option: its key, then its title cut with an ellipsis when long, since a key alone says nothing.
fn parent_label(key: &str, title: &str) -> String {
    if title.is_empty() {
        return key.to_string();
    }
    let title = if title.chars().count() <= PARENT_TITLE_CHARS {
        title.to_string()
    } else {
        title.chars().take(PARENT_TITLE_CHARS - 1).chain(['…']).collect()
    };
    format!("{key} {title}")
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

/// Tags a header dropdown with the filter it sets.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FilterSelect(pub FilterKey);

fn on_pick(pick: On<Picked>, selects: Query<&FilterSelect>, view: Res<BoardView>, mut filter: ResMut<Filter>) {
    let Ok(&FilterSelect(key)) = selects.get(pick.entity) else { return };
    let mut next = filter.clone();
    choose(&mut next, key, view.options.of(key), pick.index);
    filter.set_if_neq(next);
}

/// Tags the header button that shows or hides the closed columns.
#[derive(Component)]
struct ClosedToggle;

fn on_toggle_closed(activate: On<Activate>, toggles: Query<(), With<ClosedToggle>>, mut filter: ResMut<Filter>) {
    if toggles.contains(activate.entity) {
        filter.closed = !filter.closed;
    }
}

/// The node the columns sit in, which scrolls sideways. The panel spawns it; [`BoardPlugin`] keeps its children.
#[derive(Component)]
pub struct BoardColumns;

/// A column's shell, one entity per status for as long as the status is on the board.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct BoardColumn {
    pub status: String,
    /// How many cards the column holds, shown or not.
    pub count: usize,
}

/// The scrolling body of the column for this status, a child of its [`BoardColumn`].
#[derive(Component)]
pub struct ColumnBody(pub String);

/// A card, one entity per task for as long as the task is on the board. It carries the view model it was drawn from,
/// so it is redrawn when, and only when, that changes.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct BoardCard(pub Card);

/// The "Nothing here" line of a column without cards.
#[derive(Component)]
pub struct ColumnEmpty;

/// The "Show more" button at the foot of a column with cards past its page. `next` is how many one press adds.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct ColumnMore {
    pub status: String,
    pub next: usize,
}

/// What sits in a column's body besides the keys of its cards.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Slot {
    Card(Card),
    Empty,
    More(ColumnMore),
}

/// The entities the board has made, so a change touches only what it changes.
#[derive(Resource, Default)]
struct Tree {
    /// The [`BoardColumns`] node these belong to; a new one (the panel was rebuilt) starts them over.
    root: Option<Entity>,
    columns: Keyed<BoardColumn>,
    bodies: HashMap<String, Entity>,
    slots: Keyed<Slot>,
}

/// How many cards each column has been asked to build, by status; a column not in it builds [`PAGE`].
#[derive(Resource, Default)]
struct Shown(HashMap<String, usize>);

impl Shown {
    fn limit(&self, status: &str) -> usize {
        self.0.get(status).copied().unwrap_or(PAGE)
    }

    fn more(&mut self, status: &str) {
        let next = self.limit(status) + PAGE;
        self.0.insert(status.to_string(), next);
    }
}

/// Brings the columns and cards under [`BoardColumns`] to [`BoardView`], touching only what changed: a card that
/// left is despawned, a new one spawned, one that moved is re-parented, and one whose view model changed is given the
/// new model. Everything else keeps its entity, so nothing flashes and the scroll positions stay put.
fn sync_tree(
    view: Res<BoardView>,
    shown: Res<Shown>,
    root: Query<Entity, With<BoardColumns>>,
    mut tree: ResMut<Tree>,
    mut commands: Commands,
) {
    let root = root.single().ok();
    let Tree { root: drawn, columns, bodies, slots } = &mut *tree;
    if *drawn != root {
        // The old nodes went with their panel.
        *drawn = root;
        columns.forget();
        slots.forget();
        bodies.clear();
    } else if !(view.is_changed() || shown.is_changed()) {
        return;
    }
    let Some(root) = root else { return };

    let wanted = [(
        root,
        view.columns
            .iter()
            .map(|c| (c.status.clone(), BoardColumn { status: c.status.clone(), count: c.cards.len() }))
            .collect(),
    )];
    columns.sync(
        &mut commands,
        &wanted,
        |commands, status, model| {
            let shell = commands.spawn(model.clone()).id();
            let body = commands.spawn((ColumnBody(status.to_string()), ChildOf(shell))).id();
            bodies.insert(status.to_string(), body);
            shell
        },
        |commands, shell, model| {
            commands.entity(shell).insert(model.clone());
        },
    );

    let wanted: Vec<(Entity, Vec<(String, Slot)>)> = view
        .columns
        .iter()
        .filter_map(|column| {
            let (cards, hidden) = visible(&column.cards, shown.limit(&column.status));
            let mut items = Vec::new();
            if column.cards.is_empty() {
                items.push((format!("empty/{}", column.status), Slot::Empty));
            }
            items.extend(cards.iter().map(|c| (c.id.clone(), Slot::Card(c.clone()))));
            if hidden > 0 {
                let more = ColumnMore { status: column.status.clone(), next: hidden.min(PAGE) };
                items.push((format!("more/{}", column.status), Slot::More(more)));
            }
            Some((*bodies.get(&column.status)?, items))
        })
        .collect();
    slots.sync(
        &mut commands,
        &wanted,
        |commands, _, slot| match slot {
            Slot::Card(card) => commands.spawn(BoardCard(card.clone())).id(),
            Slot::Empty => commands.spawn(ColumnEmpty).id(),
            Slot::More(more) => commands.spawn(more.clone()).id(),
        },
        |commands, entity, slot| match slot {
            Slot::Card(card) => {
                commands.entity(entity).insert(BoardCard(card.clone()));
            }
            Slot::More(more) => {
                commands.entity(entity).insert(more.clone());
            }
            Slot::Empty => {}
        },
    );
}

/// Keeps [`Filter`], [`BoardView`] and the entities of the columns and cards from the mirror.
pub struct BoardPlugin;

impl Plugin for BoardPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Filter>()
            .init_resource::<BoardView>()
            .init_resource::<Shown>()
            .init_resource::<Tree>()
            .add_systems(Update, (rebuild_view, sync_tree).chain())
            .add_observer(on_pick)
            .add_observer(on_toggle_closed);
    }
}

/// Draws the board while in [`View::Board`].
pub struct BoardUiPlugin;

impl Plugin for BoardUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(BoardPlugin)
            .init_state::<View>()
            .add_systems(OnEnter(View::Board), spawn_panel)
            .add_systems(OnExit(View::Board), (save_scroll, despawn_panel).chain())
            .init_resource::<BoardScroll>()
            .add_observer(on_card_press)
            .add_observer(on_more_press)
            .add_systems(
                Update,
                (dress_columns, dress_cards, dress_slots, update_counts, tools_follow_connection, redraw_header)
                    .chain()
                    .after(sync_tree)
                    .run_if(in_state(View::Board)),
            );
    }
}

#[derive(Component)]
struct BoardPanel;

fn spawn_panel(mut commands: Commands, kept: Res<BoardScroll>) {
    let panel = commands
        .spawn((
            Name::new("board"),
            BoardPanel,
            Node {
                width: percent(100),
                height: percent(100),
                // A layout node is never smaller than its content unless told so: without these the panel grew past
                // the window to fit every card, so the board and its columns never overflowed (TRAIL-836).
                min_width: px(0),
                min_height: px(0),
                overflow: Overflow::clip(),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(px(8)),
                row_gap: px(6),
                ..default()
            },
        ))
        .id();
    commands.spawn((
        Name::new("header"),
        HeaderBar,
        Node { align_items: AlignItems::End, column_gap: px(space::GROUPS), ..default() },
        ChildOf(panel),
    ));
    // Columns past the window's edge are reached by scrolling sideways (Shift + wheel, or a touchpad swipe), and a
    // long column by scrolling down on its own. This node and the columns in it live as long as the panel.
    let columns = commands
        .spawn((
            Node {
                column_gap: px(space::GROUPS),
                align_items: AlignItems::FlexStart,
                flex_grow: 1.0,
                min_height: px(0),
                overflow: Overflow::scroll_x(),
                ..default()
            },
            BoardColumns,
            ScrollBoth,
            ScrollPosition(kept.board),
            ChildOf(panel),
        ))
        .id();
    commands.queue_spawn_scene(widgets::scrollbar(columns, ControlOrientation::Horizontal)).insert(ChildOf(panel));
}

fn despawn_panel(mut commands: Commands, panel: Query<Entity, With<BoardPanel>>) {
    for e in &panel {
        commands.entity(e).despawn();
    }
}

/// The header bar, which lives across redraws of the columns so an open dropdown isn't closed under the pointer.
#[derive(Component)]
struct HeaderBar;

/// What the cards of one column build now: the first `limit` of them, and how many are left out.
pub fn visible(cards: &[Card], limit: usize) -> (&[Card], usize) {
    let shown = limit.min(cards.len());
    (&cards[..shown], cards.len() - shown)
}

/// Where the board was scrolled to when the view last left it. The panel's nodes stay as they are while the board is
/// showing, so this only brings the positions back after a trip to another view, which builds the panel anew.
#[derive(Resource, Default)]
struct BoardScroll {
    board: Vec2,
    columns: HashMap<String, f32>,
}

fn save_scroll(
    board: Query<&ScrollPosition, With<BoardColumns>>,
    columns: Query<(&ColumnBody, &ScrollPosition)>,
    mut kept: ResMut<BoardScroll>,
) {
    if let Ok(position) = board.single() {
        kept.board = position.0;
    }
    for (column, position) in &columns {
        kept.columns.insert(column.0.clone(), position.y);
    }
}

/// Gives a new column its look: the shell, header, scrolling body and scrollbar.
fn dress_columns(
    bodies: Query<(Entity, &ColumnBody, &ChildOf), Added<ColumnBody>>,
    columns: Query<&BoardColumn>,
    kept: Res<BoardScroll>,
    mut commands: Commands,
) {
    for (body, ColumnBody(status), parent) in &bodies {
        let shell = parent.parent();
        let count = columns.get(shell).map_or(0, |c| c.count);
        let at = kept.columns.get(status).copied().unwrap_or_default();
        commands.entity(shell).insert(widgets::column_shell(status));
        commands.entity(body).insert((widgets::column_body(), ScrollPosition(Vec2::new(0.0, at))));
        // The body and its bar share a frame under the header, so the bar's track is the body's.
        let frame = commands.spawn((widgets::column_frame(), ChildOf(shell))).id();
        commands.entity(body).insert(ChildOf(frame));
        widgets::column_scrollbar(&mut commands, frame, body);
        widgets::column_header(&mut commands, shell, status, count);
    }
}

/// Keeps each header's count in step with its column, in place.
fn update_counts(mut counts: Query<(&widgets::ColumnCount, &mut Text)>, columns: Query<&BoardColumn>) {
    for (count, mut text) in &mut counts {
        if let Ok(column) = columns.get(count.0) {
            let now = column.count.to_string();
            if text.0 != now {
                text.0 = now;
            }
        }
    }
}

/// Marks a card's tool button, which is disabled while the connection isn't live.
#[derive(Component)]
struct CardTool;

/// Draws a card when it is new, and again only when its view model changed.
fn dress_cards(
    cards: Query<(Entity, &BoardCard, Option<&Children>), Changed<BoardCard>>,
    connection: Res<State<Connection>>,
    mut commands: Commands,
) {
    let live = *connection.get() == Connection::Live;
    for (shell, BoardCard(card), children) in &cards {
        for child in children.into_iter().flatten() {
            commands.entity(*child).despawn();
        }
        commands.entity(shell).apply_scene(card_shell(card.depth));
        commands.queue_spawn_scene(badge(card.key.clone(), None, None)).insert(ChildOf(shell));
        commands.queue_spawn_scene(card_title(card.title.clone())).insert(ChildOf(shell));
        if let Some(parent) = &card.parent {
            commands.queue_spawn_scene(card_under(parent.clone())).insert(ChildOf(shell));
        }
        let badges = commands.queue_spawn_scene(card_row()).insert(ChildOf(shell)).id();
        for b in &card.badges {
            let tone = (b.kind == BadgeKind::Questions).then_some(token::WARN);
            commands.queue_spawn_scene(badge(b.label.clone(), None, tone)).insert(ChildOf(badges));
        }
        let tools = commands.queue_spawn_scene(card_row()).insert(ChildOf(shell)).id();
        for action in &card.actions {
            let (icon, hint) = match action {
                CardAction::Edit => (Icon::Pencil, "Edit"),
                CardAction::Comment => (Icon::Comment, "Comment"),
                CardAction::Answer { .. } => (Icon::Help, "Answer the open question"),
            };
            let mut tool = commands.queue_spawn_scene(tool_button(icon, hint, ButtonVariant::Plain));
            tool.insert((action.act(&card.key), CardTool, ChildOf(tools)));
            if !live {
                tool.insert(InteractionDisabled);
            }
        }
    }
}

/// Enables and disables the cards' tool buttons as the connection goes live and away, without redrawing them.
fn tools_follow_connection(
    connection: Res<State<Connection>>,
    tools: Query<Entity, With<CardTool>>,
    mut commands: Commands,
) {
    if !connection.is_changed() {
        return;
    }
    let live = *connection.get() == Connection::Live;
    for tool in &tools {
        if live {
            commands.entity(tool).remove::<InteractionDisabled>();
        } else {
            commands.entity(tool).insert(InteractionDisabled);
        }
    }
}

/// Draws the "Nothing here" line and the "Show more" button when they appear, and the button again when its count
/// changes.
fn dress_slots(
    empty: Query<Entity, Added<ColumnEmpty>>,
    more: Query<(Entity, &ColumnMore, Option<&Children>), Changed<ColumnMore>>,
    mut commands: Commands,
) {
    for entity in &empty {
        commands.entity(entity).queue_apply_scene(widgets::column_empty());
    }
    for (entity, more, children) in &more {
        for child in children.into_iter().flatten() {
            commands.entity(*child).despawn();
        }
        let label = format!("Show {} more", more.next);
        commands.entity(entity).queue_apply_scene(button(label, None, ButtonVariant::Normal));
    }
}

/// Opens a card in the inspector when it is pressed.
fn on_card_press(activate: On<Activate>, cards: Query<&BoardCard>, mut selection: ResMut<Selection>) {
    if let Ok(BoardCard(card)) = cards.get(activate.entity) {
        selection.0 = Some(Subject::Task(card.id.clone()));
    }
}

fn on_more_press(activate: On<Activate>, more: Query<&ColumnMore>, mut shown: ResMut<Shown>) {
    if let Ok(more) = more.get(activate.entity) {
        shown.more(&more.status);
    }
}

/// What the header bar was last drawn from, so it is rebuilt only when one of these changes.
#[derive(Clone, PartialEq)]
struct HeaderDrawn {
    bar: Entity,
    filter: Filter,
    options: Options,
    connection: Connection,
    reason: String,
}

/// The connection chip's tone, words and hint.
fn connection_chip(
    connection: Connection,
    status: &ConnectionStatus,
) -> (bevy::feathers::theme::ThemeToken, &'static str, String) {
    let reason = status.reason.as_ref().map(|r| r.to_string());
    match connection {
        Connection::Live => (token::OK, "Live", "Connected to the factory".into()),
        Connection::Syncing => {
            (token::WARN, "Syncing", reason.unwrap_or_else(|| "Catching up with the factory".into()))
        }
        Connection::Offline => {
            (token::DANGER, "Offline", reason.unwrap_or_else(|| "Not connected to the factory".into()))
        }
    }
}

/// Draws the header bar: the app name, a dropdown per filter, New task, and the connection chip.
#[allow(clippy::too_many_arguments)]
fn redraw_header(
    view: Res<BoardView>,
    filter: Res<Filter>,
    connection: Res<State<Connection>>,
    status: Res<ConnectionStatus>,
    bar: Single<(Entity, Option<&Children>), With<HeaderBar>>,
    mut commands: Commands,
    popups: Query<&Visibility, With<FeathersMenuPopup>>,
    mut drawn: Local<Option<HeaderDrawn>>,
) {
    let (bar, children) = *bar;
    let now = HeaderDrawn {
        bar,
        filter: filter.clone(),
        options: view.options.clone(),
        connection: *connection.get(),
        reason: status.reason.as_ref().map(|r| r.to_string()).unwrap_or_default(),
    };
    // Rebuilding the bar would close a dropdown under the pointer; it is drawn when the dropdown has closed.
    if drawn.as_ref() == Some(&now) || popups.iter().any(|v| *v != Visibility::Hidden) {
        return;
    }
    for child in children.into_iter().flatten() {
        commands.entity(*child).despawn();
    }
    commands.queue_spawn_scene(label_text("Lookout", size::APP_TITLE)).insert(ChildOf(bar));
    for key in FilterKey::ALL {
        let choices = view.options.of(key);
        let mut names = vec!["all".to_string()];
        names.extend(choices.iter().map(|c| c.label.clone()));
        // A chosen value that is no longer offered shows as "all".
        let at = filter.get(key).as_ref().and_then(|v| choices.iter().position(|c| &c.value == v)).map_or(0, |i| i + 1);
        let control = bsn_list! { @select(names, Some(at), 8) FilterSelect({ key }) };
        commands.queue_spawn_scene(field(key.name(), control)).insert(ChildOf(bar));
    }
    let caption = if filter.closed { "closed: shown" } else { "closed: hidden" };
    commands.queue_spawn_scene(button(caption, None, ButtonVariant::Normal)).insert((ClosedToggle, ChildOf(bar)));
    commands
        .queue_spawn_scene(button("Trail map", None, ButtonVariant::Normal))
        .insert(ChildOf(bar))
        .observe(|_: On<Activate>, mut next: ResMut<NextState<View>>| next.set(View::Map));
    let live = *connection.get() == Connection::Live;
    let mut new = commands.queue_spawn_scene(button("New task", Some(Icon::Plus), ButtonVariant::Primary));
    new.insert((Act::New, ChildOf(bar)));
    if !live {
        new.insert(InteractionDisabled);
    }
    let (tone, words, hint) = connection_chip(*connection.get(), &status);
    commands.spawn((Node { flex_grow: 1.0, ..default() }, ChildOf(bar)));
    commands.queue_spawn_scene(chip(tone, words, hint)).insert(ChildOf(bar));
    *drawn = Some(now);
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
        let view = run(&Filter { closed: true, ..Default::default() });
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
    fn done_and_cancelled_are_hidden_until_asked_for() {
        let names = |f: Filter| run(&f).columns.into_iter().map(|c| c.status).collect::<Vec<_>>();
        assert_eq!(names(Filter::default()), ["open", "in_progress", "needs_input", "in_review", "approved"]);
        assert_eq!(names(Filter { closed: true, ..Default::default() }), STATUSES);
    }

    fn many(n: usize) -> Vec<Card> {
        (0..n)
            .map(|i| Card {
                id: format!("t{i}"),
                key: format!("TRAIL-{i}"),
                title: String::new(),
                parent: None,
                depth: 0,
                badges: vec![],
                actions: vec![],
            })
            .collect()
    }

    #[test]
    fn a_long_column_builds_one_page_and_counts_the_rest() {
        let cards = many(366);
        let (shown, hidden) = visible(&cards, PAGE);
        assert_eq!((shown.len(), hidden), (PAGE, 366 - PAGE));
        assert_eq!(shown[0].key, "TRAIL-0");
        // Show more adds a page, in the same order.
        let mut asked = Shown::default();
        assert_eq!(asked.limit("open"), PAGE);
        asked.more("open");
        let (shown, hidden) = visible(&cards, asked.limit("open"));
        assert_eq!((shown.len(), hidden), (2 * PAGE, 366 - 2 * PAGE));
        assert_eq!(shown[PAGE].key, format!("TRAIL-{PAGE}"));
        // Other columns are untouched, and a short one has nothing hidden.
        assert_eq!(asked.limit("done"), PAGE);
        assert_eq!(visible(&many(3), PAGE).1, 0);
        assert!(visible(&[], PAGE).0.is_empty());
    }
    #[test]
    fn filters_narrow_the_board() {
        let by = |f: Filter| -> Vec<String> {
            run(&f).columns.iter().flat_map(|c| c.cards.iter().map(|c| c.key.clone())).collect()
        };
        assert_eq!(by(Filter { project: Some("OPS".into()), ..Default::default() }), ["OPS-1"]);
        assert_eq!(by(Filter { parent: Some("TRAIL-102".into()), ..Default::default() }), ["TRAIL-105", "TRAIL-103"]);
        assert_eq!(by(Filter { holder: Some("a1".into()), ..Default::default() }), ["TRAIL-103"]);
        let none = Filter { project: Some("OPS".into()), holder: Some("a1".into()), ..Default::default() };
        assert!(by(none).is_empty());
    }

    #[test]
    fn options_list_what_the_data_has() {
        let view = run(&Filter::default());
        let labels = |c: &[Choice]| c.iter().map(|c| c.label.clone()).collect::<Vec<_>>();
        assert_eq!(labels(&view.options.projects), ["OPS", "TRAIL"]);
        assert_eq!(labels(&view.options.parents), ["TRAIL-102 title TRAIL-102", "TRAIL-103 title TRAIL-103"]);
        assert_eq!(view.options.parents[0].value, "TRAIL-102", "the filter still takes the key");
        assert_eq!(labels(&view.options.holders), ["factory/trail-103-a1", "u1"]);
    }

    #[test]
    fn a_long_parent_title_is_cut_with_an_ellipsis() {
        let label = parent_label("TRAIL-685", &"x".repeat(60));
        assert_eq!(label.chars().count(), "TRAIL-685 ".len() + PARENT_TITLE_CHARS);
        assert!(label.ends_with('…'));
        assert_eq!(parent_label("TRAIL-9", ""), "TRAIL-9");
    }

    #[test]
    fn choosing_a_row_sets_the_filter_and_row_zero_clears_it() {
        let view = run(&Filter::default());
        let projects = &view.options.projects;
        let mut filter = Filter::default();
        choose(&mut filter, FilterKey::Project, projects, 2);
        assert_eq!(filter.project.as_deref(), Some("TRAIL"));
        choose(&mut filter, FilterKey::Project, projects, 1);
        assert_eq!(filter.project.as_deref(), Some("OPS"));
        // A row past the list leaves it alone.
        choose(&mut filter, FilterKey::Project, projects, 9);
        assert_eq!(filter.project.as_deref(), Some("OPS"));
        choose(&mut filter, FilterKey::Project, projects, 0);
        assert_eq!(filter, Filter::default());
    }

    #[test]
    fn picking_a_dropdown_option_sets_the_filter() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(BoardPlugin);
        *app.world_mut().resource_mut::<BoardView>() = run(&Filter::default());
        let parent = app.world_mut().spawn(FilterSelect(FilterKey::Parent)).id();
        app.world_mut().trigger(Picked { entity: parent, index: 2 });
        assert_eq!(app.world().resource::<Filter>().parent.as_deref(), Some("TRAIL-103"));
        app.world_mut().trigger(Picked { entity: parent, index: 0 });
        assert_eq!(app.world().resource::<Filter>().parent, None);
    }

    #[test]
    fn the_closed_toggle_shows_and_hides_done_and_cancelled() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(BoardPlugin);
        let toggle = app.world_mut().spawn(ClosedToggle).id();
        let closed_columns = |app: &App| {
            let columns = run(app.world().resource::<Filter>()).columns;
            columns.into_iter().map(|c| c.status).filter(|s| CLOSED.contains(&s.as_str())).collect::<Vec<_>>()
        };
        assert!(closed_columns(&app).is_empty());
        app.world_mut().trigger(Activate { entity: toggle });
        assert_eq!(closed_columns(&app), ["done", "cancelled"]);
        app.world_mut().trigger(Activate { entity: toggle });
        assert!(closed_columns(&app).is_empty());
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
        app.world_mut().resource_mut::<Filter>().closed = true;
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

#[cfg(test)]
mod in_place_tests {
    use serde_json::json;

    use super::*;
    use crate::mirror::MirrorPlugin;
    use crate::sync::{Snapshot, SyncItem};

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins((MirrorPlugin, BoardPlugin));
        app.world_mut().spawn(BoardColumns);
        let task = |id: &str, status: &str| json!({"id": id, "key": id.to_uppercase(), "title": id, "project_id": "p1", "status": status, "priority": 2});
        let tasks = vec![task("t1", "open"), task("t2", "open"), task("t3", "in_progress"), task("t4", "in_progress")];
        let projects = vec![json!({"id": "p1", "key": "TRAIL"}), json!({"id": "p2", "key": "OPS"})];
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot { at: 1, projects, tasks, agents: vec![] }));
        settle(&mut app);
        app
    }

    fn settle(app: &mut App) {
        for _ in 0..3 {
            app.update();
        }
    }

    fn update_task(app: &mut App, id: &str, status: &str) {
        app.world_mut().write_message(SyncItem::Event(json!({
            "id": 9, "entity": "task", "entity_id": id, "kind": "task.updated",
            "after": {"id": id, "key": id.to_uppercase(), "title": id, "project_id": "p1", "status": status,
                      "priority": 2, "version": 2},
        })));
        settle(app);
    }

    fn cards(app: &mut App) -> HashMap<String, Entity> {
        let mut q = app.world_mut().query::<(Entity, &BoardCard)>();
        q.iter(app.world()).map(|(e, c)| (c.0.id.clone(), e)).collect()
    }

    fn column_of(app: &mut App, card: Entity) -> String {
        let body = app.world().get::<ChildOf>(card).expect("a card has a column").parent();
        app.world().get::<ColumnBody>(body).expect("in a column body").0.clone()
    }

    fn count_cards(app: &mut App) -> usize {
        let mut q = app.world_mut().query::<&BoardCard>();
        q.iter(app.world()).count()
    }

    #[test]
    fn changing_one_task_leaves_every_other_card_as_it_was() {
        let mut app = app();
        let before = cards(&mut app);
        assert_eq!(before.len(), 4);
        let mut q = app.world_mut().query::<(Entity, &BoardColumn)>();
        let columns: Vec<Entity> = q.iter(app.world()).map(|(e, _)| e).collect();

        app.world_mut().write_message(SyncItem::Event(json!({
            "id": 9, "entity": "task", "entity_id": "t1", "kind": "task.updated",
            "after": {"id": "t1", "key": "T1", "title": "renamed", "project_id": "p1", "status": "open",
                      "priority": 2, "version": 2},
        })));
        settle(&mut app);
        let after = cards(&mut app);
        assert_eq!(before, after, "the same entities, one of them redrawn");
        let renamed = app.world().get::<BoardCard>(after["t1"]).unwrap();
        assert_eq!(renamed.0.title, "renamed");
        let mut q = app.world_mut().query::<(Entity, &BoardColumn)>();
        assert_eq!(columns, q.iter(app.world()).map(|(e, _)| e).collect::<Vec<_>>());
    }

    #[test]
    fn a_card_that_changes_column_keeps_its_entity_and_leaves_no_copy() {
        let mut app = app();
        let before = cards(&mut app);
        assert_eq!(column_of(&mut app, before["t1"]), "open");

        update_task(&mut app, "t1", "in_progress");
        let after = cards(&mut app);
        assert_eq!(after, before);
        assert_eq!(count_cards(&mut app), 4, "no stale copy in the old column");
        assert_eq!(column_of(&mut app, after["t1"]), "in_progress");
        // The columns' children are what the view says.
        let mut q = app.world_mut().query::<(&ColumnBody, &Children)>();
        let held: HashMap<String, usize> = q.iter(app.world()).map(|(b, c)| (b.0.clone(), c.len())).collect();
        assert_eq!((held["open"], held["in_progress"]), (1, 3));
    }

    #[test]
    fn a_filter_change_despawns_only_the_cards_it_hides() {
        let mut app = app();
        let before = cards(&mut app);
        // Give two of the tasks to the other project.
        for id in ["t2", "t4"] {
            app.world_mut().write_message(SyncItem::Event(json!({
                "id": if id == "t2" { 9 } else { 10 }, "entity": "task", "entity_id": id, "kind": "task.updated",
                "after": {"id": id, "key": id.to_uppercase(), "title": id, "project_id": "p2",
                          "status": if id == "t2" { "open" } else { "in_progress" }, "priority": 2, "version": 2},
            })));
        }
        settle(&mut app);
        assert_eq!(cards(&mut app), before);

        app.world_mut().resource_mut::<Filter>().project = Some("TRAIL".into());
        settle(&mut app);
        let after = cards(&mut app);
        let mut ids: Vec<_> = after.keys().cloned().collect();
        ids.sort();
        assert_eq!(ids, ["t1", "t3"]);
        for id in ids {
            assert_eq!(after[&id], before[&id]);
        }
        for id in ["t2", "t4"] {
            assert!(app.world().get_entity(before[id]).is_err(), "{id} is despawned");
        }
    }

    #[test]
    fn closed_columns_come_and_go_without_touching_the_open_ones() {
        let mut app = app();
        let before = cards(&mut app);
        update_task(&mut app, "t1", "done");
        // Done is hidden: its card is gone, the rest are untouched.
        let hidden = cards(&mut app);
        assert_eq!(hidden.len(), 3);
        assert_eq!(hidden["t2"], before["t2"]);

        app.world_mut().resource_mut::<Filter>().closed = true;
        settle(&mut app);
        let shown = cards(&mut app);
        assert_eq!(shown.len(), 4);
        assert_eq!(column_of(&mut app, shown["t1"]), "done");
        for id in ["t2", "t3", "t4"] {
            assert_eq!(shown[id], before[id]);
        }
    }

    #[test]
    fn showing_more_adds_cards_and_keeps_the_ones_shown() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins((MirrorPlugin, BoardPlugin));
        app.world_mut().spawn(BoardColumns);
        let tasks: Vec<Value> = (0..PAGE + 5)
            .map(|i| {
                json!({"id": format!("t{i}"), "key": format!("TRAIL-{i}"), "title": "", "project_id": "p1",
                            "status": "open", "priority": 2})
            })
            .collect();
        let projects = vec![json!({"id": "p1", "key": "TRAIL"})];
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot { at: 1, projects, tasks, agents: vec![] }));
        settle(&mut app);
        let before = cards(&mut app);
        assert_eq!(before.len(), PAGE);
        let mut q = app.world_mut().query::<&ColumnMore>();
        assert_eq!(q.single(app.world()).unwrap().next, 5);

        app.world_mut().resource_mut::<Shown>().more("open");
        settle(&mut app);
        let after = cards(&mut app);
        assert_eq!(after.len(), PAGE + 5);
        for (id, e) in &before {
            assert_eq!(after[id], *e);
        }
        let mut q = app.world_mut().query::<&ColumnMore>();
        assert_eq!(q.iter(app.world()).count(), 0, "nothing left to show");
    }
}
