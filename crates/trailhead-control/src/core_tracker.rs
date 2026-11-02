//! [`Tracker`] in-process on a Trailhead [`Store`], for when the server is the control plane.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use trailhead_core::{
    CommentKind, Ctx, GradeFilter, NewComment, Principal, Store, TaskFilter, TaskPatch, TokenOptions,
};
use trailhead_factory::messages;
use trailhead_factory::{
    Answered, Credential, Error, FollowUpTask, HumanContext, METADATA_KEY, ModelId, Result, RunUpdate, Status, Task,
    TaskKey, TaskNote, Tracker, Verdict, VerdictKind, is_factory_agent, is_raised_by,
};
use uuid::Uuid;

const PAGE_SIZE: i64 = 1000;

#[derive(Clone)]
pub struct CoreTracker {
    store: Arc<Store>,
    /// The agent this handle authenticates as.
    ctx: Ctx,
    /// Set when acting as one of `ctx`'s sub-agents, by the part after its name.
    sub: Option<String>,
    agent: String,
    /// The bad factory metadata already noted on each task (key to note), so each is said once.
    flagged: Arc<Mutex<HashMap<String, String>>>,
    /// How many tasks one `list_tasks` call reads; longer lists are read a page at a time.
    page_size: i64,
}

impl CoreTracker {
    /// A tracker acting as the agent behind `ctx`, whose full name is `agent`.
    pub fn new(store: Arc<Store>, ctx: Ctx, agent: &str) -> CoreTracker {
        CoreTracker { store, ctx, sub: None, agent: agent.to_string(), flagged: Arc::default(), page_size: PAGE_SIZE }
    }

    /// Read lists `page_size` tasks at a time instead of the default, so a test needn't make a thousand tasks.
    pub fn with_page_size(mut self, page_size: usize) -> CoreTracker {
        self.page_size = page_size.max(1) as i64;
        self
    }

    /// Every task matching `filter`, read a page at a time. `filter.limit` and `filter.offset` are ignored.
    async fn list_all(&self, ctx: &Ctx, filter: TaskFilter) -> Result<Vec<trailhead_core::Task>> {
        let mut all = Vec::new();
        loop {
            let page = TaskFilter { limit: Some(self.page_size), offset: Some(all.len() as i64), ..filter.clone() };
            let tasks = self.store.list_tasks(ctx, page).await.map_err(map_err)?;
            let full = tasks.len() as i64 >= self.page_size;
            all.extend(tasks);
            if !full {
                return Ok(all);
            }
        }
    }

    /// A tracker for whoever holds this credential's token, as a daemon would connect.
    pub async fn authenticate(store: Arc<Store>, cred: &Credential) -> Result<CoreTracker> {
        let ctx = store.authenticate(&cred.token).await.map_err(map_err)?;
        Ok(CoreTracker::new(store, ctx, &cred.agent))
    }

    /// The context for each call, re-resolved so a revoked sub-agent stops working at once.
    async fn ctx(&self) -> Result<Ctx> {
        let ctx = match &self.sub {
            Some(name) => self.store.sub_agent_ctx(&self.ctx, name).await.map_err(map_err)?,
            None => self.ctx.clone(),
        };
        Ok(ctx.next_batch())
    }

    /// A name for each comment author: an agent's name, else a person's, else the raw id.
    async fn author_names(&self, ctx: &Ctx, comments: &[trailhead_core::Comment]) -> Result<HashMap<Uuid, String>> {
        let mut names: HashMap<Uuid, String> = HashMap::new();
        for a in self.store.list_agents(ctx).await.map_err(map_err)? {
            names.insert(a.id, a.name);
        }
        for c in comments {
            if let Entry::Vacant(e) = names.entry(c.author_id) {
                // Not an agent, so a person.
                let name = match self.store.get_user(ctx, c.author_id).await {
                    Ok(u) => u.name,
                    Err(_) => c.author_id.to_string(),
                };
                e.insert(name);
            }
        }
        Ok(names)
    }

    async fn task(&self, ctx: &Ctx, t: trailhead_core::Task) -> Result<Task> {
        let claims = self.store.live_claims(ctx, &[t.id]).await.map_err(map_err)?;
        let run = run_update(&t)?;
        let project = t.key.rsplit_once('-').map(|(p, _)| p.to_string()).unwrap_or_default();
        Ok(Task {
            key: TaskKey(t.key),
            project,
            title: t.title,
            description: t.description,
            status: status(t.status),
            grade: t.grade.and_then(|g| u8::try_from(g).ok()),
            holder: claims.get(&t.id).map(|c| c.holder.clone()),
            run,
        })
    }

    /// Converts the listed tasks, skipping any whose factory metadata won't parse, so one bad value doesn't
    /// stop the factory for the whole project. A skipped task gets a note naming the problem, once.
    async fn tasks(&self, ctx: &Ctx, tasks: impl IntoIterator<Item = trailhead_core::Task>) -> Result<Vec<Task>> {
        let mut out = Vec::new();
        for t in tasks {
            if let Err(e) = parse_run_update(&t) {
                self.flag_bad_metadata(ctx, &t, &e.to_string()).await;
                continue;
            }
            out.push(self.task(ctx, t).await?);
        }
        Ok(out)
    }

    /// Notes on the task, and logs, that its factory metadata is skipped, unless this note is already there.
    async fn flag_bad_metadata(&self, ctx: &Ctx, t: &trailhead_core::Task, error: &str) {
        let fields = bad_fields(t);
        let body = messages::bad_metadata_note(METADATA_KEY, &fields, error);
        if self.flagged.lock().unwrap().get(&t.key) == Some(&body) {
            return;
        }
        tracing::warn!(task = %t.key, error, "skipping a task with bad factory metadata");
        // A note from before a restart counts, so a restart doesn't repeat it.
        let posted = match self.store.list_comments(ctx, &t.key).await {
            Ok(cs) if cs.iter().any(|c| c.kind == CommentKind::Note && c.body == body) => true,
            _ => self.comment(&TaskKey(t.key.clone()), CommentKind::Note, &body).await.is_ok(),
        };
        if posted {
            self.flagged.lock().unwrap().insert(t.key.clone(), body);
        } else {
            tracing::warn!(task = %t.key, "couldn't note the bad factory metadata on the task");
        }
    }

    async fn set_status(&self, task: &TaskKey, to: trailhead_core::Status) -> Result<()> {
        let ctx = self.ctx().await?;
        let patch = TaskPatch { status: Some(to), ..TaskPatch::default() };
        self.store.update_task(&ctx, &task.0, patch, None).await.map_err(map_err)?;
        Ok(())
    }

    async fn comment(&self, task: &TaskKey, kind: CommentKind, body: &str) -> Result<()> {
        let ctx = self.ctx().await?;
        let new = NewComment::new(kind, body.to_string());
        self.store.add_comment(&ctx, &task.0, new).await.map_err(map_err)?;
        Ok(())
    }
}

fn run_update(t: &trailhead_core::Task) -> Result<RunUpdate> {
    parse_run_update(t).map_err(|e| Error::Other(format!("{}: bad factory metadata: {e}", t.key)))
}

fn parse_run_update(t: &trailhead_core::Task) -> serde_json::Result<RunUpdate> {
    match t.metadata.get(METADATA_KEY) {
        Some(v) => serde_json::from_value(v.clone()),
        None => Ok(RunUpdate::default()),
    }
}

/// The fields of the task's factory metadata that don't parse on their own.
fn bad_fields(t: &trailhead_core::Task) -> Vec<String> {
    let Some(serde_json::Value::Object(map)) = t.metadata.get(METADATA_KEY) else { return Vec::new() };
    let bad =
        |(k, v): &(&String, &serde_json::Value)| serde_json::from_value::<RunUpdate>(json!({ k.as_str(): v })).is_err();
    map.iter().filter(bad).map(|(k, _)| format!("`{k}`")).collect()
}

fn same_title(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

fn status(s: trailhead_core::Status) -> Status {
    use trailhead_core::Status as S;
    match s {
        S::Open => Status::Open,
        S::InProgress => Status::InProgress,
        S::NeedsInput => Status::NeedsInput,
        S::InReview => Status::InReview,
        S::Approved => Status::Approved,
        S::Done => Status::Done,
        S::Cancelled => Status::Cancelled,
    }
}

fn map_err(e: trailhead_core::Error) -> Error {
    use trailhead_core::Error as E;
    match e {
        E::NotFound(what) => Error::NotFound(what),
        E::Forbidden(why) => Error::Forbidden(why),
        E::Unauthorized => Error::Forbidden("missing or invalid token".into()),
        e @ (E::ClaimHeld { .. }
        | E::NotClaimHolder
        | E::ClaimLost(_)
        | E::VersionConflict { .. }
        | E::BlockedBy(_)
        | E::InvalidTransition { .. }
        | E::ReviewRequired
        | E::AlreadyExists(_)) => Error::Conflict(e.to_string()),
        e => Error::Other(e.to_string()),
    }
}

#[async_trait]
impl Tracker for CoreTracker {
    fn agent(&self) -> &str {
        &self.agent
    }

    fn sub_agent(&self, name: &str) -> Arc<dyn Tracker> {
        let mut sub = self.clone();
        sub.agent = format!("{}/{name}", self.agent);
        sub.sub = Some(match &self.sub {
            Some(parent) => format!("{parent}/{name}"),
            None => name.to_string(),
        });
        Arc::new(sub)
    }

    async fn ready(&self, project: &str, queue: &[TaskKey], limit: usize) -> Result<Vec<Task>> {
        let ctx = self.ctx().await?;
        let tasks = match queue {
            [] => self.store.ready(&ctx, Some(project), Some(limit as i64), GradeFilter::Any).await.map_err(map_err)?,
            queue => {
                // The store orders ready tasks by priority, so filtering its one list orders across every queue and
                // lists a task once even if it is under two of them.
                let mut queued = std::collections::HashSet::new();
                for q in queue {
                    let filter = TaskFilter {
                        project: Some(project.to_string()),
                        parent: Some(q.0.clone()),
                        ..TaskFilter::default()
                    };
                    queued.extend(self.list_all(&ctx, filter).await?.into_iter().map(|t| t.id));
                }
                // Unlimited: a cap here would hide queued tasks behind other ready ones.
                let ready = self.store.ready(&ctx, Some(project), Some(-1), GradeFilter::Any).await.map_err(map_err)?;
                ready.into_iter().filter(|t| queued.contains(&t.id)).take(limit).collect()
            }
        };
        self.tasks(&ctx, tasks).await
    }

    async fn show(&self, task: &TaskKey) -> Result<Task> {
        let ctx = self.ctx().await?;
        let t = self.store.get_task(&ctx, &task.0).await.map_err(map_err)?;
        self.task(&ctx, t).await
    }

    async fn human_context(&self, task: &TaskKey) -> Result<HumanContext> {
        let ctx = self.ctx().await?;
        let comments = self.store.list_comments(&ctx, &task.0).await.map_err(map_err)?;
        let names = self.author_names(&ctx, &comments).await?;
        let date = |c: &trailhead_core::Comment| c.created_at.to_zoned(jiff::tz::TimeZone::UTC).date().to_string();
        let mut out = HumanContext::default();
        // An answer is the note posted to the question's author as the question is resolved.
        let mut answer_ids = std::collections::HashSet::new();
        for q in comments.iter().filter(|c| c.kind == CommentKind::Question) {
            let Some(resolved_at) = q.resolved_at else { continue };
            let answer = comments.iter().find(|a| {
                a.kind == CommentKind::Note
                    && a.addressed_to == Some(q.author_id)
                    && a.created_at >= q.created_at
                    && a.created_at <= resolved_at
                    && !answer_ids.contains(&a.id)
            });
            if let Some(a) = answer {
                answer_ids.insert(a.id);
                out.answers.push(Answered {
                    question: q.body.clone(),
                    answer: a.body.clone(),
                    by: names[&a.author_id].clone(),
                    date: date(a),
                });
            }
        }
        for c in &comments {
            if !matches!(c.kind, CommentKind::Decision | CommentKind::Note) || answer_ids.contains(&c.id) {
                continue;
            }
            let author = &names[&c.author_id];
            if is_factory_agent(author) {
                continue;
            }
            out.notes.push(TaskNote { author: author.clone(), date: date(c), body: c.body.clone() });
        }
        Ok(out)
    }

    async fn last_review(&self, task: &TaskKey) -> Result<Option<Verdict>> {
        let ctx = self.ctx().await?;
        let comments = self.store.list_comments(&ctx, &task.0).await.map_err(map_err)?;
        let Some(c) = comments.iter().rev().find(|c| c.kind == CommentKind::Review) else { return Ok(None) };
        let Some(verdict) = c.verdict else { return Ok(None) };
        let reviewer = self.author_names(&ctx, std::slice::from_ref(c)).await?.remove(&c.author_id).unwrap_or_default();
        let kind = match verdict {
            trailhead_core::Verdict::Approve => VerdictKind::Approve,
            trailhead_core::Verdict::RequestChanges => VerdictKind::RequestChanges,
        };
        Ok(Some(Verdict {
            kind,
            body: c.body.clone(),
            reviewer,
            model: c.model.clone().map(ModelId),
            raise_grade: None,
        }))
    }

    async fn in_flight(&self, project: &str, queue: &[TaskKey]) -> Result<Vec<Task>> {
        let ctx = self.ctx().await?;
        let parents: Vec<Option<String>> =
            if queue.is_empty() { vec![None] } else { queue.iter().map(|q| Some(q.0.clone())).collect() };
        let mut tasks = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for parent in parents {
            let filter = TaskFilter { project: Some(project.to_string()), parent, ..TaskFilter::default() };
            tasks.extend(self.list_all(&ctx, filter).await?.into_iter().filter(|t| seen.insert(t.id)));
        }
        self.tasks(&ctx, tasks.into_iter().filter(|t| t.metadata.get(METADATA_KEY).is_some())).await
    }

    async fn claim(&self, task: &TaskKey) -> Result<()> {
        let ctx = self.ctx().await?;
        self.store.claim(&ctx, &task.0, None).await.map_err(map_err)?;
        Ok(())
    }

    async fn renew(&self, task: &TaskKey) -> Result<()> {
        let ctx = self.ctx().await?;
        self.store.renew(&ctx, &task.0, None).await.map_err(map_err)?;
        Ok(())
    }

    async fn release(&self, task: &TaskKey) -> Result<()> {
        let ctx = self.ctx().await?;
        self.store.release(&ctx, &task.0).await.map_err(map_err)?;
        Ok(())
    }

    async fn report(&self, task: &TaskKey, update: RunUpdate) -> Result<()> {
        let ctx = self.ctx().await?;
        let fields = serde_json::to_value(update).map_err(|e| Error::Other(e.to_string()))?;
        let patch = TaskPatch { metadata: Some(json!({ METADATA_KEY: fields })), ..TaskPatch::default() };
        self.store.update_task(&ctx, &task.0, patch, None).await.map_err(map_err)?;
        Ok(())
    }

    async fn request_review(&self, task: &TaskKey) -> Result<()> {
        self.set_status(task, trailhead_core::Status::InReview).await
    }

    async fn approve(&self, task: &TaskKey) -> Result<()> {
        match self.set_status(task, trailhead_core::Status::Approved).await {
            // Core's `ReviewRequired` here means the caller is neither a human nor a delegated reviewer. A reviewer
            // that did the work in review is refused with its own `Forbidden`, which passes through.
            Err(Error::Conflict(why)) if why == trailhead_core::Error::ReviewRequired.to_string() => {
                Err(Error::Forbidden(format!(
                    "{} can't approve {}: it isn't a delegated reviewer, so a person has to list it as one",
                    self.agent, task.0
                )))
            }
            result => result,
        }
    }

    async fn unapprove(&self, task: &TaskKey) -> Result<()> {
        self.set_status(task, trailhead_core::Status::InReview).await
    }

    async fn reopen(&self, task: &TaskKey) -> Result<()> {
        self.set_status(task, trailhead_core::Status::Open).await
    }

    async fn raise_grade(&self, task: &TaskKey, grade: u8) -> Result<()> {
        let ctx = self.ctx().await?;
        let patch = TaskPatch { grade: Some(Some(i64::from(grade))), ..TaskPatch::default() };
        self.store.update_task(&ctx, &task.0, patch, None).await.map_err(map_err)?;
        Ok(())
    }

    async fn record_verdict(&self, task: &TaskKey, verdict: &Verdict) -> Result<()> {
        let ctx = self.ctx().await?;
        let kind = match verdict.kind {
            VerdictKind::Approve => trailhead_core::Verdict::Approve,
            VerdictKind::RequestChanges => trailhead_core::Verdict::RequestChanges,
        };
        let new = NewComment {
            model: verdict.model.as_ref().map(|m| m.0.clone()),
            ..NewComment::review(kind, &verdict.body)
        };
        self.store.add_comment(&ctx, &task.0, new).await.map_err(map_err)?;
        Ok(())
    }

    async fn ask(&self, task: &TaskKey, question: &str) -> Result<()> {
        self.comment(task, CommentKind::Question, question).await
    }

    async fn note(&self, task: &TaskKey, body: &str) -> Result<()> {
        self.comment(task, CommentKind::Note, body).await
    }

    async fn finish(&self, task: &TaskKey, handoff: &str) -> Result<()> {
        self.comment(task, CommentKind::Handoff, handoff).await?;
        self.set_status(task, trailhead_core::Status::Done).await
    }

    async fn create_task(
        &self,
        project: &str,
        title: &str,
        description: &str,
        priority: Option<i64>,
    ) -> Result<TaskKey> {
        let ctx = self.ctx().await?;
        let new = trailhead_core::NewTask {
            project: project.to_string(),
            title: title.to_string(),
            description: description.to_string(),
            priority,
            ..Default::default()
        };
        let task = self.store.create_task(&ctx, new).await.map_err(map_err)?;
        Ok(TaskKey(task.key))
    }

    async fn follow_ups(&self, task: &TaskKey) -> Result<Vec<FollowUpTask>> {
        let ctx = self.ctx().await?;
        let project = task.0.rsplit_once('-').map_or(task.0.as_str(), |(p, _)| p);
        let filter = TaskFilter { project: Some(project.to_string()), ..TaskFilter::default() };
        let tasks = self.list_all(&ctx, filter).await?;
        Ok(tasks
            .into_iter()
            .filter(|t| is_raised_by(&t.description, task))
            .map(|t| FollowUpTask { key: TaskKey(t.key), title: t.title, description: t.description })
            .collect())
    }

    async fn find_open_by_title(&self, project: &str, title: &str) -> Result<Option<TaskKey>> {
        let ctx = self.ctx().await?;
        let filter = TaskFilter { project: Some(project.to_string()), ..TaskFilter::default() };
        let tasks = self.list_all(&ctx, filter).await?;
        Ok(tasks.into_iter().find(|t| same_title(&t.title, title)).map(|t| TaskKey(t.key)))
    }

    async fn create_sub_agent(&self, name: &str, project: &str, ttl: Duration) -> Result<Credential> {
        let ctx = self.ctx().await?;
        let full = format!("{}/{name}", self.agent);
        let agent = match self.store.create_agent(&ctx, name).await {
            Ok(a) => a,
            Err(trailhead_core::Error::AlreadyExists(_)) => {
                let agents = self.store.list_agents(&ctx).await.map_err(map_err)?;
                agents.into_iter().find(|a| a.name == full).ok_or_else(|| Error::NotFound(full.clone()))?
            }
            Err(e) => return Err(map_err(e)),
        };
        let expires_in = jiff::SignedDuration::try_from(ttl).map_err(|e| Error::Other(e.to_string()))?;
        let opts = TokenOptions {
            projects: Some(vec![project.to_string()]),
            rate_limit_per_minute: None,
            expires_in: Some(expires_in),
        };
        let (_, secret) =
            self.store.create_token_with(&ctx, Principal::Agent(agent.id), name, opts).await.map_err(map_err)?;
        Ok(Credential { agent: agent.name, token: secret })
    }

    async fn revoke_sub_agent(&self, name: &str) -> Result<()> {
        let ctx = self.ctx().await?;
        match self.store.revoke_agent(&ctx, &format!("{}/{name}", self.agent)).await {
            Ok(_) | Err(trailhead_core::Error::NotFound(_)) => Ok(()),
            Err(e) => Err(map_err(e)),
        }
    }
}
