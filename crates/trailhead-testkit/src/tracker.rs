use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use trailhead_factory::{
    Answered, Credential, Error, FollowUpTask, Grade, HumanContext, Priority, Result, RunUpdate, Status, Task, TaskKey,
    TaskNote, Tracker, Verdict, VerdictKind, is_factory_agent, is_raised_by,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeComment {
    pub author: String,
    pub kind: String,
    pub body: String,
    /// A review's verdict; `None` on every other kind.
    pub verdict: Option<VerdictKind>,
}

#[derive(Default)]
struct Agent {
    tokens: Vec<String>,
    revoked: bool,
}

#[derive(Default)]
struct State {
    tasks: BTreeMap<TaskKey, Task>,
    comments: HashMap<TaskKey, Vec<FakeComment>>,
    agents: HashMap<String, Agent>,
    next: HashMap<String, u32>,
    parents: HashMap<TaskKey, TaskKey>,
    priorities: HashMap<TaskKey, Priority>,
    context_fails: bool,
    /// Agents refused when they approve, as ones a human hasn't listed as the project's reviewers would be.
    undelegated: HashSet<String>,
    /// Agent and operation to how many of that agent's calls to let through, then how many more fail.
    failing: HashMap<(String, TrackerOp), (usize, usize)>,
    /// Whether every `report`, by any agent, fails.
    reports_fail: bool,
    finish_unapproved_forbidden: bool,
    /// Whether `create_sub_agent` fails with a plain error, not `Forbidden`.
    sub_agents_fail: bool,
    /// The message `create_sub_agent` is refused with as `Rejected`, a permanent refusal.
    sub_agents_rejected: Option<String>,
    /// How many times each task has been read with `show`.
    shows: HashMap<TaskKey, usize>,
    renews: HashMap<TaskKey, usize>,
    /// How many times each task has been reported on, failed or not.
    reports: HashMap<TaskKey, usize>,
    /// Tasks whose current claim was taken from review, which a release or lapse returns there while the review
    /// still asks for changes.
    claimed_from_review: HashSet<TaskKey>,
}

/// The tracker calls `fail_next` and `fail_after` can make fail; each is checked by one `inject` call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrackerOp {
    Claim,
    Release,
    Report,
    RequestReview,
    Reopen,
    Ask,
    Note,
    Finish,
    RevokeSubAgent,
}

impl std::fmt::Display for TrackerOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Claim => "claim",
            Self::Release => "release",
            Self::Report => "report",
            Self::RequestReview => "request_review",
            Self::Reopen => "reopen",
            Self::Ask => "ask",
            Self::Note => "note",
            Self::Finish => "finish",
            Self::RevokeSubAgent => "revoke_sub_agent",
        })
    }
}

/// Trailhead's claim and status rules, in memory. Each handle acts as one
/// agent; sub-agents are `<parent>/<name>` and revoking one revokes everything under it.
#[derive(Clone)]
pub struct FakeTracker {
    state: Arc<Mutex<State>>,
    agent: String,
}

impl FakeTracker {
    /// A tracker with one root agent, which the returned handle acts as.
    pub fn new(agent: &str) -> FakeTracker {
        let state = State { agents: HashMap::from([(agent.to_string(), Agent::default())]), ..State::default() };
        FakeTracker { state: Arc::new(Mutex::new(state)), agent: agent.to_string() }
    }

    /// A handle for whoever holds this token, as a daemon would get from its credential.
    pub fn authenticate(&self, cred: &Credential) -> Result<FakeTracker> {
        let s = self.state.lock().unwrap();
        match s.agents.get(&cred.agent) {
            Some(a) if a.tokens.contains(&cred.token) => {
                Ok(FakeTracker { state: self.state.clone(), agent: cred.agent.clone() })
            }
            _ => Err(Error::Forbidden("bad token".into())),
        }
    }

    /// A subtask of `parent`, as handed to a factory with a queue.
    pub fn add_subtask(&self, parent: &TaskKey, title: &str, description: &str) -> TaskKey {
        let project = parent.project().to_string();
        let key = self.add_task(&project, title, description);
        self.state.lock().unwrap().parents.insert(key.clone(), parent.clone());
        key
    }

    /// Takes a task off its queue, as `edit --parent none` does.
    pub fn detach(&self, task: &TaskKey) {
        self.state.lock().unwrap().parents.remove(task);
    }

    pub fn add_task(&self, project: &str, title: &str, description: &str) -> TaskKey {
        let mut s = self.state.lock().unwrap();
        let n = s.next.entry(project.to_string()).or_insert(0);
        *n += 1;
        let key = TaskKey(format!("{project}-{n}"));
        s.tasks.insert(
            key.clone(),
            Task {
                key: key.clone(),
                project: project.to_string(),
                title: title.to_string(),
                description: description.to_string(),
                status: Status::Open,
                grade: None,
                priority: Priority::default(),
                holder: None,
                changes_requested: false,
                run: RunUpdate::default(),
            },
        );
        key
    }

    /// Makes reading a task's human context fail (or heals it), as a tracker outage would.
    pub fn fail_human_context(&self, fails: bool) {
        self.state.lock().unwrap().context_fails = fails;
    }

    /// Makes the next `times` calls `agent` makes to `op` fail, as a tracker outage would.
    pub fn fail_next(&self, agent: &str, op: TrackerOp, times: usize) {
        self.fail_after(agent, op, 0, times);
    }

    /// Like `fail_next`, but lets the first `skip` calls through, for a failure that should hit a later caller of `op` acting as the same agent.
    pub fn fail_after(&self, agent: &str, op: TrackerOp, skip: usize, times: usize) {
        self.state.lock().unwrap().failing.insert((agent.to_string(), op), (skip, times));
    }

    /// Makes every `report`, whichever agent makes it, fail (or heal), as the tracker write outage of 2026-10-28 did.
    pub fn fail_reports(&self, fails: bool) {
        self.state.lock().unwrap().reports_fail = fails;
    }

    /// Makes finishing a task still in review refuse with `Forbidden`, as core does when the factory did the work
    /// and nobody approved it.
    pub fn forbid_finishing_unapproved(&self, forbids: bool) {
        self.state.lock().unwrap().finish_unapproved_forbidden = forbids;
    }

    /// Makes `create_sub_agent` fail (or heal) with an error that isn't `Forbidden`, as a whole-tracker outage does.
    pub fn fail_sub_agents(&self, fails: bool) {
        self.state.lock().unwrap().sub_agents_fail = fails;
    }

    /// Makes `create_sub_agent` refuse for good with `Error::Rejected` carrying `message`.
    pub fn reject_sub_agents(&self, message: &str) {
        self.state.lock().unwrap().sub_agents_rejected = Some(message.to_string());
    }

    fn inject(&self, op: TrackerOp) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        if op == TrackerOp::Report && s.reports_fail {
            return Err(Error::Other("injected report failure".into()));
        }
        match s.failing.get_mut(&(self.agent.clone(), op)) {
            Some((skip, _)) if *skip > 0 => {
                *skip -= 1;
                Ok(())
            }
            Some((_, left)) if *left > 0 => {
                *left -= 1;
                Err(Error::Other(format!("injected {op} failure")))
            }
            _ => Ok(()),
        }
    }

    /// A human grades a task, which is what makes it the factory's work.
    pub fn set_grade(&self, key: &TaskKey, grade: u8) {
        self.state.lock().unwrap().tasks.get_mut(key).unwrap().grade =
            Some(Grade::new(grade).expect("a grade is 1 to 4"));
    }

    /// Sets a task's priority (1 is highest), as a person editing it would.
    pub fn set_priority(&self, key: &TaskKey, priority: u8) {
        self.state.lock().unwrap().tasks.get_mut(key).unwrap().priority =
            Priority::new(priority).expect("a priority is 1 to 4");
    }

    /// Sets the factory fields of a task as they stand: a PR and changes requested on it make a rework.
    pub fn set_run(&self, key: &TaskKey, run: RunUpdate) {
        self.state.lock().unwrap().tasks.get_mut(key).unwrap().run = run;
    }

    /// A human takes a task's grade away again.
    pub fn clear_grade(&self, key: &TaskKey) {
        self.state.lock().unwrap().tasks.get_mut(key).unwrap().grade = None;
    }

    /// Whether this agent may approve tasks, as a delegated reviewer can; every agent may until told otherwise.
    pub fn set_delegated(&self, agent: &str, delegated: bool) {
        let mut s = self.state.lock().unwrap();
        match delegated {
            true => s.undelegated.remove(agent),
            false => s.undelegated.insert(agent.to_string()),
        };
    }

    /// A person approves a task in review, as with `trailhead approve`.
    pub fn approve_as_human(&self, key: &TaskKey) {
        let mut s = self.state.lock().unwrap();
        let t = s.tasks.get_mut(key).unwrap();
        assert_eq!(t.status, Status::InReview, "{} isn't in review", key.0);
        t.status = Status::Approved;
    }

    /// How many times `show` has read the task.
    pub fn shows(&self, key: &TaskKey) -> usize {
        self.state.lock().unwrap().shows.get(key).copied().unwrap_or(0)
    }

    /// How many times the task's claim has been renewed.
    pub fn renews(&self, key: &TaskKey) -> usize {
        self.state.lock().unwrap().renews.get(key).copied().unwrap_or(0)
    }

    /// How many times `report` has been called on the task, failed or not.
    pub fn reports(&self, key: &TaskKey) -> usize {
        self.state.lock().unwrap().reports.get(key).copied().unwrap_or(0)
    }

    pub fn task(&self, key: &TaskKey) -> Task {
        let s = self.state.lock().unwrap();
        flagged(&s, &s.tasks[key])
    }

    /// Every task in `project`, oldest first.
    pub fn tasks_in(&self, project: &str) -> Vec<Task> {
        let s = self.state.lock().unwrap();
        let mut tasks: Vec<Task> = s.tasks.values().filter(|t| t.project == project).cloned().collect();
        tasks.sort_by_key(|t| t.key.0.rsplit_once('-').and_then(|(_, n)| n.parse::<u32>().ok()));
        tasks
    }

    /// The priority a task was filed with, if it was given one.
    pub fn priority(&self, key: &TaskKey) -> Option<Priority> {
        self.state.lock().unwrap().priorities.get(key).copied()
    }

    pub fn comments(&self, key: &TaskKey) -> Vec<FakeComment> {
        self.state.lock().unwrap().comments.get(key).cloned().unwrap_or_default()
    }

    /// A person's `decision` comment on a task, as from the CLI.
    pub fn decide(&self, key: &TaskKey, body: &str) {
        push_comment(&mut self.state.lock().unwrap(), key, "lane", "decision", body);
    }

    pub fn is_revoked(&self, agent: &str) -> bool {
        revoked(&self.state.lock().unwrap(), agent)
    }

    /// Every agent ever registered, revoked or not.
    pub fn agents(&self) -> Vec<String> {
        let mut names: Vec<String> = self.state.lock().unwrap().agents.keys().cloned().collect();
        names.sort();
        names
    }

    /// A human answers a parked task. The agent's lease has lapsed by then, so it goes back to the queue.
    pub fn answer(&self, key: &TaskKey, body: &str) {
        let mut s = self.state.lock().unwrap();
        let t = s.tasks.get_mut(key).unwrap();
        t.status = Status::Open;
        t.holder = None;
        push_comment(&mut s, key, "lane", "answer", body);
    }

    /// The asker's lease on a parked task lapses: nobody holds it any more, and it stays parked.
    pub fn lapse_lease(&self, key: &TaskKey) {
        self.state.lock().unwrap().tasks.get_mut(key).unwrap().holder = None;
    }

    /// The holder's lease on an in-progress task runs out, as in core: nobody holds it any more, and it goes back to
    /// open, or to review when it was claimed from review and the review still asks for changes.
    pub fn expire_claim(&self, key: &TaskKey) {
        let mut s = self.state.lock().unwrap();
        let back = s.claimed_from_review.remove(key) && review_asks_changes(&s, key);
        let t = s.tasks.get_mut(key).unwrap();
        assert_eq!(t.status, Status::InProgress, "{} isn't in progress", key.0);
        t.holder = None;
        t.status = if back { Status::InReview } else { Status::Open };
    }

    /// A human answers while the asker's claim is still live: the task goes back to in progress under the same holder.
    pub fn answer_within_lease(&self, key: &TaskKey, body: &str) {
        let mut s = self.state.lock().unwrap();
        s.tasks.get_mut(key).unwrap().status = Status::InProgress;
        push_comment(&mut s, key, "lane", "answer", body);
    }

    /// A human moves a task, as from the CLI.
    pub fn set_status(&self, key: &TaskKey, status: Status) {
        let mut s = self.state.lock().unwrap();
        let t = s.tasks.get_mut(key).unwrap();
        t.status = status;
        t.holder = None;
    }

    fn write<T>(&self, key: &TaskKey, f: impl FnOnce(&mut Task, &str) -> Result<T>) -> Result<T> {
        let mut s = self.state.lock().unwrap();
        if revoked(&s, &self.agent) {
            return Err(Error::Forbidden(format!("{} has been revoked", self.agent)));
        }
        let t = s.tasks.get_mut(key).ok_or_else(|| Error::NotFound(key.0.clone()))?;
        f(t, &self.agent)
    }

    fn comment(&self, key: &TaskKey, kind: &str, body: &str) {
        push_comment(&mut self.state.lock().unwrap(), key, &self.agent, kind, body);
    }

    fn check_live(&self) -> Result<()> {
        if self.is_revoked(&self.agent) {
            return Err(Error::Forbidden(format!("{} has been revoked", self.agent)));
        }
        Ok(())
    }
}

/// Whether the newest review on the task asks for changes and no handoff has come since, whatever its status.
fn review_asks_changes(s: &State, key: &TaskKey) -> bool {
    let comments = s.comments.get(key).map(Vec::as_slice).unwrap_or_default();
    let Some(i) = comments.iter().rposition(|c| c.kind == "review" && c.verdict.is_some()) else { return false };
    comments[i].verdict == Some(VerdictKind::RequestChanges) && !comments[i + 1..].iter().any(|c| c.kind == "handoff")
}

/// The task as the tracker shows it: in review with changes requested, or not.
fn flagged(s: &State, t: &Task) -> Task {
    Task { changes_requested: t.status == Status::InReview && review_asks_changes(s, &t.key), ..t.clone() }
}

fn revoked(s: &State, agent: &str) -> bool {
    let mut name = agent;
    loop {
        if s.agents.get(name).is_none_or(|a| a.revoked) {
            return true;
        }
        match name.rsplit_once('/') {
            Some((parent, _)) => name = parent,
            None => return false,
        }
    }
}

fn push_comment(s: &mut State, key: &TaskKey, author: &str, kind: &str, body: &str) {
    s.comments.entry(key.clone()).or_default().push(FakeComment {
        author: author.to_string(),
        kind: kind.to_string(),
        body: body.to_string(),
        verdict: None,
    });
}

/// Core's `Status::can_transition_to`, which the fake must agree with.
fn can_transition(from: Status, to: Status) -> bool {
    use Status::*;
    if from == to {
        return true;
    }
    match from {
        Open => matches!(to, InProgress | NeedsInput | InReview | Done | Cancelled),
        InProgress => matches!(to, Open | NeedsInput | InReview | Done | Cancelled),
        NeedsInput => matches!(to, Open | InProgress | InReview | Cancelled),
        InReview => matches!(to, Open | InProgress | NeedsInput | Approved | Done | Cancelled),
        Approved => matches!(to, Open | InReview | Done | Cancelled),
        Done | Cancelled => matches!(to, Open),
    }
}

fn held_by_me(t: &Task, me: &str) -> Result<()> {
    match &t.holder {
        Some(h) if h == me => Ok(()),
        Some(h) => Err(Error::Conflict(format!("{} is held by {h}", t.key.0))),
        None => Err(Error::Conflict(format!("{} isn't claimed", t.key.0))),
    }
}

#[async_trait]
impl Tracker for FakeTracker {
    fn agent(&self) -> &str {
        &self.agent
    }

    fn sub_agent(&self, name: &str) -> Arc<dyn Tracker> {
        Arc::new(FakeTracker { state: self.state.clone(), agent: format!("{}/{name}", self.agent) })
    }

    async fn ready(&self, project: &str, queue: &[TaskKey], limit: usize) -> Result<Vec<Task>> {
        self.check_live()?;
        let s = self.state.lock().unwrap();
        Ok(s.tasks
            .values()
            .map(|t| flagged(&s, t))
            .filter(|t| t.project == project && t.holder.is_none() && (t.status == Status::Open || t.changes_requested))
            .filter(|t| queue.is_empty() || s.parents.get(&t.key).is_some_and(|p| queue.contains(p)))
            .take(limit)
            .collect())
    }

    async fn show(&self, task: &TaskKey) -> Result<Task> {
        self.check_live()?;
        let mut s = self.state.lock().unwrap();
        *s.shows.entry(task.clone()).or_default() += 1;
        s.tasks.get(task).map(|t| flagged(&s, t)).ok_or_else(|| Error::NotFound(task.0.clone()))
    }

    async fn human_context(&self, task: &TaskKey) -> Result<HumanContext> {
        self.check_live()?;
        let s = self.state.lock().unwrap();
        if s.context_fails {
            return Err(Error::Other("tracker unavailable".into()));
        }
        let mut out = HumanContext::default();
        // The fake dates everything the same; answers follow their question.
        let date = "2026-01-01".to_string();
        let comments = s.comments.get(task).map(Vec::as_slice).unwrap_or_default();
        for (i, c) in comments.iter().enumerate() {
            match c.kind.as_str() {
                "question" => {
                    let answer =
                        comments[i + 1..].iter().take_while(|n| n.kind != "question").find(|n| n.kind == "answer");
                    if let Some(a) = answer {
                        out.answers.push(Answered {
                            question: c.body.clone(),
                            answer: a.body.clone(),
                            by: a.author.clone(),
                            date: date.clone(),
                        });
                    }
                }
                "decision" | "note" if !is_factory_agent(&c.author) => {
                    out.notes.push(TaskNote { author: c.author.clone(), date: date.clone(), body: c.body.clone() });
                }
                _ => {}
            }
        }
        Ok(out)
    }

    async fn last_review(&self, task: &TaskKey) -> Result<Option<Verdict>> {
        self.check_live()?;
        let s = self.state.lock().unwrap();
        let comments = s.comments.get(task).map(Vec::as_slice).unwrap_or_default();
        Ok(comments.iter().rev().find(|c| c.kind == "review").and_then(|c| {
            let (kind, body) = c.body.split_once(": ")?;
            let kind = match kind {
                "approve" => VerdictKind::Approve,
                "request_changes" => VerdictKind::RequestChanges,
                _ => return None,
            };
            Some(Verdict { kind, body: body.to_string(), reviewer: c.author.clone(), model: None, raise_grade: None })
        }))
    }

    async fn in_flight(&self, project: &str, queue: &[TaskKey]) -> Result<Vec<Task>> {
        self.check_live()?;
        let s = self.state.lock().unwrap();
        Ok(s.tasks
            .values()
            .filter(|t| {
                t.project == project && !t.run.is_empty() && !matches!(t.status, Status::Done | Status::Cancelled)
            })
            .filter(|t| queue.is_empty() || s.parents.get(&t.key).is_some_and(|p| queue.contains(p)))
            .map(|t| flagged(&s, t))
            .collect())
    }

    async fn strays(&self, project: &str, queues: &[TaskKey]) -> Result<Vec<Task>> {
        self.check_live()?;
        let s = self.state.lock().unwrap();
        Ok(s.tasks
            .values()
            .filter(|t| {
                t.project == project && !t.run.is_empty() && !matches!(t.status, Status::Done | Status::Cancelled)
            })
            .filter(|t| !s.parents.get(&t.key).is_some_and(|p| queues.contains(p)))
            .map(|t| flagged(&s, t))
            .collect())
    }

    async fn claim(&self, task: &TaskKey) -> Result<()> {
        self.inject(TrackerOp::Claim)?;
        let from_review = self
            .state
            .lock()
            .unwrap()
            .tasks
            .get(task)
            .is_some_and(|t| t.status == Status::InReview && t.holder.is_none());
        let claimed = self.write(task, |t, me| match (&t.holder, t.status) {
            (Some(h), Status::InProgress) if h == me => Ok(()),
            // Holding a parked or in-review task and claiming it moves it to `in_progress`, as in core.
            (Some(h), Status::NeedsInput | Status::InReview) if h == me => {
                t.status = Status::InProgress;
                Ok(())
            }
            // As in core, parked and in-review work is claimable without passing through `open`.
            (None, Status::Open | Status::NeedsInput | Status::InReview) => {
                t.holder = Some(me.to_string());
                t.status = Status::InProgress;
                Ok(())
            }
            _ => Err(Error::Conflict(format!("{} isn't claimable", t.key.0))),
        });
        if claimed.is_ok() && from_review {
            self.state.lock().unwrap().claimed_from_review.insert(task.clone());
        }
        claimed
    }

    async fn renew(&self, task: &TaskKey) -> Result<()> {
        *self.state.lock().unwrap().renews.entry(task.clone()).or_default() += 1;
        self.write(task, |t, me| held_by_me(t, me))
    }

    async fn release(&self, task: &TaskKey) -> Result<()> {
        self.inject(TrackerOp::Release)?;
        // As in core, a rework that is given back returns to review while the review still asks for changes.
        let back = {
            let s = self.state.lock().unwrap();
            s.claimed_from_review.contains(task) && review_asks_changes(&s, task)
        };
        let released = self.write(task, |t, me| {
            held_by_me(t, me)?;
            t.holder = None;
            t.status = if back { Status::InReview } else { Status::Open };
            Ok(())
        });
        if released.is_ok() {
            self.state.lock().unwrap().claimed_from_review.remove(task);
        }
        released
    }

    async fn report(&self, task: &TaskKey, update: RunUpdate) -> Result<()> {
        *self.state.lock().unwrap().reports.entry(task.clone()).or_default() += 1;
        self.inject(TrackerOp::Report)?;
        self.write(task, |t, me| {
            if t.holder.as_deref().is_some_and(|h| h != me) {
                return Err(Error::Forbidden(format!("{} is held by another agent", t.key.0)));
            }
            t.run.merge(update);
            Ok(())
        })
    }

    async fn request_review(&self, task: &TaskKey, handoff: &str) -> Result<()> {
        self.inject(TrackerOp::RequestReview)?;
        self.write(task, |t, me| {
            held_by_me(t, me)?;
            t.holder = None;
            t.status = Status::InReview;
            Ok(())
        })?;
        self.comment(task, "handoff", handoff);
        Ok(())
    }

    async fn handoff(&self, task: &TaskKey, body: &str) -> Result<()> {
        self.check_live()?;
        self.comment(task, "handoff", body);
        Ok(())
    }

    async fn approve(&self, task: &TaskKey) -> Result<()> {
        let delegated = !self.state.lock().unwrap().undelegated.contains(&self.agent);
        self.write(task, |t, me| {
            if !delegated {
                return Err(Error::Forbidden(trailhead_factory::messages::not_a_reviewer(
                    me,
                    &t.key.0,
                    t.key.project(),
                )));
            }
            if t.status != Status::InReview {
                return Err(Error::Conflict(format!("{} isn't in review", t.key.0)));
            }
            t.status = Status::Approved;
            Ok(())
        })
    }

    async fn unapprove(&self, task: &TaskKey) -> Result<()> {
        self.write(task, |t, _| {
            if t.status != Status::Approved {
                return Err(Error::Conflict(format!("{} isn't approved", t.key.0)));
            }
            t.status = Status::InReview;
            Ok(())
        })
    }

    async fn reopen(&self, task: &TaskKey) -> Result<()> {
        self.inject(TrackerOp::Reopen)?;
        self.write(task, |t, me| {
            // Like core's `check_claim_access`: not a task another agent holds.
            if t.holder.as_deref().is_some_and(|h| h != me) {
                return Err(Error::Forbidden(format!("{} is held by another agent", t.key.0)));
            }
            if !can_transition(t.status, Status::Open) {
                return Err(Error::Conflict(format!("{} can't be reopened from {:?}", t.key.0, t.status)));
            }
            t.holder = None;
            t.status = Status::Open;
            Ok(())
        })
    }

    async fn raise_grade(&self, task: &TaskKey, grade: Grade) -> Result<()> {
        self.write(task, |t, _| match t.grade {
            None => Err(Error::Forbidden("only a human can set a task's first grade".into())),
            Some(g) if grade < g => Err(Error::Forbidden("only a human can lower or clear a task's grade".into())),
            Some(_) => {
                t.grade = Some(grade);
                Ok(())
            }
        })
    }

    async fn record_verdict(&self, task: &TaskKey, verdict: &Verdict) -> Result<()> {
        self.write(task, |t, _| {
            if t.status != Status::InReview {
                return Err(Error::Conflict(format!("{} isn't in review", t.key.0)));
            }
            Ok(())
        })?;
        let kind = match verdict.kind {
            VerdictKind::Approve => "approve",
            VerdictKind::RequestChanges => "request_changes",
        };
        self.comment(task, "review", &format!("{kind}: {}", verdict.body));
        if let Some(c) = self.state.lock().unwrap().comments.get_mut(task).and_then(|c| c.last_mut()) {
            c.verdict = Some(verdict.kind);
        }
        Ok(())
    }

    async fn ask(&self, task: &TaskKey, question: &str) -> Result<()> {
        self.inject(TrackerOp::Ask)?;
        self.write(task, |t, me| {
            held_by_me(t, me)?;
            t.status = Status::NeedsInput;
            Ok(())
        })?;
        self.comment(task, "question", question);
        Ok(())
    }

    async fn note(&self, task: &TaskKey, body: &str) -> Result<()> {
        self.inject(TrackerOp::Note)?;
        self.check_live()?;
        self.comment(task, "note", body);
        Ok(())
    }

    async fn finish(&self, task: &TaskKey, handoff: &str) -> Result<()> {
        self.inject(TrackerOp::Finish)?;
        if self.state.lock().unwrap().finish_unapproved_forbidden
            && self.state.lock().unwrap().tasks.get(task).is_some_and(|t| t.status == Status::InReview)
        {
            return Err(Error::Forbidden(
                "a reviewer can't approve or finish work it did; another reviewer or a human has to".into(),
            ));
        }
        self.write(task, |t, me| {
            // Like core: review states need no claim, and an unclaimed task can be finished by anyone, but
            // not one another agent holds.
            if !matches!(t.status, Status::InReview | Status::Approved) && t.holder.is_some() {
                held_by_me(t, me)?;
            }
            if !can_transition(t.status, Status::Done) {
                return Err(Error::Conflict(format!("{} can't be finished from {:?}", t.key.0, t.status)));
            }
            t.holder = None;
            t.status = Status::Done;
            Ok(())
        })?;
        self.comment(task, "handoff", handoff);
        Ok(())
    }

    async fn create_task(
        &self,
        project: &str,
        title: &str,
        description: &str,
        priority: Option<Priority>,
    ) -> Result<TaskKey> {
        self.check_live()?;
        let key = self.add_task(project, title, description);
        if let Some(p) = priority {
            self.state.lock().unwrap().priorities.insert(key.clone(), p);
        }
        Ok(key)
    }

    async fn follow_ups(&self, task: &TaskKey) -> Result<Vec<FollowUpTask>> {
        self.check_live()?;
        let s = self.state.lock().unwrap();
        Ok(s.tasks
            .values()
            .filter(|t| {
                !matches!(t.status, Status::Done | Status::Cancelled)
                    && t.key != *task
                    && is_raised_by(&t.description, task)
            })
            .map(|t| FollowUpTask { key: t.key.clone(), title: t.title.clone(), description: t.description.clone() })
            .collect())
    }

    async fn find_open_by_title(&self, project: &str, title: &str) -> Result<Option<TaskKey>> {
        self.check_live()?;
        let s = self.state.lock().unwrap();
        Ok(s.tasks
            .values()
            .find(|t| {
                t.project == project
                    && !matches!(t.status, Status::Done | Status::Cancelled)
                    && t.title.trim().eq_ignore_ascii_case(title.trim())
            })
            .map(|t| t.key.clone()))
    }

    async fn create_sub_agent(&self, name: &str, _project: &str, _ttl: Duration) -> Result<Credential> {
        self.check_live()?;
        let full = format!("{}/{name}", self.agent);
        let mut s = self.state.lock().unwrap();
        if let Some(message) = &s.sub_agents_rejected {
            return Err(Error::Rejected(message.clone()));
        }
        if s.sub_agents_fail {
            return Err(Error::Other("injected sub-agent failure".into()));
        }
        let agent = s.agents.entry(full.clone()).or_default();
        if agent.revoked {
            return Err(Error::Forbidden(format!("{full} has been revoked")));
        }
        let token = format!("tok-{full}-{}", agent.tokens.len() + 1);
        agent.tokens.push(token.clone());
        Ok(Credential { agent: full, token })
    }

    async fn revoke_sub_agent(&self, name: &str) -> Result<()> {
        self.check_live()?;
        self.inject(TrackerOp::RevokeSubAgent)?;
        let full = format!("{}/{name}", self.agent);
        let mut s = self.state.lock().unwrap();
        for (n, a) in s.agents.iter_mut() {
            if *n == full || n.starts_with(&format!("{full}/")) {
                a.revoked = true;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn claim_takes_unheld_open_needs_input_and_in_review_tasks_like_core() {
        for status in [Status::Open, Status::NeedsInput, Status::InReview] {
            let tracker = FakeTracker::new("factory");
            let key = tracker.add_task("TRAIL", "t", "d");
            tracker.state.lock().unwrap().tasks.get_mut(&key).unwrap().status = status;
            tracker.claim(&key).await.unwrap();
            let s = tracker.state.lock().unwrap();
            let t = &s.tasks[&key];
            assert_eq!(t.status, Status::InProgress, "claiming from {status:?}");
            assert_eq!(t.holder.as_deref(), Some("factory"));
        }
    }

    #[tokio::test]
    async fn claim_refuses_approved_and_closed_tasks() {
        for status in [Status::Approved, Status::Done, Status::Cancelled] {
            let tracker = FakeTracker::new("factory");
            let key = tracker.add_task("TRAIL", "t", "d");
            tracker.state.lock().unwrap().tasks.get_mut(&key).unwrap().status = status;
            assert!(matches!(tracker.claim(&key).await, Err(Error::Conflict(_))), "claiming from {status:?}");
        }
    }

    #[tokio::test]
    async fn reopen_refuses_a_task_another_agent_holds_like_core() {
        let tracker = FakeTracker::new("factory");
        let key = tracker.add_task("TRAIL", "t", "d");
        {
            let mut s = tracker.state.lock().unwrap();
            let t = s.tasks.get_mut(&key).unwrap();
            t.status = Status::NeedsInput;
            t.holder = Some("someone-else".into());
        }
        assert!(matches!(tracker.reopen(&key).await, Err(Error::Forbidden(_))));
        let s = tracker.state.lock().unwrap();
        assert_eq!(s.tasks[&key].status, Status::NeedsInput);
        assert_eq!(s.tasks[&key].holder.as_deref(), Some("someone-else"));
    }

    #[tokio::test]
    async fn reopen_clears_the_callers_own_claim() {
        let tracker = FakeTracker::new("factory");
        let key = tracker.add_task("TRAIL", "t", "d");
        {
            let mut s = tracker.state.lock().unwrap();
            let t = s.tasks.get_mut(&key).unwrap();
            t.status = Status::NeedsInput;
            t.holder = Some("factory".into());
        }
        tracker.reopen(&key).await.unwrap();
        let s = tracker.state.lock().unwrap();
        assert_eq!(s.tasks[&key].status, Status::Open);
        assert_eq!(s.tasks[&key].holder, None);
    }
}
