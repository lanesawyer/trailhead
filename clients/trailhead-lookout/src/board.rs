//! The task board (`docs/factory/ui.md#views`), read-only: tasks by status with subtasks under their parent, each
//! a card with badges (priority, grade, holder, open questions) and icon actions, filtered by project,
//! parent and holder, with the closed columns behind a toggle.
//!
//! [`build`] is a pure function over the mirror's JSON, so the queries are tested without a window. [`BoardPlugin`]
//! keeps a [`BoardView`] from the mirror and the [`Filter`]; [`BoardUiPlugin`] draws it with feathers, greyed out
//! with the reason whenever the connection isn't [`Connection::Live`].

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy::ui::InteractionDisabled;
use bevy::ui_widgets::{Activate, ControlOrientation};
use jiff::Timestamp;
use trailhead_model::{Agent, Claim, Comment, CommentKind, Dependency, Project, Status, Task, User};
use uuid::Uuid;

use crate::author::{Act, Authoring};
use crate::connection::Connection;
use crate::factory::{FactoryStatus, Run};
use crate::inspector::{Neighbours, Selection, Subject};
use crate::mirror::{Mirror, Mirrored, Rev, rows};
use crate::search::{Deep, DeepHit};
use crate::widgets;
use crate::widgets::{
    ButtonKind, Hint, Icon, Keyed, Picked, ScrollBoth, SelectEdit, Token, badge, button, card as card_shell,
    card_found, card_row, card_title, card_under, field, grade_tone, priority_tone, select, space, status_label, token,
    tool_button,
};

/// The views of the app: the task board, the trail map, and one project's page ([`crate::project`]).
#[derive(States, Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum View {
    #[default]
    Board,
    Map,
    Project,
}

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
    /// Show only the factory's tasks: those under one of its queue tasks and those with `factory` metadata from a run.
    pub factory: bool,
    /// What the header's search box says. Narrows the board to tasks whose key or title match, and adds the ones the
    /// server found deeper down.
    pub query: String,
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
    /// A task under a factory queue with no grade: the factory skips it until a person grades it.
    NeedsGrade,
    Holder,
    /// A task the factory has: queued, or carrying a run's metadata.
    Factory,
    /// Open questions, with their count.
    Questions,
    /// An in-review task whose newest review asks for changes, with no handoff since: the rework is the factory's
    /// move (or a person's), not the reviewer's.
    ChangesRequested,
    /// An open task with unfinished blockers, with their count; the hint names them.
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Badge {
    pub kind: BadgeKind,
    pub label: String,
    /// The theme token that tints the badge; none leaves it dim.
    pub tone: Option<Token>,
    /// What a hover says, for a badge whose label is short for something longer.
    pub hint: Option<String>,
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
    /// Priority, factory, grade, holder and open questions, in that order, leaving out what the task doesn't have.
    pub badges: Vec<Badge>,
    /// Edit and comment, and answer when a question is open.
    pub actions: Vec<CardAction>,
    /// Where a search matched this task when its key and title did not: "in description: …".
    pub found: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub status: Status,
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
    pub projects: &'a [Project],
    pub tasks: &'a [Task],
    pub claims: &'a [Claim],
    /// The blocker edges.
    pub dependencies: &'a [Dependency],
    pub comments: &'a [Comment],
    pub agents: &'a [Agent],
    pub users: &'a [User],
    /// The runs in flight, which name a holder the agent list hasn't caught up with.
    pub runs: &'a [Run],
    /// The keys of the factory's queue tasks, whose subtasks the factory works.
    pub queues: &'a [String],
    /// What the server found for the filter's query, if it has answered it.
    pub deep: &'a [DeepHit],
    /// The time claims are judged against: core expires them lazily, so one past its `expires_at` may still be
    /// in the mirror.
    pub now: Timestamp,
}

/// Groups the tasks passing `filter` into one column per status (empty ones included; done and cancelled
/// only when `closed`), with subtasks under their parent, and works out
/// what each filter can offer.
pub fn build(input: Input, filter: &Filter) -> BoardView {
    build_with(input, filter, true)
}

/// Every task in the project keyed `project`, closed ones included, as cards in the board's order (by status, then
/// urgency) without nesting: a subtask is a card of its own that names its parent.
pub fn grid(input: Input, project: &str) -> Vec<Card> {
    let filter = Filter { project: Some(project.to_string()), closed: true, ..default() };
    build_with(input, &filter, false).columns.into_iter().flat_map(|c| c.cards).collect()
}

/// [`build`], with subtasks under their parent in a column when `nest`, else each a top-level card naming its parent.
fn build_with(input: Input, filter: &Filter, nest: bool) -> BoardView {
    let project_key: HashMap<Uuid, &str> = input.projects.iter().map(|p| (p.id, p.key.as_str())).collect();
    let task_key: HashMap<Uuid, &str> = input.tasks.iter().map(|t| (t.id, t.key.as_str())).collect();
    // Holders are agents or, for a claim a person made, users.
    let holder_name: HashMap<Uuid, &str> = input
        .agents
        .iter()
        .map(|a| (a.id, a.name.as_str()))
        .chain(input.users.iter().map(|u| (u.id, u.name.as_str())))
        .collect();
    // The claim is keyed by its task.
    let holder_of: HashMap<Uuid, Uuid> =
        input.claims.iter().filter(|c| c.is_live(input.now)).map(|c| (c.task_id, c.holder_id)).collect();
    let running: HashSet<&str> = input.runs.iter().map(|r| r.status.task.0.as_str()).collect();
    // The agent list only changes with a sync snapshot, so a sub-agent made for a run after it isn't known yet. A
    // holder of a task that has a run is a factory agent all the same: name it for the task until the list has it.
    let holder_label = |task: Uuid, id: Uuid| match holder_name.get(&id) {
        Some(n) => n.to_string(),
        None => match task_key.get(&task) {
            Some(k) if running.contains(k) => format!("factory/{}", k.to_lowercase()),
            _ => short(id),
        },
    };
    // The blockers of each task that haven't finished, by the statuses mirrored now (the rule of core's `ready`), so
    // one finishing clears the badge with no fetch. A blocker the mirror lacks (deleted, out of scope) doesn't count.
    let status_of: HashMap<Uuid, Status> = input.tasks.iter().map(|t| (t.id, t.status)).collect();
    let mut unfinished: HashMap<Uuid, Vec<&str>> = HashMap::new();
    for d in input.dependencies {
        if let (Some(status), Some(key)) = (status_of.get(&d.blocker_id), task_key.get(&d.blocker_id))
            && !status.is_closed()
        {
            unfinished.entry(d.blocked_id).or_default().push(key);
        }
    }
    let parent_key = |t: &Task| t.parent_id.and_then(|p| task_key.get(&p).copied());

    let mut questions: HashMap<Uuid, Vec<&Comment>> = HashMap::new();
    for c in input.comments {
        if c.kind == CommentKind::Question && c.resolved_at.is_none() {
            questions.entry(c.task_id).or_default().push(c);
        }
    }

    // The comments of each in-review task, which decide whether it is waiting on a rework.
    let in_review: HashSet<Uuid> = input.tasks.iter().filter(|t| t.status == Status::InReview).map(|t| t.id).collect();
    let mut reviews: HashMap<Uuid, Vec<&Comment>> = HashMap::new();
    for c in input.comments.iter().filter(|c| in_review.contains(&c.task_id)) {
        reviews.entry(c.task_id).or_default().push(c);
    }

    let in_queue = |t: &Task| parent_key(t).is_some_and(|k| input.queues.iter().any(|q| q == k));
    let is_factory = |t: &Task| in_queue(t) || has_run(t);

    let passes = |t: &Task| {
        filter.project.as_deref().is_none_or(|k| project_key.get(&t.project_id) == Some(&k))
            && filter.parent.as_deref().is_none_or(|k| parent_key(t) == Some(k))
            && filter.holder.as_deref().is_none_or(|h| holder_of.get(&t.id).is_some_and(|id| id.to_string() == h))
            && (!filter.factory || is_factory(t))
    };
    let query = filter.query.trim();
    let deep_hit = |t: &Task| input.deep.iter().find(|h| h.id == t.id.to_string());
    let local = |t: &Task| matches_key_or_title(query, t.key.as_str(), &t.title);
    // A task only the server found is shown with where it matched.
    let found = |t: &Task| if query.is_empty() || local(t) { None } else { deep_hit(t).map(found_hint) };
    let searched = |t: &Task| query.is_empty() || local(t) || deep_hit(t).is_some();

    let card = |t: &Task, depth: usize, with_parent: bool| {
        let mut open: Vec<&Comment> = questions.get(&t.id).cloned().unwrap_or_default();
        open.sort_by_key(|c| c.created_at);
        let p = t.priority;
        let mut badges =
            vec![Badge { kind: BadgeKind::Priority, label: format!("P{p}"), tone: Some(priority_tone(p)), hint: None }];
        if is_factory(t) {
            badges.push(Badge {
                kind: BadgeKind::Factory,
                label: "factory".into(),
                tone: Some(token::INFO),
                hint: None,
            });
        }
        if let Some(grade) = t.grade {
            badges.push(Badge {
                kind: BadgeKind::Grade,
                label: format!("G{grade}"),
                tone: Some(grade_tone(grade)),
                hint: None,
            });
        } else if !t.status.is_closed() && in_queue(t) {
            badges.push(Badge {
                kind: BadgeKind::NeedsGrade,
                label: "needs grade".into(),
                tone: Some(token::WARN),
                hint: None,
            });
        }
        if let Some(holder) = holder_of.get(&t.id) {
            badges.push(Badge {
                kind: BadgeKind::Holder,
                label: holder_badge(&holder_label(t.id, *holder)),
                tone: None,
                hint: None,
            });
        }
        if trailhead_model::changes_requested(t.status, reviews.get(&t.id).into_iter().flatten().copied()).is_some() {
            badges.push(Badge {
                kind: BadgeKind::ChangesRequested,
                label: "changes requested".into(),
                tone: Some(token::WARN),
                hint: None,
            });
        }
        if !t.status.is_closed()
            && let Some(keys) = unfinished.get(&t.id)
        {
            let mut keys = keys.clone();
            keys.sort_unstable();
            badges.push(Badge {
                kind: BadgeKind::Blocked,
                label: format!("blocked · {}", keys.len()),
                tone: Some(token::WARN),
                hint: Some(format!("Blocked by {}", keys.join(", "))),
            });
        }
        if !open.is_empty() {
            badges.push(Badge {
                kind: BadgeKind::Questions,
                label: format!("? {}", open.len()),
                tone: Some(token::WARN),
                hint: None,
            });
        }
        let mut actions = vec![CardAction::Edit, CardAction::Comment];
        if let Some(q) = open.first() {
            actions.push(CardAction::Answer { comment_id: q.id.to_string(), question: q.body.clone() });
        }
        Card {
            id: t.id.to_string(),
            key: t.key.0.clone(),
            title: t.title.clone(),
            parent: parent_key(t).filter(|_| with_parent).map(str::to_string),
            depth,
            badges,
            actions,
            found: found(t),
        }
    };

    let shown: Vec<&Task> = input.tasks.iter().filter(|t| passes(t) && searched(t)).collect();
    let mut columns = Vec::new();
    for status in Status::ALL {
        if !filter.closed && status.is_closed() {
            continue;
        }
        let mut here: Vec<&Task> = shown.iter().copied().filter(|t| t.status == status).collect();
        // Urgent first, then by key number, so the order doesn't shuffle with every update.
        here.sort_by_key(|t| (t.priority, key_number(t.key.as_str()), t.key.clone()));
        let ids: HashSet<Uuid> = here.iter().map(|t| t.id).collect();
        let nested = |t: &Task| nest && t.parent_id.is_some_and(|p| ids.contains(&p));
        let mut cards = Vec::new();
        for t in here.iter().filter(|t| !nested(t)) {
            cards.push(card(t, 0, true));
            if nest {
                cards.extend(here.iter().filter(|c| c.parent_id == Some(t.id)).map(|c| card(c, 1, false)));
            }
        }
        columns.push(Column { status, cards });
    }

    let choice = |value: &str, label: String| Choice { value: value.to_string(), label };
    let projects = {
        let mut keys: Vec<&str> = project_key.values().copied().collect();
        keys.sort();
        keys.into_iter().map(|k| choice(k, k.to_string())).collect()
    };
    let parents = {
        let ids: BTreeSet<Uuid> = input.tasks.iter().filter_map(|t| t.parent_id).collect();
        let title: HashMap<Uuid, &str> = input.tasks.iter().map(|t| (t.id, t.title.as_str())).collect();
        let mut keys: Vec<(&str, &str)> = ids
            .iter()
            .filter_map(|id| Some((*task_key.get(id)?, title.get(id).copied().unwrap_or_default())))
            .collect();
        keys.sort_by_key(|(k, _)| (key_number(k), k.to_string()));
        keys.into_iter().map(|(k, t)| choice(k, parent_label(k, t))).collect()
    };
    let holders = {
        let ids: BTreeMap<Uuid, Uuid> = holder_of.iter().map(|(t, h)| (*h, *t)).collect();
        let mut v: Vec<Choice> = ids.into_iter().map(|(id, t)| choice(&id.to_string(), holder_label(t, id))).collect();
        v.sort_by(|a, b| a.label.cmp(&b.label));
        v
    };
    BoardView { columns, options: Options { projects, parents, holders } }
}

/// Whether a run left `factory` metadata on the task (in progress, in review, parked or waiting for a rework).
fn has_run(t: &Task) -> bool {
    t.metadata.get("factory").is_some_and(|v| !v.is_null())
}

/// Whether every word of `query` matches the task's key or title, ignoring case. A word matches the key when it starts
/// it (`trail-80`) or its number (`801`), so `801` finds TRAIL-801 and not TRAIL-1801; it matches the title anywhere.
fn matches_key_or_title(query: &str, key: &str, title: &str) -> bool {
    let (key, title) = (key.to_lowercase(), title.to_lowercase());
    let number = key.rsplit('-').next().unwrap_or_default();
    query.split_whitespace().map(str::to_lowercase).all(|w| {
        key.starts_with(&w) || (w.chars().all(|c| c.is_ascii_digit()) && number.starts_with(&w)) || title.contains(&w)
    })
}

/// The card's line for a task the server found by its description or a comment.
fn found_hint(hit: &DeepHit) -> String {
    let place = match hit.field.as_str() {
        "comment" => "comment",
        "title" => "title",
        _ => "description",
    };
    format!("in {place}: {}", hit.snippet.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn short(id: Uuid) -> String {
    id.to_string().chars().take(8).collect()
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

/// What the views of tasks build from: the mirror's rows, the factory's queues, and what says any of them changed.
#[derive(SystemParam)]
pub struct Sources<'w, 's> {
    mirror: Res<'w, Mirror>,
    changed: Query<'w, 's, (), Changed<Rev>>,
    removed: RemovedComponents<'w, 's, Rev>,
    removed_runs: RemovedComponents<'w, 's, Run>,
    projects: Query<'w, 's, &'static Mirrored<Project>>,
    tasks: Query<'w, 's, &'static Mirrored<Task>>,
    claims: Query<'w, 's, &'static Mirrored<Claim>>,
    dependencies: Query<'w, 's, &'static Mirrored<Dependency>>,
    comments: Query<'w, 's, &'static Mirrored<Comment>>,
    agents: Query<'w, 's, &'static Mirrored<Agent>>,
    users: Query<'w, 's, &'static Mirrored<User>>,
    runs: Query<'w, 's, &'static Run>,
    runs_changed: Query<'w, 's, (), Changed<Run>>,
    authoring: Res<'w, Authoring>,
    factory: Option<Res<'w, FactoryStatus>>,
}

impl Sources<'_, '_> {
    /// Whether anything changed since the system using this last ran. It reads the removals, so call it on every run.
    pub fn changed(&mut self) -> bool {
        let gone = self.removed.read().count() + self.removed_runs.read().count() > 0;
        gone || self.mirror.is_changed()
            || self.authoring.is_changed()
            || self.factory.as_ref().is_some_and(|f| f.is_changed())
            || !self.changed.is_empty()
            || !self.runs_changed.is_empty()
    }

    /// The rows, copied out of the mirror for [`Rows::input`].
    pub fn rows(&self) -> Rows {
        Rows {
            projects: rows(&self.projects),
            tasks: rows(&self.tasks),
            claims: rows(&self.claims),
            dependencies: rows(&self.dependencies),
            comments: rows(&self.comments),
            agents: rows(&self.agents),
            users: rows(&self.users),
            runs: self.runs.iter().cloned().collect(),
            queues: queue_keys(&self.authoring, self.factory.as_deref()),
        }
    }
}

/// The mirror's rows as [`Sources::rows`] copied them.
pub struct Rows {
    pub projects: Vec<Project>,
    pub tasks: Vec<Task>,
    pub claims: Vec<Claim>,
    pub dependencies: Vec<Dependency>,
    pub comments: Vec<Comment>,
    pub agents: Vec<Agent>,
    pub users: Vec<User>,
    pub runs: Vec<Run>,
    pub queues: Vec<String>,
}

impl Rows {
    pub fn input<'a>(&'a self, deep: &'a [DeepHit], now: Timestamp) -> Input<'a> {
        Input {
            projects: &self.projects,
            tasks: &self.tasks,
            claims: &self.claims,
            dependencies: &self.dependencies,
            comments: &self.comments,
            agents: &self.agents,
            users: &self.users,
            runs: &self.runs,
            queues: &self.queues,
            deep,
            now,
        }
    }
}

fn rebuild_view(
    filter: Res<Filter>,
    mut sources: Sources,
    deep: Option<Res<Deep>>,
    mut view: ResMut<BoardView>,
    mut expiry: Local<Option<Timestamp>>,
) {
    let changed = sources.changed();
    let now = Timestamp::now();
    let expired = expiry.is_some_and(|at| at <= now);
    if !(changed || filter.is_changed() || deep.as_ref().is_some_and(|d| d.is_changed()) || expired) {
        return;
    }
    let rows = sources.rows();
    *expiry = next_expiry(&rows.claims, now);
    let deep = deep.as_ref().map_or(&[][..], |d| d.for_query(filter.query.trim()));
    let next = build(rows.input(deep, now), &filter);
    if *view != next {
        *view = next;
    }
}

/// The first moment a claim still live at `now` runs out, if any does.
pub fn next_expiry(claims: &[Claim], now: Timestamp) -> Option<Timestamp> {
    claims.iter().filter(|c| c.is_live(now)).filter_map(|c| c.expires_at).min()
}

/// The cards before and after the task `id` in its column, in the board's order (a subtask under its parent counts
/// where it sits). A task is in one column at most, since its status picks it.
pub fn neighbours(view: &BoardView, id: &str) -> Neighbours {
    for column in &view.columns {
        if let Some(i) = column.cards.iter().position(|c| c.id == id) {
            return Neighbours {
                previous: i.checked_sub(1).map(|p| column.cards[p].id.clone()),
                next: column.cards.get(i + 1).map(|c| c.id.clone()),
            };
        }
    }
    Neighbours::default()
}

/// Keeps [`Neighbours`] for the selected task as the selection and the board change.
fn follow_neighbours(selection: Res<Selection>, view: Res<BoardView>, mut neighbours: ResMut<Neighbours>) {
    if !(selection.is_changed() || view.is_changed()) {
        return;
    }
    let next = match &selection.0 {
        Some(Subject::Task(id)) => self::neighbours(&view, id),
        _ => Neighbours::default(),
    };
    if *neighbours != next {
        *neighbours = next;
    }
}

/// The factory's queue task keys: what the control plane reports for its repos, or else the one parent Lookout knows.
fn queue_keys(authoring: &Authoring, factory: Option<&FactoryStatus>) -> Vec<String> {
    let mut keys: Vec<String> =
        factory.into_iter().flat_map(|f| f.repos.values()).flat_map(|r| r.queue.clone()).collect();
    if keys.is_empty() {
        keys.push(authoring.queue.clone());
    }
    keys
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

/// Tags the header button that limits the board to the factory's tasks.
#[derive(Component)]
struct FactoryToggle;

fn on_toggle_factory(activate: On<Activate>, toggles: Query<(), With<FactoryToggle>>, mut filter: ResMut<Filter>) {
    if toggles.contains(activate.entity) {
        filter.factory = !filter.factory;
    }
}

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
    pub status: Status,
    /// How many cards the column holds, shown or not.
    pub count: usize,
}

/// The scrolling body of the column for this status. It is a grandchild of its [`BoardColumn`]: the body and its
/// scrollbar share a frame, which sits under the shell after the header.
#[derive(Component)]
pub struct ColumnBody(pub Status);

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
    pub status: Status,
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
    bodies: HashMap<Status, Entity>,
    slots: Keyed<Slot>,
}

/// How many cards each column has been asked to build, by status; a column not in it builds [`PAGE`].
#[derive(Resource, Default)]
struct Shown(HashMap<Status, usize>);

impl Shown {
    fn limit(&self, status: Status) -> usize {
        self.0.get(&status).copied().unwrap_or(PAGE)
    }

    fn more(&mut self, status: Status) {
        let next = self.limit(status) + PAGE;
        self.0.insert(status, next);
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
            .map(|c| (c.status.as_str().to_string(), BoardColumn { status: c.status, count: c.cards.len() }))
            .collect(),
    )];
    columns.sync(
        &mut commands,
        &wanted,
        |commands, _, model| {
            let shell = commands.spawn(model.clone()).id();
            let body = commands.spawn((ColumnBody(model.status), ChildOf(shell))).id();
            bodies.insert(model.status, body);
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
            let (cards, hidden) = visible(&column.cards, shown.limit(column.status));
            let mut items = Vec::new();
            if column.cards.is_empty() {
                items.push((format!("empty/{}", column.status), Slot::Empty));
            }
            items.extend(cards.iter().map(|c| (c.id.clone(), Slot::Card(c.clone()))));
            if hidden > 0 {
                let more = ColumnMore { status: column.status, next: hidden.min(PAGE) };
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
            .init_resource::<Authoring>()
            .init_resource::<BoardView>()
            .init_resource::<Shown>()
            .init_resource::<Tree>()
            .init_resource::<Selection>()
            .init_resource::<Neighbours>()
            .add_systems(Update, (rebuild_view, sync_tree, follow_neighbours).chain())
            .add_observer(on_pick)
            .add_observer(on_toggle_closed)
            .add_observer(on_toggle_factory);
    }
}

/// Draws the board while in [`View::Board`], and every [`BoardCard`] in any view.
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
                (dress_columns, dress_slots, update_counts, dress_header, update_header)
                    .chain()
                    .after(sync_tree)
                    .run_if(in_state(View::Board)),
            )
            // Cards are drawn wherever they are, on the board or on a project's page.
            .add_systems(Update, (dress_cards, tools_follow_connection));
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
                // A layout node is never smaller than its content unless told so: without these the panel grew past
                // the window to fit every card, so the board and its columns never overflowed (TRAIL-836).
                min_width: px(0),
                min_height: px(0),
                overflow: Overflow::clip(),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(px(space::PANEL_INSET)),
                row_gap: px(space::ROWS),
                ..widgets::beside_sidebar()
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

/// The header bar, spawned with the panel; its widgets are spawned once and updated in place.
#[derive(Component)]
struct HeaderBar;

/// What the cards of one column build now: the first `limit` of them, and how many are left out. A page that ends
/// partway through a parent's subtasks runs on to the last of them, so a parent is never split from its children.
pub fn visible(cards: &[Card], limit: usize) -> (&[Card], usize) {
    let mut shown = limit.min(cards.len());
    while shown > 0 && shown < cards.len() && cards[shown].depth > 0 {
        shown += 1;
    }
    (&cards[..shown], cards.len() - shown)
}

/// Where the board was scrolled to when the view last left it. The panel's nodes stay as they are while the board is
/// showing, so this only brings the positions back after a trip to another view, which builds the panel anew.
#[derive(Resource, Default)]
struct BoardScroll {
    board: Vec2,
    columns: HashMap<Status, f32>,
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
        kept.columns.insert(column.0, position.y);
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
        commands.entity(shell).insert(widgets::column_shell(*status));
        commands.entity(body).insert((widgets::column_body(), ScrollPosition(Vec2::new(0.0, at))));
        // The body and its bar share a frame under the header, so the bar's track is the body's.
        let frame = commands.spawn((widgets::column_frame(), ChildOf(shell))).id();
        commands.entity(body).insert(ChildOf(frame));
        widgets::column_scrollbar(&mut commands, frame, body);
        widgets::column_header(&mut commands, shell, *status, count);
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
        if let Some(found) = &card.found {
            commands.queue_spawn_scene(card_found(found.clone())).insert(ChildOf(shell));
        }
        let badges = commands.queue_spawn_scene(card_row()).insert(ChildOf(shell)).id();
        for b in &card.badges {
            let mut shown = commands.queue_spawn_scene(badge(b.label.clone(), None, b.tone.clone()));
            shown.insert(ChildOf(badges));
            if let Some(hint) = &b.hint {
                shown.insert(Hint(hint.clone()));
            }
        }
        let tools = commands.queue_spawn_scene(card_row()).insert(ChildOf(shell)).id();
        for action in &card.actions {
            let (icon, hint) = match action {
                CardAction::Edit => (Icon::Pencil, "Edit"),
                CardAction::Comment => (Icon::Comment, "Comment"),
                CardAction::Answer { .. } => (Icon::Help, "Answer the open question"),
            };
            let mut tool = commands.queue_spawn_scene(tool_button(icon, hint, ButtonKind::Plain));
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
        commands.entity(entity).queue_apply_scene(button(label, None, ButtonKind::Normal));
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
        shown.more(more.status);
    }
}

/// The names a filter dropdown lists, and which is chosen. A chosen value that is no longer offered shows as "all".
fn select_state(key: FilterKey, filter: &Filter, options: &Options) -> (Vec<String>, Option<usize>) {
    let choices = options.of(key);
    let mut names = vec!["all".to_string()];
    names.extend(choices.iter().map(|c| c.label.clone()));
    let at = filter.get(key).as_ref().and_then(|v| choices.iter().position(|c| &c.value == v)).map_or(0, |i| i + 1);
    (names, Some(at))
}

fn factory_caption(filter: &Filter) -> String {
    format!("Factory only: {}", if filter.factory { "on" } else { "off" })
}

fn closed_caption(filter: &Filter) -> String {
    let closed = Status::ALL.into_iter().filter(|s| s.is_closed()).map(status_label).collect::<Vec<_>>().join(" & ");
    format!("{closed}: {}", if filter.closed { "shown" } else { "hidden" })
}

/// Spawns the header bar's widgets once, when the bar appears: a dropdown per filter, the toggles and New task.
/// [`update_header`] changes them in place afterwards.
fn dress_header(
    bar: Query<Entity, Added<HeaderBar>>,
    view: Res<BoardView>,
    filter: Res<Filter>,
    connection: Res<State<Connection>>,
    mut commands: Commands,
) {
    for bar in &bar {
        for key in FilterKey::ALL {
            let (names, at) = select_state(key, &filter, &view.options);
            let control = bsn_list! { @select(names, at, 8) FilterSelect({ key }) };
            commands.queue_spawn_scene(field(key.name(), control)).insert(ChildOf(bar));
        }
        commands
            .queue_spawn_scene(button(closed_caption(&filter), None, ButtonKind::Normal))
            .insert((ClosedToggle, ChildOf(bar)));
        commands
            .queue_spawn_scene(button(factory_caption(&filter), None, ButtonKind::Normal))
            .insert((FactoryToggle, ChildOf(bar)));
        let mut new = commands.queue_spawn_scene(button("New task", Some(Icon::Plus), ButtonKind::Primary));
        new.insert((Act::New, NewTask, ChildOf(bar)));
        if *connection.get() != Connection::Live {
            new.insert(InteractionDisabled);
        }
    }
}

/// Marks the header's New task button, which is disabled while the connection isn't live.
#[derive(Component)]
struct NewTask;

/// What the header was last updated from, so it is touched only when one of these changes.
#[derive(Clone, PartialEq)]
struct HeaderDrawn {
    filter: Filter,
    options: Options,
    connection: Connection,
}

/// Brings the header's widgets up to date in place: each select's rows and chosen row, the toggles' captions, and
/// New task's disabled state. Nothing is despawned, so a dropdown just used stays the same entity. A
/// select whose popup is open is left alone and tried again once it has closed.
#[allow(clippy::too_many_arguments)]
fn update_header(
    view: Res<BoardView>,
    filter: Res<Filter>,
    connection: Res<State<Connection>>,
    selects: Query<(Entity, &FilterSelect)>,
    toggle: Single<Entity, With<ClosedToggle>>,
    factory_toggle: Single<Entity, With<FactoryToggle>>,
    new: Single<Entity, With<NewTask>>,
    children: Query<&Children>,
    mut texts: Query<&mut Text>,
    mut edit: SelectEdit,
    mut commands: Commands,
    mut drawn: Local<Option<HeaderDrawn>>,
) {
    let now = HeaderDrawn { filter: filter.clone(), options: view.options.clone(), connection: *connection.get() };
    if drawn.as_ref() == Some(&now) {
        return;
    }
    let mut settled = true;
    for (select, FilterSelect(key)) in &selects {
        let (names, at) = select_state(*key, &filter, &view.options);
        settled &= edit.set(select, &names, at);
    }
    for (toggle, caption) in [(*toggle, closed_caption(&filter)), (*factory_toggle, factory_caption(&filter))] {
        for e in children.iter_descendants(toggle) {
            if let Ok(mut text) = texts.get_mut(e)
                && text.0 != caption
            {
                text.0 = caption.clone();
            }
        }
    }
    if *connection.get() == Connection::Live {
        commands.entity(*new).remove::<InteractionDisabled>();
    } else {
        commands.entity(*new).insert(InteractionDisabled);
    }
    // A select that was open is looked at again next frame.
    *drawn = settled.then_some(now);
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use serde_json::Value;
    use trailhead_model::Priority;

    use super::*;
    use crate::fixture::{agent_json, at, claim_json, comment_json, project_json, sid, task_json};
    use crate::sync::{Snapshot, SyncItem};

    /// The board's fixture as the API's JSON.
    #[allow(clippy::type_complexity)]
    fn input_json() -> (Vec<Value>, Vec<Value>, Vec<Value>, Vec<Value>, Vec<Value>) {
        let projects =
            vec![project_json(json!({"id": "p1", "key": "TRAIL"})), project_json(json!({"id": "p2", "key": "OPS"}))];
        let task = |id: &str, key: &str, project: &str, status: &str, parent: Option<&str>, prio: i64| {
            task_json(json!({"id": id, "key": key, "title": format!("title {key}"), "project_id": project,
                             "status": status, "parent_id": parent, "priority": prio}))
        };
        let mut tasks = vec![
            task("t102", "TRAIL-102", "p1", "open", None, 2),
            task("t103", "TRAIL-103", "p1", "in_progress", Some("t102"), 2),
            task("t104", "TRAIL-104", "p1", "in_progress", Some("t103"), 1),
            task("t105", "TRAIL-105", "p1", "open", Some("t102"), 3),
            task("t200", "OPS-1", "p2", "in_progress", None, 2),
        ];
        tasks[1]["grade"] = json!(3);
        tasks[2]["metadata"] = json!({"factory": {"attempts": 1}});
        let claims = vec![
            claim_json(json!({"task_id": "t103", "holder_id": "a1"})),
            claim_json(json!({"task_id": "t200", "holder_id": "u1"})),
        ];
        let comments = vec![
            comment_json(
                json!({"id": "c1", "task_id": "t103", "kind": "question", "body": "which db?", "created_at": at(1)}),
            ),
            comment_json(
                json!({"id": "c2", "task_id": "t103", "kind": "question", "body": "old", "resolved_at": at(2), "created_at": at(1)}),
            ),
            comment_json(
                json!({"id": "c3", "task_id": "t103", "kind": "handoff", "body": "first", "created_at": at(1)}),
            ),
            comment_json(
                json!({"id": "c4", "task_id": "t103", "kind": "handoff", "body": "second\nmore", "created_at": at(2)}),
            ),
        ];
        let agents = vec![agent_json(json!({"id": "a1", "name": "factory/trail-103-a1"}))];
        (projects, tasks, claims, comments, agents)
    }

    fn typed<T: serde::de::DeserializeOwned>(values: Vec<Value>) -> Vec<T> {
        values.into_iter().map(|v| serde_json::from_value(v).unwrap()).collect()
    }

    fn at_now() -> Timestamp {
        "2026-10-15T00:00:00Z".parse().unwrap()
    }

    #[test]
    fn an_expired_claim_is_not_a_holder() {
        let live = typed::<Claim>(vec![claim_json(json!({"task_id": "t103", "expires_at": "2026-10-16T00:00:00Z"}))]);
        let gone = typed::<Claim>(vec![claim_json(json!({"task_id": "t103", "expires_at": "2026-10-14T00:00:00Z"}))]);
        let (projects, tasks, _, comments, agents) = input_json();
        let (projects, tasks, comments, agents) =
            (typed::<Project>(projects), typed::<Task>(tasks), typed::<Comment>(comments), typed::<Agent>(agents));
        let holders = |claims: &[Claim], filter: &Filter| {
            let input = Input {
                projects: &projects,
                tasks: &tasks,
                claims,
                dependencies: &[],
                comments: &comments,
                agents: &agents,
                users: &[],
                runs: &[],
                queues: &[],
                deep: &[],
                now: at_now(),
            };
            build(input, filter)
        };
        assert_eq!(holders(&live, &Filter::default()).options.holders.len(), 1);
        let view = holders(&gone, &Filter::default());
        assert!(view.options.holders.is_empty(), "an expired claim isn't offered as a holder");
        let by_holder = Filter { holder: Some(sid("a1")), ..Default::default() };
        let view = holders(&gone, &by_holder);
        assert!(view.columns.iter().all(|c| c.cards.is_empty()), "nor does it match the filter");
        assert_eq!(next_expiry(&live, at_now()), Some("2026-10-16T00:00:00Z".parse().unwrap()));
        assert_eq!(next_expiry(&gone, at_now()), None);
    }

    pub(super) fn run(filter: &Filter) -> BoardView {
        run_with(filter, &[])
    }

    fn run_with(filter: &Filter, runs: &[Run]) -> BoardView {
        let (projects, tasks, claims, comments, agents) = input_json();
        let (projects, tasks, claims, comments, agents) = (
            typed::<Project>(projects),
            typed::<Task>(tasks),
            typed::<Claim>(claims),
            typed::<Comment>(comments),
            typed::<Agent>(agents),
        );
        build(
            Input {
                projects: &projects,
                tasks: &tasks,
                claims: &claims,
                dependencies: &[],
                comments: &comments,
                agents: &agents,
                users: &[],
                runs,
                queues: &["TRAIL-102".to_string()],
                deep: &[],
                now: at_now(),
            },
            filter,
        )
    }

    fn keys(view: &BoardView, status: Status) -> Vec<(String, usize)> {
        let col = view.columns.iter().find(|c| c.status == status).unwrap();
        col.cards.iter().map(|c| (c.key.clone(), c.depth)).collect()
    }

    #[test]
    fn groups_by_status_with_subtasks_under_their_parent() {
        let view = run(&Filter { closed: true, ..Default::default() });
        assert_eq!(view.columns.len(), Status::ALL.len());
        // TRAIL-105 is a subtask of TRAIL-102 in the same column, so it nests under it.
        assert_eq!(keys(&view, Status::Open), [("TRAIL-102".into(), 0), ("TRAIL-105".into(), 1)]);
        // TRAIL-104 nests under TRAIL-103; TRAIL-103's parent is in another column, so it names it instead.
        assert_eq!(
            keys(&view, Status::InProgress),
            [("OPS-1".into(), 0), ("TRAIL-103".into(), 0), ("TRAIL-104".into(), 1)]
        );
        let col = view.columns.iter().find(|c| c.status == Status::InProgress).unwrap();
        let parents: Vec<_> = col.cards.iter().map(|c| c.parent.as_deref()).collect();
        assert_eq!(parents, [None, Some("TRAIL-102"), None]);
    }

    #[test]
    fn neighbours_are_the_cards_either_side_in_the_column_and_none_past_its_ends() {
        let view = run(&Filter::default());
        let id = |key: &str| card(&view, key).id.clone();
        let around = |key: &str| {
            let n = neighbours(&view, &id(key));
            let key_of = |id: Option<String>| {
                id.map(|id| view.columns.iter().flat_map(|c| &c.cards).find(|c| c.id == id).unwrap().key.clone())
            };
            (key_of(n.previous), key_of(n.next))
        };
        // In progress is OPS-1, TRAIL-103, then TRAIL-104 nested under it.
        assert_eq!(around("OPS-1"), (None, Some("TRAIL-103".into())));
        assert_eq!(around("TRAIL-103"), (Some("OPS-1".into()), Some("TRAIL-104".into())));
        assert_eq!(around("TRAIL-104"), (Some("TRAIL-103".into()), None));
        // Never across into another column.
        assert_eq!(around("TRAIL-102"), (None, Some("TRAIL-105".into())));
        assert_eq!(neighbours(&view, "not on the board"), Neighbours::default());
    }

    fn card<'a>(view: &'a BoardView, key: &str) -> &'a Card {
        view.columns.iter().flat_map(|c| &c.cards).find(|c| c.key == key).unwrap()
    }

    fn badges(card: &Card) -> Vec<(BadgeKind, &str)> {
        card.badges.iter().map(|b| (b.kind, b.label.as_str())).collect()
    }

    #[test]
    fn a_task_with_unfinished_blockers_says_how_many_and_names_them() {
        let (projects, mut tasks, claims, comments, agents) = input_json();
        // OPS-1 is finished, so it blocks nothing any more, and being done it shows no badge of its own.
        tasks[4]["status"] = json!("done");
        let dep = |blocker: &str, blocked: &str| json!({"workspace_id": sid("w1"), "blocker_id": sid(blocker), "blocked_id": sid(blocked), "created_at": at(1)});
        let dependencies = typed::<Dependency>(vec![
            dep("t104", "t105"),
            dep("t103", "t105"),
            dep("t200", "t105"),
            dep("t104", "t200"),
            dep("t999", "t102"),
        ]);
        let (projects, tasks, claims, comments, agents) = (
            typed::<Project>(projects),
            typed::<Task>(tasks),
            typed::<Claim>(claims),
            typed::<Comment>(comments),
            typed::<Agent>(agents),
        );
        let view = build(
            Input {
                projects: &projects,
                tasks: &tasks,
                claims: &claims,
                dependencies: &dependencies,
                comments: &comments,
                agents: &agents,
                now: at_now(),
                ..Default::default()
            },
            &Filter { closed: true, ..Default::default() },
        );
        let blocked = |key: &str| card(&view, key).badges.iter().find(|b| b.kind == BadgeKind::Blocked).cloned();
        let badge = blocked("TRAIL-105").unwrap();
        assert_eq!(badge.label, "blocked · 2");
        assert_eq!(badge.hint.as_deref(), Some("Blocked by TRAIL-103, TRAIL-104"));
        assert_eq!(badge.tone, Some(token::WARN));
        // Done, or blocked only by a task the mirror doesn't have: nothing to say.
        assert_eq!(blocked("OPS-1"), None);
        assert_eq!(blocked("TRAIL-102"), None);
        assert_eq!(blocked("TRAIL-103"), None);
    }

    #[test]
    fn an_in_review_task_with_changes_requested_says_so_and_stays_in_review() {
        use crate::fixture::{comment, task};
        let first = "01900000-0000-7000-8000-000000000001";
        let second = "01900000-0000-7000-8000-000000000002";
        let tasks = [
            task(json!({"id": "t1", "key": "TRAIL-1", "status": "in_review"})),
            task(json!({"id": "t2", "key": "TRAIL-2", "status": "in_review"})),
            task(json!({"id": "t3", "key": "TRAIL-3", "status": "open"})),
        ];
        let comments = [
            comment(json!({"id": first, "task_id": "t1", "kind": "review", "verdict": "request_changes"})),
            // TRAIL-2's request was answered by a handoff after it.
            comment(json!({"id": first, "task_id": "t2", "kind": "review", "verdict": "request_changes"})),
            comment(json!({"id": second, "task_id": "t2", "kind": "handoff"})),
            // Not in review, so nothing to wait on.
            comment(json!({"id": first, "task_id": "t3", "kind": "review", "verdict": "request_changes"})),
        ];
        let projects = [crate::fixture::project(json!({"id": "p1", "key": "TRAIL"}))];
        let view = build(
            Input { projects: &projects, tasks: &tasks, comments: &comments, now: at_now(), ..Default::default() },
            &Filter::default(),
        );
        let flagged = |key: &str| card(&view, key).badges.iter().any(|b| b.kind == BadgeKind::ChangesRequested);
        assert_eq!(keys(&view, Status::InReview), [("TRAIL-1".into(), 0), ("TRAIL-2".into(), 0)]);
        assert!(flagged("TRAIL-1") && !flagged("TRAIL-2") && !flagged("TRAIL-3"));
    }
    #[test]
    fn a_holder_the_agent_list_lacks_is_named_for_its_run() {
        use trailhead_factory::{RunPhase, RunStatus, TaskKey, Usd};
        let runs = [Run {
            repo: "o/r".into(),
            status: RunStatus {
                task: TaskKey("OPS-1".into()),
                phase: RunPhase::Agent,
                sandbox: None,
                steps: 1,
                tool_calls: 1,
                spend: Usd::dollars(0.0),
                last_tool: None,
                started: 0,
            },
        }];
        let view = run_with(&Filter::default(), &runs);
        assert_eq!(badges(card(&view, "OPS-1")), [(BadgeKind::Priority, "P2"), (BadgeKind::Holder, "ops-1")]);
        let holders: Vec<&str> = view.options.holders.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(holders, ["factory/ops-1", "factory/trail-103-a1"]);
    }

    #[test]
    fn badges_show_priority_grade_holder_and_the_open_question_count() {
        let view = run(&Filter::default());
        assert_eq!(
            badges(card(&view, "TRAIL-103")),
            [
                (BadgeKind::Priority, "P2"),
                (BadgeKind::Factory, "factory"),
                (BadgeKind::Grade, "G3"),
                (BadgeKind::Holder, "trail-103-a1"),
                (BadgeKind::Questions, "? 1"),
            ]
        );
        // No grade, no holder, no question: only the priority. A holder that isn't an agent shows as a short id.
        assert_eq!(badges(card(&view, "TRAIL-102")), [(BadgeKind::Priority, "P2")]);
        let short = &sid("u1")[..8];
        assert_eq!(badges(card(&view, "OPS-1")), [(BadgeKind::Priority, "P2"), (BadgeKind::Holder, short)]);
    }

    #[test]
    fn an_ungraded_task_under_a_factory_queue_is_flagged_and_a_graded_one_is_not() {
        let view = run(&Filter::default());
        // TRAIL-105 is open under the queue TRAIL-102 with no grade.
        let flagged = badges(card(&view, "TRAIL-105"));
        assert_eq!(flagged.last(), Some(&(BadgeKind::NeedsGrade, "needs grade")));
        let tone = card(&view, "TRAIL-105").badges.last().unwrap().tone.clone();
        assert_eq!(tone, Some(token::WARN));
        // TRAIL-103 is under it too but graded; TRAIL-104 and OPS-1 are not under a queue; the queue itself isn't.
        for key in ["TRAIL-103", "TRAIL-104", "OPS-1", "TRAIL-102"] {
            assert!(card(&view, key).badges.iter().all(|b| b.kind != BadgeKind::NeedsGrade), "{key}");
        }
    }
    #[test]
    fn the_priority_badge_carries_its_priority_token_and_others_do_not() {
        let view = run(&Filter::default());
        let badge =
            |key: &str, kind: BadgeKind| card(&view, key).badges.iter().find(|b| b.kind == kind).unwrap().tone.clone();
        assert_eq!(badge("TRAIL-103", BadgeKind::Priority), Some(token::PRIORITY_2));
        assert_eq!(badge("TRAIL-103", BadgeKind::Grade), Some(token::GRADE_3));
        assert_eq!(badge("TRAIL-103", BadgeKind::Questions), Some(token::WARN));
        let tones = Priority::ALL.map(priority_tone);
        assert_eq!(tones, [token::PRIORITY_1, token::PRIORITY_2, token::PRIORITY_3, token::PRIORITY_4]);
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
        let answer = CardAction::Answer { comment_id: sid("c1"), question: "which db?".into() };
        assert_eq!(asked.actions, [CardAction::Edit, CardAction::Comment, answer.clone()]);
        // The buttons send the commands the forms already handle.
        assert_eq!(asked.actions[0].act(&asked.key), Act::Edit("TRAIL-103".into()));
        assert_eq!(asked.actions[1].act(&asked.key), Act::Comment("TRAIL-103".into()));
        assert_eq!(
            answer.act(&asked.key),
            Act::Answer { key: "TRAIL-103".into(), comment_id: sid("c1"), question: "which db?".into() }
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
    fn the_factory_filter_keeps_queued_tasks_and_tasks_with_run_metadata() {
        let keys = |view: &BoardView| {
            view.columns.iter().flat_map(|c| c.cards.iter().map(|c| c.key.clone())).collect::<Vec<_>>()
        };
        let all = run(&Filter::default());
        let only = run(&Filter { factory: true, ..Default::default() });
        // 103 and 105 are under the queue task, 104 has a run's metadata; 102 and OPS-1 are neither.
        assert_eq!(keys(&only), ["TRAIL-105", "TRAIL-103", "TRAIL-104"].map(String::from));
        assert!(keys(&all).len() > keys(&only).len());
        // They stay in their usual columns.
        for (status, key) in [(Status::InProgress, "TRAIL-103"), (Status::Open, "TRAIL-105")] {
            assert!(only.columns.iter().find(|c| c.status == status).unwrap().cards.iter().any(|c| c.key == key));
        }
        // Off, nothing is left out; the badge marks the factory's tasks either way.
        let flagged = |view: &BoardView| {
            let mut v: Vec<_> = view
                .columns
                .iter()
                .flat_map(|c| &c.cards)
                .filter(|c| c.badges.iter().any(|b| b.kind == BadgeKind::Factory))
                .map(|c| c.key.clone())
                .collect();
            v.sort();
            v
        };
        assert_eq!(flagged(&all), ["TRAIL-103", "TRAIL-104", "TRAIL-105"]);
        // It combines with the other filters.
        let both = run(&Filter { factory: true, parent: Some("TRAIL-102".into()), ..Default::default() });
        assert_eq!(keys(&both), ["TRAIL-105", "TRAIL-103"].map(String::from));
    }

    #[test]
    fn done_and_cancelled_are_hidden_until_asked_for() {
        let names = |f: Filter| run(&f).columns.into_iter().map(|c| c.status).collect::<Vec<_>>();
        use Status::*;
        assert_eq!(names(Filter::default()), [Open, InProgress, NeedsInput, InReview, Approved]);
        assert_eq!(names(Filter { closed: true, ..Default::default() }), Status::ALL);
    }

    /// The keys on the board for a search over TRAIL-801, TRAIL-1801 and OPS-9, with the server's `deep` hits.
    fn searched(query: &str, filter: Filter, deep: &[DeepHit]) -> Vec<String> {
        let task = |id: &str, key: &str, title: &str, project: &str| {
            task_json(json!({"id": id, "key": key, "title": title, "project_id": project}))
        };
        let tasks = typed::<Task>(vec![
            task("t801", "TRAIL-801", "Search the board", "p1"),
            task("t1801", "TRAIL-1801", "Fix the Migration", "p1"),
            task("t9", "OPS-9", "Rotate the search keys", "p2"),
        ]);
        let projects = typed::<Project>(vec![
            project_json(json!({"id": "p1", "key": "TRAIL"})),
            project_json(json!({"id": "p2", "key": "OPS"})),
        ]);
        let view = build(
            Input { projects: &projects, tasks: &tasks, deep, ..Default::default() },
            &Filter { query: query.into(), ..filter },
        );
        let mut keys: Vec<String> = view.columns.iter().flat_map(|c| c.cards.iter().map(|c| c.key.clone())).collect();
        keys.sort();
        keys
    }

    #[test]
    fn a_number_finds_that_task_and_not_one_that_merely_ends_in_it() {
        for q in ["801", "trail-801", "TRAIL-801", " 801 "] {
            assert_eq!(searched(q, Filter::default(), &[]), ["TRAIL-801"], "{q}");
        }
        assert_eq!(searched("1801", Filter::default(), &[]), ["TRAIL-1801"]);
    }

    #[test]
    fn a_title_word_matches_in_any_case_and_every_word_must() {
        assert_eq!(searched("SEARCH", Filter::default(), &[]), ["OPS-9", "TRAIL-801"]);
        assert_eq!(searched("search keys", Filter::default(), &[]), ["OPS-9"]);
        assert!(searched("nothing", Filter::default(), &[]).is_empty());
        assert_eq!(searched("", Filter::default(), &[]).len(), 3);
    }

    #[test]
    fn the_query_combines_with_a_filter() {
        let ops = Filter { project: Some("OPS".into()), ..Default::default() };
        assert_eq!(searched("search", ops, &[]), ["OPS-9"]);
    }

    #[test]
    fn a_task_only_the_server_found_is_added_with_where_it_matched() {
        let hit = DeepHit { id: sid("t1801"), field: "description".into(), snippet: "uses the\n  zarr  store".into() };
        assert_eq!(searched("zarr", Filter::default(), std::slice::from_ref(&hit)), ["TRAIL-1801"]);
        // The filter still applies to it.
        let ops = Filter { project: Some("OPS".into()), ..Default::default() };
        assert!(searched("zarr", ops, std::slice::from_ref(&hit)).is_empty());
        assert_eq!(found_hint(&hit), "in description: uses the zarr store");
        let comment = DeepHit { field: "comment".into(), ..hit };
        assert_eq!(found_hint(&comment), "in comment: uses the zarr store");
        let title = DeepHit { field: "title".into(), ..comment };
        assert_eq!(found_hint(&title), "in title: uses the zarr store");
    }

    #[test]
    fn a_task_the_local_match_already_has_carries_no_hint() {
        let tasks = typed::<Task>(vec![task_json(json!({"id": "t1", "key": "TRAIL-1", "title": "zarr"}))]);
        let deep = [DeepHit { id: sid("t1"), field: "title".into(), snippet: "zarr".into() }];
        let view = build(
            Input { tasks: &tasks, deep: &deep, ..Default::default() },
            &Filter { query: "zarr".into(), ..Default::default() },
        );
        let card = view.columns.iter().flat_map(|c| &c.cards).next().unwrap();
        assert_eq!(card.found, None);
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
                found: None,
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
        assert_eq!(asked.limit(Status::Open), PAGE);
        asked.more(Status::Open);
        let (shown, hidden) = visible(&cards, asked.limit(Status::Open));
        assert_eq!((shown.len(), hidden), (2 * PAGE, 366 - 2 * PAGE));
        assert_eq!(shown[PAGE].key, format!("TRAIL-{PAGE}"));
        // Other columns are untouched, and a short one has nothing hidden.
        assert_eq!(asked.limit(Status::Done), PAGE);
        assert_eq!(visible(&many(3), PAGE).1, 0);
        assert!(visible(&[], PAGE).0.is_empty());
    }

    #[test]
    fn a_page_does_not_split_a_parent_from_its_subtasks() {
        let mut cards = many(PAGE + 5);
        // The parent is the page's last card; three subtasks follow it.
        for card in &mut cards[PAGE..PAGE + 3] {
            card.depth = 1;
        }
        let (shown, hidden) = visible(&cards, PAGE);
        assert_eq!((shown.len(), hidden), (PAGE + 3, 2));
        assert_eq!(shown[PAGE + 2].depth, 1);
        // A page that ends on a top-level boundary is cut where it was.
        let (shown, hidden) = visible(&cards, PAGE - 1);
        assert_eq!((shown.len(), hidden), (PAGE - 1, 6));
    }
    #[test]
    fn filters_narrow_the_board() {
        let by = |f: Filter| -> Vec<String> {
            run(&f).columns.iter().flat_map(|c| c.cards.iter().map(|c| c.key.clone())).collect()
        };
        assert_eq!(by(Filter { project: Some("OPS".into()), ..Default::default() }), ["OPS-1"]);
        assert_eq!(by(Filter { parent: Some("TRAIL-102".into()), ..Default::default() }), ["TRAIL-105", "TRAIL-103"]);
        assert_eq!(by(Filter { holder: Some(sid("a1")), ..Default::default() }), ["TRAIL-103"]);
        let none = Filter { project: Some("OPS".into()), holder: Some(sid("a1")), ..Default::default() };
        assert!(by(none).is_empty());
    }

    #[test]
    fn options_list_what_the_data_has() {
        let view = run(&Filter::default());
        let labels = |c: &[Choice]| c.iter().map(|c| c.label.clone()).collect::<Vec<_>>();
        assert_eq!(labels(&view.options.projects), ["OPS", "TRAIL"]);
        assert_eq!(labels(&view.options.parents), ["TRAIL-102 title TRAIL-102", "TRAIL-103 title TRAIL-103"]);
        assert_eq!(view.options.parents[0].value, "TRAIL-102", "the filter still takes the key");
        // A holder that isn't an agent is its short id, which sorts first here.
        assert_eq!(labels(&view.options.holders), [&sid("u1")[..8], "factory/trail-103-a1"]);
    }

    #[test]
    fn a_person_holding_a_claim_is_named_by_the_users_list() {
        let (projects, tasks, claims, comments, agents) = input_json();
        let (projects, tasks, claims, comments, agents) = (
            typed::<Project>(projects),
            typed::<Task>(tasks),
            typed::<Claim>(claims),
            typed::<Comment>(comments),
            typed::<Agent>(agents),
        );
        let users = typed::<User>(vec![json!({
            "id": sid("u1"), "workspace_id": sid("w1"), "name": "Lane", "email": null,
            "created_at": "2026-10-01T00:00:00Z"
        })]);
        let view = build(
            Input {
                projects: &projects,
                tasks: &tasks,
                claims: &claims,
                comments: &comments,
                agents: &agents,
                users: &users,
                now: at_now(),
                ..Default::default()
            },
            &Filter::default(),
        );
        assert_eq!(badges(card(&view, "OPS-1")), [(BadgeKind::Priority, "P2"), (BadgeKind::Holder, "Lane")]);
        let labels: Vec<_> = view.options.holders.iter().map(|c| c.label.clone()).collect();
        assert_eq!(labels, ["Lane", "factory/trail-103-a1"]);
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
            columns.into_iter().map(|c| c.status).filter(|s| s.is_closed()).collect::<Vec<_>>()
        };
        assert!(closed_columns(&app).is_empty());
        app.world_mut().trigger(Activate { entity: toggle });
        assert_eq!(closed_columns(&app), [Status::Done, Status::Cancelled]);
        app.world_mut().trigger(Activate { entity: toggle });
        assert!(closed_columns(&app).is_empty());
    }

    #[test]
    fn the_view_follows_the_mirror() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(crate::mirror::MirrorPlugin).add_plugins(BoardPlugin);
        let (projects, tasks, claims, comments, agents) = input_json();
        let snapshot = Snapshot { at: 5, projects, tasks, agents, ..Default::default() };
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
            "id": 6, "entity": "task", "entity_id": sid("t200"), "kind": "task.updated",
            "after": task_json(json!({"id": "t200", "key": "OPS-1", "project_id": "p2", "status": "done", "priority": 2, "version": 2})),
        })));
        app.update();
        app.update();
        let view = app.world().resource::<BoardView>();
        assert_eq!(keys(view, Status::Done), [("OPS-1".into(), 0)]);
    }
}

#[cfg(test)]
mod in_place_tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::fixture::{project_json, sid, task_json};
    use crate::mirror::MirrorPlugin;
    use crate::sync::{Snapshot, SyncItem};

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins((MirrorPlugin, BoardPlugin));
        app.world_mut().spawn(BoardColumns);
        let task = |id: &str, status: &str| {
            task_json(
                json!({"id": id, "key": id.to_uppercase(), "title": id, "project_id": "p1", "status": status, "priority": 2}),
            )
        };
        let tasks = vec![task("t1", "open"), task("t2", "open"), task("t3", "in_progress"), task("t4", "in_progress")];
        let projects =
            vec![project_json(json!({"id": "p1", "key": "TRAIL"})), project_json(json!({"id": "p2", "key": "OPS"}))];
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot {
            at: 1,
            projects,
            tasks,
            agents: vec![],
            ..Default::default()
        }));
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
            "id": 9, "entity": "task", "entity_id": sid(id), "kind": "task.updated",
            "after": task_json(json!({"id": id, "key": id.to_uppercase(), "title": id, "project_id": "p1", "status": status,
                      "priority": 2, "version": 2})),
        })));
        settle(app);
    }

    fn cards(app: &mut App) -> HashMap<String, Entity> {
        let mut q = app.world_mut().query::<(Entity, &BoardCard)>();
        // By the task's short name: these tasks' keys are their names in capitals.
        q.iter(app.world()).map(|(e, c)| (c.0.key.to_lowercase(), e)).collect()
    }

    fn column_of(app: &mut App, card: Entity) -> Status {
        let body = app.world().get::<ChildOf>(card).expect("a card has a column").parent();
        app.world().get::<ColumnBody>(body).expect("in a column body").0
    }

    fn count_cards(app: &mut App) -> usize {
        let mut q = app.world_mut().query::<&BoardCard>();
        q.iter(app.world()).count()
    }

    #[test]
    fn a_dressed_column_is_a_header_and_a_frame_holding_the_body_and_scrollbar() {
        let mut app = app();
        // The plain test app never dresses columns. Run that system alone, on bodies it sees as new.
        app.add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .init_resource::<BoardScroll>()
            .add_systems(Update, dress_columns);
        let bodies: Vec<(Entity, Status)> =
            app.world_mut().query::<(Entity, &ColumnBody)>().iter(app.world()).map(|(e, b)| (e, b.0)).collect();
        for (id, status) in bodies {
            app.world_mut().entity_mut(id).remove::<ColumnBody>().insert(ColumnBody(status));
        }
        settle(&mut app);
        let mut q = app.world_mut().query::<(Entity, &BoardColumn)>();
        let shells: Vec<Entity> = q.iter(app.world()).map(|(e, _)| e).collect();
        assert!(!shells.is_empty());
        for shell in shells {
            let world = app.world();
            let kids: Vec<Entity> = world.get::<Children>(shell).expect("a dressed shell has children").to_vec();
            assert_eq!(
                kids.len(),
                2,
                "shell holds the header and the frame: {:?}",
                kids.iter().map(|k| world.get::<Name>(*k).map(Name::to_string)).collect::<Vec<_>>()
            );
            let (header, frame) = (kids[0], kids[1]);
            assert!(world.get::<ColumnBody>(header).is_none());
            assert_eq!(world.get::<Name>(frame).map(Name::as_str), Some("column frame"));
            let inner: Vec<Entity> = world.get::<Children>(frame).expect("the frame has children").to_vec();
            assert_eq!(inner.len(), 2, "frame holds the body and the scrollbar");
            assert!(world.get::<ColumnBody>(inner[0]).is_some(), "the body comes first");
            assert!(world.get::<ColumnBody>(inner[1]).is_none(), "the scrollbar comes second");
            assert_eq!(world.get::<ChildOf>(inner[0]).map(ChildOf::parent), Some(frame));
        }
    }

    #[test]
    fn changing_one_task_leaves_every_other_card_as_it_was() {
        let mut app = app();
        let before = cards(&mut app);
        assert_eq!(before.len(), 4);
        let mut q = app.world_mut().query::<(Entity, &BoardColumn)>();
        let columns: Vec<Entity> = q.iter(app.world()).map(|(e, _)| e).collect();

        app.world_mut().write_message(SyncItem::Event(json!({
            "id": 9, "entity": "task", "entity_id": sid("t1"), "kind": "task.updated",
            "after": task_json(json!({"id": "t1", "key": "T1", "title": "renamed", "project_id": "p1", "status": "open",
                      "priority": 2, "version": 2})),
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
        assert_eq!(column_of(&mut app, before["t1"]), Status::Open);

        update_task(&mut app, "t1", "in_progress");
        let after = cards(&mut app);
        assert_eq!(after, before);
        assert_eq!(count_cards(&mut app), 4, "no stale copy in the old column");
        assert_eq!(column_of(&mut app, after["t1"]), Status::InProgress);
        // The columns' children are what the view says.
        let mut q = app.world_mut().query::<(&ColumnBody, &Children)>();
        let held: HashMap<Status, usize> = q.iter(app.world()).map(|(b, c)| (b.0, c.len())).collect();
        assert_eq!((held[&Status::Open], held[&Status::InProgress]), (1, 3));
    }

    /// Every card sits in a column's body, and there is one card per task shown.
    fn assert_placed(app: &mut App, step: &str) {
        let world = app.world_mut();
        let mut q = world.query::<(Entity, &BoardCard, Option<&ChildOf>)>();
        let cards: Vec<(Entity, String, Option<Entity>)> =
            q.iter(world).map(|(e, c, p)| (e, c.0.key.clone(), p.map(ChildOf::parent))).collect();
        for (e, key, parent) in &cards {
            let in_body = parent.is_some_and(|p| world.get::<ColumnBody>(p).is_some());
            assert!(in_body, "{step}: card {key} ({e}) is not in a column (parent {parent:?})");
        }
        let mut keys: Vec<&String> = cards.iter().map(|(_, k, _)| k).collect();
        keys.sort();
        let n = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), n, "{step}: two cards for one task: {cards:?}");
    }

    #[test]
    fn cards_stay_in_their_columns_whichever_way_they_move() {
        // A card moving back to an earlier column was left without a parent, drawn at the window's top left
        // (TRAIL-1009).
        let mut app = app();
        let mut n = 100;
        let mut version = 10;
        let mut move_to = |app: &mut App, id: &str, status: &str, title: &str, settle_after: bool| {
            n += 1;
            version += 1;
            app.world_mut().write_message(SyncItem::Event(json!({
                "id": n, "entity": "task", "entity_id": sid(id), "kind": "task.updated",
                "after": task_json(json!({"id": id, "key": id.to_uppercase(), "title": title, "project_id": "p1",
                          "status": status, "priority": 2, "version": version})),
            })));
            if settle_after {
                settle(app);
            }
        };
        let steps: &[(&str, &str)] = &[
            ("t3", "in_review"),
            ("t3", "in_progress"),
            ("t3", "open"),
            ("t1", "needs_input"),
            ("t1", "open"),
            ("t4", "in_review"),
            ("t2", "in_review"),
            ("t4", "open"),
            ("t2", "in_progress"),
            ("t2", "done"),
            ("t2", "open"),
            ("t1", "in_review"),
            ("t1", "in_progress"),
        ];
        for (i, (id, status)) in steps.iter().enumerate() {
            move_to(&mut app, id, status, &format!("{id} v{i}"), true);
            assert_placed(&mut app, &format!("step {i} {id}->{status}"));
        }
        // Several in one frame, back and forth.
        move_to(&mut app, "t3", "in_review", "a", false);
        move_to(&mut app, "t4", "needs_input", "b", false);
        move_to(&mut app, "t3", "open", "c", false);
        settle(&mut app);
        assert_placed(&mut app, "burst");
        // Closed columns shown, a card moved into one, hidden again.
        app.world_mut().resource_mut::<Filter>().closed = true;
        settle(&mut app);
        move_to(&mut app, "t1", "cancelled", "d", true);
        assert_placed(&mut app, "into cancelled");
        app.world_mut().resource_mut::<Filter>().closed = false;
        settle(&mut app);
        assert_placed(&mut app, "closed hidden");
        move_to(&mut app, "t1", "in_progress", "e", true);
        assert_placed(&mut app, "out of cancelled");
        // One frame at a time, with a move every frame.
        for (i, status) in ["in_review", "open", "needs_input", "in_progress", "in_review"].iter().enumerate() {
            move_to(&mut app, "t2", status, &format!("f{i}"), false);
            app.update();
            assert_placed(&mut app, &format!("frame {i} t2->{status}"));
        }
    }

    #[test]
    fn a_filter_change_despawns_only_the_cards_it_hides() {
        let mut app = app();
        let before = cards(&mut app);
        // Give two of the tasks to the other project.
        for (n, id, status) in [(9, "t2", "open"), (10, "t4", "in_progress")] {
            let after = task_json(json!({"id": id, "key": id.to_uppercase(), "title": id, "project_id": "p2",
                                         "status": status, "priority": 2, "version": 2}));
            app.world_mut().write_message(SyncItem::Event(json!({
                "id": n, "entity": "task", "entity_id": sid(id), "kind": "task.updated", "after": after,
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
        assert_eq!(column_of(&mut app, shown["t1"]), Status::Done);
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
                task_json(json!({"id": format!("t{i}"), "key": format!("TRAIL-{i}"), "title": "", "project_id": "p1",
                            "status": "open", "priority": 2}))
            })
            .collect();
        let projects = vec![project_json(json!({"id": "p1", "key": "TRAIL"}))];
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot {
            at: 1,
            projects,
            tasks,
            agents: vec![],
            ..Default::default()
        }));
        settle(&mut app);
        let before = cards(&mut app);
        assert_eq!(before.len(), PAGE);
        let mut q = app.world_mut().query::<&ColumnMore>();
        assert_eq!(q.single(app.world()).unwrap().next, 5);

        app.world_mut().resource_mut::<Shown>().more(Status::Open);
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

#[cfg(test)]
mod header_tests {
    use bevy::ecs::system::RunSystemOnce;
    use bevy::state::app::StatesPlugin;

    use super::tests::run;
    use super::*;

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin, AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .init_state::<Connection>()
            .init_resource::<Filter>()
            .insert_resource(run(&Filter::default()))
            .add_systems(Update, (dress_header, update_header).chain());
        app.world_mut().spawn(HeaderBar);
        settle(&mut app);
        app
    }

    fn settle(app: &mut App) {
        for _ in 0..4 {
            app.update();
        }
    }

    /// Every entity under the header bar, in order.
    fn header(app: &mut App) -> Vec<Entity> {
        let world = app.world_mut();
        let bar = world.query_filtered::<Entity, With<HeaderBar>>().single(world).unwrap();
        let mut found = vec![bar];
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

    #[test]
    fn the_header_has_its_widgets_once() {
        let mut app = app();
        let mut q = app.world_mut().query::<&FilterSelect>();
        assert_eq!(q.iter(app.world()).count(), FilterKey::ALL.len());
        one::<ClosedToggle>(&mut app);
        one::<FactoryToggle>(&mut app);
        one::<NewTask>(&mut app);
    }

    #[test]
    fn picking_a_filter_leaves_every_widget_of_the_header_as_it_was() {
        let mut app = app();
        let before = header(&mut app);
        app.world_mut().resource_mut::<Filter>().project = Some("OPS".into());
        settle(&mut app);
        assert_eq!(header(&mut app), before, "the same entities, the selects, toggle, chip and New task among them");
        // The select shows the pick: its row is marked selected.
        let mut q = app.world_mut().query::<(&FilterSelect, Entity)>();
        let project = q.iter(app.world()).find(|(s, _)| s.0 == FilterKey::Project).unwrap().1;
        let chosen = app
            .world_mut()
            .run_system_once(move |edit: crate::widgets::SelectEdit| edit.selected_rows(project))
            .unwrap();
        assert_eq!(chosen.len(), 1);
        assert_eq!(
            chosen[0],
            1 + run(&Filter::default())
                .options
                .of(FilterKey::Project)
                .iter()
                .position(|c| c.value == "p2")
                .unwrap_or(0)
        );
    }

    fn is_under(world: &World, mut e: Entity, ancestor: Entity) -> bool {
        while let Some(parent) = world.get::<ChildOf>(e) {
            e = parent.parent();
            if e == ancestor {
                return true;
            }
        }
        false
    }

    #[test]
    fn toggling_closed_and_losing_the_connection_leave_the_header_as_it_was() {
        let mut app = app();
        let before = header(&mut app);
        app.world_mut().resource_mut::<Filter>().closed = true;
        settle(&mut app);
        assert_eq!(header(&mut app), before);

        app.world_mut().resource_mut::<NextState<Connection>>().set(Connection::Offline);
        settle(&mut app);
        assert_eq!(header(&mut app), before);
        let new = one::<NewTask>(&mut app);
        assert!(app.world().get::<InteractionDisabled>(new).is_some(), "New task is off while offline");

        app.world_mut().resource_mut::<NextState<Connection>>().set(Connection::Live);
        settle(&mut app);
        assert_eq!(header(&mut app), before);
        assert!(app.world().get::<InteractionDisabled>(new).is_none());
    }

    #[test]
    fn new_options_rebuild_only_that_selects_rows() {
        let mut app = app();
        let before = header(&mut app);
        let mut q = app.world_mut().query::<(Entity, &FilterSelect)>();
        let project = q.iter(app.world()).find(|(_, s)| s.0 == FilterKey::Project).unwrap().0;
        let mine: Vec<Entity> = app
            .world()
            .entity(project)
            .get::<Children>()
            .map_or(vec![], |_| before.iter().copied().filter(|e| is_under(app.world(), *e, project)).collect());
        app.world_mut().resource_mut::<BoardView>().options.projects.pop();
        settle(&mut app);
        let after = header(&mut app);
        let gone: Vec<_> = before.iter().filter(|e| !after.contains(e)).collect();
        assert!(!gone.is_empty(), "the select's old rows are replaced");
        for e in gone {
            assert!(mine.contains(e), "only rows of the select that changed");
        }
        assert!(after.contains(&project) && after.contains(&one::<ClosedToggle>(&mut app)));
    }
}
