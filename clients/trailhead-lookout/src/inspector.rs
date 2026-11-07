//! The inspector (`docs/factory/ui.md#views`): the selected task, run or agent, and the controls to steer it: release
//! a claim, cancel, move the status, answer a question, undo the last change, revert what an agent did since a time.
//! Each is a command ([`crate::commands`]) whose refusal is shown beside the control that sent it.
//!
//! A task shows in the [`TaskModal`], in the middle of the window: its title, description, subtasks, open questions
//! and PR in a main column, and its facts (project, status, priority, grade, claim, dates, dependencies, run) in a
//! sidebar. The whole task is fetched when it is selected ([`crate::detail`]), for what the mirror doesn't hold. Its
//! header has up and down arrows (and the keys up, down, K and J) to the previous and next task in its board column,
//! and a ⋯ menu of what else can be done to it: release, cancel, undo, revert. An agent still shows in the side
//! panel, which stays for anything that suits a panel better than a modal.
//!
//! There is no approve or done button. A factory task is approved by the merger once its gates pass and finished when
//! its PR merges (`docs/factory/lifecycle.md`), so the panel links the PR and shows its merge state and gates from the
//! task's `factory` metadata and leaves the merge to GitHub. Status moves on a factory task leave out `approved` and
//! `done` for the same reason.
//!
//! [`build`] is a pure function from the mirror's JSON to an [`InspectorView`], and [`plan`] turns an [`Action`] into
//! the [`Request`] it sends, so both are tested without a window. "answer" opens the answer form from
//! [`crate::author`], as the board's does; "resolve" closes a question without one.

use std::collections::HashMap;

use bevy::feathers::controls::ButtonVariant;
use bevy::feathers::tokens;
use bevy::input::keyboard::KeyboardInput;
use bevy::input_focus::{FocusCause, FocusedInput, InputFocus};
use bevy::prelude::*;
use bevy::text::EditableText;
use bevy::ui::InteractionDisabled;
use bevy::ui_widgets::{Activate, ControlOrientation, RequestClose};
use reqwest::Method;
use serde_json::{Value, json};
use trailhead_client::text::Text as Wording;
use trailhead_factory::{GateKind, METADATA_KEY, MergeDecision, MergedBy, RunUpdate};
use trailhead_model::{Agent, Claim, Comment, CommentKind, Grade, Priority, Project, Status, Task, TaskDetail, User};
use uuid::Uuid;

use crate::author::{Act, grade_choices, grade_index, grade_options};
use crate::board::Input;
use crate::commands::{CommandDone, Outcome, Request, Submit};
use crate::connection::{Api, Connection, ConnectionStatus};
use crate::detail::{Detail, Refetch};
use crate::factory::FactoryStatus;
use crate::mirror::{Mirror, Mirrored, Rev, rows};
use crate::sync::SyncItem;
use crate::widgets::{
    DetailPanes, Icon, InlineChosen, InlineEdited, InlineField, InlineText, Keyed, Modal, ModalClosed, ModalRoot,
    ModalSlots, PanelTitle, Picked, SelectEdit, badge, button, danger_button, display, extent, icon_menu, inline_field,
    inline_text_sized, layer, menu_item, open_modal, panel, panel_header, priority_tone, row, scroll_body,
    scroll_frame, scrollbar, section_frame, select, size, space, spawn_detail_panes, status_label, text, text_dim,
    text_input, text_toned, token, tool_button,
};
use bevy::feathers::theme::{ThemeTextColor, ThemeToken};

/// What the inspector is looking at: a task (and its run, if it has one), or an agent. Ids, not keys.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Subject {
    Task(String),
    Agent(String),
}

#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct Selection(pub Option<Subject>);

/// The tasks before and after the selected one in its board column (ids), which the task modal's arrows go to. The
/// board keeps it ([`crate::board`]); none at a column's end or for a task the board doesn't show.
#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct Neighbours {
    pub previous: Option<String>,
    pub next: Option<String>,
}

/// How far back "revert" goes, in hours; one of [`WINDOWS`], picked from a dropdown.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevertWindow(pub i64);

impl Default for RevertWindow {
    fn default() -> Self {
        RevertWindow(1)
    }
}

const WINDOWS: [i64; 4] = [1, 6, 24, 72];

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
            Action::Revert(h) => format!("revert last {h}h"),
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
    pub failing: Vec<String>,
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
    /// The project and the parent with its title.
    pub project: String,
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
        failing: decision.map(|d| d.failing.clone()).unwrap_or_default(),
        would_merge: decision.is_some_and(|d| d.would_merge),
        problem: decision.and_then(|d| d.problem.clone().or_else(|| d.refused.clone())),
    })
}

/// The statuses a task can be moved to by hand from the panel: any the task can go to next, but not cancelled (its
/// own action) and, on the factory's tasks, not approved or done (the factory finishes those).
fn status_moves(current: Status, factory: bool) -> Vec<Action> {
    Status::ALL
        .into_iter()
        .filter(|to| *to != current && *to != Status::Cancelled && current.can_transition_to(*to))
        .filter(|to| !factory || !matches!(to, Status::Approved | Status::Done))
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
    let task_key = |id: Uuid| input.tasks.iter().find(|t| t.id == id).map(|t| t.key.clone());
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
            let is_factory = task.metadata.get(METADATA_KEY).is_some_and(Value::is_object);
            let run = RunUpdate::of(task).ok();
            let holder = input.claims.iter().find(|c| c.task_id == task.id).map(|c| c.holder_id);
            let holder_agent = holder.and_then(agent_name);

            let mut lines = vec![format!("status: {}", status_label(status))];
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
                .map(|q| Question { key: task.key.clone(), id: q.id.to_string(), body: q.body.clone() })
                .collect();

            let pr = run.as_ref().and_then(pr_panel);
            let merge_note = (is_factory && !status.is_closed()).then(|| match &pr {
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
fn clock() -> &'static Wording {
    static WORDING: std::sync::OnceLock<Wording> = std::sync::OnceLock::new();
    WORDING.get_or_init(|| Wording::terminal(jiff::tz::TimeZone::system()))
}

/// The task modal's facts for `task`. `detail` adds what the mirror lacks (dependencies, a fresh claim) when it is
/// this task's.
fn facts(
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
    crumbs.extend(parent.map(|p| p.key.clone()));
    crumbs.push(task.key.clone());
    let project = match (&project_key, parent) {
        (Some(k), Some(p)) => format!("{k} › {} {}", p.key, p.title),
        (Some(k), None) => k.clone(),
        (None, Some(p)) => format!("{} {}", p.key, p.title),
        (None, None) => String::new(),
    };

    let mut subtasks: Vec<&Task> = input.tasks.iter().filter(|t| t.parent_id == Some(task.id)).collect();
    subtasks.sort_by_key(|t| (key_number(&t.key), t.key.clone()));
    let subtasks = subtasks
        .into_iter()
        .map(|t| Subtask { id: t.id.to_string(), key: t.key.clone(), title: t.title.clone(), status: t.status })
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
                links.push(format!("{what} {}", keys.join(", ")));
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
            }
        })
        .collect();

    TaskFacts {
        breadcrumb: crumbs.join(" › "),
        title: task.title.clone(),
        description: task.description.clone(),
        project,
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
fn key_number(key: &str) -> i64 {
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

/// Where a control's command stands. Lives on the control's own entity, which outlives redraws.
#[derive(Component, Debug, Clone, PartialEq, Eq, Default)]
pub enum Control {
    #[default]
    Idle,
    Busy,
    Done,
    Refused(String),
}

/// One persistent entity per subject and action, so the answer finds its control however often the panel is redrawn,
/// and one subject's result never shows on another's.
#[derive(Resource, Default)]
struct Controls(HashMap<(Subject, Action), Entity>);

/// The id of the event that last changed each task, for undo.
#[derive(Resource, Default, Debug)]
pub struct Latest(pub HashMap<String, i64>);

#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct InspectorState(pub Option<InspectorView>);

fn track_events(mut items: MessageReader<SyncItem>, mut latest: ResMut<Latest>) {
    for item in items.read() {
        let SyncItem::Event(e) = item else { continue };
        let Some(id) = e["id"].as_i64() else { continue };
        let task = match e["entity"].as_str() {
            Some("task") => e["entity_id"].as_str(),
            Some("comment" | "claim") => e["after"]["task_id"].as_str().or_else(|| e["before"]["task_id"].as_str()),
            _ => None,
        };
        if let Some(task) = task {
            latest.0.insert(task.to_string(), id);
        }
    }
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn rebuild(
    selection: Res<Selection>,
    latest: Res<Latest>,
    mirror: Res<Mirror>,
    changed: Query<(), Changed<Rev>>,
    mut removed: RemovedComponents<Rev>,
    projects: Query<&Mirrored<Project>>,
    tasks: Query<&Mirrored<Task>>,
    claims: Query<&Mirrored<Claim>>,
    comments: Query<&Mirrored<Comment>>,
    (agents, users): (Query<&Mirrored<Agent>>, Query<&Mirrored<User>>),
    detail: Option<Res<Detail>>,
    api: Option<Res<Api>>,
    mut state: ResMut<InspectorState>,
) {
    let gone = removed.read().count() > 0;
    let fetched = detail.as_ref().is_some_and(|d| d.is_changed());
    if !(selection.is_changed() || latest.is_changed() || mirror.is_changed() || fetched || gone || !changed.is_empty())
    {
        return;
    }
    let next = selection.0.as_ref().and_then(|subject| {
        let (projects, tasks, claims, comments, agents, users) =
            (rows(&projects), rows(&tasks), rows(&claims), rows(&comments), rows(&agents), rows(&users));
        let input = Input {
            projects: &projects,
            tasks: &tasks,
            claims: &claims,
            comments: &comments,
            agents: &agents,
            users: &users,
            now: jiff::Timestamp::now(),
            ..Input::default()
        };
        let me = api.as_ref().map(|a| (a.0.user_id.as_str(), a.0.user.as_str()));
        let fetched = match &detail {
            Some(d) => Fetched { detail: d.data.as_ref(), error: d.error.as_deref(), me },
            None => Fetched { me, ..Fetched::default() },
        };
        build(input, &latest.0, subject, fetched)
    });
    if state.0 != next {
        state.0 = next;
    }
}

fn settle(mut done: MessageReader<CommandDone>, mut controls: Query<&mut Control>) {
    for d in done.read() {
        if let Ok(mut c) = controls.get_mut(d.source) {
            *c = describe(&d.outcome).map_or(Control::Done, Control::Refused);
        }
    }
}

/// Keeps the [`Selection`] and the panel's contents from the mirror.
pub struct InspectorPlugin;

impl Plugin for InspectorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Selection>()
            .init_resource::<Neighbours>()
            .init_resource::<RevertWindow>()
            .init_resource::<Latest>()
            .init_resource::<InspectorState>()
            // Registering a message twice is a no-op, so these are safe beside the mirror's and commands' own.
            .add_message::<SyncItem>()
            .add_message::<CommandDone>()
            .add_message::<Submit>()
            .add_message::<Refetch>()
            .add_observer(on_pick)
            .add_observer(on_close_request)
            .add_observer(on_escape)
            .add_systems(Update, (track_events, rebuild, settle).chain());
    }
}

/// Draws the inspector over whichever view is showing: it is not tied to a [`crate::board::View`].
pub struct InspectorUiPlugin;

impl Plugin for InspectorUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(InspectorPlugin)
            .init_resource::<Controls>()
            .init_resource::<FactoryStatus>()
            .init_resource::<ModalUi>()
            .init_resource::<Compose>()
            .init_resource::<InputFocus>()
            .add_observer(on_modal_closed)
            .add_observer(on_open_task)
            .add_observer(on_open_composer)
            .add_observer(on_compose_press)
            .add_observer(on_step)
            .add_observer(on_step_key)
            .add_observer(on_menu_item)
            .add_observer(on_inline_edited)
            .add_observer(on_inline_chosen)
            .add_systems(Startup, spawn_panel)
            .add_systems(Update, answer_edits)
            // After the view is rebuilt: a modal closed from inside must not be reopened from the view it just left.
            .add_systems(
                Update,
                (follow_compose, sync_modal, sync_panel, sync_tools, dress, focus_composer, clear_sent)
                    .chain()
                    .after(rebuild),
            );
    }
}

#[derive(Component)]
struct InspectorPanel;

/// The panel's fixed header: the title and the close button, above the scrolling body.
#[derive(Component)]
struct InspectorHeader;

/// The panel's scrolling body, which lives as long as the panel so its scroll position survives redraws.
#[derive(Component)]
struct InspectorBody;

/// The dropdown that moves a task to another status: `statuses[i]` is what its option `i` means, the task's own
/// status first. Its answer shows beside it on `source`.
#[derive(Component, Debug, Clone)]
struct StatusMove {
    source: Entity,
    subject: Subject,
    statuses: Vec<Status>,
}

/// The dropdown that sets the task's grade: option `i` is `GRADES[i]`. Its answer shows beside it on `source`.
#[derive(Component, Debug, Clone)]
struct GradeChoice {
    source: Entity,
    subject: Subject,
}

/// The dropdown that sets how far back "revert" goes.
#[derive(Component, Debug, Clone, Copy, Default)]
struct WindowChoice;

fn spawn_panel(mut commands: Commands) {
    let panel = commands
        .spawn_scene(bsn! {
            @panel()
            Node {
                position_type: PositionType::Absolute,
                right: px(0),
                top: px(0),
                width: { px(extent::INSPECTOR_WIDTH) },
                height: percent(100),
                display: Display::None,
            }
            // Above the board, which fills the window and is spawned later, so it would take every click here; below
            // the form that "answer" opens.
            GlobalZIndex({ layer::INSPECTOR })
        })
        .insert((Name::new("inspector"), InspectorPanel))
        .id();
    // The header stays put; the body under it scrolls, held to the window's height by the panel and its frame.
    commands
        .spawn(Node { flex_direction: FlexDirection::Column, flex_shrink: 0.0, ..default() })
        .insert((InspectorHeader, ChildOf(panel)));
    let frame = commands.spawn(scroll_frame()).insert(ChildOf(panel)).id();
    let body = commands.spawn((scroll_body(), InspectorBody)).insert(ChildOf(frame)).id();
    commands.queue_spawn_scene(scrollbar(body, ControlOrientation::Vertical)).insert(ChildOf(frame));
}

/// The X in the panel's header (or anything else inside it) asked to close it.
fn on_close_request(
    close: On<RequestClose>,
    panels: Query<(), With<InspectorPanel>>,
    mut selection: ResMut<Selection>,
) {
    if panels.contains(close.event_target()) {
        selection.0 = None;
    }
}

/// Escape closes the inspector, unless a modal is open (its own Escape closes it, and only it) or the press came from
/// a text input (whose Escape just leaves the field).
fn on_escape(
    key: On<FocusedInput<KeyboardInput>>,
    modals: Query<(), With<ModalRoot>>,
    inputs: Query<(), With<EditableText>>,
    panels: Query<Entity, With<InspectorPanel>>,
    mut commands: Commands,
) {
    // The press bubbles up from the focused entity; act once, where it starts.
    if key.event_target() != key.original_event_target() {
        return;
    }
    let pressed = &key.input;
    if pressed.key_code != KeyCode::Escape || !pressed.state.is_pressed() || pressed.repeat {
        return;
    }
    if !modals.is_empty() || inputs.contains(key.original_event_target()) {
        return;
    }
    for panel in &panels {
        commands.trigger(RequestClose { source: panel });
    }
}

/// What a dropdown of the panel picked: a status to move to, how far back "revert" goes, or the comment box's kind.
#[allow(clippy::too_many_arguments)]
fn on_pick(
    picked: On<Picked>,
    moves: Query<&StatusMove>,
    grades: Query<&GradeChoice>,
    windows: Query<(), With<WindowChoice>>,
    kinds: Query<&ComposePart>,
    mut window: ResMut<RevertWindow>,
    compose: Option<ResMut<Compose>>,
    mut commands: Commands,
) {
    if kinds.get(picked.entity) == Ok(&ComposePart::Kind) {
        if let (Some(mut compose), Some(kind)) = (compose, COMPOSE_KINDS.get(picked.index)) {
            compose.kind = *kind;
        }
    } else if windows.contains(picked.entity) {
        if let Some(hours) = WINDOWS.get(picked.index) {
            window.0 = *hours;
        }
    } else if let Ok(GradeChoice { source, subject }) = grades.get(picked.entity) {
        if let Some(grade) = grade_choices().nth(picked.index) {
            let (source, subject, action) = (*source, subject.clone(), Action::Grade(grade));
            commands.queue(move |world: &mut World| send(world, source, &subject, &action));
        }
    } else if let Ok(StatusMove { source, subject, statuses }) = moves.get(picked.entity)
        && picked.index > 0
        && let Some(status) = statuses.get(picked.index)
    {
        let (source, subject, action) = (*source, subject.clone(), Action::Status(*status));
        commands.queue(move |world: &mut World| send(world, source, &subject, &action));
    }
}

/// Builds the request when the button is pressed, from the mirror as it is then: the version for `If-Match`, the
/// event to undo and the `since` of a revert must not be those of the last redraw.
fn send(world: &mut World, source: Entity, subject: &Subject, action: &Action) {
    fn all<T: Clone + Send + Sync + 'static>(world: &mut World) -> Vec<T> {
        world.query::<&Mirrored<T>>().iter(world).map(|m| m.0.clone()).collect()
    }
    let (projects, tasks, claims, comments, agents) = (all(world), all(world), all(world), all(world), all(world));
    let input = Input {
        projects: &projects,
        tasks: &tasks,
        claims: &claims,
        comments: &comments,
        agents: &agents,
        ..Input::default()
    };
    // The side panel's revert is one control however far back it goes (`Revert(0)`); its window is read as the button
    // is pressed. The modal's menu names its hours.
    let action = &match action {
        Action::Revert(0) => Action::Revert(world.resource::<RevertWindow>().0),
        other => other.clone(),
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let request = plan(action, input, &world.resource::<Latest>().0, subject, now);
    let outcome = match request {
        Some(request) => {
            world.resource_mut::<Messages<Submit>>().write(Submit { source, request });
            Control::Busy
        }
        None => Control::Refused("no longer possible: the task changed".into()),
    };
    if let Some(mut c) = world.get_mut::<Control>(source) {
        *c = outcome;
    }
}

/// Sends `action` for `subject` when the button it is on is pressed.
fn sends(source: Entity, subject: Subject, action: Action) -> impl Scene {
    bsn! {
        on(move |_: On<Activate>, mut commands: Commands| {
            let (subject, action) = (subject.clone(), action.clone());
            commands.queue(move |world: &mut World| send(world, source, &subject, &action));
        })
    }
}

/// The control for `action` on `subject`, made the first time it is asked for.
fn control(controls: &mut Controls, commands: &mut Commands, subject: &Subject, action: &Action) -> Entity {
    *controls.0.entry((subject.clone(), action.clone())).or_insert_with(|| commands.spawn(Control::Idle).id())
}

/// The status dropdown's options, read as words; its [`StatusMove`] keeps the API values it sends.
fn status_labels(statuses: &[Status]) -> Vec<String> {
    statuses.iter().map(|s| status_label(*s).to_string()).collect()
}

/// The key of the one control every status move shares, whichever status it moves to.
const MOVE_CONTROL: Action = Action::Status(Status::Open);

/// Whether a line of [`InspectorView::lines`] is about the run rather than the task's status.
fn is_run_line(line: &str) -> bool {
    line.starts_with("run:")
}

/// How a [`Block::Text`] is colored; fixed for the item that holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    Body,
    Dim,
    Warn,
}

/// One thing in a section of the panel. Items are keyed (`status/line/0`, `q/<comment id>`, `act/Release`), so a
/// change in the task changes the entities of what changed and nothing else.
#[derive(Debug, Clone, PartialEq)]
enum Block {
    /// A line of text; only the words change while the key stays.
    Text { text: String, size: f32, tone: Tone },
    /// The `P1` badge. Its key holds the priority, so a new priority is a new badge.
    Priority(Priority),
    /// The wrapping row of a PR's gates, which are [`Gate`]s keyed under it.
    Gates,
    /// An open question, with its answer and resolve buttons and what became of the last resolve.
    Question { key: String, id: String, body: String, live: bool, note: Option<Control> },
    /// The grade dropdown (live only); the offline grade is a [`Block::Text`].
    Grade { grade: Option<Grade>, note: Option<Control> },
    /// The status dropdown: the task's own status first.
    Move { statuses: Vec<Status>, note: Option<Control> },
    /// A button for `action` (a revert has its window's dropdown, `window` hours).
    Act { action: Action, window: Option<i64>, note: Option<Control> },
    /// A subtask's row, which opens it. Its key holds what it shows, so a change is a new row.
    Subtask(Subtask),
    /// A comment in the activity. An open question has answer and resolve buttons; its key holds whether it is open.
    Entry { key: String, entry: Entry, live: bool, note: Option<Control> },
    /// The comment box at the foot of the activity, spawned once per task so what is typed survives redraws.
    Composer { answering: Option<String>, live: bool, note: Option<Control> },
    /// The title or the description, edited where it is read. Its key doesn't hold the words, so an edit in progress
    /// survives a change from elsewhere.
    Inline { field: Field, value: String },
    /// A sidebar field picked from a dropdown where it is read: `options[i]` is shown for what `statuses[i]` (status)
    /// or the field's scale (priority, grade) means.
    Choice { field: Field, options: Vec<String>, selected: Option<usize>, statuses: Vec<Status> },
    /// The button that opens the new-task form under this task.
    AddSubtask { key: String, live: bool },
}

/// A field of a task the modal edits in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    Title,
    Description,
    Status,
    Priority,
    Grade,
}

impl Field {
    fn label(self) -> &'static str {
        match self {
            Field::Title => "Title",
            Field::Description => "Description",
            Field::Status => "Status",
            Field::Priority => "Priority",
            Field::Grade => "Grade",
        }
    }
}

/// What changed in an edit in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Text(String),
    /// The option picked.
    Pick(usize),
}

/// On an inline widget of the modal: which field it edits and, while a save is out, what to go back to.
#[derive(Component, Debug, Clone)]
struct Edits {
    field: Field,
    /// What a status option means, for [`Field::Status`].
    statuses: Vec<Status>,
    pending: Option<Pending>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Pending {
    Text { old: String, new: String },
    Pick { old: Option<usize> },
}

/// The PATCH an edit in place sends: only the field that changed, on the version the modal shows. `statuses` says what
/// a status option means.
pub fn edit_request(
    key: &str,
    version: i64,
    field: Field,
    change: &Change,
    statuses: &[Status],
) -> Result<Request, String> {
    let body = match (field, change) {
        (Field::Title, Change::Text(t)) if t.trim().is_empty() => return Err("a task needs a title".into()),
        (Field::Title, Change::Text(t)) => json!({"title": t.trim()}),
        (Field::Description, Change::Text(t)) => json!({"description": t}),
        (Field::Status, Change::Pick(i)) => json!({"status": statuses.get(*i).ok_or("no such status")?}),
        (Field::Priority, Change::Pick(i)) => json!({"priority": Priority::ALL.get(*i).ok_or("no such priority")?}),
        (Field::Grade, Change::Pick(i)) => json!({"grade": grade_choices().nth(*i).ok_or("no such grade")?}),
        _ => return Err("that field isn't edited that way".into()),
    };
    Ok(Request::new(Method::PATCH, format!("/v1/tasks/{key}"), Some(body), Some(version)))
}

/// One of a PR's gates: its key holds the state, so a gate that changes state is a new badge.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Gate {
    name: String,
    state: GateState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateState {
    Passed,
    Waiting,
    Failing,
}

/// A block on its entity, which [`dress`] brings in line with it when it changes.
#[derive(Component, Debug, Clone)]
struct Shown(Block);

/// The parts of a block's entity that are set in place.
#[derive(Component, Debug, Clone, Copy, Default)]
struct Parts {
    text: Option<Entity>,
    answer: Option<Entity>,
    resolve: Option<Entity>,
    select: Option<Entity>,
    note: Option<Entity>,
}

type Plan = Vec<(String, Vec<(String, Block)>)>;

fn text_block(text: impl Into<String>, size: f32, tone: Tone) -> Block {
    Block::Text { text: text.into(), size, tone }
}

/// The panel's sections and their items, in order. `note` is where a control's command stands, by action.
fn layout(
    view: &InspectorView,
    live: bool,
    offline: &str,
    window: i64,
    note: &dyn Fn(&Action) -> Option<Control>,
) -> Plan {
    let mut plan: Plan = Vec::new();
    let mut section = |key: &str, heading: &str, mut items: Vec<(String, Block)>| {
        if !heading.is_empty() {
            items.insert(0, (format!("{key}/heading"), text_block(heading, size::SECONDARY, Tone::Dim)));
        }
        plan.push((key.to_string(), items));
    };

    let (run, status): (Vec<&String>, Vec<&String>) = view.lines.iter().partition(|l| is_run_line(l));
    for (key, heading, lines) in [("status", "Status", status), ("run", "Run", run)] {
        if lines.is_empty() {
            continue;
        }
        let mut items = Vec::new();
        if let (true, Some(p)) = (key == "status", view.priority) {
            items.push((format!("{key}/priority/{p}"), Block::Priority(p)));
        }
        items.extend(
            lines
                .into_iter()
                .enumerate()
                .map(|(i, l)| (format!("{key}/line/{i}"), text_block(l, size::BODY, Tone::Body))),
        );
        if key == "status" && view.status.is_some() {
            if live {
                let note = note(&Action::Grade(None));
                items.push(("status/grade".to_string(), Block::Grade { grade: view.grade, note }));
            } else {
                let grade = view.grade.map_or_else(|| "none".to_string(), |g| g.to_string());
                items.push((
                    "status/grade/off".to_string(),
                    text_block(format!("grade: {grade}"), size::BODY, Tone::Body),
                ));
            }
        }
        section(key, heading, items);
    }

    if !view.questions.is_empty() {
        let items = view
            .questions
            .iter()
            .map(|q| {
                let block = Block::Question {
                    key: q.key.clone(),
                    id: q.id.clone(),
                    body: q.body.clone(),
                    live,
                    note: note(&Action::Resolve(q.id.clone())),
                };
                (format!("q/{}", q.id), block)
            })
            .collect();
        section("questions", "Questions", items);
    }

    if view.pr.is_some() || view.merge_note.is_some() {
        let mut items = Vec::new();
        if let Some(pr) = &view.pr {
            items
                .push(("pr/url".to_string(), text_block(format!("#{} {}", pr.number, pr.url), size::BODY, Tone::Body)));
            let grade = pr.grade.map_or(String::new(), |g| format!(", grade {g}"));
            items
                .push(("pr/state".to_string(), text_block(format!("{}{grade}", pr.state), size::SECONDARY, Tone::Dim)));
            if !gates(pr).is_empty() {
                items.push(("pr/gates".to_string(), Block::Gates));
            }
            if let Some(problem) = &pr.problem {
                items.push(("pr/problem".to_string(), text_block(problem, size::BODY, Tone::Warn)));
            }
        }
        if let Some(merge_note) = &view.merge_note {
            items.push(("pr/note".to_string(), text_block(merge_note, size::SECONDARY, Tone::Dim)));
        }
        section("pr", "Pull request", items);
    }

    if !live {
        let items = vec![("offline/text".to_string(), text_block(offline, size::BODY, Tone::Warn))];
        section("offline", "", items);
        return plan;
    }

    let mut items = Vec::new();
    // One dropdown for every status the task can move to, the current one showing.
    let moves: Vec<Status> =
        view.actions.iter().filter_map(|a| if let Action::Status(st) = a { Some(*st) } else { None }).collect();
    if let (Some(current), false) = (view.status, moves.is_empty()) {
        let statuses = std::iter::once(current).chain(moves).collect();
        items.push(("move".to_string(), Block::Move { statuses, note: note(&MOVE_CONTROL) }));
    }
    for action in &view.actions {
        let action = match action {
            Action::Status(_) | Action::Resolve(_) => continue,
            // One control however far back, so the answer finds it when the window changes.
            Action::Revert(_) => Action::Revert(0),
            other => other.clone(),
        };
        let window = matches!(action, Action::Revert(_)).then_some(window);
        let block = Block::Act { note: note(&action), window, action: action.clone() };
        items.push((format!("act/{action:?}"), block));
    }
    section("actions", "Actions", items);
    plan
}

/// The task modal's two columns, each a list of sections: the main column (title, description, subtasks, open
/// questions, the PR) and the sidebar (one fact per section, then the actions). Section keys are unique across both.
fn layout_task(
    view: &InspectorView,
    facts: &TaskFacts,
    live: bool,
    offline: &str,
    human: &[Grade],
    answering: Option<&str>,
    note: &dyn Fn(&Action) -> Option<Control>,
) -> (Plan, Plan) {
    fn add(plan: &mut Plan, key: &str, heading: &str, mut items: Vec<(String, Block)>) {
        if !heading.is_empty() {
            items.insert(0, (format!("{key}/heading"), text_block(heading, size::SECONDARY, Tone::Dim)));
        }
        plan.push((key.to_string(), items));
    }
    let lines = |key: &str, lines: &[String]| -> Vec<(String, Block)> {
        lines.iter().enumerate().map(|(i, l)| (format!("{key}/{i}"), text_block(l, size::BODY, Tone::Body))).collect()
    };

    let mut main: Plan = Vec::new();
    let key = key_of(view);
    let (title, description) = if live {
        (
            ("m/title/edit".to_string(), Block::Inline { field: Field::Title, value: facts.title.clone() }),
            (
                "m/description/edit".to_string(),
                Block::Inline { field: Field::Description, value: facts.description.clone() },
            ),
        )
    } else {
        let description = if facts.description.is_empty() {
            text_block("No description", size::BODY, Tone::Dim)
        } else {
            text_block(&facts.description, size::BODY, Tone::Body)
        };
        (
            ("m/title/text".to_string(), text_block(&facts.title, size::DETAIL_TITLE, Tone::Body)),
            ("m/description/text".to_string(), description),
        )
    };
    add(&mut main, "m/title", "", vec![title]);
    add(&mut main, "m/description", "", vec![description]);
    let mut rows: Vec<(String, Block)> = facts
        .subtasks
        .iter()
        .map(|t| (format!("m/sub/{}/{:?}/{}", t.id, t.status, t.title), Block::Subtask(t.clone())))
        .collect();
    rows.push(("m/sub/add".into(), Block::AddSubtask { key: key.clone(), live }));
    add(&mut main, "m/subtasks", "Sub-tasks", rows);
    if view.pr.is_some() || view.merge_note.is_some() {
        let mut items = Vec::new();
        if let Some(pr) = &view.pr {
            items
                .push(("pr/url".to_string(), text_block(format!("#{} {}", pr.number, pr.url), size::BODY, Tone::Body)));
            let grade = pr.grade.map_or(String::new(), |g| format!(", grade {g}"));
            items
                .push(("pr/state".to_string(), text_block(format!("{}{grade}", pr.state), size::SECONDARY, Tone::Dim)));
            if !gates(pr).is_empty() {
                items.push(("pr/gates".to_string(), Block::Gates));
            }
            if let Some(problem) = &pr.problem {
                items.push(("pr/problem".to_string(), text_block(problem, size::BODY, Tone::Warn)));
            }
        }
        if let Some(merge_note) = &view.merge_note {
            items.push(("pr/note".to_string(), text_block(merge_note, size::SECONDARY, Tone::Dim)));
        }
        add(&mut main, "m/pr", "Pull request", items);
    }
    let mut activity: Vec<(String, Block)> = facts
        .activity
        .iter()
        .map(|e| {
            let note = if e.open_question { note(&Action::Resolve(e.id.clone())) } else { None };
            let block = Block::Entry { key: key.clone(), entry: e.clone(), live, note };
            (format!("a/{}/{}", e.id, e.open_question), block)
        })
        .collect();
    if activity.is_empty() {
        activity.push(("a/none".into(), text_block("No comments yet", size::BODY, Tone::Dim)));
    }
    activity.push((
        "a/compose".into(),
        Block::Composer { answering: answering.map(str::to_string), live, note: note(&Action::Comment) },
    ));
    add(&mut main, "m/activity", "Activity", activity);
    if let Some(why) = &facts.fetch_error {
        let words = format!("Couldn't load the whole task: {why}");
        add(&mut main, "m/fetch", "", vec![("m/fetch/text".into(), text_block(words, size::SECONDARY, Tone::Warn))]);
    }
    if !live {
        add(&mut main, "m/offline", "", vec![("m/offline/text".into(), text_block(offline, size::BODY, Tone::Warn))]);
    }

    let mut side: Plan = Vec::new();
    if !facts.project.is_empty() {
        add(&mut side, "s/project", "Project", lines("s/project", std::slice::from_ref(&facts.project)));
    }
    let status = view.status.unwrap_or(Status::Open);
    let mut fields = Vec::new();
    if live {
        let moves = view.actions.iter().filter_map(|a| if let Action::Status(st) = a { Some(*st) } else { None });
        let statuses: Vec<Status> = std::iter::once(status).chain(moves).collect();
        let options = status_labels(&statuses);
        fields.push((
            "s/fields/status".to_string(),
            Block::Choice { field: Field::Status, options, selected: Some(0), statuses },
        ));
        let priorities = Priority::ALL.iter().map(|p| format!("P{p}")).collect();
        let selected = view.priority.and_then(|p| Priority::ALL.iter().position(|q| *q == p));
        fields.push((
            "s/fields/priority".to_string(),
            Block::Choice { field: Field::Priority, options: priorities, selected, statuses: Vec::new() },
        ));
        fields.push((
            "s/fields/grade".to_string(),
            Block::Choice {
                field: Field::Grade,
                options: grade_options(human),
                selected: grade_index(view.grade),
                statuses: Vec::new(),
            },
        ));
    } else {
        fields.push((
            "s/fields/status/text".to_string(),
            text_block(format!("Status: {}", status_label(status)), size::BODY, Tone::Body),
        ));
        if let Some(p) = view.priority {
            fields.push((format!("s/fields/priority/{p}"), Block::Priority(p)));
        }
        let words = view.grade.map_or_else(|| "none".to_string(), |g| format!("G{g}"));
        fields.push(("s/fields/grade/text".to_string(), text_block(format!("Grade: {words}"), size::BODY, Tone::Body)));
    }
    if let Some(g) = view.grade {
        let words = if human.contains(&g) {
            "Above Treeline: a person merges it"
        } else {
            "Below Treeline: merges once its gates pass"
        };
        fields.push(("s/fields/treeline".to_string(), text_block(words, size::SECONDARY, Tone::Dim)));
    }
    add(&mut side, "s/fields", "", fields);
    let claim = match &facts.claim {
        Some(line) => vec![("s/claim/text".to_string(), text_block(line, size::BODY, Tone::Body))],
        None => vec![("s/claim/none".to_string(), text_block("Nobody holds it", size::BODY, Tone::Dim))],
    };
    add(&mut side, "s/claim", "Claim", claim);
    for (key, heading, rows) in [
        ("s/dates", "Dates", &facts.dates),
        ("s/links", "Dependencies", &facts.links),
        ("s/run", "Factory run", &facts.run),
        ("s/details", "Details", &facts.details),
    ] {
        if !rows.is_empty() {
            add(&mut side, key, heading, lines(key, rows));
        }
    }
    (main, side)
}

/// The task's key, the last part of the view's breadcrumb.
fn key_of(view: &InspectorView) -> String {
    view.task.as_ref().and_then(|f| f.breadcrumb.rsplit(" › ").next()).unwrap_or_default().to_string()
}

/// How many lines the comment box's input shows.
const COMPOSE_LINES: f32 = 4.0;

/// The kinds the comment box can send, in the order of its dropdown.
const COMPOSE_KINDS: [CommentKind; 4] =
    [CommentKind::Note, CommentKind::Handoff, CommentKind::Decision, CommentKind::Question];

/// A comment kind in words, as the activity and the comment box show it.
fn kind_label(kind: CommentKind) -> &'static str {
    match kind {
        CommentKind::Note => "Note",
        CommentKind::Handoff => "Handoff",
        CommentKind::Decision => "Decision",
        CommentKind::Question => "Question",
        CommentKind::Progress => "Progress",
        CommentKind::Review => "Review",
    }
}

/// The comment box's state: what it sends next, and whether it answers a question.
#[derive(Resource, Debug)]
pub struct Compose {
    /// The task the box belongs to; another task starts over.
    pub subject: Option<Subject>,
    pub kind: CommentKind,
    /// The question (its id and body) the box answers, if it does.
    pub answering: Option<(String, String)>,
    /// The box's input should take the keyboard once it exists.
    pub focus: bool,
}

impl Default for Compose {
    fn default() -> Self {
        Compose { subject: None, kind: CommentKind::Note, answering: None, focus: false }
    }
}

/// Opens a task in the modal with the keyboard in its comment box, answering a question if there is one. The board's
/// comment and answer buttons send it, and so does a question's answer button in the modal.
#[derive(Event, Debug, Clone, PartialEq, Eq)]
pub struct OpenComposer {
    pub key: String,
    /// The question's id and body.
    pub answer: Option<(String, String)>,
}

/// On the comment box's parts.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Default)]
enum ComposePart {
    #[default]
    Input,
    Kind,
    Send,
    Answering,
    StopAnswering,
}

/// A PR's gates, in the order the panel shows them.
fn gates(pr: &PrPanel) -> Vec<(String, Gate)> {
    [
        (&pr.passed, GateState::Passed, "passed"),
        (&pr.waiting, GateState::Waiting, "waiting"),
        (&pr.failing, GateState::Failing, "failing"),
    ]
    .into_iter()
    .flat_map(|(names, state, word)| {
        names.iter().map(move |name| (format!("{word}/{name}"), Gate { name: name.clone(), state }))
    })
    .collect()
}

fn window_labels() -> Vec<String> {
    WINDOWS.iter().map(|h| format!("{h}h ago")).collect()
}

/// The words beside a control for where its command stands, and their color; no words for none.
fn note_look(note: &Option<Control>) -> (String, ThemeToken) {
    match note {
        Some(Control::Busy) => ("sending…".into(), tokens::TEXT_DIM),
        // The command went through: not the task's status, so not `status_label`'s.
        Some(Control::Done) => (String::from("done"), tokens::TEXT_DIM),
        Some(Control::Refused(why)) => (why.clone(), token::DANGER),
        _ => (String::new(), tokens::TEXT_DIM),
    }
}

fn spawn_note(commands: &mut Commands, parent: Entity, note: &Option<Control>) -> Entity {
    let (words, tone) = note_look(note);
    let display = if words.is_empty() { Display::None } else { Display::Flex };
    commands
        .spawn_scene(bsn! { @text_toned(words, size::SECONDARY, tone) Node { display: { display } } })
        .insert(ChildOf(parent))
        .id()
}

/// Disables a button while the connection is not live.
fn gate_button(commands: &mut Commands, button: Entity, live: bool) {
    if live {
        commands.entity(button).remove::<InteractionDisabled>();
    } else {
        commands.entity(button).insert(InteractionDisabled);
    }
}

/// Makes the entity for a new block, not yet parented.
fn spawn_block(
    commands: &mut Commands,
    controls: &mut Controls,
    subject: &Subject,
    block: &Block,
    human: &[Grade],
) -> Entity {
    let item = match block {
        Block::Text { text: words, size, tone } => {
            let scene: Box<dyn Scene> = match tone {
                Tone::Body => Box::new(text(words.clone(), *size)),
                Tone::Dim => Box::new(text_dim(words.clone(), *size)),
                Tone::Warn => Box::new(text_toned(words.clone(), *size, token::WARN)),
            };
            commands.spawn_scene(scene).id()
        }
        Block::Priority(p) => commands.spawn_scene(badge(format!("P{p}"), None, Some(priority_tone(*p)))).id(),
        Block::Gates => commands.spawn_scene(bsn! { @row() Node { flex_wrap: FlexWrap::Wrap } }).id(),
        Block::Question { key, id, body, live, note } => {
            let item = commands
                .spawn(Node { flex_direction: FlexDirection::Column, row_gap: px(space::ROWS), ..default() })
                .id();
            let line = commands.spawn_scene(bsn! { @row() }).insert(ChildOf(item)).id();
            let words = commands
                .spawn_scene(
                    bsn! { @text_toned(format!("? {body}"), size::BODY, token::WARN) Node { flex_grow: 1.0, flex_shrink: 1.0, min_width: px(0) } },
                )
                .insert(ChildOf(line))
                .id();
            // The same form the board opens; it saves even after the inspector moves on.
            let act = Act::Answer { key: key.clone(), comment_id: id.clone(), question: body.clone() };
            let answer = commands
                .spawn_scene(bsn! { @tool_button(Icon::Comment, "Answer", ButtonVariant::Normal) })
                .insert((act, ChildOf(line)))
                .id();
            let resolve_action = Action::Resolve(id.clone());
            let source = control(controls, commands, subject, &resolve_action);
            let resolve = commands
                .spawn_scene(bsn! {
                    @tool_button(Icon::Check, "Resolve without an answer", ButtonVariant::Normal)
                    @sends(source, subject.clone(), resolve_action.clone())
                })
                .insert(ChildOf(line))
                .id();
            gate_button(commands, answer, *live);
            gate_button(commands, resolve, *live);
            let note = spawn_note(commands, item, note);
            commands.entity(item).insert(Parts {
                text: Some(words),
                answer: Some(answer),
                resolve: Some(resolve),
                note: Some(note),
                ..default()
            });
            item
        }
        Block::Grade { grade, note } => {
            let line = commands.spawn_scene(bsn! { @row() }).id();
            commands.spawn_scene(text_dim("Grade", size::SECONDARY)).insert(ChildOf(line));
            let source = control(controls, commands, subject, &Action::Grade(None));
            let options = grade_options(human);
            let max_visible = options.len();
            let picker = commands
                .spawn_scene(select(options, grade_index(*grade), max_visible))
                .insert((GradeChoice { source, subject: subject.clone() }, ChildOf(line)))
                .id();
            let note = spawn_note(commands, line, note);
            commands.entity(line).insert(Parts { select: Some(picker), note: Some(note), ..default() });
            line
        }
        Block::Move { statuses, note } => {
            let line = commands.spawn_scene(bsn! { @row() }).id();
            commands.spawn_scene(text_dim("Move to", size::SECONDARY)).insert(ChildOf(line));
            let source = control(controls, commands, subject, &MOVE_CONTROL);
            let max_visible = statuses.len();
            let picker = commands
                .spawn_scene(select(status_labels(statuses), Some(0), max_visible))
                .insert((StatusMove { source, subject: subject.clone(), statuses: statuses.clone() }, ChildOf(line)))
                .id();
            let note = spawn_note(commands, line, note);
            commands.entity(line).insert(Parts { select: Some(picker), note: Some(note), ..default() });
            line
        }
        Block::Entry { key, entry, live, note } => {
            let item = commands
                .spawn(Node { flex_direction: FlexDirection::Column, row_gap: px(space::STACKED), ..default() })
                .id();
            let mut head = vec![kind_label(entry.kind).to_string(), entry.author.clone(), entry.when.clone()];
            head.extend(entry.tags.iter().cloned());
            commands.spawn_scene(text_dim(head.join(" · "), size::SECONDARY)).insert(ChildOf(item));
            let tone = if entry.open_question { token::WARN } else { tokens::TEXT_MAIN };
            commands.spawn_scene(text_toned(entry.body.clone(), size::BODY, tone)).insert(ChildOf(item));
            if entry.open_question {
                let line = commands.spawn_scene(bsn! { @row() }).insert(ChildOf(item)).id();
                let act = Act::Answer { key: key.clone(), comment_id: entry.id.clone(), question: entry.body.clone() };
                let answer = commands
                    .spawn_scene(bsn! { @tool_button(Icon::Comment, "Answer", ButtonVariant::Normal) })
                    .insert((act, ChildOf(line)))
                    .id();
                let resolve_action = Action::Resolve(entry.id.clone());
                let source = control(controls, commands, subject, &resolve_action);
                let resolve = commands
                    .spawn_scene(bsn! {
                        @tool_button(Icon::Check, "Resolve without an answer", ButtonVariant::Normal)
                        @sends(source, subject.clone(), resolve_action.clone())
                    })
                    .insert(ChildOf(line))
                    .id();
                gate_button(commands, answer, *live);
                gate_button(commands, resolve, *live);
                let note = spawn_note(commands, item, note);
                commands.entity(item).insert(Parts {
                    answer: Some(answer),
                    resolve: Some(resolve),
                    note: Some(note),
                    ..default()
                });
            }
            item
        }
        Block::Composer { answering, live, note } => {
            let item = commands
                .spawn(Node { flex_direction: FlexDirection::Column, row_gap: px(space::ROWS), ..default() })
                .id();
            let line = commands.spawn_scene(bsn! { @row() }).insert(ChildOf(item)).id();
            let words = answering.as_ref().map_or_else(String::new, |q| format!("Answering: {q}"));
            let shown = display(answering.is_some());
            let answering_text = commands
                .spawn_scene(bsn! {
                    @text_dim(words, size::SECONDARY)
                    Node { display: { shown }, flex_shrink: 1.0, min_width: px(0) }
                })
                .insert((ComposePart::Answering, ChildOf(line)))
                .id();
            let stop = commands
                .spawn_scene(bsn! { @tool_button(Icon::X, "Comment instead", ButtonVariant::Plain) })
                .insert((ComposePart::StopAnswering, ChildOf(line)))
                .id();
            let labels: Vec<String> = COMPOSE_KINDS.iter().map(|k| kind_label(*k).to_string()).collect();
            let kind = commands
                .spawn_scene(select(labels, Some(0), COMPOSE_KINDS.len()))
                .insert((ComposePart::Kind, ChildOf(line)))
                .id();
            commands.spawn_scene(text_input(COMPOSE_LINES, bsn! { ComposePart::Input })).insert(ChildOf(item));
            let foot = commands.spawn_scene(bsn! { @row() }).insert(ChildOf(item)).id();
            let send = commands
                .spawn_scene(bsn! { @button("Send", Some(Icon::Send), ButtonVariant::Primary) })
                .insert((ComposePart::Send, ChildOf(foot)))
                .id();
            gate_button(commands, send, *live);
            // The box's answer goes to its own control, which outlives redraws like every other.
            control(controls, commands, subject, &Action::Comment);
            let note = spawn_note(commands, foot, note);
            commands.entity(item).insert(Parts {
                text: Some(answering_text),
                answer: Some(stop),
                resolve: Some(send),
                select: Some(kind),
                note: Some(note),
            });
            // The answering line and the kind's dropdown take turns; `dress` sets which once the parts exist.
            item
        }
        Block::Inline { field, value } => {
            let (hint, lines, font) = match field {
                Field::Title => ("Add a title", 1.0, size::DETAIL_TITLE),
                _ => ("Add a description", DESCRIPTION_LINES, size::BODY),
            };
            commands
                .spawn_scene(inline_text_sized(value.clone(), hint, lines, font))
                .insert(Edits { field: *field, statuses: Vec::new(), pending: None })
                .id()
        }
        Block::Choice { field, options, selected, statuses } => commands
            .spawn_scene(inline_field(field.label(), options.clone(), *selected))
            .insert(Edits { field: *field, statuses: statuses.clone(), pending: None })
            .id(),
        Block::AddSubtask { key, live } => {
            let button = commands
                .spawn_scene(bsn! { @button("Add sub-task", Some(Icon::Plus), ButtonVariant::Normal) })
                .insert(Act::NewSubtask(key.clone()))
                .id();
            gate_button(commands, button, *live);
            button
        }
        Block::Subtask(sub) => {
            let line = commands.spawn_scene(bsn! { @row() }).id();
            let caption = format!("{}  {}", sub.key, sub.title);
            commands
                .spawn_scene(bsn! { @button(caption, None, ButtonVariant::Plain) })
                .insert((OpensTask(sub.id.clone()), ChildOf(line)));
            commands.spawn_scene(text_dim(status_label(sub.status), size::SECONDARY)).insert(ChildOf(line));
            line
        }
        Block::Act { action, window, note } => {
            let line = commands.spawn_scene(bsn! { @row() }).id();
            let source = control(controls, commands, subject, action);
            let press = sends(source, subject.clone(), action.clone());
            match action {
                Action::Release => commands
                    .spawn_scene(bsn! { @button(action.label(), Some(Icon::Release), ButtonVariant::Normal) @{press} }),
                Action::Undo => commands
                    .spawn_scene(bsn! { @button(action.label(), Some(Icon::Undo), ButtonVariant::Normal) @{press} }),
                Action::Cancel => {
                    commands.spawn_scene(bsn! { @danger_button(action.label(), Some(Icon::Stop)) @{press} })
                }
                _ => commands.spawn_scene(bsn! { @danger_button("revert since", Some(Icon::History)) @{press} }),
            }
            .insert(ChildOf(line));
            let mut parts = Parts::default();
            if let Some(hours) = window {
                let selected = WINDOWS.iter().position(|h| h == hours);
                parts.select = Some(
                    commands
                        .spawn_scene(select(window_labels(), selected, WINDOWS.len()))
                        .insert((WindowChoice, ChildOf(line)))
                        .id(),
                );
            }
            parts.note = Some(spawn_note(commands, line, note));
            commands.entity(line).insert(parts);
            line
        }
    };
    commands.entity(item).insert(Shown(block.clone()));
    item
}

/// Sets a note's words, color and whether it takes room.
fn dress_note(
    commands: &mut Commands,
    texts: &mut Query<&mut Text>,
    nodes: &mut Query<&mut Node>,
    note: Entity,
    state: &Option<Control>,
) {
    let (words, tone) = note_look(state);
    let display = if words.is_empty() { Display::None } else { Display::Flex };
    if let Ok(mut t) = texts.get_mut(note)
        && t.0 != words
    {
        t.0 = words;
    }
    if let Ok(mut node) = nodes.get_mut(note)
        && node.display != display
    {
        node.display = display;
    }
    commands.entity(note).insert(ThemeTextColor(tone));
}

/// Brings the parts of changed blocks in line with them: words, notes, disabled buttons, dropdown rows.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn dress(
    shown: Query<(&Shown, Option<&Parts>)>,
    every: Query<Entity, With<Shown>>,
    factory: Res<FactoryStatus>,
    changed: Query<Entity, Changed<Shown>>,
    mut texts: Query<&mut Text>,
    mut nodes: Query<&mut Node>,
    mut moves: Query<&mut StatusMove>,
    mut edit: SelectEdit,
    (mut inline, mut fields, mut edits): (Query<&mut InlineText>, Query<&mut InlineField>, Query<&mut Edits>),
    mut again: Local<Vec<Entity>>,
    mut commands: Commands,
) {
    let mut todo: Vec<Entity> = std::mem::take(&mut *again);
    todo.extend(changed.iter());
    if factory.is_changed() {
        todo.extend(every.iter());
    }
    todo.dedup();
    for entity in todo {
        let Ok((Shown(block), parts)) = shown.get(entity) else { continue };
        match block {
            // A value from elsewhere; the widget leaves it alone while it is being edited or saved.
            Block::Inline { value, .. } => {
                if let Ok(mut t) = inline.get_mut(entity)
                    && t.value() != value
                {
                    t.set_value(value);
                }
                continue;
            }
            Block::Choice { options, selected, statuses, .. } => {
                if let Ok(mut f) = fields.get_mut(entity) {
                    f.set(options.clone(), *selected);
                }
                if let Ok(mut e) = edits.get_mut(entity)
                    && e.statuses != *statuses
                {
                    e.statuses = statuses.clone();
                }
                continue;
            }
            Block::AddSubtask { live, .. } => {
                gate_button(&mut commands, entity, *live);
                continue;
            }
            _ => {}
        }
        if let Block::Text { text: words, .. } = block {
            if let Ok(mut t) = texts.get_mut(entity)
                && t.0 != *words
            {
                t.0 = words.clone();
            }
            continue;
        }
        let Some(parts) = parts else { continue };
        match block {
            Block::Question { key, id, body, live, note } => {
                let words = format!("? {body}");
                if let Some(Ok(mut t)) = parts.text.map(|e| texts.get_mut(e))
                    && t.0 != words
                {
                    t.0 = words;
                }
                for button in [parts.answer, parts.resolve].into_iter().flatten() {
                    gate_button(&mut commands, button, *live);
                }
                if let Some(answer) = parts.answer {
                    let act = Act::Answer { key: key.clone(), comment_id: id.clone(), question: body.clone() };
                    commands.entity(answer).insert(act);
                }
                if let Some(n) = parts.note {
                    dress_note(&mut commands, &mut texts, &mut nodes, n, note);
                }
            }
            Block::Grade { grade, note } => {
                if let Some(picker) = parts.select
                    && !edit.set(picker, &grade_options(&factory.human_grades()), grade_index(*grade))
                {
                    again.push(entity);
                }
                if let Some(n) = parts.note {
                    dress_note(&mut commands, &mut texts, &mut nodes, n, note);
                }
            }
            Block::Move { statuses, note } => {
                if let Some(picker) = parts.select {
                    if let Ok(mut m) = moves.get_mut(picker)
                        && m.statuses != *statuses
                    {
                        m.statuses = statuses.clone();
                    }
                    if !edit.set(picker, &status_labels(statuses), Some(0)) {
                        again.push(entity);
                    }
                }
                if let Some(n) = parts.note {
                    dress_note(&mut commands, &mut texts, &mut nodes, n, note);
                }
            }
            Block::Act { window, note, .. } => {
                if let (Some(picker), Some(hours)) = (parts.select, window)
                    && !edit.set(picker, &window_labels(), WINDOWS.iter().position(|h| h == hours))
                {
                    again.push(entity);
                }
                if let Some(n) = parts.note {
                    dress_note(&mut commands, &mut texts, &mut nodes, n, note);
                }
            }
            Block::Entry { live, note, .. } => {
                for button in [parts.answer, parts.resolve].into_iter().flatten() {
                    gate_button(&mut commands, button, *live);
                }
                if let Some(n) = parts.note {
                    dress_note(&mut commands, &mut texts, &mut nodes, n, note);
                }
            }
            Block::Composer { answering, live, note } => {
                let words = answering.as_ref().map_or_else(String::new, |q| format!("Answering: {q}"));
                if let Some(Ok(mut t)) = parts.text.map(|e| texts.get_mut(e))
                    && t.0 != words
                {
                    t.0 = words;
                }
                let turns = [
                    (parts.text, answering.is_some()),
                    (parts.answer, answering.is_some()),
                    (parts.select, answering.is_none()),
                ];
                for (part, shown) in turns {
                    if let Some(Ok(mut node)) = part.map(|e| nodes.get_mut(e))
                        && node.display != display(shown)
                    {
                        node.display = display(shown);
                    }
                }
                if let Some(send) = parts.resolve {
                    gate_button(&mut commands, send, *live);
                }
                if let Some(n) = parts.note {
                    dress_note(&mut commands, &mut texts, &mut nodes, n, note);
                }
            }
            Block::Text { .. }
            | Block::Priority(_)
            | Block::Gates
            | Block::Subtask(_)
            | Block::Inline { .. }
            | Block::Choice { .. }
            | Block::AddSubtask { .. } => {}
        }
    }
}

/// How many lines the description's input is while it is edited.
const DESCRIPTION_LINES: f32 = 6.0;

/// The title or description was edited: send it, or put it back with why not.
fn on_inline_edited(
    edited: On<InlineEdited>,
    mut edits: Query<&mut Edits>,
    mut texts: Query<&mut InlineText>,
    state: Res<InspectorState>,
    connection: Res<State<Connection>>,
    mut submit: MessageWriter<Submit>,
) {
    let Ok(mut e) = edits.get_mut(edited.entity) else { return };
    let change = Change::Text(edited.new.clone());
    match edit_for(&state, &connection, e.field, &change, &e.statuses) {
        Ok(request) => {
            e.pending = Some(Pending::Text { old: edited.old.clone(), new: edited.new.clone() });
            submit.write(Submit { source: edited.entity, request });
        }
        Err(why) => {
            if let Ok(mut t) = texts.get_mut(edited.entity) {
                t.fail_keeping(edited.old.clone(), edited.new.clone(), why);
            }
        }
    }
}

/// A sidebar field was picked: send it, or put it back with why not.
fn on_inline_chosen(
    chosen: On<InlineChosen>,
    mut edits: Query<&mut Edits>,
    mut fields: Query<&mut InlineField>,
    state: Res<InspectorState>,
    connection: Res<State<Connection>>,
    mut submit: MessageWriter<Submit>,
) {
    let Ok(mut e) = edits.get_mut(chosen.entity) else { return };
    match edit_for(&state, &connection, e.field, &Change::Pick(chosen.new), &e.statuses) {
        Ok(request) => {
            e.pending = Some(Pending::Pick { old: chosen.old });
            submit.write(Submit { source: chosen.entity, request });
        }
        Err(why) => {
            if let Ok(mut f) = fields.get_mut(chosen.entity) {
                f.fail(chosen.old, why);
            }
        }
    }
}

/// The request for an edit of the task shown, if it can be sent now.
fn edit_for(
    state: &InspectorState,
    connection: &State<Connection>,
    field: Field,
    change: &Change,
    statuses: &[Status],
) -> Result<Request, String> {
    if *connection.get() != Connection::Live {
        return Err("Not connected to the server, so not saved.".into());
    }
    let view = state.0.as_ref().ok_or("no task is shown")?;
    let facts = view.task.as_ref().ok_or("no task is shown")?;
    edit_request(&key_of(view), facts.version, field, change, statuses)
}

/// The server answered an edit in place: it stays, or goes back with the reason. One that lost to another change
/// keeps what was typed for the next edit and fetches the task again, so that edit goes on the newer version.
fn answer_edits(
    mut done: MessageReader<CommandDone>,
    mut edits: Query<&mut Edits>,
    mut texts: Query<&mut InlineText>,
    mut fields: Query<&mut InlineField>,
    mut refetch: MessageWriter<Refetch>,
) {
    for d in done.read() {
        let Ok(mut e) = edits.get_mut(d.source) else { continue };
        let Some(pending) = e.pending.take() else { continue };
        let conflict = matches!(d.outcome, Outcome::Conflict { .. });
        if conflict {
            refetch.write(Refetch);
        }
        let why = describe(&d.outcome);
        match (pending, why) {
            (Pending::Text { .. }, None) => {
                if let Ok(mut t) = texts.get_mut(d.source) {
                    t.confirm();
                }
            }
            (Pending::Pick { .. }, None) => {
                if let Ok(mut f) = fields.get_mut(d.source) {
                    f.confirm();
                }
            }
            (Pending::Text { old, new }, Some(why)) => {
                if let Ok(mut t) = texts.get_mut(d.source) {
                    if conflict {
                        t.fail_keeping(old, new, format!("{why}. Yours is kept: edit to save it again."));
                    } else {
                        t.fail(old, why);
                    }
                }
            }
            (Pending::Pick { old }, Some(why)) => {
                if let Ok(mut f) = fields.get_mut(d.source) {
                    f.fail(old, why);
                }
            }
        }
    }
}

/// The task modal: the inspector for a task, in the middle of the window, laid out as a main column and a sidebar
/// of facts (`docs/factory/ui.md#views`). It is open while a task is selected; closing it clears the selection.
#[derive(Component, Default)]
pub struct TaskModal {
    title: String,
}

impl Modal for TaskModal {
    fn title(&self) -> String {
        self.title.clone()
    }
    fn width(&self) -> f32 {
        extent::DETAIL_WIDTH
    }
    fn tall(&self) -> bool {
        true
    }
}

/// The open task modal and, once its body exists, its two columns.
#[derive(Resource, Default, Debug)]
struct ModalUi {
    root: Option<Entity>,
    panes: Option<DetailPanes>,
    tools: Option<Tools>,
}

/// The task modal's own header controls, before its X.
#[derive(Debug, Clone)]
struct Tools {
    previous: Entity,
    next: Entity,
    /// The ⋯ menu's root, hidden while there is nothing it can do.
    menu: Entity,
    /// Where the menu's last command stands.
    note: Entity,
    items: Vec<(Action, Entity)>,
}

/// On an arrow in the task modal's header.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Previous,
    Next,
}

/// On an item of the task modal's ⋯ menu: what it does.
#[derive(Component, Debug, Clone)]
struct MenuAct(Action);

/// What the ⋯ menu can hold, in order; each shows while the task offers it.
const MENU_ACTIONS: [Action; 7] = [
    Action::Release,
    Action::Cancel,
    Action::Undo,
    Action::Revert(1),
    Action::Revert(6),
    Action::Revert(24),
    Action::Revert(72),
];

/// A menu item's caption and icon, and whether it is destructive.
fn menu_look(action: &Action) -> (String, Icon, bool) {
    match action {
        Action::Release => ("Release the claim".into(), Icon::Release, false),
        Action::Cancel => ("Cancel the task".into(), Icon::CircleX, true),
        Action::Undo => ("Undo the last change".into(), Icon::Undo, false),
        Action::Revert(h) => (format!("Revert the holder's last {h}h"), Icon::History, true),
        other => (other.label(), Icon::Circle, false),
    }
}

/// Whether the task offers what a menu item does (a revert of any window).
fn offers(view: &InspectorView, action: &Action) -> bool {
    match action {
        Action::Revert(_) => view.actions.iter().any(|a| matches!(a, Action::Revert(_))),
        other => view.actions.contains(other),
    }
}

/// Puts the arrows, the menu and its note at the start of the modal header's tools, before the X.
fn spawn_tools(commands: &mut Commands, tools: Entity) -> Tools {
    let row =
        commands.spawn(Node { align_items: AlignItems::Center, column_gap: px(space::CONTROLS), ..default() }).id();
    let note = spawn_note(commands, row, &None);
    let arrow = |commands: &mut Commands, icon, hint: &str, step| {
        commands.spawn_scene(tool_button(icon, hint, ButtonVariant::Plain)).insert((step, ChildOf(row))).id()
    };
    let previous = arrow(commands, Icon::ChevronUp, "Previous task in the column (K)", Step::Previous);
    let next = arrow(commands, Icon::ChevronDown, "Next task in the column (J)", Step::Next);
    let menu = icon_menu(commands, row, Icon::Ellipsis, "More actions");
    let items = MENU_ACTIONS
        .iter()
        .map(|action| {
            let (caption, icon, danger) = menu_look(action);
            let item = commands
                .spawn_scene(menu_item(caption, Some(icon), danger))
                .insert((MenuAct(action.clone()), ChildOf(menu.popup)))
                .id();
            (action.clone(), item)
        })
        .collect();
    commands.entity(tools).insert_children(0, &[row]);
    Tools { previous, next, menu: menu.root, note, items }
}

/// Keeps the header's controls in line with the task: an arrow is off at its column's end, the menu shows only what
/// the task offers (and nothing while offline), and the note says where its last command stands.
#[allow(clippy::too_many_arguments)]
fn sync_tools(
    state: Res<InspectorState>,
    connection: Res<State<Connection>>,
    neighbours: Res<Neighbours>,
    ui: Res<ModalUi>,
    controls: Res<Controls>,
    states: Query<&Control>,
    changed_controls: Query<(), Changed<Control>>,
    mut texts: Query<&mut Text>,
    mut nodes: Query<&mut Node>,
    mut commands: Commands,
) {
    if !(state.is_changed()
        || connection.is_changed()
        || neighbours.is_changed()
        || ui.is_changed()
        || !changed_controls.is_empty())
    {
        return;
    }
    let (Some(tools), Some(view)) = (&ui.tools, state.0.as_ref().filter(|v| v.task.is_some())) else { return };
    let live = *connection.get() == Connection::Live;
    gate_button(&mut commands, tools.previous, neighbours.previous.is_some());
    gate_button(&mut commands, tools.next, neighbours.next.is_some());
    let mut any = false;
    for (action, item) in &tools.items {
        let shown = live && offers(view, action);
        any |= shown;
        set_display(&mut nodes, *item, shown);
    }
    set_display(&mut nodes, tools.menu, any);
    let note = controls.0.get(&(view.subject.clone(), Action::Menu)).and_then(|e| states.get(*e).ok()).cloned();
    dress_note(&mut commands, &mut texts, &mut nodes, tools.note, &note);
}

fn set_display(nodes: &mut Query<&mut Node>, entity: Entity, shown: bool) {
    let display = if shown { Display::Flex } else { Display::None };
    if let Ok(mut node) = nodes.get_mut(entity)
        && node.display != display
    {
        node.display = display;
    }
}

/// An arrow in the header was pressed: show the task before or after this one.
fn on_step(activate: On<Activate>, steps: Query<&Step>, neighbours: Res<Neighbours>, selection: ResMut<Selection>) {
    if let Ok(step) = steps.get(activate.entity) {
        step_to(*step, &neighbours, selection);
    }
}

/// Up and K, down and J, step through the column while the task modal is open, unless a field has the keyboard or a
/// control in the modal took the key (a menu or a dropdown's list, which stop the arrows they use).
fn on_step_key(
    key: On<FocusedInput<KeyboardInput>>,
    ui: Res<ModalUi>,
    windows: Query<(), With<Window>>,
    inputs: Query<(), With<EditableText>>,
    neighbours: Res<Neighbours>,
    selection: ResMut<Selection>,
) {
    let Some(root) = ui.root else { return };
    let (at, from) = (key.event_target(), key.original_event_target());
    // Once: at the modal's root, where what nothing in it took arrives, or at the window when nothing has the keyboard.
    if !(at == root || (at == from && windows.contains(at))) || inputs.contains(from) {
        return;
    }
    let pressed = &key.input;
    if !pressed.state.is_pressed() {
        return;
    }
    let step = match pressed.key_code {
        KeyCode::ArrowUp | KeyCode::KeyK => Step::Previous,
        KeyCode::ArrowDown | KeyCode::KeyJ => Step::Next,
        _ => return,
    };
    step_to(step, &neighbours, selection);
}

fn step_to(step: Step, neighbours: &Neighbours, mut selection: ResMut<Selection>) {
    let to = match step {
        Step::Previous => &neighbours.previous,
        Step::Next => &neighbours.next,
    };
    if let Some(id) = to {
        selection.0 = Some(Subject::Task(id.clone()));
    }
}

/// An item of the ⋯ menu was pressed: send it for the task shown, with its answer beside the menu.
fn on_menu_item(
    activate: On<Activate>,
    items: Query<&MenuAct>,
    state: Res<InspectorState>,
    mut controls: ResMut<Controls>,
    mut commands: Commands,
) {
    let Ok(MenuAct(action)) = items.get(activate.entity) else { return };
    let Some(subject) = state.0.as_ref().map(|v| v.subject.clone()) else { return };
    let source = control(&mut controls, &mut commands, &subject, &Action::Menu);
    let action = action.clone();
    commands.queue(move |world: &mut World| send(world, source, &subject, &action));
}

/// On a subtask's row: opens that task in the modal.
#[derive(Component, Clone, Debug, Default)]
struct OpensTask(String);

/// Opens the task modal while a task is selected and closes it when none is, and makes its columns once its body has
/// spawned. Changing to another task keeps the modal; [`sync_panel`] redraws what is in it.
fn sync_modal(
    state: Res<InspectorState>,
    mut ui: ResMut<ModalUi>,
    modals: Query<Option<&ModalSlots>, With<TaskModal>>,
    mut commands: Commands,
) {
    let wanted = state.0.as_ref().and_then(|v| v.task.as_ref());
    if let Some(root) = ui.root
        && !modals.contains(root)
    {
        // Closed from inside (the X, Escape, a click outside): it took its columns with it.
        *ui = ModalUi::default();
    }
    match (wanted, ui.root) {
        (Some(facts), None) => {
            ui.root = Some(open_modal(&mut commands, TaskModal { title: facts.breadcrumb.clone() }));
        }
        (None, Some(root)) => {
            commands.entity(root).try_despawn();
            *ui = ModalUi::default();
        }
        _ => {}
    }
    if let (Some(root), None) = (ui.root, ui.panes)
        && let Ok(Some(slots)) = modals.get(root)
    {
        ui.panes = Some(spawn_detail_panes(&mut commands, slots.body));
        ui.tools = Some(spawn_tools(&mut commands, slots.tools));
    }
}

/// The modal was closed from inside it: nothing is selected any more.
fn on_modal_closed(closed: On<ModalClosed>, modals: Query<(), With<TaskModal>>, mut selection: ResMut<Selection>) {
    if modals.contains(closed.modal) {
        selection.0 = None;
    }
}

/// A subtask's row was pressed: show that task instead.
fn on_open_task(activate: On<Activate>, rows: Query<&OpensTask>, mut selection: ResMut<Selection>) {
    if let Ok(OpensTask(id)) = rows.get(activate.entity) {
        selection.0 = Some(Subject::Task(id.clone()));
    }
}

/// A different task starts the comment box over.
fn follow_compose(state: Res<InspectorState>, mut compose: ResMut<Compose>) {
    if !state.is_changed() {
        return;
    }
    let subject = state.0.as_ref().filter(|v| v.task.is_some()).map(|v| v.subject.clone());
    if compose.subject != subject {
        let focus = compose.focus && subject.is_some();
        let answering = if focus { compose.answering.take() } else { None };
        *compose = Compose { subject, answering, focus, ..Compose::default() };
    }
}

/// Opens the task in the modal with its comment box ready, answering the question if there is one.
fn on_open_composer(
    open: On<OpenComposer>,
    tasks: Query<&Mirrored<Task>>,
    mut selection: ResMut<Selection>,
    mut compose: ResMut<Compose>,
) {
    let Some(task) = tasks.iter().find(|t| t.0.key == open.key) else { return };
    let subject = Subject::Task(task.0.id.to_string());
    if compose.subject.as_ref() != Some(&subject) {
        *compose = Compose::default();
    }
    compose.answering = open.answer.clone();
    compose.focus = true;
    selection.0 = Some(subject);
}

/// Gives the comment box's input the keyboard once it exists, when it was asked for.
fn focus_composer(
    mut compose: ResMut<Compose>,
    inputs: Query<Entity, (With<ComposePart>, With<EditableText>)>,
    parts: Query<&ComposePart>,
    mut focus: ResMut<InputFocus>,
) {
    if !compose.focus {
        return;
    }
    if let Some(input) = inputs.iter().find(|e| parts.get(*e) == Ok(&ComposePart::Input)) {
        focus.set(input, FocusCause::Navigated);
        compose.focus = false;
    }
}

/// The comment box's buttons: send, and stop answering.
#[allow(clippy::too_many_arguments)]
fn on_compose_press(
    activate: On<Activate>,
    parts: Query<&ComposePart>,
    inputs: Query<(&ComposePart, &EditableText)>,
    state: Res<InspectorState>,
    mut compose: ResMut<Compose>,
    controls: Res<Controls>,
    mut sources: Query<&mut Control>,
    mut submit: MessageWriter<Submit>,
) {
    let Ok(part) = parts.get(activate.entity) else { return };
    match part {
        ComposePart::StopAnswering => compose.answering = None,
        ComposePart::Send => {
            let Some(view) = &state.0 else { return };
            let Some(source) = controls.0.get(&(view.subject.clone(), Action::Comment)).copied() else { return };
            let body = inputs
                .iter()
                .find(|(p, _)| **p == ComposePart::Input)
                .map(|(_, t)| t.value().to_string())
                .unwrap_or_default();
            let outcome = match compose_request(&key_of(view), compose.kind, compose.answering.as_ref(), &body) {
                Ok(request) => {
                    submit.write(Submit { source, request });
                    Control::Busy
                }
                Err(why) => Control::Refused(why),
            };
            if let Ok(mut c) = sources.get_mut(source) {
                *c = outcome;
            }
        }
        _ => {}
    }
}

/// The request the comment box sends: a comment of `kind` on the task, or the answer to a question.
pub fn compose_request(
    key: &str,
    kind: CommentKind,
    answering: Option<&(String, String)>,
    body: &str,
) -> Result<Request, String> {
    let body = body.trim();
    if body.is_empty() {
        return Err(if answering.is_some() { "write the answer first" } else { "write the comment first" }.into());
    }
    Ok(match answering {
        Some((id, _)) => {
            Request::new(Method::POST, format!("/v1/comments/{id}/resolve"), Some(json!({"answer": body})), None)
        }
        None => Request::new(
            Method::POST,
            format!("/v1/tasks/{key}/comments"),
            Some(json!({"kind": kind, "body": body})),
            None,
        ),
    })
}

/// A comment the box sent went through: empty the box and stop answering.
fn clear_sent(
    controls: Res<Controls>,
    state: Res<InspectorState>,
    changed: Query<&Control, Changed<Control>>,
    mut inputs: Query<(&ComposePart, &mut EditableText)>,
    mut compose: ResMut<Compose>,
) {
    let Some(view) = &state.0 else { return };
    let Some(source) = controls.0.get(&(view.subject.clone(), Action::Comment)) else { return };
    if changed.get(*source) != Ok(&Control::Done) {
        return;
    }
    for (part, mut text) in &mut inputs {
        if *part == ComposePart::Input {
            text.editor_mut().set_text("");
        }
    }
    if compose.answering.is_some() {
        compose.answering = None;
    }
}

/// What the panel has on its entities: the sections of the body, the items in them and a PR's gates.
#[derive(Default)]
struct Drawn {
    subject: Option<Subject>,
    /// The modal the sections were drawn in, if they were; a different one (or none) means they went with it.
    modal: Option<Entity>,
    header: Option<Entity>,
    sections: Keyed<()>,
    items: Keyed<Block>,
    gates: Keyed<Gate>,
}

/// Keeps the panel's entities in step with [`InspectorState`]: a task's in the task modal's two columns, an agent's
/// in the side panel. Selecting a different subject builds the sections anew, once; while the same one stays
/// selected, only what changed is set: text through its `Text`, a list's new rows appended, the rest left as the same
/// entities (and the scroll position where it was).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn sync_panel(
    state: Res<InspectorState>,
    connection: Res<State<Connection>>,
    factory: Res<FactoryStatus>,
    status: Res<ConnectionStatus>,
    window: Res<RevertWindow>,
    (ui, compose): (Res<ModalUi>, Res<Compose>),
    mut controls: ResMut<Controls>,
    states: Query<&Control>,
    changed_controls: Query<(), Changed<Control>>,
    mut panel_node: Single<&mut Node, With<InspectorPanel>>,
    header: Single<Entity, With<InspectorHeader>>,
    body: Single<(Entity, &mut ScrollPosition), With<InspectorBody>>,
    mut titles: Query<(Entity, &mut Text), With<PanelTitle>>,
    parents: Query<&ChildOf>,
    mut drawn: Local<Drawn>,
    mut commands: Commands,
) {
    if !(state.is_changed()
        || connection.is_changed()
        || status.is_changed()
        || window.is_changed()
        || ui.is_changed()
        || compose.is_changed()
        || factory.is_changed()
        || !changed_controls.is_empty())
    {
        return;
    }
    let (body, mut scroll) = body.into_inner();
    let drawn = &mut *drawn;
    let subject = state.0.as_ref().map(|v| v.subject.clone());
    let in_modal = state.0.as_ref().is_some_and(|v| v.task.is_some());
    let target = if in_modal { ui.root } else { None };
    if subject != drawn.subject || target != drawn.modal {
        // A different subject (or none, or another place to draw it) starts over, at the top.
        if let Some(old) = drawn.header.take() {
            commands.entity(old).try_despawn();
        }
        if drawn.modal.is_some() && drawn.modal != target {
            // That modal is gone, and its sections with it.
            drawn.sections.forget();
        } else {
            drawn.sections.clear(&mut commands);
        }
        drawn.items.forget();
        drawn.gates.forget();
        scroll.0 = Vec2::ZERO;
        drawn.subject = subject;
        drawn.modal = target;
    }
    let Some(view) = &state.0 else {
        panel_node.display = Display::None;
        return;
    };

    let live = *connection.get() == Connection::Live;
    let why = status.reason.as_ref().map_or_else(String::new, |r| format!(": {r}"));
    let offline = format!("{:?}{why}. Controls are off.", connection.get());
    let subject = &view.subject;
    let note =
        |action: &Action| controls.0.get(&(subject.clone(), action.clone())).and_then(|e| states.get(*e).ok()).cloned();
    let human = factory.human_grades();

    let (title, root, wanted) = match &view.task {
        Some(facts) => {
            panel_node.display = Display::None;
            // The modal's columns come a frame or two after it opens; this runs again when they do.
            let (Some(root), Some(panes)) = (ui.root, ui.panes) else { return };
            let answering = compose.answering.as_ref().map(|(_, q)| q.as_str());
            let (main, side) = layout_task(view, facts, live, &offline, &human, answering, &note);
            let keys = |plan: &Plan| plan.iter().map(|(k, _)| (k.clone(), ())).collect::<Vec<_>>();
            let parents = vec![(panes.main, keys(&main)), (panes.side, keys(&side))];
            (facts.breadcrumb.clone(), root, (parents, main.into_iter().chain(side).collect::<Plan>()))
        }
        None => {
            panel_node.display = Display::Flex;
            let root = *drawn.header.get_or_insert_with(|| {
                commands.spawn_scene(panel_header(view.title.clone(), true)).insert(ChildOf(*header)).id()
            });
            let plan = layout(view, live, &offline, window.0, &note);
            let keys = plan.iter().map(|(k, _)| (k.clone(), ())).collect::<Vec<_>>();
            (view.title.clone(), root, (vec![(body, keys)], plan))
        }
    };
    // Only this surface's own title: every modal's header is a `panel_header` too, with a `PanelTitle` of its own.
    for (entity, mut words) in &mut titles {
        if words.0 != title && parents.iter_ancestors(entity).any(|a| a == root) {
            words.0 = title.clone();
        }
    }

    let (sections, plan) = wanted;
    drawn.sections.sync(
        &mut commands,
        &sections,
        |commands, _, _| commands.spawn_scene(section_frame()).id(),
        |_, _, _| {},
    );
    let wanted: Vec<(Entity, Vec<(String, Block)>)> =
        plan.into_iter().filter_map(|(key, items)| Some((drawn.sections.entity(&key)?, items))).collect();
    let controls = &mut *controls;
    drawn.items.sync(
        &mut commands,
        &wanted,
        |commands, _, block| spawn_block(commands, controls, subject, block, &human),
        |commands, entity, block| {
            commands.entity(entity).insert(Shown(block.clone()));
        },
    );
    let rows = view.pr.as_ref().map(gates).unwrap_or_default();
    let wanted: Vec<(Entity, Vec<(String, Gate)>)> =
        drawn.items.entity("pr/gates").map(|row| (row, rows)).into_iter().collect();
    drawn.gates.sync(
        &mut commands,
        &wanted,
        |commands, _, gate| {
            let (icon, tone) = match gate.state {
                GateState::Passed => (Icon::CircleCheck, token::OK),
                GateState::Waiting => (Icon::Circle, tokens::TEXT_DIM),
                GateState::Failing => (Icon::CircleX, token::DANGER),
            };
            commands.spawn_scene(badge(gate.name.clone(), Some(icon), Some(tone))).id()
        },
        |_, _, _| {},
    );
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::fixture::{agent, at, claim, comment, comment_json, sid, task};
    use crate::sync::Snapshot;
    use crate::widgets::ScrollDown;

    struct Fixture {
        projects: Vec<Project>,
        tasks: Vec<Task>,
        claims: Vec<Claim>,
        comments: Vec<Comment>,
        agents: Vec<Agent>,
    }

    fn as_json<T: serde::Serialize>(items: &[T]) -> Vec<Value> {
        items.iter().map(|i| serde_json::to_value(i).unwrap()).collect()
    }

    impl Fixture {
        fn input(&self) -> Input<'_> {
            Input {
                projects: &self.projects,
                tasks: &self.tasks,
                claims: &self.claims,
                comments: &self.comments,
                agents: &self.agents,
                ..Input::default()
            }
        }
    }

    fn fixture() -> Fixture {
        let factory = json!({
            "sandbox": "trail-7-a1", "attempt": 2,
            "pr": {"repo": {"owner": "o", "name": "r"}, "number": 12, "url": "https://github.com/o/r/pull/12"},
            "grade": {"grade": 2},
            "merge": {"grade": 2, "head": "abc", "passed": ["checks", "ci"], "waiting": ["agent_review"], "failing": [],
                      "would_merge": false, "problem": null, "message": "Grade 2: waiting for agent review."},
        });
        Fixture {
            projects: vec![],
            tasks: vec![
                task(
                    json!({"id": "t7", "key": "TRAIL-7", "title": "do it", "status": "in_review", "priority": 1, "version": 4,
                       "metadata": {"factory": factory}}),
                ),
                task(json!({"id": "t8", "key": "TRAIL-8", "title": "plain", "status": "open", "version": 1})),
            ],
            claims: vec![claim(json!({"task_id": "t7", "holder_id": "a1"}))],
            comments: vec![
                comment(
                    json!({"id": "c1", "task_id": "t7", "kind": "question", "body": "which db?", "created_at": at(1)}),
                ),
                comment(
                    json!({"id": "c2", "task_id": "t7", "kind": "question", "body": "old", "resolved_at": at(2), "created_at": at(1)}),
                ),
            ],
            agents: vec![agent(json!({"id": "a1", "name": "factory/trail-7-a1"}))],
        }
    }

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

    fn latest() -> HashMap<String, i64> {
        HashMap::from([(sid("t7"), 41)])
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
        assert_eq!(activity[2].tags, ["request changes", "m-1"]);
        assert!(!activity[2].open_question);
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
        assert!(v.merge_note.unwrap().contains("Merge on GitHub"));
        for a in &v.actions {
            assert!(!matches!(a, Action::Status(Status::Done | Status::Approved)), "{a:?}");
        }
        assert_eq!(v.questions.len(), 1);
        for want in [Action::Release, Action::Cancel, Action::Undo, Action::Revert(1), Action::Resolve(sid("c1"))] {
            assert!(v.actions.contains(&want), "{want:?}");
        }
        assert!(!v.actions.contains(&Action::Status(Status::InReview)));
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
    fn the_window_dropdown_sets_how_far_back_revert_goes() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(crate::mirror::MirrorPlugin).add_plugins(InspectorPlugin);
        let select = app.world_mut().spawn(WindowChoice).id();
        app.world_mut().trigger(Picked { entity: select, index: 2 });
        app.update();
        assert_eq!(app.world().resource::<RevertWindow>().0, 24);
        // A row past the end changes nothing.
        app.world_mut().trigger(Picked { entity: select, index: 9 });
        app.update();
        assert_eq!(app.world().resource::<RevertWindow>().0, 24);
    }

    #[test]
    fn the_status_dropdown_sends_the_move_and_ignores_the_current_status() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(crate::mirror::MirrorPlugin).add_plugins(InspectorPlugin);
        app.add_message::<Submit>();
        let f = fixture();
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot {
            at: 5,
            projects: vec![],
            tasks: as_json(&f.tasks),
            agents: vec![],
            ..Default::default()
        }));
        app.update();
        let source = app.world_mut().spawn(Control::Idle).id();
        let statuses = vec![Status::Open, Status::InProgress];
        let select = app.world_mut().spawn(StatusMove { source, subject: Subject::Task(sid("t8")), statuses }).id();
        app.world_mut().trigger(Picked { entity: select, index: 0 });
        app.update();
        assert_eq!(app.world().get::<Control>(source), Some(&Control::Idle));
        app.world_mut().trigger(Picked { entity: select, index: 1 });
        app.update();
        assert_eq!(app.world().get::<Control>(source), Some(&Control::Busy));
        let sent: Vec<_> = app.world_mut().resource_mut::<Messages<Submit>>().drain().collect();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].request.path, "/v1/tasks/TRAIL-8");
        assert_eq!(sent[0].request.body, Some(json!({"status": "in_progress"})));
    }

    #[test]
    fn the_status_dropdown_shows_words_for_the_api_values() {
        let statuses = vec![Status::NeedsInput, Status::InProgress];
        assert_eq!(status_labels(&statuses), ["Needs Input", "In Progress"]);
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

    #[test]
    fn the_panel_follows_the_mirror_and_an_answer_reaches_its_control() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(crate::mirror::MirrorPlugin).add_plugins(InspectorPlugin);
        let f = fixture();
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot {
            at: 5,
            projects: vec![],
            tasks: as_json(&f.tasks),
            agents: as_json(&f.agents),
            ..Default::default()
        }));
        app.world_mut().write_message(SyncItem::Event(json!({
            "id": 6, "entity": "comment", "entity_id": sid("c1"), "kind": "comment.created",
            "after": comment_json(json!({"id": "c1", "task_id": "t7", "kind": "question", "body": "q", "resolved_at": null})),
        })));
        app.world_mut().resource_mut::<Selection>().0 = Some(Subject::Task(sid("t7")));
        app.update();
        app.update();
        assert_eq!(app.world().resource::<Latest>().0.get(&sid("t7")), Some(&6));
        let view = app.world().resource::<InspectorState>().0.clone().unwrap();
        assert_eq!(view.title, "TRAIL-7 do it");
        assert!(view.actions.contains(&Action::Undo));

        // A refusal lands on the control that sent it.
        let control = app.world_mut().spawn(Control::Busy).id();
        app.world_mut().write_message(CommandDone {
            source: control,
            id: crate::commands::CommandId(1),
            outcome: Outcome::Forbidden("humans only".into()),
        });
        app.update();
        assert_eq!(app.world().get::<Control>(control), Some(&Control::Refused("not allowed: humans only".into())));

        // Deselecting empties the panel.
        app.world_mut().resource_mut::<Selection>().0 = None;
        app.update();
        assert!(app.world().resource::<InspectorState>().0.is_none());
    }

    fn escape(app: &mut App, from: Entity) {
        let input = KeyboardInput {
            key_code: KeyCode::Escape,
            logical_key: bevy::input::keyboard::Key::Escape,
            state: bevy::input::ButtonState::Pressed,
            text: None,
            repeat: false,
            window: from,
        };
        let window = app.world_mut().spawn_empty().id();
        app.world_mut().trigger(FocusedInput::new(from, input, window));
        app.update();
    }

    fn open_inspector(app: &mut App) -> Entity {
        app.world_mut().resource_mut::<Selection>().0 = Some(Subject::Task(sid("t7")));
        app.world_mut().spawn(InspectorPanel).id()
    }

    fn closing_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(crate::mirror::MirrorPlugin).add_plugins(InspectorPlugin);
        app
    }

    #[test]
    fn the_x_closes_the_inspector() {
        let mut app = closing_app();
        let panel = open_inspector(&mut app);
        let x = app.world_mut().spawn(ChildOf(panel)).id();
        app.world_mut().trigger(RequestClose { source: x });
        app.update();
        assert_eq!(app.world().resource::<Selection>().0, None);
    }

    #[test]
    fn escape_closes_the_inspector() {
        let mut app = closing_app();
        open_inspector(&mut app);
        let target = app.world_mut().spawn_empty().id();
        escape(&mut app, target);
        assert_eq!(app.world().resource::<Selection>().0, None);
    }

    #[test]
    fn escape_with_a_modal_open_leaves_the_inspector() {
        let mut app = closing_app();
        open_inspector(&mut app);
        app.world_mut().spawn(ModalRoot);
        let target = app.world_mut().spawn_empty().id();
        escape(&mut app, target);
        assert!(app.world().resource::<Selection>().0.is_some());
    }

    #[test]
    fn escape_in_a_text_input_leaves_the_inspector() {
        let mut app = closing_app();
        open_inspector(&mut app);
        let field = app.world_mut().spawn(EditableText::new("x")).id();
        escape(&mut app, field);
        assert!(app.world().resource::<Selection>().0.is_some());
    }

    #[test]
    fn the_body_is_a_scroll_area_held_to_the_window_under_a_fixed_header() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), bevy::scene::ScenePlugin))
            .add_systems(Update, spawn_panel);
        app.update();
        let world = app.world_mut();
        let panel = world.query_filtered::<Entity, With<InspectorPanel>>().single(world).unwrap();
        assert_eq!(world.get::<Node>(panel).unwrap().height, percent(100));
        let header = world.query_filtered::<Entity, With<InspectorHeader>>().single(world).unwrap();
        let (body, node) = world.query_filtered::<(Entity, &Node), With<InspectorBody>>().single(world).unwrap();
        assert_eq!((node.overflow, node.min_height), (Overflow::scroll_y(), px(0)));
        assert!(world.get::<ScrollDown>(body).is_some());
        // The body's frame is the panel's child beside the header, and may shrink below its content.
        let frame = world.get::<ChildOf>(body).unwrap().parent();
        assert_eq!(world.get::<ChildOf>(frame).unwrap().parent(), panel);
        assert_eq!(world.get::<Node>(frame).unwrap().min_height, px(0));
        assert_eq!(world.get::<ChildOf>(header).unwrap().parent(), panel);
    }

    #[test]
    fn picking_a_grade_sends_the_patch_with_the_version_and_a_refusal_shows_beside_it() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(crate::mirror::MirrorPlugin).add_plugins(InspectorPlugin);
        app.add_message::<Submit>();
        let f = fixture();
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot {
            at: 5,
            projects: vec![],
            tasks: as_json(&f.tasks),
            agents: as_json(&f.agents),
            ..Default::default()
        }));
        app.update();
        let subject = Subject::Task(sid("t8"));
        let source = app.world_mut().spawn(Control::Idle).id();
        let select = app.world_mut().spawn(GradeChoice { source, subject }).id();
        // Row 3 of the dropdown is grade 3 (none, 1, 2, 3, 4).
        app.world_mut().trigger(Picked { entity: select, index: 3 });
        app.update();
        let sent: Vec<Submit> = app.world_mut().resource_mut::<Messages<Submit>>().drain().collect();
        assert_eq!(sent.len(), 1);
        let r = &sent[0].request;
        assert_eq!((r.method.clone(), r.path.as_str(), r.if_match), (Method::PATCH, "/v1/tasks/TRAIL-8", Some(1)));
        assert_eq!(r.body, Some(json!({"grade": 3})));
        assert_eq!(app.world().get::<Control>(source), Some(&Control::Busy));

        // Clearing sends null.
        app.world_mut().trigger(Picked { entity: select, index: 0 });
        app.update();
        let sent: Vec<Submit> = app.world_mut().resource_mut::<Messages<Submit>>().drain().collect();
        assert_eq!(sent[0].request.body, Some(json!({"grade": null})));

        // The server's refusal is kept on the control; the task's grade, and so the dropdown, stays as it was.
        app.world_mut().write_message(CommandDone {
            source,
            id: crate::commands::CommandId(1),
            outcome: Outcome::Forbidden("only a human can set the first grade".into()),
        });
        app.update();
        assert_eq!(
            app.world().get::<Control>(source),
            Some(&Control::Refused("not allowed: only a human can set the first grade".into()))
        );
        let f = fixture();
        let v = build(f.input(), &latest(), &Subject::Task(sid("t8")), Fetched::default()).unwrap();
        assert_eq!(v.grade, None);
    }
}

#[cfg(test)]
mod in_place_tests {
    use bevy::state::app::StatesPlugin;

    use super::*;
    use crate::fixture::{agent_json, at, claim_json, comment_json, sid, task_json};
    use crate::sync::Snapshot;

    fn question(id: &str, body: &str) -> SyncItem {
        SyncItem::Event(json!({
            "id": id.trim_start_matches('c').parse::<i64>().unwrap() + 100, "entity": "comment", "entity_id": sid(id),
            "kind": "comment.created",
            "after": comment_json(json!({"id": id, "task_id": "t7", "kind": "question", "body": body,
                      "created_at": at(id.trim_start_matches('c').parse().unwrap())})),
        }))
    }

    fn settle(app: &mut App) {
        for _ in 0..5 {
            app.update();
        }
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin, AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .init_state::<Connection>()
            .init_resource::<ConnectionStatus>()
            .add_plugins((
                crate::mirror::MirrorPlugin,
                InspectorUiPlugin,
                crate::widgets::ModalPlugin,
                crate::widgets::InlinePlugin,
            ));
        let tasks = vec![
            task_json(
                json!({"id": "t7", "key": "TRAIL-7", "title": "do it", "status": "in_progress", "priority": 1, "version": 4}),
            ),
            task_json(json!({"id": "t8", "key": "TRAIL-8", "title": "other", "status": "open", "version": 1})),
            task_json(json!({"id": "t9", "key": "TRAIL-9", "title": "a part", "status": "open", "parent_id": "t7"})),
        ];
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot {
            at: 5,
            projects: vec![],
            tasks,
            agents: vec![],
            ..Default::default()
        }));
        app.world_mut().write_message(question("c1", "which db?"));
        app.world_mut().resource_mut::<NextState<Connection>>().set(Connection::Live);
        app.world_mut().resource_mut::<Selection>().0 = Some(Subject::Task(sid("t7")));
        settle(&mut app);
        app
    }

    /// Every entity in the task modal's two columns, in order.
    fn body(app: &mut App) -> Vec<Entity> {
        let world = app.world_mut();
        let main = world.query_filtered::<Entity, With<crate::widgets::DetailMain>>().single(world).unwrap();
        let side = world.query_filtered::<Entity, With<crate::widgets::DetailSide>>().single(world).unwrap();
        let mut found = vec![main, side];
        let mut at = 0;
        while at < found.len() {
            if let Some(children) = world.get::<Children>(found[at]) {
                found.extend(children.iter());
            }
            at += 1;
        }
        found
    }

    fn texts(app: &mut App) -> Vec<String> {
        let world = app.world_mut();
        world.query::<&Text>().iter(world).map(|t| t.0.clone()).collect()
    }

    #[test]
    fn a_new_comment_adds_its_row_and_leaves_every_other_entity_as_it_was() {
        let mut app = app();
        let before = body(&mut app);
        assert!(texts(&mut app).iter().any(|t| t == "which db?"));

        app.world_mut().write_message(question("c2", "and the cache?"));
        settle(&mut app);
        let after = body(&mut app);
        assert!(texts(&mut app).iter().any(|t| t == "and the cache?"));
        let added: Vec<_> = after.iter().filter(|e| !before.contains(e)).collect();
        assert!(!added.is_empty(), "the new question has entities");
        // Everything that was there still is, in the same place: the first question, the comment box and the rest.
        let kept: Vec<_> = after.iter().filter(|e| before.contains(e)).copied().collect();
        assert_eq!(kept.len(), before.len());
        let world = app.world();
        let questions = |entities: &[Entity]| {
            entities
                .iter()
                .filter(|e| {
                    world
                        .get::<Shown>(**e)
                        .is_some_and(|s| matches!(&s.0, Block::Entry { entry, .. } if entry.open_question))
                })
                .count()
        };
        assert_eq!((questions(&before), questions(&after)), (1, 2));
    }

    #[test]
    fn a_changed_title_and_a_pressed_control_leave_the_panel_as_it_was() {
        let mut app = app();
        let before = body(&mut app);
        app.world_mut().write_message(SyncItem::Event(json!({
            "id": 300, "entity": "task", "entity_id": sid("t7"), "kind": "task.updated",
            "after": task_json(json!({"id": "t7", "key": "TRAIL-7", "title": "renamed", "status": "in_progress", "priority": 1,
                      "version": 5})),
        })));
        settle(&mut app);
        assert!(texts(&mut app).iter().any(|t| t == "renamed"));
        let controls: Vec<Entity> = {
            let world = app.world_mut();
            world.query_filtered::<Entity, With<Control>>().iter(world).collect()
        };
        assert!(!controls.is_empty());
        for c in controls {
            *app.world_mut().get_mut::<Control>(c).unwrap() = Control::Refused("nope".into());
        }
        settle(&mut app);
        assert_eq!(body(&mut app), before);
        assert!(texts(&mut app).iter().any(|t| t == "nope"));
    }

    #[test]
    fn an_update_leaves_an_open_modals_title_alone() {
        let mut app = app();
        let modal = app.world_mut().commands().spawn_scene(panel_header("Answer TRAIL-7", true)).id();
        settle(&mut app);
        app.world_mut().write_message(SyncItem::Event(json!({
            "id": 301, "entity": "task", "entity_id": sid("t7"), "kind": "task.updated",
            "after": task_json(json!({"id": "t7", "key": "TRAIL-7", "title": "renamed", "status": "in_progress", "priority": 1,
                      "version": 5})),
        })));
        settle(&mut app);
        assert!(texts(&mut app).iter().any(|t| t == "renamed"));
        let world = app.world_mut();
        let titles: Vec<String> = world
            .query_filtered::<(Entity, &Text), With<PanelTitle>>()
            .iter(world)
            .filter(|(e, _)| world.get::<ChildOf>(*e).is_some_and(|c| c.parent() == modal))
            .map(|(_, t)| t.0.clone())
            .collect();
        assert_eq!(titles, ["Answer TRAIL-7"]);
    }

    #[test]
    fn a_different_subject_rebuilds_the_panel() {
        let mut app = app();
        let before = body(&mut app);
        app.world_mut().resource_mut::<Selection>().0 = Some(Subject::Task(sid("t8")));
        settle(&mut app);
        let after = body(&mut app);
        assert!(after.iter().skip(2).all(|e| !before.contains(e)), "nothing is carried over");
        assert!(texts(&mut app).iter().any(|t| t == "other"));
        assert!(!texts(&mut app).iter().any(|t| t == "? which db?"));
    }

    fn modal_count(app: &mut App) -> usize {
        app.world_mut().query_filtered::<(), With<TaskModal>>().iter(app.world()).count()
    }

    #[test]
    fn a_selected_task_opens_one_modal_with_its_title_facts_and_subtasks() {
        let mut app = app();
        assert_eq!(modal_count(&mut app), 1);
        let shown = texts(&mut app);
        for want in
            ["do it", "Add a description", "Sub-tasks", "TRAIL-9  a part", "Status", "Priority", "Claim", "Details"]
        {
            assert!(shown.iter().any(|t| t == want), "{want} in {shown:?}");
        }
        // The modal's header names the task.
        let world = app.world_mut();
        let titles: Vec<String> =
            world.query_filtered::<&Text, With<PanelTitle>>().iter(world).map(|t| t.0.clone()).collect();
        assert!(titles.iter().any(|t| t == "TRAIL-7"), "{titles:?}");
        // The side panel stays hidden for a task.
        let panel = world.query_filtered::<&Node, With<InspectorPanel>>().single(world).unwrap();
        assert_eq!(panel.display, Display::None);
    }

    #[test]
    fn closing_the_modal_clears_the_selection_and_a_new_one_opens_it_again() {
        let mut app = app();
        let root = app.world_mut().query_filtered::<Entity, With<TaskModal>>().single(app.world()).unwrap();
        app.world_mut().trigger(RequestClose { source: root });
        settle(&mut app);
        assert_eq!(app.world().resource::<Selection>().0, None);
        assert_eq!(modal_count(&mut app), 0);
        app.world_mut().resource_mut::<Selection>().0 = Some(Subject::Task(sid("t8")));
        settle(&mut app);
        assert_eq!(modal_count(&mut app), 1);
        assert!(texts(&mut app).iter().any(|t| t == "other"));
    }

    #[test]
    fn a_subtasks_row_opens_it_in_the_same_modal() {
        let mut app = app();
        let root = app.world_mut().query_filtered::<Entity, With<TaskModal>>().single(app.world()).unwrap();
        let row = app.world_mut().query_filtered::<Entity, With<OpensTask>>().single(app.world()).unwrap();
        app.world_mut().trigger(Activate { entity: row });
        settle(&mut app);
        assert_eq!(app.world().resource::<Selection>().0, Some(Subject::Task(sid("t9"))));
        assert!(texts(&mut app).iter().any(|t| t == "a part"));
        let still = app.world_mut().query_filtered::<Entity, With<TaskModal>>().single(app.world()).unwrap();
        assert_eq!(still, root, "the same modal shows the subtask");
    }

    #[test]
    fn an_agent_still_opens_the_side_panel_and_no_modal() {
        let mut app = app();
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot {
            at: 6,
            projects: vec![],
            tasks: vec![],
            agents: vec![crate::fixture::agent_json(json!({"id": "a1", "name": "factory/trail-7-a1"}))],
            ..Default::default()
        }));
        app.world_mut().resource_mut::<Selection>().0 = Some(Subject::Agent(sid("a1")));
        settle(&mut app);
        assert_eq!(modal_count(&mut app), 0);
        let world = app.world_mut();
        let panel = world.query_filtered::<&Node, With<InspectorPanel>>().single(world).unwrap();
        assert_eq!(panel.display, Display::Flex);
    }

    fn composer_input(app: &mut App) -> Entity {
        let world = app.world_mut();
        let mut q = world.query::<(Entity, &ComposePart)>();
        q.iter(world).find(|(_, p)| **p == ComposePart::Input).map(|(e, _)| e).expect("the comment box's input")
    }

    fn compose_button(app: &mut App, part: ComposePart) -> Entity {
        let world = app.world_mut();
        let mut q = world.query::<(Entity, &ComposePart)>();
        q.iter(world).find(|(_, p)| **p == part).map(|(e, _)| e).unwrap()
    }

    fn sent(app: &mut App) -> Vec<Submit> {
        app.world_mut().resource_mut::<Messages<Submit>>().drain().collect()
    }

    #[test]
    fn the_comment_box_sends_a_comment_and_empties_once_it_went_through() {
        let mut app = app();
        let input = composer_input(&mut app);
        app.world_mut().get_mut::<EditableText>(input).unwrap().editor_mut().set_text("looks good");
        app.world_mut().resource_mut::<Compose>().kind = CommentKind::Decision;
        let send = compose_button(&mut app, ComposePart::Send);
        app.world_mut().trigger(Activate { entity: send });
        let sent = sent(&mut app);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].request.path, "/v1/tasks/TRAIL-7/comments");
        assert_eq!(sent[0].request.body, Some(json!({"kind": "decision", "body": "looks good"})));
        assert_eq!(app.world().get::<Control>(sent[0].source), Some(&Control::Busy));
        // The server took it.
        *app.world_mut().get_mut::<Control>(sent[0].source).unwrap() = Control::Done;
        settle(&mut app);
        assert_eq!(app.world().get::<EditableText>(input).unwrap().value().to_string(), "");
    }

    #[test]
    fn an_empty_comment_box_says_so_and_sends_nothing() {
        let mut app = app();
        let send = compose_button(&mut app, ComposePart::Send);
        app.world_mut().trigger(Activate { entity: send });
        settle(&mut app);
        assert!(sent(&mut app).is_empty());
        assert!(texts(&mut app).iter().any(|t| t == "write the comment first"));
    }

    #[test]
    fn answering_opens_the_task_with_the_box_answering_and_sends_the_answer() {
        let mut app = app();
        app.world_mut().resource_mut::<Selection>().0 = None;
        settle(&mut app);
        let c1 = sid("c1");
        app.world_mut().trigger(OpenComposer { key: "TRAIL-7".into(), answer: Some((c1.clone(), "which db?".into())) });
        settle(&mut app);
        assert_eq!(app.world().resource::<Selection>().0, Some(Subject::Task(sid("t7"))));
        assert!(texts(&mut app).iter().any(|t| t == "Answering: which db?"));
        let input = composer_input(&mut app);
        assert_eq!(app.world().resource::<InputFocus>().get(), Some(input), "the keyboard is in the box");
        app.world_mut().get_mut::<EditableText>(input).unwrap().editor_mut().set_text("sqlite");
        let send = compose_button(&mut app, ComposePart::Send);
        app.world_mut().trigger(Activate { entity: send });
        let sent = sent(&mut app);
        assert_eq!(sent[0].request.path, format!("/v1/comments/{c1}/resolve"));
        assert_eq!(sent[0].request.body, Some(json!({"answer": "sqlite"})));
        // Stopping answering goes back to a comment.
        let stop = compose_button(&mut app, ComposePart::StopAnswering);
        app.world_mut().trigger(Activate { entity: stop });
        assert_eq!(app.world().resource::<Compose>().answering, None);
    }

    /// The test app with TRAIL-7 held by agent a1, and its open question, whose event makes it undoable.
    fn claimed_app() -> App {
        let mut app = app();
        let mut t7 = task_json(
            json!({"id": "t7", "key": "TRAIL-7", "title": "do it", "status": "in_progress", "priority": 1, "version": 4}),
        );
        t7["claim"] = claim_json(json!({"task_id": "t7", "holder_id": "a1"}));
        let snapshot = Snapshot {
            at: 6,
            projects: vec![],
            tasks: vec![t7],
            agents: vec![agent_json(json!({"id": "a1"}))],
            ..Default::default()
        };
        app.world_mut().write_message(SyncItem::Snapshot(snapshot));
        app.world_mut().write_message(question("c1", "which db?"));
        settle(&mut app);
        app
    }

    fn menu_item(app: &mut App, action: Action) -> Entity {
        let world = app.world_mut();
        let mut q = world.query::<(Entity, &MenuAct)>();
        q.iter(world).find(|(_, m)| m.0 == action).map(|(e, _)| e).expect("the menu item")
    }

    fn shows(app: &App, entity: Entity) -> bool {
        app.world().get::<Node>(entity).is_some_and(|n| n.display != Display::None)
    }

    #[test]
    fn the_menu_sends_what_the_task_offers_and_its_answer_shows_beside_it() {
        let mut app = claimed_app();
        for action in MENU_ACTIONS {
            let item = menu_item(&mut app, action.clone());
            assert!(shows(&app, item), "{action:?} is offered");
        }
        let press = |app: &mut App, action: Action| {
            let item = menu_item(app, action);
            app.world_mut().trigger(Activate { entity: item });
            app.update();
            sent(app).remove(0)
        };
        let release = press(&mut app, Action::Release);
        assert_eq!(release.request.path, "/v1/tasks/TRAIL-7/release");
        assert_eq!(app.world().get::<Control>(release.source), Some(&Control::Busy));
        let cancel = press(&mut app, Action::Cancel);
        assert_eq!(cancel.request.body, Some(json!({"status": "cancelled"})));
        assert_eq!(press(&mut app, Action::Undo).request.path, "/v1/undo");
        let revert = press(&mut app, Action::Revert(6));
        assert_eq!(revert.request.path, "/v1/revert");
        assert_eq!(revert.request.body.as_ref().unwrap()["actor_id"], json!(sid("a1")));
        assert_eq!(revert.source, release.source, "the menu's items share the one note beside it");
        *app.world_mut().get_mut::<Control>(revert.source).unwrap() = Control::Refused("not yours".into());
        settle(&mut app);
        let note = app.world().resource::<ModalUi>().tools.as_ref().unwrap().note;
        assert_eq!(app.world().get::<Text>(note).map(|t| t.0.as_str()), Some("not yours"));
        assert!(shows(&app, note));
    }

    #[test]
    fn the_menu_leaves_out_what_an_unheld_task_cant_do_and_goes_offline() {
        let mut app = app();
        let item = menu_item(&mut app, Action::Release);
        assert!(!shows(&app, item));
        let item = menu_item(&mut app, Action::Revert(1));
        assert!(!shows(&app, item));
        let item = menu_item(&mut app, Action::Cancel);
        assert!(shows(&app, item));
        let menu = app.world().resource::<ModalUi>().tools.as_ref().unwrap().menu;
        assert!(shows(&app, menu));
        app.world_mut().resource_mut::<NextState<Connection>>().set(Connection::Offline);
        settle(&mut app);
        assert!(!shows(&app, menu), "nothing to do offline");
    }

    #[test]
    fn the_sidebar_has_no_actions_now_the_menu_holds_them() {
        let mut app = claimed_app();
        let acts =
            app.world_mut().query::<&Shown>().iter(app.world()).filter(|s| matches!(s.0, Block::Act { .. })).count();
        assert_eq!(acts, 0);
    }

    fn arrow(app: &mut App, step: Step) -> Entity {
        let world = app.world_mut();
        let mut q = world.query::<(Entity, &Step)>();
        q.iter(world).find(|(_, s)| **s == step).map(|(e, _)| e).unwrap()
    }

    fn next_is_t8(app: &mut App) {
        *app.world_mut().resource_mut::<Neighbours>() = Neighbours { previous: None, next: Some(sid("t8")) };
        settle(app);
    }

    #[test]
    fn the_arrows_step_through_the_column_and_are_off_at_its_ends() {
        let mut app = app();
        next_is_t8(&mut app);
        let (previous, next) = (arrow(&mut app, Step::Previous), arrow(&mut app, Step::Next));
        assert!(app.world().get::<InteractionDisabled>(previous).is_some(), "nothing before the first");
        assert!(app.world().get::<InteractionDisabled>(next).is_none());
        app.world_mut().trigger(Activate { entity: next });
        settle(&mut app);
        assert_eq!(app.world().resource::<Selection>().0, Some(Subject::Task(sid("t8"))));
        assert_eq!(modal_count(&mut app), 1, "the same modal shows the next task");
    }

    fn key(app: &mut App, from: Entity, code: KeyCode) {
        let input = KeyboardInput {
            key_code: code,
            logical_key: bevy::input::keyboard::Key::Unidentified(bevy::input::keyboard::NativeKey::Unidentified),
            state: bevy::input::ButtonState::Pressed,
            text: None,
            repeat: false,
            window: from,
        };
        // A real window: the press climbs to it last and stops there. An entity without `Window` would be handed
        // the press again forever.
        let window = app.world_mut().spawn(Window::default()).id();
        app.world_mut().trigger(FocusedInput::new(from, input, window));
        settle(app);
    }

    #[test]
    fn j_and_k_step_through_the_column_from_inside_the_modal_but_not_while_typing() {
        let mut app = app();
        next_is_t8(&mut app);
        let input = composer_input(&mut app);
        key(&mut app, input, KeyCode::KeyJ);
        key(&mut app, input, KeyCode::ArrowDown);
        assert_eq!(app.world().resource::<Selection>().0, Some(Subject::Task(sid("t7"))), "typing in the box");
        let from = arrow(&mut app, Step::Previous);
        key(&mut app, from, KeyCode::KeyK);
        assert_eq!(app.world().resource::<Selection>().0, Some(Subject::Task(sid("t7"))), "nothing before it");
        key(&mut app, from, KeyCode::KeyJ);
        assert_eq!(app.world().resource::<Selection>().0, Some(Subject::Task(sid("t8"))));
    }

    fn editor(app: &mut App, field: Field) -> Entity {
        let world = app.world_mut();
        let mut q = world.query::<(Entity, &Edits)>();
        q.iter(world).find(|(_, e)| e.field == field).map(|(e, _)| e).expect("the field's editor")
    }

    fn answer(app: &mut App, source: Entity, outcome: Outcome) {
        app.world_mut().write_message(CommandDone { source, id: crate::commands::CommandId(0), outcome });
        settle(app);
    }

    #[test]
    fn each_field_sends_only_itself_on_the_version_shown() {
        let st = [Status::InProgress, Status::InReview];
        let r = |field, change| edit_request("TRAIL-7", 4, field, &change, &st).unwrap();
        let title = r(Field::Title, Change::Text("  New title ".into()));
        assert_eq!(
            (title.method.clone(), title.path.as_str(), title.if_match),
            (Method::PATCH, "/v1/tasks/TRAIL-7", Some(4))
        );
        assert_eq!(title.body, Some(json!({"title": "New title"})));
        assert_eq!(r(Field::Description, Change::Text("why".into())).body, Some(json!({"description": "why"})));
        assert_eq!(r(Field::Status, Change::Pick(1)).body, Some(json!({"status": "in_review"})));
        assert_eq!(r(Field::Priority, Change::Pick(0)).body, Some(json!({"priority": 1})));
        assert_eq!(r(Field::Grade, Change::Pick(2)).body, Some(json!({"grade": 2})));
        assert_eq!(r(Field::Grade, Change::Pick(0)).body, Some(json!({"grade": null})), "none clears it");
        assert!(edit_request("TRAIL-7", 4, Field::Title, &Change::Text(" ".into()), &st).is_err());
    }

    #[test]
    fn an_edited_title_is_sent_and_stays_once_it_went_through() {
        let mut app = app();
        let title = editor(&mut app, Field::Title);
        app.world_mut().trigger(InlineEdited { entity: title, old: "do it".into(), new: "do it well".into() });
        let sent = sent(&mut app);
        assert_eq!(sent.len(), 1);
        assert_eq!((sent[0].source, sent[0].request.if_match), (title, Some(4)));
        assert_eq!(sent[0].request.body, Some(json!({"title": "do it well"})));
        answer(&mut app, title, Outcome::Ok { version: Some(5) });
        assert!(app.world().get::<Edits>(title).unwrap().pending.is_none());
        assert!(app.world().get::<InlineText>(title).unwrap().error().is_empty());
    }

    #[test]
    fn a_conflict_fetches_again_says_so_and_keeps_what_was_typed() {
        let mut app = app();
        let description = editor(&mut app, Field::Description);
        app.world_mut().trigger(InlineEdited { entity: description, old: String::new(), new: "mine".into() });
        sent(&mut app);
        app.world_mut().resource_mut::<Messages<Refetch>>().clear();
        answer(&mut app, description, Outcome::Conflict { current_version: Some(5), message: "stale".into() });
        let t = app.world().get::<InlineText>(description).unwrap();
        assert_eq!(t.draft(), Some("mine"));
        assert!(t.error().contains("changed since you looked"), "{}", t.error());
        let asked = app.world_mut().resource_mut::<Messages<Refetch>>().drain().count();
        assert_eq!(asked, 1, "the task is fetched again");
    }

    #[test]
    fn a_refused_status_move_shows_its_reason_beside_the_field() {
        let mut app = app();
        let status = editor(&mut app, Field::Status);
        let statuses = app.world().get::<Edits>(status).unwrap().statuses.clone();
        assert_eq!(statuses[0], Status::InProgress, "its own status first");
        let to = statuses.iter().position(|s| *s == Status::NeedsInput).unwrap();
        app.world_mut().trigger(InlineChosen { entity: status, old: Some(0), new: to });
        let sent = sent(&mut app);
        assert_eq!(sent[0].request.body, Some(json!({"status": "needs_input"})));
        answer(&mut app, status, Outcome::Failed("say why it's blocked".into()));
        let f = app.world().get::<InlineField>(status).unwrap();
        assert_eq!((f.selected(), f.error()), (Some(0), "say why it's blocked"));
    }

    #[test]
    fn offline_the_fields_are_text_and_add_sub_task_is_off() {
        let mut app = app();
        assert!(app.world_mut().query::<&Edits>().iter(app.world()).count() >= 5);
        app.world_mut().resource_mut::<NextState<Connection>>().set(Connection::Offline);
        settle(&mut app);
        assert_eq!(app.world_mut().query::<&Edits>().iter(app.world()).count(), 0);
        let add = app.world_mut().query_filtered::<Entity, With<Act>>().iter(app.world()).collect::<Vec<_>>();
        let world = app.world();
        assert!(add.iter().any(|e| world.get::<Act>(*e) == Some(&Act::NewSubtask("TRAIL-7".into()))
            && world.get::<InteractionDisabled>(*e).is_some()));
    }
}
