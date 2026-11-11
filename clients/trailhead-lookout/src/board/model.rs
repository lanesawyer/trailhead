use super::*;

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
    pub(super) const ALL: [FilterKey; 3] = [FilterKey::Project, FilterKey::Parent, FilterKey::Holder];

    pub(super) fn name(self) -> &'static str {
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

    pub(super) fn get_mut(&mut self, key: FilterKey) -> &mut Option<String> {
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
    pub(super) fn act(&self, key: &str) -> Act {
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
pub(super) fn build_with(input: Input, filter: &Filter, nest: bool) -> BoardView {
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
    let is_factory = |t: &Task| in_queue(t) || crate::factory::factory_ran(t);

    let in_project = |t: &Task| filter.project.as_deref().is_none_or(|k| project_key.get(&t.project_id) == Some(&k));
    let passes = |t: &Task| {
        in_project(t)
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
                tone: Some(token::FACTORY),
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
                tone: Some(token::STATUS_WARN),
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
                tone: Some(token::STATUS_WARN),
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
                tone: Some(token::STATUS_WARN),
                hint: Some(format!("Blocked by {}", keys.join(", "))),
            });
        }
        if !open.is_empty() {
            badges.push(Badge {
                kind: BadgeKind::Questions,
                label: format!("? {}", open.len()),
                tone: Some(token::STATUS_WARN),
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
    // With a project picked, only the parents of its tasks.
    let parents = {
        let ids: BTreeSet<Uuid> = input.tasks.iter().filter(|t| in_project(t)).filter_map(|t| t.parent_id).collect();
        let title: HashMap<Uuid, &str> = input.tasks.iter().map(|t| (t.id, t.title.as_str())).collect();
        let mut keys: Vec<(&str, &str)> = ids
            .iter()
            .filter_map(|id| Some((*task_key.get(id)?, title.get(id).copied().unwrap_or_default())))
            .collect();
        keys.sort_by_key(|(k, _)| (key_number(k), k.to_string()));
        keys.into_iter().map(|(k, t)| choice(k, parent_label(k, t))).collect()
    };
    // Likewise only the holders of its tasks.
    let holders = {
        let ids: BTreeMap<Uuid, Uuid> = input
            .tasks
            .iter()
            .filter(|t| in_project(t))
            .filter_map(|t| holder_of.get(&t.id).map(|h| (*h, t.id)))
            .collect();
        let mut v: Vec<Choice> = ids.into_iter().map(|(id, t)| choice(&id.to_string(), holder_label(t, id))).collect();
        v.sort_by(|a, b| a.label.cmp(&b.label));
        v
    };
    BoardView { columns, options: Options { projects, parents, holders } }
}

/// Whether every word of `query` matches the task's key or title, ignoring case. A word matches the key when it starts
/// it (`trail-80`) or its number (`801`), so `801` finds TRAIL-801 and not TRAIL-1801; it matches the title anywhere.
pub(super) fn matches_key_or_title(query: &str, key: &str, title: &str) -> bool {
    let (key, title) = (key.to_lowercase(), title.to_lowercase());
    let number = key.rsplit('-').next().unwrap_or_default();
    query.split_whitespace().map(str::to_lowercase).all(|w| {
        key.starts_with(&w) || (w.chars().all(|c| c.is_ascii_digit()) && number.starts_with(&w)) || title.contains(&w)
    })
}

/// The card's line for a task the server found by its description or a comment.
pub(super) fn found_hint(hit: &DeepHit) -> String {
    let place = match hit.field.as_str() {
        "comment" => "comment",
        "title" => "title",
        _ => "description",
    };
    format!("in {place}: {}", hit.snippet.split_whitespace().collect::<Vec<_>>().join(" "))
}

pub(super) fn short(id: Uuid) -> String {
    id.to_string().chars().take(8).collect()
}

/// How many characters of a holder's name a badge shows.
pub(super) const HOLDER_BADGE_CHARS: usize = 12;

/// An agent's name for a badge: without its parent (`factory/trail-103-a1` is `trail-103-a1`), cut with an ellipsis
/// when still long.
pub(super) fn holder_badge(name: &str) -> String {
    let own = name.rsplit('/').next().unwrap_or(name);
    if own.chars().count() <= HOLDER_BADGE_CHARS {
        return own.to_string();
    }
    own.chars().take(HOLDER_BADGE_CHARS - 1).chain(['…']).collect()
}

/// How many characters of a parent's title its filter option shows.
pub(super) const PARENT_TITLE_CHARS: usize = 40;

/// A parent's filter option: its key, then its title cut with an ellipsis when long, since a key alone says nothing.
pub(super) fn parent_label(key: &str, title: &str) -> String {
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

pub(super) fn key_number(key: &str) -> i64 {
    key.rsplit('-').next().and_then(|n| n.parse().ok()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use trailhead_model::Priority;

    use super::super::testing::*;
    use super::*;
    use crate::fixture::{at, project_json, sid, task_json};

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
        assert_eq!(badge.tone, Some(token::STATUS_WARN));
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
                phase_started: 0,
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
        assert_eq!(tone, Some(token::STATUS_WARN));
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
        assert_eq!(badge("TRAIL-103", BadgeKind::Questions), Some(token::STATUS_WARN));
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
        // 103 and 105 are under the queue task, 104 has a run's metadata; 102 and OPS-1 (only skipped as ungraded) are
        // neither.
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
    fn a_picked_project_offers_only_its_own_parents() {
        let ops = run(&Filter { project: Some("OPS".into()), ..Default::default() });
        assert!(ops.options.parents.is_empty());
        let trail = run(&Filter { project: Some("TRAIL".into()), ..Default::default() });
        assert_eq!(trail.options.parents, run(&Filter::default()).options.parents);
    }

    #[test]
    fn a_picked_project_offers_only_the_holders_of_its_tasks() {
        let labels = |project: &str| {
            let view = run(&Filter { project: Some(project.into()), ..Default::default() });
            view.options.holders.into_iter().map(|c| c.label).collect::<Vec<_>>()
        };
        assert_eq!(labels("TRAIL"), ["factory/trail-103-a1"]);
        assert_eq!(labels("OPS"), [&sid("u1")[..8]]);
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
}
