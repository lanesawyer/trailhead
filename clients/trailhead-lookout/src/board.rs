//! The task board (`docs/factory/ui.md#views`), read-only: tasks by status with subtasks under their parent, each
//! a card with badges (priority, grade, holder, open questions) and icon actions, filtered by project,
//! parent and holder, with the closed columns behind a toggle.
//!
//! [`build`] is a pure function over the mirror's JSON, so the queries are tested without a window. [`BoardPlugin`]
//! keeps a [`BoardView`] from the mirror and the [`Filter`]; [`BoardUiPlugin`] draws it with feathers, greyed out
//! with the reason whenever the connection isn't [`Connection::Live`].

use std::collections::{BTreeSet, HashMap, HashSet};

use bevy::feathers::controls::ButtonVariant;
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
    Icon, Picked, ScrollBoth, badge, button, card as card_shell, card_row, card_title, card_under, chip, field, select,
    size, space, text as label_text, token, tool_button,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterKey {
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
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
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

/// Keeps [`Filter`] and [`BoardView`] from the mirror.
pub struct BoardPlugin;

impl Plugin for BoardPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Filter>()
            .init_resource::<BoardView>()
            .add_systems(Update, rebuild_view)
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
            .add_systems(OnExit(View::Board), despawn_panel)
            .init_resource::<BoardScroll>()
            .init_resource::<Shown>()
            .add_systems(Update, (remember_scroll, redraw, redraw_header).chain().run_if(in_state(View::Board)));
    }
}

#[derive(Component)]
struct BoardPanel;

fn spawn_panel(mut commands: Commands) {
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

/// Where the board is scrolled to, kept across redraws: each redraw builds new scrolling nodes, which would start
/// back at the top left.
#[derive(Resource, Default)]
struct BoardScroll {
    board: Vec2,
    columns: HashMap<String, f32>,
}

/// The node holding the columns, which scrolls both ways.
#[derive(Component)]
struct BoardColumns;

/// The scrolling body of the column for this status.
#[derive(Component)]
struct ColumnBody(String);

fn remember_scroll(
    board: Query<&ScrollPosition, (With<BoardColumns>, Changed<ScrollPosition>)>,
    columns: Query<(&ColumnBody, &ScrollPosition), Changed<ScrollPosition>>,
    mut kept: ResMut<BoardScroll>,
) {
    if let Some(position) = board.iter().next() {
        kept.board = position.0;
    }
    for (column, position) in &columns {
        kept.columns.insert(column.0.clone(), position.y);
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
fn redraw_header(
    view: Res<BoardView>,
    filter: Res<Filter>,
    connection: Res<State<Connection>>,
    status: Res<ConnectionStatus>,
    bar: Single<(Entity, Option<&Children>), With<HeaderBar>>,
    mut commands: Commands,
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
    if drawn.as_ref() == Some(&now) {
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
        let field = commands.queue_spawn_scene(field(key.name())).insert(ChildOf(bar)).id();
        commands.queue_spawn_scene(select(names, Some(at), 8)).insert((FilterSelect(key), ChildOf(field)));
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

/// Redraws the whole panel when the view, filter, connection or a column's page changes. Each column builds at most
/// [`PAGE`] cards at a time, so a redraw costs the same however long the backlog.
#[allow(clippy::too_many_arguments)]
fn redraw(
    view: Res<BoardView>,
    filter: Res<Filter>,
    connection: Res<State<Connection>>,
    status: Res<ConnectionStatus>,
    scroll: Res<BoardScroll>,
    shown: Res<Shown>,
    panel: Single<(Entity, Option<&Children>), With<BoardPanel>>,
    header: Query<(), With<HeaderBar>>,
    mut commands: Commands,
    mut drawn: Local<Option<Entity>>,
) {
    let (panel, children) = *panel;
    let fresh = *drawn != Some(panel);
    if !(fresh
        || view.is_changed()
        || filter.is_changed()
        || shown.is_changed()
        || connection.is_changed()
        || status.is_changed())
    {
        return;
    }
    *drawn = Some(panel);
    for child in children.into_iter().flatten().filter(|c| !header.contains(**c)) {
        commands.entity(*child).despawn();
    }
    let live = *connection.get() == Connection::Live;

    // Columns past the window's edge are reached by scrolling sideways (Shift + wheel, or a touchpad swipe), and a
    // long column by scrolling down on its own.
    let columns = commands
        .spawn((
            Node {
                column_gap: px(space::GROUPS),
                align_items: AlignItems::Stretch,
                flex_grow: 1.0,
                min_height: px(0),
                overflow: Overflow::scroll_x(),
                ..default()
            },
            BoardColumns,
            ScrollBoth,
            ScrollPosition(scroll.board),
            ChildOf(panel),
        ))
        .id();
    commands.queue_spawn_scene(widgets::scrollbar(columns, ControlOrientation::Horizontal)).insert(ChildOf(panel));
    for column in &view.columns {
        let (cards, hidden) = visible(&column.cards, shown.limit(&column.status));
        let col = widgets::column(
            &mut commands,
            columns,
            &column.status,
            column.cards.len(),
            scroll.columns.get(&column.status).copied().unwrap_or_default(),
        );
        commands.entity(col).insert(ColumnBody(column.status.clone()));
        if column.cards.is_empty() {
            commands.queue_spawn_scene(widgets::column_empty()).insert(ChildOf(col));
        }
        for card in cards {
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
        if hidden > 0 {
            let status = column.status.clone();
            let label = format!("Show {} more", hidden.min(PAGE));
            let mut more = commands.queue_spawn_scene(button(label, None, ButtonVariant::Normal));
            more.insert(ChildOf(col)).observe(move |_: On<Activate>, mut shown: ResMut<Shown>| shown.more(&status));
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
        assert_eq!(labels(&view.options.parents), ["TRAIL-102", "TRAIL-103"]);
        assert_eq!(labels(&view.options.holders), ["factory/trail-103-a1", "u1"]);
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
