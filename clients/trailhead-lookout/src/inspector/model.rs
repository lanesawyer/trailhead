use super::*;

/// One thing the inspector can do.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Action {
    Release,
    Cancel,
    /// Move the task to this status.
    Status(Status),
    /// Resolve this question (a comment id) without an answer. Answering opens the form instead.
    Resolve(String),
    /// Undo the event that last changed the task.
    Undo,
    /// Revert what the holder (an agent id) did in the last `n` hours.
    Revert(i64),
    /// Set the task's grade; none clears it.
    Grade(Option<Grade>),
    /// What the task modal's comment box sends: only the key of its control, since the box builds its own request.
    Comment,
    /// The key of the one control the task modal's ⋯ menu shares, so the last item's answer shows beside the menu.
    Menu,
}

impl Action {
    pub fn label(&self) -> String {
        match self {
            Action::Release => "release claim".into(),
            Action::Cancel => "cancel task".into(),
            Action::Status(s) => format!("move to {}", status_label(*s)),
            Action::Resolve(_) => "resolve".into(),
            Action::Undo => "undo last change".into(),
            // The window is a dropdown beside the button, so the caption does not name it.
            Action::Revert(_) => "revert since".into(),
            Action::Grade(Some(g)) => format!("grade {g}"),
            Action::Grade(None) => "clear grade".into(),
            Action::Comment => "comment".into(),
            Action::Menu => "more".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    /// The task's key, which the answer form needs.
    pub key: String,
    pub id: String,
    pub body: String,
}

/// A task's PR as the `factory` metadata describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrPanel {
    pub url: String,
    pub number: u64,
    /// `open`, or `merged by a person` / `merged by the merger` with the commit.
    pub state: String,
    pub grade: Option<Grade>,
    pub passed: Vec<String>,
    pub waiting: Vec<String>,
    pub would_merge: bool,
    /// Why the merger couldn't act or the host refused to merge.
    pub problem: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectorView {
    pub subject: Subject,
    pub title: String,
    /// Plain lines under the title: status, holder, run.
    pub lines: Vec<String>,
    pub questions: Vec<Question>,
    pub pr: Option<PrPanel>,
    /// Shown instead of a merge control: who finishes the task.
    pub merge_note: Option<String>,
    /// The task's own status; none for an agent.
    pub status: Option<Status>,
    /// The task's priority; none for an agent.
    pub priority: Option<Priority>,
    /// The task's risk grade; none for an agent, or a task with none.
    pub grade: Option<Grade>,
    pub actions: Vec<Action>,
    /// What the task modal shows beyond the panel's lines; none for an agent.
    pub task: Option<TaskFacts>,
}

/// A task's facts for the task modal, as words.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TaskFacts {
    /// Project, parent and the task's key: `TRAIL › TRAIL-685 › TRAIL-882`.
    pub breadcrumb: String,
    pub title: String,
    pub description: String,
    /// The project's key, which opens its page.
    pub project: Option<String>,
    /// The parent, a row the modal can open.
    pub parent: Option<Subtask>,
    pub subtasks: Vec<Subtask>,
    /// Who holds the claim and for how long; none when nobody does.
    pub claim: Option<String>,
    /// Scheduled and due, each a line.
    pub dates: Vec<String>,
    /// Blocked by, blocks, duplicates: each a line. Only once the task is fetched.
    pub links: Vec<String>,
    /// The factory's run: attempt, sandbox, spend, review rounds.
    pub run: Vec<String>,
    /// Created, updated, version.
    pub details: Vec<String>,
    /// The version shown, which an edit's `If-Match` names.
    pub version: i64,
    /// Why the task couldn't be fetched in full.
    pub fetch_error: Option<String>,
    /// Every comment, oldest first: all of them once the task is fetched, until then what the mirror holds (open
    /// questions and the last handoff).
    pub activity: Vec<Entry>,
}

/// One comment in the task modal's activity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub kind: CommentKind,
    pub author: String,
    /// When it was written, in the reader's zone.
    pub when: String,
    /// To whom, open or resolved for a question, a review's verdict and model.
    pub tags: Vec<String>,
    pub body: String,
    /// A question nobody has resolved yet, which can be answered or resolved from here.
    pub open_question: bool,
    /// Written by the reader, so it sits on the right.
    pub mine: bool,
}

/// What the modal knows beyond the mirror: the task fetched in full, why it couldn't be, and who is reading (their
/// id and name), to name what they wrote.
#[derive(Debug, Clone, Copy, Default)]
pub struct Fetched<'a> {
    pub detail: Option<&'a TaskDetail>,
    pub error: Option<&'a str>,
    pub me: Option<(&'a str, &'a str)>,
}

/// One of a task's subtasks, a row the modal can open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subtask {
    pub id: String,
    pub key: String,
    pub title: String,
    pub status: Status,
}

/// The PR panel from the task's run; none when it has no PR yet.
pub fn pr_panel(run: &RunUpdate) -> Option<PrPanel> {
    let merged = run.merged.as_ref();
    let pr = merged.map(|m| &m.pr).or(run.pr.as_ref())?;
    let state = match merged {
        Some(m) => {
            let by = match m.by {
                MergedBy::Merger => "the merger",
                MergedBy::Human => "a person",
            };
            let commit =
                m.commit.as_ref().map_or(String::new(), |c| format!(" as {}", c.chars().take(8).collect::<String>()));
            format!("merged by {by}{commit}")
        }
        None => "open".to_string(),
    };
    let decision = run.merge.as_ref();
    let gates = |of: fn(&MergeDecision) -> &Vec<GateKind>| {
        decision.map(|d| of(d).iter().map(|g| g.as_str().to_string()).collect()).unwrap_or_default()
    };
    Some(PrPanel {
        url: pr.url.clone(),
        number: pr.number,
        state,
        grade: run.grade.as_ref().map(|g| g.grade).or(decision.map(|d| d.grade)),
        passed: gates(|d| &d.passed),
        waiting: gates(|d| &d.waiting),
        would_merge: decision.is_some_and(|d| d.would_merge),
        problem: decision.and_then(|d| d.problem.clone().or_else(|| d.refused.clone())),
    })
}

/// The statuses a task can be moved to by hand from the panel: any the task can go to next, but not cancelled (its
/// own action) and, on the factory's tasks, not done: the factory closes those once their PR merges. Approving one is
/// the human gate, which the merger merges after (`docs/factory/review.md`).
pub(super) fn status_moves(current: Status, factory: bool) -> Vec<Action> {
    Status::ALL
        .into_iter()
        .filter(|to| *to != current && *to != Status::Cancelled && current.can_transition_to(*to))
        .filter(|to| !factory || *to != Status::Done)
        .map(Action::Status)
        .collect()
}

/// The panel for `subject`, or none when it isn't in the mirror (deleted, or not synced yet). `latest` maps a task
/// id to the id of the event that last changed it.
pub fn build(
    input: Input,
    latest: &HashMap<String, i64>,
    subject: &Subject,
    fetched: Fetched,
) -> Option<InspectorView> {
    let Fetched { detail, error: fetch_error, me } = fetched;
    let agent_name = |id: Uuid| {
        input
            .agents
            .iter()
            .find(|a| a.id == id)
            .map(|a| a.name.clone())
            .or_else(|| input.users.iter().find(|u| u.id == id).map(|u| u.name.clone()))
    };
    let task_key = |id: Uuid| input.tasks.iter().find(|t| t.id == id).map(|t| t.key.0.clone());
    match subject {
        Subject::Agent(id) => {
            let id = Uuid::parse_str(id).ok()?;
            let name = agent_name(id)?;
            let held: Vec<String> =
                input.claims.iter().filter(|c| c.holder_id == id).filter_map(|c| task_key(c.task_id)).collect();
            let lines =
                vec![if held.is_empty() { "holds nothing".into() } else { format!("holds {}", held.join(", ")) }];
            Some(InspectorView {
                subject: subject.clone(),
                title: name,
                lines,
                questions: Vec::new(),
                pr: None,
                merge_note: None,
                status: None,
                priority: None,
                grade: None,
                actions: vec![Action::Revert(1)],
                task: None,
            })
        }
        Subject::Task(id) => {
            // The fetched copy is the newer, when there is one for this task.
            let task = detail
                .map(|d| &d.task)
                .filter(|t| t.id.to_string() == *id)
                .or_else(|| input.tasks.iter().find(|t| t.id.to_string() == *id))?;
            let status = task.status;
            let is_factory = crate::factory::factory_ran(task);
            let run = RunUpdate::of(task).ok();
            let holder = input.claims.iter().find(|c| c.task_id == task.id).map(|c| c.holder_id);
            let holder_agent = holder.and_then(agent_name);

            let mut lines = Vec::new();
            match (&holder_agent, holder) {
                (Some(name), _) => lines.push(format!("held by {name}")),
                (None, Some(h)) => lines.push(format!("held by {}", h.to_string().chars().take(8).collect::<String>())),
                (None, None) => {}
            }
            if let Some(run) = &run
                && let Some(sandbox) = &run.sandbox
            {
                let attempt = run.attempt.map_or(String::new(), |a| format!(", attempt {a}"));
                lines.push(format!("run: {sandbox}{attempt}"));
            }

            let mut questions: Vec<&Comment> = input
                .comments
                .iter()
                .filter(|c| c.task_id == task.id && c.kind == CommentKind::Question && c.resolved_at.is_none())
                .collect();
            questions.sort_by_key(|c| c.created_at);
            let questions: Vec<Question> = questions
                .into_iter()
                .map(|q| Question { key: task.key.0.clone(), id: q.id.to_string(), body: q.body.clone() })
                .collect();

            let pr = run.as_ref().and_then(pr_panel);
            let merge_note = (is_factory && !status.is_closed()).then(|| match &pr {
                Some(p) if p.state == "open" && status == Status::Approved => {
                    "Approved: the merger merges the PR, and the factory closes the task once it has.".to_string()
                }
                Some(p) if p.state == "open" && p.waiting.iter().any(|g| g == "human") => {
                    "Waiting for you: approve it here and the merger merges the PR (or merge it on GitHub).".to_string()
                }
                Some(p) if p.state == "open" => {
                    "Merge on GitHub (or let the merger): the factory approves it and closes the task once the PR merges.".to_string()
                }
                Some(_) => "The PR is merged; the factory is closing the task.".to_string(),
                None => "No PR yet; the factory finishes the task when its PR merges.".to_string(),
            });

            let mut actions = Vec::new();
            if holder.is_some() {
                actions.push(Action::Release);
            }
            if status != Status::Cancelled && status.can_transition_to(Status::Cancelled) {
                actions.push(Action::Cancel);
            }
            actions.extend(status_moves(status, is_factory));
            actions.extend(questions.iter().map(|q| Action::Resolve(q.id.clone())));
            if latest.contains_key(id) {
                actions.push(Action::Undo);
            }
            if holder_agent.is_some() {
                actions.push(Action::Revert(1));
            }
            Some(InspectorView {
                subject: subject.clone(),
                title: format!("{} {}", task.key, task.title),
                lines,
                questions,
                pr,
                merge_note,
                status: Some(status),
                priority: Some(task.priority),
                grade: task.grade,
                actions,
                task: Some(facts(&input, task, Fetched { detail, error: fetch_error, me }, run.as_ref(), agent_name)),
            })
        }
    }
}

/// How times read in the modal: the reader's zone, as the CLI shows them.
pub(super) fn clock() -> &'static Wording {
    static WORDING: std::sync::OnceLock<Wording> = std::sync::OnceLock::new();
    WORDING.get_or_init(|| Wording::terminal(jiff::tz::TimeZone::system()))
}

/// The task modal's facts for `task`. `detail` adds what the mirror lacks (dependencies, a fresh claim) when it is
/// this task's.
pub(super) fn facts(
    input: &Input,
    task: &Task,
    fetched: Fetched,
    run: Option<&RunUpdate>,
    agent_name: impl Fn(Uuid) -> Option<String>,
) -> TaskFacts {
    let text = clock();
    let Fetched { detail, error: fetch_error, me } = fetched;
    let detail = detail.filter(|d| d.task.id == task.id);
    let name_of = |id: Uuid| match me {
        Some((my_id, my_name)) if my_id == id.to_string() => my_name.to_string(),
        _ => agent_name(id).unwrap_or_else(|| id.to_string().chars().take(8).collect()),
    };
    let project_key = input.projects.iter().find(|p| p.id == task.project_id).map(|p| p.key.clone());
    let parent = task.parent_id.and_then(|p| input.tasks.iter().find(|t| t.id == p));
    let mut crumbs: Vec<String> = project_key.iter().cloned().collect();
    crumbs.extend(parent.map(|p| p.key.0.clone()));
    crumbs.push(task.key.0.clone());
    let project = project_key.clone();
    let parent = parent.map(|p| Subtask {
        id: p.id.to_string(),
        key: p.key.0.clone(),
        title: p.title.clone(),
        status: p.status,
    });

    let mut subtasks: Vec<&Task> = input.tasks.iter().filter(|t| t.parent_id == Some(task.id)).collect();
    subtasks.sort_by_key(|t| (key_number(t.key.as_str()), t.key.0.clone()));
    let subtasks = subtasks
        .into_iter()
        .map(|t| Subtask { id: t.id.to_string(), key: t.key.0.clone(), title: t.title.clone(), status: t.status })
        .collect();

    let claim = match detail {
        Some(d) => d.claim.clone(),
        None => input.claims.iter().find(|c| c.task_id == task.id).cloned(),
    }
    .filter(|c| c.is_live(input.now))
    .map(|c| format!("{}, {}", name_of(c.holder_id), text.lease(&c, input.now)));

    let dates = [("scheduled", text.scheduled(task)), ("due", text.due(task))]
        .into_iter()
        .filter_map(|(what, when)| when.map(|w| format!("{what} {w}")))
        .collect();

    let mut links = Vec::new();
    if let Some(d) = detail {
        for (what, keys) in [("blocked by", &d.blocked_by), ("blocks", &d.blocks), ("duplicates", &d.duplicates)] {
            if !keys.is_empty() {
                links.push(format!("{what} {}", keys.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", ")));
            }
        }
        if let Some(k) = &d.duplicate_of {
            links.push(format!("duplicate of {k}"));
        }
    }

    let mut run_lines = Vec::new();
    if let Some(run) = run {
        if let Some(a) = run.attempt {
            run_lines.push(format!("attempt {a}"));
        }
        if let Some(sandbox) = &run.sandbox {
            run_lines.push(format!("sandbox {sandbox}"));
        }
        if let Some(spend) = run.spend {
            run_lines.push(format!("spent ${:.2}", spend.as_dollars()));
        }
        if let Some(rounds) = run.review_rounds {
            run_lines.push(format!("review rounds {rounds}"));
        }
    }

    let mut comments: Vec<&Comment> = match detail {
        Some(d) => d.comments.iter().collect(),
        None => input.comments.iter().filter(|c| c.task_id == task.id).collect(),
    };
    comments.retain(|c| c.deleted_at.is_none());
    comments.sort_by_key(|c| (c.created_at, c.id));
    let activity = comments
        .into_iter()
        .map(|c| {
            let mut tags = Vec::new();
            if let Some(to) = c.addressed_to {
                tags.push(format!("to {}", name_of(to)));
            }
            let open_question = c.kind == CommentKind::Question && c.resolved_at.is_none();
            if c.kind == CommentKind::Question {
                tags.push(if open_question { "open".into() } else { "resolved".into() });
            }
            if let Some(v) = c.verdict {
                tags.push(v.as_str().replace('_', " "));
            }
            if let Some(m) = &c.model {
                tags.push(m.clone());
            }
            Entry {
                id: c.id.to_string(),
                kind: c.kind,
                author: name_of(c.author_id),
                when: text.time(c.created_at, None),
                tags,
                body: c.body.clone(),
                open_question,
                mine: me.is_some_and(|(id, _)| id == c.author_id.to_string()),
            }
        })
        .collect();

    TaskFacts {
        breadcrumb: crumbs.join(" › "),
        title: task.title.clone(),
        description: task.description.clone(),
        project,
        parent,
        subtasks,
        claim,
        dates,
        links,
        run: run_lines,
        details: vec![
            format!("created {}", text.time(task.created_at, None)),
            format!("updated {}", text.time(task.updated_at, None)),
            format!("version {}", task.version),
        ],
        fetch_error: fetch_error.map(str::to_string),
        activity,
        version: task.version,
    }
}

/// The number in a key, for ordering `TRAIL-9` before `TRAIL-10`.
pub(super) fn key_number(key: &str) -> i64 {
    key.rsplit('-').next().and_then(|n| n.parse().ok()).unwrap_or(0)
}

/// The request an action sends for the subject, or none when what it needs isn't there. `now` is unix seconds.
pub fn plan(
    action: &Action,
    input: Input,
    latest: &HashMap<String, i64>,
    subject: &Subject,
    now: i64,
) -> Option<Request> {
    let task = match subject {
        Subject::Task(id) => input.tasks.iter().find(|t| t.id.to_string() == *id),
        Subject::Agent(_) => None,
    };
    let key = task.map(|t| t.key.as_str());
    let version = task.map(|t| t.version);
    let path = |tail: &str| format!("/v1/tasks/{}{tail}", key.unwrap_or_default());
    Some(match action {
        Action::Release => Request::new(Method::POST, path("/release"), None, None),
        Action::Cancel => Request::new(Method::PATCH, path(""), Some(json!({"status": Status::Cancelled})), version),
        Action::Status(st) => Request::new(Method::PATCH, path(""), Some(json!({"status": st})), version),
        Action::Grade(g) => Request::new(Method::PATCH, path(""), Some(json!({"grade": g})), version),
        Action::Resolve(comment) => Request::new(Method::POST, format!("/v1/comments/{comment}/resolve"), None, None),
        Action::Comment | Action::Menu => return None,
        Action::Undo => {
            let Subject::Task(id) = subject else { return None };
            Request::new(Method::POST, "/v1/undo", Some(json!({"event_id": latest.get(id)?})), None)
        }
        Action::Revert(hours) => {
            let actor = match subject {
                Subject::Agent(id) => id.clone(),
                Subject::Task(id) => {
                    input.claims.iter().find(|c| c.task_id.to_string() == *id).map(|c| c.holder_id.to_string())?
                }
            };
            let body = json!({"actor_id": actor, "since": rfc3339(now - hours * 3600)});
            Request::new(Method::POST, "/v1/revert", Some(body), None)
        }
    })
}

/// A unix time as RFC 3339 in UTC.
pub fn rfc3339(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// What to say beside a control for the server's answer; none for success.
pub fn describe(outcome: &Outcome) -> Option<String> {
    match outcome {
        Outcome::Ok { .. } => None,
        Outcome::Conflict { message, .. } => Some(format!("changed since you looked: {message}")),
        Outcome::Forbidden(m) => Some(format!("not allowed: {m}")),
        Outcome::RateLimited { message, retry_after } => Some(match retry_after {
            Some(after) => format!("rate limited, try again in {}s: {message}", after.as_secs()),
            None => format!("rate limited: {message}"),
        }),
        Outcome::Failed(m) => Some(m.clone()),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::testing::*;
    use super::*;

    use crate::fixture::{at, sid, task};

    #[test]
    fn a_fetched_task_adds_its_dependencies_claim_run_and_details() {
        let f = fixture();
        let detail: TaskDetail = serde_json::from_value(json!({
            "task": serde_json::to_value(&f.tasks[0]).unwrap(),
            "claim": crate::fixture::claim_json(json!({"task_id": "t7", "holder_id": "a1", "expires_at": "2099-01-01T00:00:00Z"})),
            "blocked_by": ["TRAIL-3"], "blocks": ["TRAIL-9", "TRAIL-10"], "comments": [],
            "duplicate_of": null, "duplicates": [],
        }))
        .unwrap();
        let v = build(
            f.input(),
            &latest(),
            &Subject::Task(sid("t7")),
            Fetched { detail: Some(&detail), ..Fetched::default() },
        )
        .unwrap();
        let facts = v.task.unwrap();
        assert_eq!((facts.breadcrumb.as_str(), facts.title.as_str()), ("TRAIL-7", "do it"));
        assert_eq!(facts.links, ["blocked by TRAIL-3", "blocks TRAIL-9, TRAIL-10"]);
        assert!(facts.claim.unwrap().starts_with("factory/trail-7-a1, until "));
        assert_eq!(facts.run, ["attempt 2", "sandbox trail-7-a1"]);
        assert_eq!(facts.details.last().unwrap(), "version 4");

        // Without the fetch there are no dependencies yet, and a failed fetch says why.
        let v = build(
            f.input(),
            &latest(),
            &Subject::Task(sid("t7")),
            Fetched { error: Some("the server answered 500"), ..Fetched::default() },
        )
        .unwrap();
        let facts = v.task.unwrap();
        assert!(facts.links.is_empty());
        assert_eq!(facts.fetch_error.as_deref(), Some("the server answered 500"));
        // An agent has no facts: it stays in the side panel.
        assert!(build(f.input(), &latest(), &Subject::Agent(sid("a1")), Fetched::default()).unwrap().task.is_none());
    }

    #[test]
    fn the_activity_lists_every_comment_oldest_first_naming_who_wrote_it() {
        let f = fixture();
        let me = sid("u1");
        let detail: TaskDetail = serde_json::from_value(json!({
            "task": serde_json::to_value(&f.tasks[0]).unwrap(),
            "claim": null, "blocked_by": [], "blocks": [], "duplicate_of": null, "duplicates": [],
            "comments": [
                crate::fixture::comment_json(json!({"id": "c3", "task_id": "t7", "kind": "review", "author_id": "a1",
                    "body": "looks right", "verdict": "request_changes", "model": "m-1", "created_at": at(3)})),
                crate::fixture::comment_json(json!({"id": "c1", "task_id": "t7", "kind": "question", "author_id": "a1",
                    "addressed_to": "u1", "body": "which db?", "created_at": at(1)})),
                crate::fixture::comment_json(json!({"id": "c2", "task_id": "t7", "kind": "note", "author_id": "u1",
                    "body": "sqlite", "created_at": at(2)})),
            ],
        }))
        .unwrap();
        let fetched = Fetched { detail: Some(&detail), me: Some((me.as_str(), "lane")), ..Fetched::default() };
        let activity = build(f.input(), &latest(), &Subject::Task(sid("t7")), fetched).unwrap().task.unwrap().activity;
        let bodies: Vec<&str> = activity.iter().map(|e| e.body.as_str()).collect();
        assert_eq!(bodies, ["which db?", "sqlite", "looks right"]);
        assert_eq!(activity[0].author, "factory/trail-7-a1");
        assert_eq!(activity[0].tags, ["to lane", "open"]);
        assert!(activity[0].open_question);
        assert_eq!(activity[1].author, "lane");
        let mine: Vec<bool> = activity.iter().map(|e| e.mine).collect();
        assert_eq!(mine, [false, true, false], "only what the reader wrote is theirs");
        assert_eq!(activity[2].tags, ["request changes", "m-1"]);
        assert!(!activity[2].open_question);
    }

    #[test]
    fn the_status_line_shows_only_when_the_move_dropdown_does_not() {
        let f = fixture();
        let v = build(f.input(), &latest(), &Subject::Task(sid("t7")), Fetched::default()).unwrap();
        let view = v;
        let texts = |live: bool| -> Vec<String> {
            let plan = layout(&view, live, "offline", 0, &[], &|_| None);
            plan.iter()
                .flat_map(|(_, items)| items)
                .filter_map(|(_, b)| if let Block::Text { text, .. } = b { Some(text.clone()) } else { None })
                .collect()
        };
        assert!(!view.lines.iter().any(|l| l.starts_with("status:")));
        let moves = view.actions.iter().any(|a| matches!(a, Action::Status(_)));
        assert_eq!(texts(true).iter().any(|t| t.starts_with("status:")), !moves);
    }

    #[test]
    fn a_task_view_carries_its_priority_and_an_agent_has_none() {
        let f = fixture();
        let v = build(f.input(), &latest(), &Subject::Task(sid("t7")), Fetched::default()).unwrap();
        assert_eq!(v.priority, Priority::new(1));
        let a = build(f.input(), &latest(), &Subject::Agent(sid("a1")), Fetched::default()).unwrap();
        assert_eq!(a.priority, None);
    }

    #[test]
    fn a_factory_task_shows_its_pr_and_gates_and_has_no_path_to_done() {
        let f = fixture();
        let v = build(f.input(), &latest(), &Subject::Task(sid("t7")), Fetched::default()).unwrap();
        assert_eq!(v.title, "TRAIL-7 do it");
        assert!(v.lines.contains(&"held by factory/trail-7-a1".to_string()));
        assert!(v.lines.contains(&"run: trail-7-a1, attempt 2".to_string()));
        let pr = v.pr.unwrap();
        assert_eq!((pr.url.as_str(), pr.number, pr.state.as_str()), ("https://github.com/o/r/pull/12", 12, "open"));
        assert_eq!(
            (pr.passed, pr.waiting, pr.grade),
            (vec!["checks".into(), "ci".into()], vec!["agent_review".into()], Grade::new(2))
        );
        // The fixture's PR waits on agent review, not on a person.
        assert!(v.merge_note.unwrap().contains("Merge on GitHub"));
        assert!(!v.actions.contains(&Action::Status(Status::Done)), "the factory closes its tasks");
        assert_eq!(v.questions.len(), 1);
        for want in [Action::Release, Action::Cancel, Action::Undo, Action::Revert(1), Action::Resolve(sid("c1"))] {
            assert!(v.actions.contains(&want), "{want:?}");
        }
        assert!(!v.actions.contains(&Action::Status(Status::InReview)));
    }

    #[test]
    fn a_task_the_factory_only_skipped_for_its_grade_can_be_approved_and_finished_by_hand() {
        // Interactive work filed under a queue task, ungraded so the factory skips it (TRAIL-1159).
        let mut f = fixture();
        let t8 = f.tasks.iter_mut().find(|t| t.key.0 == "TRAIL-8").unwrap();
        t8.status = Status::InReview;
        t8.metadata = json!({"factory": {"ungraded": true}});
        let v = build(f.input(), &HashMap::new(), &Subject::Task(sid("t8")), Fetched::default()).unwrap();
        assert!(v.merge_note.is_none());
        assert!(v.actions.contains(&Action::Status(Status::Approved)), "{:?}", v.actions);
    }

    #[test]
    fn a_factory_task_waiting_on_a_person_can_be_approved_here_for_the_merger_to_merge() {
        // Grade 3 and up wait on Lane's approval, which the merger merges after (TRAIL-1160).
        let mut f = fixture();
        let t7 = f.tasks.iter_mut().find(|t| t.key.0 == "TRAIL-7").unwrap();
        t7.metadata["factory"]["merge"]["waiting"] = json!(["human"]);
        let v = build(f.input(), &latest(), &Subject::Task(sid("t7")), Fetched::default()).unwrap();
        assert!(v.actions.contains(&Action::Status(Status::Approved)), "{:?}", v.actions);
        assert!(!v.actions.contains(&Action::Status(Status::Done)));
        assert!(v.merge_note.unwrap().starts_with("Waiting for you"));

        let t7 = f.tasks.iter_mut().find(|t| t.key.0 == "TRAIL-7").unwrap();
        t7.status = Status::Approved;
        let v = build(f.input(), &latest(), &Subject::Task(sid("t7")), Fetched::default()).unwrap();
        assert!(v.merge_note.unwrap().starts_with("Approved: the merger merges"));
        assert!(!v.actions.contains(&Action::Status(Status::Done)));
    }

    #[test]
    fn a_plain_task_has_no_claim_controls_and_no_merge_note() {
        let f = fixture();
        let v = build(f.input(), &HashMap::new(), &Subject::Task(sid("t8")), Fetched::default()).unwrap();
        assert!(v.pr.is_none() && v.merge_note.is_none());
        assert!(!v.actions.contains(&Action::Release) && !v.actions.contains(&Action::Undo));
        assert!(v.actions.contains(&Action::Status(Status::Done)));
    }

    #[test]
    fn a_done_task_offers_no_cancel() {
        let mut f = fixture();
        f.tasks.push(task(json!({"id": "t9", "key": "TRAIL-9", "title": "finished", "status": "done", "version": 1})));
        let v = build(f.input(), &HashMap::new(), &Subject::Task(sid("t9")), Fetched::default()).unwrap();
        assert!(!v.actions.contains(&Action::Cancel));
    }

    #[test]
    fn a_cancelled_task_offers_no_cancel() {
        let mut f = fixture();
        f.tasks.push(task(
            json!({"id": "t10", "key": "TRAIL-10", "title": "dropped", "status": "cancelled", "version": 1}),
        ));
        let v = build(f.input(), &HashMap::new(), &Subject::Task(sid("t10")), Fetched::default()).unwrap();
        assert!(!v.actions.contains(&Action::Cancel));
    }

    #[test]
    fn a_merged_pr_says_who_merged_it() {
        let factory = json!({"merged": {"by": "human", "commit": "abcdef0123",
                                         "pr": {"repo": {"owner": "o", "name": "r"}, "number": 3, "url": "https://x/pull/3"}}});
        let run: RunUpdate = serde_json::from_value(factory).unwrap();
        assert_eq!(pr_panel(&run).unwrap().state, "merged by a person as abcdef01");
        assert!(pr_panel(&RunUpdate::default()).is_none());
    }

    #[test]
    fn an_agent_offers_revert() {
        let f = fixture();
        let v = build(f.input(), &latest(), &Subject::Agent(sid("a1")), Fetched::default()).unwrap();
        assert_eq!((v.title.as_str(), v.lines[0].as_str()), ("factory/trail-7-a1", "holds TRAIL-7"));
        assert_eq!(v.actions, [Action::Revert(1)]);
        assert!(build(f.input(), &latest(), &Subject::Agent(sid("nope")), Fetched::default()).is_none());
    }

    #[test]
    fn actions_become_the_requests_the_api_takes() {
        let f = fixture();
        let t = Subject::Task(sid("t7"));
        let p = |a: &Action| plan(a, f.input(), &latest(), &t, 1_700_000_000).unwrap();
        let r = p(&Action::Release);
        assert_eq!((r.method, r.path.as_str(), r.if_match), (Method::POST, "/v1/tasks/TRAIL-7/release", None));
        let r = p(&Action::Cancel);
        assert_eq!((r.method, r.path.as_str(), r.if_match), (Method::PATCH, "/v1/tasks/TRAIL-7", Some(4)));
        assert_eq!(r.body, Some(json!({"status": "cancelled"})));
        assert_eq!(p(&Action::Status(Status::Open)).body, Some(json!({"status": "open"})));
        let r = p(&Action::Resolve(sid("c1")));
        assert_eq!((r.path, r.body), (format!("/v1/comments/{}/resolve", sid("c1")), None));
        assert_eq!(p(&Action::Undo).body, Some(json!({"event_id": 41})));
        // The window lives in the dropdown, so every revert captions the same.
        assert_eq!(Action::Revert(1).label(), Action::Revert(72).label());
        let r = p(&Action::Revert(1));
        assert_eq!(
            (r.path.as_str(), r.body),
            ("/v1/revert", Some(json!({"actor_id": sid("a1"), "since": "2023-11-14T21:13:20Z"})))
        );
        // From the agent itself.
        let r = plan(&Action::Revert(24), f.input(), &latest(), &Subject::Agent(sid("a1")), 1_700_000_000).unwrap();
        assert_eq!(r.body.unwrap()["since"], "2023-11-13T22:13:20Z");
        // No event to undo.
        assert!(plan(&Action::Undo, f.input(), &HashMap::new(), &t, 0).is_none());
    }

    #[test]
    fn times() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn refusals_are_described_for_the_control() {
        assert_eq!(describe(&Outcome::Ok { version: Some(2) }), None);
        assert!(
            describe(&Outcome::Conflict { current_version: Some(3), message: "stale".into() })
                .unwrap()
                .contains("stale")
        );
        assert!(describe(&Outcome::Forbidden("humans only".into())).unwrap().contains("humans only"));
        let limited = Outcome::RateLimited { retry_after: Some(Duration::from_secs(3)), message: "slow".into() };
        assert!(describe(&limited).unwrap().contains("3s"));
        assert_eq!(describe(&Outcome::Failed("no".into())).as_deref(), Some("no"));
    }
}
