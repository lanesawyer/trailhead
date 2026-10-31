use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use trailhead_factory::{
    Answered, Credential, Error, HumanContext, Result, RunUpdate, Status, Task, TaskKey, TaskNote, Tracker, Verdict,
    VerdictKind, is_factory_agent,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeComment {
    pub author: String,
    pub kind: String,
    pub body: String,
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
    priorities: HashMap<TaskKey, i64>,
    context_fails: bool,
    /// Agents refused when they approve, as ones a human hasn't listed as the project's reviewers would be.
    undelegated: HashSet<String>,
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
        let project = parent.0.rsplit_once('-').map(|(p, _)| p.to_string()).unwrap_or_default();
        let key = self.add_task(&project, title, description);
        self.state.lock().unwrap().parents.insert(key.clone(), parent.clone());
        key
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
                holder: None,
                run: RunUpdate::default(),
            },
        );
        key
    }

    /// Makes reading a task's human context fail (or heals it), as a tracker outage would.
    pub fn fail_human_context(&self, fails: bool) {
        self.state.lock().unwrap().context_fails = fails;
    }

    /// A human grades a task, which is what makes it the factory's work.
    pub fn set_grade(&self, key: &TaskKey, grade: u8) {
        self.state.lock().unwrap().tasks.get_mut(key).unwrap().grade = Some(grade);
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

    pub fn task(&self, key: &TaskKey) -> Task {
        self.state.lock().unwrap().tasks[key].clone()
    }

    /// Every task in `project`, oldest first.
    pub fn tasks_in(&self, project: &str) -> Vec<Task> {
        let s = self.state.lock().unwrap();
        let mut tasks: Vec<Task> = s.tasks.values().filter(|t| t.project == project).cloned().collect();
        tasks.sort_by_key(|t| t.key.0.rsplit_once('-').and_then(|(_, n)| n.parse::<u32>().ok()));
        tasks
    }

    /// The priority a task was filed with, if it was given one.
    pub fn priority(&self, key: &TaskKey) -> Option<i64> {
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
    });
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

    async fn ready(&self, project: &str, queue: Option<&TaskKey>, limit: usize) -> Result<Vec<Task>> {
        self.check_live()?;
        let s = self.state.lock().unwrap();
        Ok(s.tasks
            .values()
            .filter(|t| t.project == project && t.status == Status::Open && t.holder.is_none())
            .filter(|t| queue.is_none_or(|q| s.parents.get(&t.key) == Some(q)))
            .take(limit)
            .cloned()
            .collect())
    }

    async fn show(&self, task: &TaskKey) -> Result<Task> {
        self.check_live()?;
        let s = self.state.lock().unwrap();
        s.tasks.get(task).cloned().ok_or_else(|| Error::NotFound(task.0.clone()))
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

    async fn in_flight(&self, project: &str, queue: Option<&TaskKey>) -> Result<Vec<Task>> {
        self.check_live()?;
        let s = self.state.lock().unwrap();
        Ok(s.tasks
            .values()
            .filter(|t| {
                t.project == project && !t.run.is_empty() && !matches!(t.status, Status::Done | Status::Cancelled)
            })
            .filter(|t| queue.is_none_or(|q| s.parents.get(&t.key) == Some(q)))
            .cloned()
            .collect())
    }

    async fn claim(&self, task: &TaskKey) -> Result<()> {
        self.write(task, |t, me| match (&t.holder, t.status) {
            (Some(h), _) if h == me => Ok(()),
            (None, Status::Open) => {
                t.holder = Some(me.to_string());
                t.status = Status::InProgress;
                Ok(())
            }
            _ => Err(Error::Conflict(format!("{} isn't claimable", t.key.0))),
        })
    }

    async fn renew(&self, task: &TaskKey) -> Result<()> {
        self.write(task, |t, me| held_by_me(t, me))
    }

    async fn release(&self, task: &TaskKey) -> Result<()> {
        self.write(task, |t, me| {
            held_by_me(t, me)?;
            t.holder = None;
            t.status = Status::Open;
            Ok(())
        })
    }

    async fn report(&self, task: &TaskKey, update: RunUpdate) -> Result<()> {
        self.write(task, |t, me| {
            if t.holder.as_deref().is_some_and(|h| h != me) {
                return Err(Error::Forbidden(format!("{} is held by another agent", t.key.0)));
            }
            t.run.merge(update);
            Ok(())
        })
    }

    async fn request_review(&self, task: &TaskKey) -> Result<()> {
        self.write(task, |t, me| {
            held_by_me(t, me)?;
            t.holder = None;
            t.status = Status::InReview;
            Ok(())
        })
    }

    async fn approve(&self, task: &TaskKey) -> Result<()> {
        let delegated = !self.state.lock().unwrap().undelegated.contains(&self.agent);
        self.write(task, |t, me| {
            if !delegated {
                return Err(Error::Forbidden(format!(
                    "{me} can't approve {}: it isn't a delegated reviewer, so a person has to list it as one",
                    t.key.0
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
        self.write(task, |t, _| {
            if !matches!(t.status, Status::InReview | Status::Approved) {
                return Err(Error::Conflict(format!("{} isn't in review", t.key.0)));
            }
            t.holder = None;
            t.status = Status::Open;
            Ok(())
        })
    }

    async fn raise_grade(&self, task: &TaskKey, grade: u8) -> Result<()> {
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
        Ok(())
    }

    async fn ask(&self, task: &TaskKey, question: &str) -> Result<()> {
        self.write(task, |t, me| {
            held_by_me(t, me)?;
            t.status = Status::NeedsInput;
            Ok(())
        })?;
        self.comment(task, "question", question);
        Ok(())
    }

    async fn note(&self, task: &TaskKey, body: &str) -> Result<()> {
        self.check_live()?;
        self.comment(task, "note", body);
        Ok(())
    }

    async fn finish(&self, task: &TaskKey, handoff: &str) -> Result<()> {
        self.write(task, |t, me| {
            if !matches!(t.status, Status::InReview | Status::Approved) {
                held_by_me(t, me)?;
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
        priority: Option<i64>,
    ) -> Result<TaskKey> {
        self.check_live()?;
        let key = self.add_task(project, title, description);
        if let Some(p) = priority {
            self.state.lock().unwrap().priorities.insert(key.clone(), p);
        }
        Ok(key)
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
