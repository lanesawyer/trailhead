//! [`Tracker`] over Trailhead's REST API, as a daemon reaches it through the control plane's tunnel with its
//! sandbox's sub-agent token. It does what a daemon needs; managing agents stays with the control plane, which
//! uses the in-process tracker.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use trailhead_client::http::{Client, Error as ApiError, NewComment, NewTask, TaskQuery, TaskUpdate};
use trailhead_factory::{
    Credential, Error, FollowUpTask, Grade, HumanContext, METADATA_KEY, Priority, Result, RunUpdate, Status, Task,
    TaskKey, Tracker, Verdict, VerdictKind,
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
        self.client.update_task(task, &update).await.map_err(error)
    }

    async fn status(&self, task: &TaskKey, status: Status) -> Result<()> {
        self.patch(task, TaskUpdate { status: Some(status), ..TaskUpdate::default() }).await
    }

    async fn comment(&self, task: &TaskKey, kind: CommentKind, body: &str) -> Result<()> {
        self.client.add_comment(task, &NewComment::new(kind, body)).await.map(drop).map_err(error)
    }
}

impl RestTracker {
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
            ApiError::Transport(t) if t.is_connect() => Error::Unreachable(format!("trailhead: {e}")),
            _ => Error::Other(format!("trailhead: {e}")),
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

    async fn ready(&self, project: &str, queue: &[TaskKey], limit: usize) -> Result<Vec<Task>> {
        // Never just `limit`: the cut comes after the reworks are merged in and the blockers raised, as below.
        let ready = self.client.ready(project, 1000).await.map_err(error)?;
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
        // As `CoreTracker::ready` does: reworks are listed ahead of new work within a priority, and the merge and the
        // raise both happen before the cut so a pile of low-priority reworks can't hide a higher-priority new task,
        // nor a low-priority blocker of urgent work.
        tasks.extend(self.reworks(project, queue).await?);
        self.raise_to_blocked_priority(&mut tasks).await;
        tasks.sort_by_key(|t| (t.priority, !t.changes_requested));
        tasks.truncate(limit);
        Ok(tasks)
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

    async fn in_flight(&self, project: &str, queue: &[TaskKey]) -> Result<Vec<Task>> {
        let parents: Vec<Option<&TaskKey>> =
            if queue.is_empty() { vec![None] } else { queue.iter().map(Some).collect() };
        let unfinished: Vec<Status> = Status::ALL.into_iter().filter(|s| !s.is_closed()).collect();
        let mut seen = HashSet::new();
        let mut found = Vec::new();
        for parent in parents {
            let query = TaskQuery {
                project: Some(project.to_string()),
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
        Ok(found)
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

    async fn report(&self, key: &TaskKey, update: RunUpdate) -> Result<()> {
        let metadata = json!({ METADATA_KEY: update });
        self.patch(key, TaskUpdate { metadata: Some(metadata), ..TaskUpdate::default() }).await
    }

    async fn request_review(&self, key: &TaskKey, handoff: &str) -> Result<()> {
        self.handoff(key, handoff).await?;
        self.status(key, Status::InReview).await
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
        self.comment(key, CommentKind::Handoff, handoff).await?;
        self.status(key, Status::Done).await
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
        self.client.create_task(&new).await.map_err(error)
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

    async fn create_sub_agent(&self, _: &str, _: &str, _: Duration) -> Result<Credential> {
        Err(unsupported("create agents"))
    }

    async fn revoke_sub_agent(&self, _: &str) -> Result<()> {
        Err(unsupported("revoke agents"))
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
        assert!(matches!(err, Err(Error::Unreachable(_))), "{err:?}");
    }
}
