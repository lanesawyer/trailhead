//! [`Tracker`] in-process on a Trailhead [`Store`], for when the server is the control plane.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use trailhead_core::{CommentKind, Ctx, NewComment, Principal, Store, TaskFilter, TaskPatch, TokenOptions};
use trailhead_factory::{Credential, Error, Result, RunUpdate, Status, Task, TaskKey, Tracker, Verdict, VerdictKind};

/// The metadata key the factory's run fields live under.
pub const METADATA_KEY: &str = "factory";

#[derive(Clone)]
pub struct CoreTracker {
    store: Arc<Store>,
    /// The agent this handle authenticates as.
    ctx: Ctx,
    /// Set when acting as one of `ctx`'s sub-agents, by the part after its name.
    sub: Option<String>,
    agent: String,
}

impl CoreTracker {
    /// A tracker acting as the agent behind `ctx`, whose full name is `agent`.
    pub fn new(store: Arc<Store>, ctx: Ctx, agent: &str) -> CoreTracker {
        CoreTracker { store, ctx, sub: None, agent: agent.to_string() }
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

    async fn task(&self, ctx: &Ctx, t: trailhead_core::Task) -> Result<Task> {
        let claims = self.store.live_claims(ctx, &[t.id]).await.map_err(map_err)?;
        let run = match t.metadata.get(METADATA_KEY) {
            Some(v) => serde_json::from_value(v.clone())
                .map_err(|e| Error::Other(format!("{}: bad factory metadata: {e}", t.key)))?,
            None => RunUpdate::default(),
        };
        let project = t.key.rsplit_once('-').map(|(p, _)| p.to_string()).unwrap_or_default();
        Ok(Task {
            key: TaskKey(t.key),
            project,
            title: t.title,
            description: t.description,
            status: status(t.status),
            holder: claims.get(&t.id).map(|c| c.holder.clone()),
            run,
        })
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

fn status(s: trailhead_core::Status) -> Status {
    use trailhead_core::Status as S;
    match s {
        S::Open => Status::Open,
        S::InProgress => Status::InProgress,
        S::NeedsInput => Status::NeedsInput,
        S::InReview => Status::InReview,
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

    async fn ready(&self, project: &str, limit: usize) -> Result<Vec<Task>> {
        let ctx = self.ctx().await?;
        let tasks = self.store.ready(&ctx, Some(project), Some(limit as i64)).await.map_err(map_err)?;
        let mut out = Vec::with_capacity(tasks.len());
        for t in tasks {
            out.push(self.task(&ctx, t).await?);
        }
        Ok(out)
    }

    async fn show(&self, task: &TaskKey) -> Result<Task> {
        let ctx = self.ctx().await?;
        let t = self.store.get_task(&ctx, &task.0).await.map_err(map_err)?;
        self.task(&ctx, t).await
    }

    async fn in_flight(&self, project: &str) -> Result<Vec<Task>> {
        let ctx = self.ctx().await?;
        let filter = TaskFilter { project: Some(project.to_string()), limit: Some(1000), ..TaskFilter::default() };
        let tasks = self.store.list_tasks(&ctx, filter).await.map_err(map_err)?;
        let mut out = Vec::new();
        for t in tasks.into_iter().filter(|t| t.metadata.get(METADATA_KEY).is_some()) {
            out.push(self.task(&ctx, t).await?);
        }
        Ok(out)
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

    async fn finish(&self, task: &TaskKey, handoff: &str) -> Result<()> {
        self.comment(task, CommentKind::Handoff, handoff).await?;
        self.set_status(task, trailhead_core::Status::Done).await
    }

    async fn create_sub_agent(&self, name: &str, project: &str) -> Result<Credential> {
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
        let opts = TokenOptions { projects: Some(vec![project.to_string()]), rate_limit_per_minute: None };
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
