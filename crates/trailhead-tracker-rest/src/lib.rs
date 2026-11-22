//! [`Tracker`] over Trailhead's REST API, as a daemon reaches it through the control plane's tunnel with its
//! sandbox's sub-agent token, and as `trailhead factory check`, `status` and `logs` read tasks with the user's token.
//! It does what those need; managing agents stays with the control plane, which uses the in-process tracker.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use trailhead_client::http::{
    Client, Error as ApiError, NewAttachment, NewComment, NewNotification, NewTask, ReadyQuery, TaskQuery, TaskUpdate,
};
use trailhead_factory::{
    Credential, Error, FollowUpTask, Grade, HumanContext, METADATA_KEY, NewPlanTask, Notice, Picture, PlanTask,
    PlanTaskUpdate, Priority, ProjectRepo, Result, RunPatch, SOURCE_FACTORY, Scope, Status, TASK_PLAN_KEY, Task,
    TaskKey, TaskPlan, Tracker, Verdict, VerdictKind, in_ready_order,
};
use trailhead_model::{self as model, CommentKind};

#[derive(Clone)]
pub struct RestTracker {
    client: Client,
    cred: Credential,
}

impl RestTracker {
    /// `base` is the server's root URL, e.g. `http://127.0.0.1:7241`.
    pub fn new(base: impl Into<String>, cred: Credential) -> RestTracker {
        let base = base.into();
        RestTracker { client: Client::new(base, cred.token.clone()), cred }
    }

    async fn patch(&self, task: &TaskKey, update: TaskUpdate) -> Result<()> {
        self.client.update_task(task, &update).await.map(drop).map_err(error)
    }

    async fn status(&self, task: &TaskKey, status: Status) -> Result<()> {
        self.patch(task, TaskUpdate { status: Some(status), ..TaskUpdate::default() }).await
    }

    async fn comment(&self, task: &TaskKey, kind: CommentKind, body: &str) -> Result<()> {
        self.client.add_comment(task, &NewComment::new(kind, body)).await.map(drop).map_err(error)
    }
}

impl RestTracker {
    /// [`Tracker::ready`] for one project (under `queue`, when there is one), before the raise and the cut.
    async fn ready_in(&self, project: &str, queue: &[TaskKey]) -> Result<Vec<Task>> {
        // Never just `limit`: the cut comes after the reworks are merged in and the blockers raised.
        let q = ReadyQuery { project: Some(project.to_string()), limit: Some(1000), ..ReadyQuery::default() };
        let ready = self.client.ready(&q).await.map_err(error)?;
        let mut queued = HashSet::new();
        for q in queue {
            let query = TaskQuery {
                project: Some(project.to_string()),
                parent: Some(q.clone()),
                limit: Some(1000),
                ..TaskQuery::default()
            };
            queued.extend(self.client.list_tasks(&query).await.map_err(error)?.into_iter().map(|t| t.task.key));
        }
        let mut tasks = ready
            .into_iter()
            .filter(|t| queue.is_empty() || queued.contains(&t.key))
            .map(|t| Task::from_model(t, None))
            .collect::<Result<Vec<_>>>()?;
        tasks.extend(self.reworks(project, queue).await?);
        Ok(tasks)
    }

    /// The unheld, unblocked in-review tasks of `project` (of the `queue`'s subtasks, when there is one) waiting on a
    /// rework, in priority order.
    async fn reworks(&self, project: &str, queue: &[TaskKey]) -> Result<Vec<Task>> {
        let parents: Vec<Option<&TaskKey>> =
            if queue.is_empty() { vec![None] } else { queue.iter().map(Some).collect() };
        let mut seen = HashSet::new();
        let mut reworks = Vec::new();
        for parent in parents {
            let query = TaskQuery {
                project: Some(project.to_string()),
                status: vec![Status::InReview],
                parent: parent.cloned(),
                changes_requested: Some(true),
                with_claim: true,
                limit: Some(1000),
                ..TaskQuery::default()
            };
            for t in self.client.list_tasks(&query).await.map_err(error)? {
                if t.claim.is_some() || !seen.insert(t.task.key.clone()) {
                    continue;
                }
                // A blocked task can't be claimed, so it isn't ready.
                let detail = self.client.show_task(&t.task.key).await.map_err(error)?;
                if detail.blocked_by.is_empty() {
                    reworks.push(Task { changes_requested: true, ..Task::from_model(t.task, None)? });
                }
            }
        }
        reworks.sort_by_key(|t| t.priority);
        Ok(reworks)
    }

    /// Raises each task's priority to the highest among itself and the open tasks it blocks, directly or through a
    /// chain, so work that holds up urgent work is listed as urgent. A task that can't be read is left out of the walk.
    async fn raise_to_blocked_priority(&self, tasks: &mut [Task]) {
        let mut seen: std::collections::HashMap<TaskKey, Option<(Priority, Vec<TaskKey>)>> = Default::default();
        for task in tasks.iter_mut() {
            let mut stack = vec![task.key.clone()];
            let mut visited = HashSet::new();
            while let Some(key) = stack.pop() {
                if !visited.insert(key.clone()) {
                    continue;
                }
                if !seen.contains_key(&key) {
                    let read = match self.client.show_task(&key).await {
                        Ok(d) if !d.task.status.is_closed() => Some((d.task.priority, d.blocks)),
                        _ => None,
                    };
                    seen.insert(key.clone(), read);
                }
                if let Some((priority, blocks)) = &seen[&key] {
                    task.priority = task.priority.min(*priority);
                    stack.extend(blocks.iter().cloned());
                }
            }
        }
    }
}

/// The factory's error for a failed call.
fn error(e: ApiError) -> Error {
    let ApiError::Api { status, error } = e else {
        return match &e {
            ApiError::Transport(t) if t.is_connect() => Error::Unreachable(format!("trailhead: {}", e.chain())),
            _ => Error::Other(format!("trailhead: {}", e.chain())),
        };
    };
    let msg = format!("trailhead {status} {}: {}", error.code, error.message);
    match status {
        404 => Error::NotFound(msg),
        401 | 403 => Error::Forbidden(msg),
        409 | 412 => Error::Conflict(msg),
        s => Error::from_status(s, msg),
    }
}

fn unsupported(what: &str) -> Error {
    Error::Forbidden(format!("a sandbox's tracker can't {what}; the control plane does that"))
}

#[async_trait]
impl Tracker for RestTracker {
    fn agent(&self) -> &str {
        &self.cred.agent
    }

    /// Sandboxes don't act as other agents. The handle this returns refuses everything.
    fn sub_agent(&self, name: &str) -> Arc<dyn Tracker> {
        let mut refused = self.clone();
        refused.client = self.client.with_token("");
        refused.cred = Credential { agent: format!("{}/{name}", self.cred.agent), token: String::new() };
        Arc::new(refused)
    }

    async fn ready(&self, scopes: &[Scope], limit: usize) -> Result<Vec<Task>> {
        let mut tasks: Vec<Task> = Vec::new();
        let mut seen = HashSet::new();
        for scope in scopes {
            let found = self.ready_in(&scope.project, &scope.queue).await?;
            tasks.extend(found.into_iter().filter(|t| seen.insert(t.key.clone())));
        }
        // The cut comes after the blockers are raised ([`in_ready_order`]).
        self.raise_to_blocked_priority(&mut tasks).await;
        Ok(in_ready_order(tasks, limit))
    }

    async fn show(&self, key: &TaskKey) -> Result<Task> {
        let detail = self.client.show_task(key).await.map_err(error)?;
        let flag = detail.changes_requested.is_some();
        Task::from_model(detail.task, detail.claim.map(|c| c.holder_id.to_string()))
            .map(|task| Task { changes_requested: flag, ..task })
    }

    async fn human_context(&self, _: &TaskKey) -> Result<HumanContext> {
        Err(unsupported("read a task's comments"))
    }

    async fn last_review(&self, _: &TaskKey) -> Result<Option<Verdict>> {
        Err(unsupported("read a task's comments"))
    }

    async fn last_review_by_others(&self, _: &TaskKey) -> Result<Option<Verdict>> {
        Err(unsupported("read a task's comments"))
    }

    async fn in_flight(&self, scopes: &[Scope]) -> Result<Vec<Task>> {
        let unfinished: Vec<Status> = Status::ALL.into_iter().filter(|s| !s.is_closed()).collect();
        let mut seen = HashSet::new();
        let mut found = Vec::new();
        for scope in scopes {
            let parents: Vec<Option<&TaskKey>> =
                if scope.queue.is_empty() { vec![None] } else { scope.queue.iter().map(Some).collect() };
            for parent in parents {
                let query = TaskQuery {
                    project: Some(scope.project.clone()),
                    status: unfinished.clone(),
                    parent: parent.cloned(),
                    with_claim: true,
                    limit: Some(1000),
                    ..TaskQuery::default()
                };
                for t in self.client.list_tasks(&query).await.map_err(error)? {
                    if t.task.metadata.get(METADATA_KEY).is_some() && seen.insert(t.task.key.clone()) {
                        let holder = t.claim.map(|c| c.holder.unwrap_or_else(|| c.holder_id.to_string()));
                        found.push(Task::from_model(t.task, holder)?);
                    }
                }
            }
        }
        Ok(found)
    }

    async fn project_repos(&self) -> Result<Vec<(String, ProjectRepo)>> {
        let projects = self.client.list_projects(false).await.map_err(error)?;
        Ok(projects.into_iter().filter_map(|p| Some((p.key.clone(), p.repo()?))).collect())
    }

    async fn claim(&self, key: &TaskKey) -> Result<()> {
        self.client.claim(key, None).await.map(drop).map_err(error)
    }

    async fn renew(&self, key: &TaskKey) -> Result<()> {
        self.client.renew(key, None).await.map(drop).map_err(error)
    }

    async fn release(&self, key: &TaskKey) -> Result<()> {
        self.client.release(key).await.map(drop).map_err(error)
    }

    async fn report(&self, key: &TaskKey, update: RunPatch) -> Result<()> {
        let metadata = json!({ METADATA_KEY: update });
        self.patch(key, TaskUpdate { metadata: Some(metadata), ..TaskUpdate::default() }).await
    }

    async fn request_review(&self, key: &TaskKey, handoff: &str) -> Result<()> {
        let update =
            TaskUpdate { status: Some(Status::InReview), handoff: Some(handoff.into()), ..TaskUpdate::default() };
        self.patch(key, update).await
    }

    async fn handoff(&self, key: &TaskKey, body: &str) -> Result<()> {
        self.comment(key, CommentKind::Handoff, body).await
    }

    async fn approve(&self, key: &TaskKey) -> Result<()> {
        self.status(key, Status::Approved).await
    }

    async fn unapprove(&self, key: &TaskKey) -> Result<()> {
        self.status(key, Status::InReview).await
    }

    async fn reopen(&self, key: &TaskKey) -> Result<()> {
        self.status(key, Status::Open).await
    }

    async fn raise_grade(&self, key: &TaskKey, grade: Grade) -> Result<()> {
        self.patch(key, TaskUpdate { grade: Some(Some(grade)), ..TaskUpdate::default() }).await
    }

    async fn record_verdict(&self, key: &TaskKey, verdict: &Verdict) -> Result<()> {
        let decision = match verdict.kind {
            VerdictKind::Approve => model::Verdict::Approve,
            VerdictKind::RequestChanges => model::Verdict::RequestChanges,
        };
        let comment = NewComment {
            verdict: Some(decision),
            model: verdict.model.as_ref().map(|m| m.0.clone()),
            ..NewComment::new(CommentKind::Review, verdict.body.clone())
        };
        self.client.add_comment(key, &comment).await.map(drop).map_err(error)
    }

    async fn ask(&self, key: &TaskKey, question: &str) -> Result<()> {
        self.comment(key, CommentKind::Question, question).await
    }

    async fn note(&self, key: &TaskKey, body: &str) -> Result<()> {
        self.comment(key, CommentKind::Note, body).await
    }

    async fn finish(&self, key: &TaskKey, handoff: &str) -> Result<()> {
        let update = TaskUpdate { status: Some(Status::Done), handoff: Some(handoff.into()), ..TaskUpdate::default() };
        self.patch(key, update).await
    }

    async fn attach(&self, key: &TaskKey, set: &str, picture: Picture) -> Result<()> {
        let new = NewAttachment { name: picture.name, role: Some(picture.role), set: Some(set.to_string()) };
        self.client.add_attachment(key, &new, picture.png).await.map(drop).map_err(error)
    }

    async fn pictures(&self, key: &TaskKey, set: &str) -> Result<Vec<Picture>> {
        let mut pictures = Vec::new();
        for a in self.client.list_attachments(key).await.map_err(error)? {
            let (Some(role), true) = (a.role, a.set.as_deref() == Some(set)) else { continue };
            let png = self.client.attachment_bytes(a.id).await.map_err(error)?;
            pictures.push(Picture { name: a.name, role, png });
        }
        Ok(pictures)
    }

    async fn create_task(
        &self,
        project: &str,
        title: &str,
        description: &str,
        priority: Option<Priority>,
    ) -> Result<TaskKey> {
        let new = NewTask {
            project: project.to_string(),
            title: title.to_string(),
            description: description.to_string(),
            priority,
            ..NewTask::default()
        };
        self.client.create_task(&new).await.map(|c| c.task.key).map_err(error)
    }

    async fn follow_ups(&self, _: &TaskKey) -> Result<Vec<FollowUpTask>> {
        Err(unsupported("list a task's follow-ups"))
    }

    async fn find_open_by_title(&self, project: &str, title: &str) -> Result<Option<TaskKey>> {
        let wanted = title.trim();
        let query = TaskQuery {
            project: Some(project.to_string()),
            title: Some(wanted.to_string()),
            limit: Some(100),
            ..TaskQuery::default()
        };
        let tasks = self.client.list_tasks(&query).await.map_err(error)?;
        Ok(tasks.into_iter().find(|t| t.task.title.trim().eq_ignore_ascii_case(wanted)).map(|t| t.task.key))
    }

    async fn plan_tasks(&self, spec: &TaskKey) -> Result<Vec<PlanTask>> {
        let query = TaskQuery {
            project: Some(spec.project().to_string()),
            parent: Some(spec.clone()),
            status: Status::ALL.to_vec(),
            limit: Some(1000),
            ..TaskQuery::default()
        };
        let mut children = self.client.list_tasks(&query).await.map_err(error)?;
        children.sort_by(|a, b| model::cmp_keys(&a.task.key, &b.task.key));
        let mut plan = Vec::new();
        for t in children {
            let detail = self.client.show_task(&t.task.key).await.map_err(error)?;
            let t = t.task;
            plan.push(PlanTask {
                proposed: TaskPlan::of(&t.metadata).and_then(|p| p.proposed),
                key: t.key,
                title: t.title,
                description: t.description,
                status: t.status,
                grade: t.grade,
                blocked_by: detail.blocked_by,
            });
        }
        Ok(plan)
    }

    async fn create_plan_task(&self, spec: &TaskKey, task: &NewPlanTask) -> Result<TaskKey> {
        let new = NewTask {
            project: spec.project().to_string(),
            title: task.title.clone(),
            description: task.description.clone(),
            parent: Some(spec.0.clone()),
            metadata: Some(json!({ TASK_PLAN_KEY: { "proposed": task.proposed } })),
            blocked_by: task.blocked_by.iter().map(|k| k.0.clone()).collect(),
            ..NewTask::default()
        };
        self.client.create_task(&new).await.map(|c| c.task.key).map_err(error)
    }

    async fn update_plan_task(&self, key: &TaskKey, update: &PlanTaskUpdate) -> Result<()> {
        let patch = TaskUpdate {
            title: update.title.clone(),
            description: update.description.clone(),
            metadata: update.proposed.as_ref().map(|p| json!({ TASK_PLAN_KEY: { "proposed": p } })),
            ..TaskUpdate::default()
        };
        if patch != TaskUpdate::default() {
            self.patch(key, patch).await?;
        }
        for blocker in &update.remove_blockers {
            match self.client.remove_blocker(key, blocker).await.map_err(error) {
                Ok(()) | Err(Error::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        for blocker in &update.add_blockers {
            self.client.add_blocker(key, blocker).await.map_err(error)?;
        }
        Ok(())
    }

    async fn cancel(&self, key: &TaskKey) -> Result<()> {
        self.status(key, Status::Cancelled).await
    }

    async fn set_description(&self, key: &TaskKey, description: &str) -> Result<()> {
        self.patch(key, TaskUpdate { description: Some(description.into()), ..TaskUpdate::default() }).await
    }

    async fn patch_plan(&self, key: &TaskKey, patch: serde_json::Value) -> Result<()> {
        self.patch(key, TaskUpdate { metadata: Some(json!({ TASK_PLAN_KEY: patch })), ..TaskUpdate::default() }).await
    }

    async fn create_sub_agent(&self, _: &str, _: &str, _: Duration) -> Result<Credential> {
        Err(unsupported("create agents"))
    }

    async fn revoke_sub_agent(&self, _: &str) -> Result<()> {
        Err(unsupported("revoke agents"))
    }

    async fn notify(&self, notice: &Notice) -> Result<()> {
        let new = NewNotification {
            source: SOURCE_FACTORY.into(),
            kind: notice.kind.clone(),
            title: notice.title.clone(),
            body: notice.body.clone(),
            subject: Some(notice.subject.clone()),
            task: notice.task.as_ref().map(|k| k.0.clone()),
            actions: notice.actions.clone(),
        };
        self.client.raise_notification(&new).await.map(drop).map_err(error)
    }

    async fn resolve_notices(&self, subject: &str) -> Result<()> {
        self.client.resolve_notifications_about(SOURCE_FACTORY, subject).await.map(drop).map_err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_refused_connection_is_unreachable() {
        // Bind a port, then free it, so nothing is listening there.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let cred = Credential { agent: "x".into(), token: "t".into() };
        let err = RestTracker::new(format!("http://127.0.0.1:{port}"), cred).show(&TaskKey("TRAIL-1".into())).await;
        let Err(Error::Unreachable(msg)) = err else { panic!("{err:?}") };
        let head = "trailhead: request to Trailhead failed: ";
        assert!(msg.starts_with(head) && msg.len() > head.len(), "the source's text is kept: {msg}");
    }
}
