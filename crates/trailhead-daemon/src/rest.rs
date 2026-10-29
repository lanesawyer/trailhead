//! [`Tracker`] over Trailhead's REST API, as a daemon reaches it through the control plane's tunnel with its
//! sandbox's sub-agent token. It does what a daemon needs; managing agents stays with the control plane, which
//! uses the in-process tracker.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::{Method, RequestBuilder, StatusCode};
use serde_json::{Value, json};
use trailhead_factory::{
    Credential, Error, HumanContext, Result, RunUpdate, Status, Task, TaskKey, Tracker, Verdict, VerdictKind,
};

/// The metadata key the factory's run fields live under (`trailhead_control::METADATA_KEY`).
const METADATA_KEY: &str = "factory";

#[derive(Clone)]
pub struct RestTracker {
    http: reqwest::Client,
    base: String,
    cred: Credential,
}

impl RestTracker {
    /// `base` is the server's root URL, e.g. `http://127.0.0.1:7241`.
    pub fn new(base: impl Into<String>, cred: Credential) -> RestTracker {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(60)).build().expect("http client");
        RestTracker { http, base: base.into().trim_end_matches('/').to_string(), cred }
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.http.request(method, format!("{}/v1{path}", self.base)).bearer_auth(&self.cred.token)
    }

    async fn send(&self, req: RequestBuilder) -> Result<Value> {
        let res = req.send().await.map_err(|e| Error::Other(format!("trailhead: {e}")))?;
        let status = res.status();
        let body: Value = res.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(body);
        }
        let err = &body["error"];
        let msg = format!(
            "trailhead {status} {}: {}",
            err["code"].as_str().unwrap_or("error"),
            err["message"].as_str().unwrap_or_default()
        );
        Err(match status {
            StatusCode::NOT_FOUND => Error::NotFound(msg),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Error::Forbidden(msg),
            StatusCode::CONFLICT | StatusCode::PRECONDITION_FAILED => Error::Conflict(msg),
            _ => Error::Other(msg),
        })
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.send(self.request(Method::POST, path).json(&body)).await
    }

    async fn patch(&self, task: &TaskKey, body: Value) -> Result<()> {
        self.send(self.request(Method::PATCH, &format!("/tasks/{}", task.0)).json(&body)).await.map(drop)
    }

    async fn comment(&self, task: &TaskKey, body: Value) -> Result<()> {
        self.post(&format!("/tasks/{}/comments", task.0), body).await.map(drop)
    }
}

fn status(s: &str) -> Result<Status> {
    Ok(match s {
        "open" => Status::Open,
        "in_progress" => Status::InProgress,
        "needs_input" => Status::NeedsInput,
        "in_review" => Status::InReview,
        "approved" => Status::Approved,
        "done" => Status::Done,
        "cancelled" => Status::Cancelled,
        other => return Err(Error::Other(format!("unknown status {other}"))),
    })
}

/// A task from the API's JSON. `holder` is the claim holder's name when the response includes it (`include=claim`
/// on lists), else its id.
fn task(t: &Value, claim: &Value) -> Result<Task> {
    let key = t["key"].as_str().ok_or_else(|| Error::Other(format!("task without a key: {t}")))?.to_string();
    let run = match t["metadata"].get(METADATA_KEY) {
        Some(v) => {
            serde_json::from_value(v.clone()).map_err(|e| Error::Other(format!("{key}: bad factory metadata: {e}")))?
        }
        None => RunUpdate::default(),
    };
    let holder = claim["holder"].as_str().or(claim["holder_id"].as_str()).map(str::to_string);
    Ok(Task {
        project: key.rsplit_once('-').map(|(p, _)| p.to_string()).unwrap_or_default(),
        title: t["title"].as_str().unwrap_or_default().to_string(),
        description: t["description"].as_str().unwrap_or_default().to_string(),
        status: status(t["status"].as_str().unwrap_or_default())?,
        grade: t["grade"].as_u64().and_then(|g| u8::try_from(g).ok()),
        holder,
        run,
        key: TaskKey(key),
    })
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
        refused.cred = Credential { agent: format!("{}/{name}", self.cred.agent), token: String::new() };
        Arc::new(refused)
    }

    async fn ready(&self, project: &str, queue: Option<&TaskKey>, limit: usize) -> Result<Vec<Task>> {
        let fetch = if queue.is_some() { 1000 } else { limit };
        let req = self.request(Method::GET, "/ready").query(&[("project", project), ("limit", &fetch.to_string())]);
        let ready = self.send(req).await?;
        let queued: Option<std::collections::HashSet<String>> = match queue {
            None => None,
            Some(q) => {
                let query = [("project", project), ("parent", q.0.as_str()), ("limit", "1000")];
                let subtasks = self.send(self.request(Method::GET, "/tasks").query(&query)).await?;
                Some(
                    subtasks
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|t| t["key"].as_str().map(str::to_string))
                        .collect(),
                )
            }
        };
        ready
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| queued.as_ref().is_none_or(|q| t["key"].as_str().is_some_and(|k| q.contains(k))))
            .take(limit)
            .map(|t| task(t, &Value::Null))
            .collect()
    }

    async fn show(&self, key: &TaskKey) -> Result<Task> {
        let detail = self.send(self.request(Method::GET, &format!("/tasks/{}", key.0))).await?;
        task(&detail["task"], &detail["claim"])
    }

    async fn human_context(&self, _: &TaskKey) -> Result<HumanContext> {
        Err(unsupported("read a task's comments"))
    }

    async fn last_review(&self, _: &TaskKey) -> Result<Option<Verdict>> {
        Err(unsupported("read a task's comments"))
    }

    async fn in_flight(&self, project: &str) -> Result<Vec<Task>> {
        let query = [
            ("project", project),
            ("status", "open,in_progress,needs_input,in_review,approved"),
            ("include", "claim"),
            ("limit", "1000"),
        ];
        let tasks = self.send(self.request(Method::GET, "/tasks").query(&query)).await?;
        tasks
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| t["metadata"].get(METADATA_KEY).is_some())
            .map(|t| task(t, &t["claim"]))
            .collect()
    }

    async fn claim(&self, key: &TaskKey) -> Result<()> {
        self.post(&format!("/tasks/{}/claim", key.0), json!({})).await.map(drop)
    }

    async fn renew(&self, key: &TaskKey) -> Result<()> {
        self.post(&format!("/tasks/{}/renew", key.0), json!({})).await.map(drop)
    }

    async fn release(&self, key: &TaskKey) -> Result<()> {
        self.post(&format!("/tasks/{}/release", key.0), json!({})).await.map(drop)
    }

    async fn report(&self, key: &TaskKey, update: RunUpdate) -> Result<()> {
        self.patch(key, json!({ "metadata": { METADATA_KEY: update } })).await
    }

    async fn request_review(&self, key: &TaskKey) -> Result<()> {
        self.patch(key, json!({ "status": "in_review" })).await
    }

    async fn reopen(&self, key: &TaskKey) -> Result<()> {
        self.patch(key, json!({ "status": "open" })).await
    }

    async fn raise_grade(&self, key: &TaskKey, grade: u8) -> Result<()> {
        self.patch(key, json!({ "grade": grade })).await
    }

    async fn record_verdict(&self, key: &TaskKey, verdict: &Verdict) -> Result<()> {
        let kind = match verdict.kind {
            VerdictKind::Approve => "approve",
            VerdictKind::RequestChanges => "request_changes",
        };
        let model = verdict.model.as_ref().map(|m| m.0.clone());
        self.comment(key, json!({ "kind": "review", "body": verdict.body, "verdict": kind, "model": model })).await
    }

    async fn ask(&self, key: &TaskKey, question: &str) -> Result<()> {
        self.comment(key, json!({ "kind": "question", "body": question })).await
    }

    async fn note(&self, key: &TaskKey, body: &str) -> Result<()> {
        self.comment(key, json!({ "kind": "note", "body": body })).await
    }

    async fn finish(&self, key: &TaskKey, handoff: &str) -> Result<()> {
        self.comment(key, json!({ "kind": "handoff", "body": handoff })).await?;
        self.patch(key, json!({ "status": "done" })).await
    }

    async fn create_task(
        &self,
        project: &str,
        title: &str,
        description: &str,
        priority: Option<i64>,
    ) -> Result<TaskKey> {
        let body = json!({ "project": project, "title": title, "description": description, "priority": priority });
        let created = self.post("/tasks", body).await?;
        let task = created.get("task").unwrap_or(&created);
        let key = task["key"].as_str().ok_or_else(|| Error::Other(format!("created task without a key: {created}")))?;
        Ok(TaskKey(key.to_string()))
    }

    async fn find_open_by_title(&self, project: &str, title: &str) -> Result<Option<TaskKey>> {
        let query = [("project", project), ("limit", "1000")];
        let tasks = self.send(self.request(Method::GET, "/tasks").query(&query)).await?;
        let wanted = title.trim();
        Ok(tasks
            .as_array()
            .into_iter()
            .flatten()
            .find(|t| t["title"].as_str().is_some_and(|x| x.trim().eq_ignore_ascii_case(wanted)))
            .and_then(|t| t["key"].as_str().map(|k| TaskKey(k.to_string()))))
    }

    async fn create_sub_agent(&self, _: &str, _: &str, _: Duration) -> Result<Credential> {
        Err(unsupported("create agents"))
    }

    async fn revoke_sub_agent(&self, _: &str) -> Result<()> {
        Err(unsupported("revoke agents"))
    }
}
