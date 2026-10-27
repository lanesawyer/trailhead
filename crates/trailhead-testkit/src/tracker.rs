use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use trailhead_factory::{Credential, Error, Result, RunUpdate, Status, Task, TaskKey, Tracker, Verdict, VerdictKind};

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
                holder: None,
                run: RunUpdate::default(),
            },
        );
        key
    }

    pub fn task(&self, key: &TaskKey) -> Task {
        self.state.lock().unwrap().tasks[key].clone()
    }

    pub fn comments(&self, key: &TaskKey) -> Vec<FakeComment> {
        self.state.lock().unwrap().comments.get(key).cloned().unwrap_or_default()
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

    async fn in_flight(&self, project: &str) -> Result<Vec<Task>> {
        self.check_live()?;
        let s = self.state.lock().unwrap();
        Ok(s.tasks
            .values()
            .filter(|t| {
                t.project == project && !t.run.is_empty() && !matches!(t.status, Status::Done | Status::Cancelled)
            })
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

    async fn finish(&self, task: &TaskKey, handoff: &str) -> Result<()> {
        self.write(task, |t, me| {
            if t.status != Status::InReview {
                held_by_me(t, me)?;
            }
            t.holder = None;
            t.status = Status::Done;
            Ok(())
        })?;
        self.comment(task, "handoff", handoff);
        Ok(())
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
