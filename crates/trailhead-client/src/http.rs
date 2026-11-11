//! A typed client for the Trailhead API (`http` feature): base URL, bearer token, request bodies built from
//! `trailhead-model`'s types, and failed responses decoded into an [`Error`] that carries the stable code.
//!
//! It covers the endpoints the daemon's `RestTracker` uses; the rest follow as the CLI and Lookout move over.
//! Every endpoint is a [`Route`] in [`routes`], so a test can check them against the server's OpenAPI document.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use reqwest::{Method, RequestBuilder};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use trailhead_model::{
    Claim, ClaimResult, Comment, CommentKind, Grade, HeldClaim, Priority, Status, Task, TaskDetail, TaskKey, Verdict,
};
use uuid::Uuid;

use crate::ApiError;

/// An endpoint: its method and its path as the OpenAPI document spells it (`{task}` for a parameter).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub method: Method,
    pub path: &'static str,
}

impl Route {
    const fn new(method: Method, path: &'static str) -> Route {
        Route { method, path }
    }

    /// The path with its `{task}` or `{id}` parameter filled in.
    fn url(&self, param: Option<&str>) -> String {
        match param {
            Some(p) => self.path.replace("{task}", p).replace("{id}", p),
            None => self.path.to_string(),
        }
    }
}

/// Every endpoint [`Client`] calls.
pub mod routes {
    use super::Route;
    use reqwest::Method;

    pub const READY: Route = Route::new(Method::GET, "/v1/ready");
    pub const LIST_TASKS: Route = Route::new(Method::GET, "/v1/tasks");
    pub const CREATE_TASK: Route = Route::new(Method::POST, "/v1/tasks");
    pub const SHOW_TASK: Route = Route::new(Method::GET, "/v1/tasks/{task}");
    pub const UPDATE_TASK: Route = Route::new(Method::PATCH, "/v1/tasks/{task}");
    pub const ADD_COMMENT: Route = Route::new(Method::POST, "/v1/tasks/{task}/comments");
    pub const CLAIM: Route = Route::new(Method::POST, "/v1/tasks/{task}/claim");
    pub const RENEW: Route = Route::new(Method::POST, "/v1/tasks/{task}/renew");
    pub const RELEASE: Route = Route::new(Method::POST, "/v1/tasks/{task}/release");
    pub const MY_CLAIMS: Route = Route::new(Method::GET, "/v1/claims");
    pub const RESOLVE_COMMENT: Route = Route::new(Method::POST, "/v1/comments/{id}/resolve");

    /// All of them.
    pub fn all() -> [Route; 11] {
        [
            READY,
            LIST_TASKS,
            CREATE_TASK,
            SHOW_TASK,
            UPDATE_TASK,
            ADD_COMMENT,
            CLAIM,
            RENEW,
            RELEASE,
            MY_CLAIMS,
            RESOLVE_COMMENT,
        ]
    }
}

/// Why a call failed.
#[derive(Debug)]
pub enum Error {
    /// The request didn't complete, or the body wasn't readable.
    Transport(reqwest::Error),
    /// The server answered with an error: its HTTP status and the stable code and message.
    Api { status: u16, error: ApiError },
}

impl Error {
    /// The stable error code (`claim_held`, `not_found`, ...), when the server sent one.
    pub fn code(&self) -> Option<&str> {
        match self {
            Error::Api { error, .. } => Some(&error.code),
            Error::Transport(_) => None,
        }
    }

    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Api { status, .. } => Some(*status),
            Error::Transport(_) => None,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Transport(e) => write!(f, "{e}"),
            Error::Api { status, error } => write!(f, "{status} {error}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// `POST /v1/tasks`.
#[derive(Debug, Default, Clone, Serialize)]
pub struct NewTask {
    pub project: String,
    pub title: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,
}

/// `PATCH /v1/tasks/{task}`: only the fields set are sent.
#[derive(Debug, Default, Clone, Serialize)]
pub struct TaskUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grade: Option<Grade>,
    /// Merged into the task's metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

/// `POST /v1/tasks/{task}/comments`.
#[derive(Debug, Clone, Serialize)]
pub struct NewComment {
    pub kind: CommentKind,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The user or agent the comment is for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub addressed_to: Option<Uuid>,
}

impl NewComment {
    pub fn new(kind: CommentKind, body: impl Into<String>) -> NewComment {
        NewComment { kind, body: body.into(), verdict: None, model: None, addressed_to: None }
    }
}

/// Starts the server again after a request couldn't connect; true if it did, and the request should be retried.
pub type Reconnect = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

/// `GET /v1/tasks` filters. Unset ones aren't sent.
#[derive(Debug, Default, Clone)]
pub struct TaskQuery {
    pub project: Option<String>,
    /// Statuses to list; empty for the server's default (everything unfinished).
    pub status: Vec<Status>,
    pub parent: Option<TaskKey>,
    pub title: Option<String>,
    /// `Some(true)` keeps only the in-review tasks waiting on a rework (`changes_requested=true`).
    pub changes_requested: Option<bool>,
    /// Adds each task's live claim (`include=claim`).
    pub with_claim: bool,
    pub limit: Option<usize>,
}

/// A live claim as `include=claim` lists it. Only what a client reads of it.
#[derive(Debug, Clone, Deserialize)]
pub struct ListedClaim {
    pub holder_id: Uuid,
    /// The holder's name.
    #[serde(default)]
    pub holder: Option<String>,
}

/// A task in a list, with its claim when `include=claim` asked for it.
#[derive(Debug, Clone, Deserialize)]
pub struct ListedTask {
    #[serde(flatten)]
    pub task: Task,
    #[serde(default)]
    pub claim: Option<ListedClaim>,
}

/// The API, as one token sees it.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
    reconnect: Option<Reconnect>,
}

impl Client {
    /// `base` is the server's root URL, e.g. `http://127.0.0.1:7241`.
    pub fn new(base: impl Into<String>, token: impl Into<String>) -> Client {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(60)).build().expect("http client");
        Client { http, base: base.into().trim_end_matches('/').to_string(), token: token.into(), reconnect: None }
    }

    /// This client's connection pool and server, acting with `token` instead. Any reconnect hook is kept.
    pub fn with_token(&self, token: impl Into<String>) -> Client {
        Client { token: token.into(), ..self.clone() }
    }

    /// Calls `reconnect` when a request can't connect, and retries the request once if it returns true.
    pub fn with_reconnect(mut self, reconnect: Reconnect) -> Client {
        self.reconnect = Some(reconnect);
        self
    }

    fn request(&self, route: Route, param: Option<&str>) -> RequestBuilder {
        self.http.request(route.method.clone(), format!("{}{}", self.base, route.url(param))).bearer_auth(&self.token)
    }

    async fn send<T: DeserializeOwned>(&self, req: RequestBuilder) -> Result<T> {
        let retry = req.try_clone();
        let res = match req.send().await {
            Ok(res) => res,
            Err(e) => match (&self.reconnect, retry) {
                (Some(reconnect), Some(retry)) if e.is_connect() && reconnect().await => {
                    retry.send().await.map_err(Error::Transport)?
                }
                _ => return Err(Error::Transport(e)),
            },
        };
        let status = res.status();
        if status.is_success() {
            return res.json().await.map_err(Error::Transport);
        }
        let body: Value = res.json().await.unwrap_or(Value::Null);
        Err(Error::Api { status: status.as_u16(), error: ApiError::from_body(&body) })
    }

    /// `GET /v1/ready`.
    pub async fn ready(&self, project: &str, limit: usize) -> Result<Vec<Task>> {
        self.send(self.request(routes::READY, None).query(&[("project", project), ("limit", &limit.to_string())])).await
    }

    /// `GET /v1/tasks`.
    pub async fn list_tasks(&self, q: &TaskQuery) -> Result<Vec<ListedTask>> {
        let mut query: Vec<(&str, String)> = Vec::new();
        if let Some(p) = &q.project {
            query.push(("project", p.clone()));
        }
        if !q.status.is_empty() {
            query.push(("status", q.status.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(",")));
        }
        if let Some(p) = &q.parent {
            query.push(("parent", p.0.clone()));
        }
        if let Some(t) = &q.title {
            query.push(("title", t.clone()));
        }
        if let Some(c) = q.changes_requested {
            query.push(("changes_requested", c.to_string()));
        }
        if q.with_claim {
            query.push(("include", "claim".into()));
        }
        if let Some(l) = q.limit {
            query.push(("limit", l.to_string()));
        }
        self.send(self.request(routes::LIST_TASKS, None).query(&query)).await
    }

    /// `GET /v1/tasks/{task}`.
    pub async fn show_task(&self, key: &TaskKey) -> Result<TaskDetail> {
        self.send(self.request(routes::SHOW_TASK, Some(key.as_str()))).await
    }

    /// `POST /v1/tasks`; the new task's key.
    pub async fn create_task(&self, new: &NewTask) -> Result<TaskKey> {
        let task: Task = self.send(self.request(routes::CREATE_TASK, None).json(new)).await?;
        Ok(task.key)
    }

    /// `PATCH /v1/tasks/{task}`.
    pub async fn update_task(&self, key: &TaskKey, update: &TaskUpdate) -> Result<()> {
        self.send::<Value>(self.request(routes::UPDATE_TASK, Some(key.as_str())).json(update)).await.map(drop)
    }

    /// `POST /v1/tasks/{task}/comments`.
    pub async fn add_comment(&self, key: &TaskKey, comment: &NewComment) -> Result<Comment> {
        self.send(self.request(routes::ADD_COMMENT, Some(key.as_str())).json(comment)).await
    }

    /// `POST /v1/comments/{id}/resolve`, optionally with an answer.
    pub async fn resolve_comment(&self, id: Uuid, answer: Option<&str>) -> Result<Comment> {
        let req = self.request(routes::RESOLVE_COMMENT, Some(&id.to_string()));
        self.send(req.json(&serde_json::json!({ "answer": answer }))).await
    }

    /// `POST /v1/tasks/{task}/claim`, for `ttl` seconds or the default.
    pub async fn claim(&self, key: &TaskKey, ttl: Option<i64>) -> Result<ClaimResult> {
        self.send(self.request(routes::CLAIM, Some(key.as_str())).json(&serde_json::json!({ "ttl": ttl }))).await
    }

    /// `POST /v1/tasks/{task}/renew`.
    pub async fn renew(&self, key: &TaskKey, ttl: Option<i64>) -> Result<Claim> {
        self.send(self.request(routes::RENEW, Some(key.as_str())).json(&serde_json::json!({ "ttl": ttl }))).await
    }

    /// `POST /v1/tasks/{task}/release`: the task, open again.
    pub async fn release(&self, key: &TaskKey) -> Result<Task> {
        self.send(self.request(routes::RELEASE, Some(key.as_str())).json(&serde_json::json!({}))).await
    }

    /// `GET /v1/claims`: the live claims the token holds.
    pub async fn my_claims(&self) -> Result<Vec<HeldClaim>> {
        self.send(self.request(routes::MY_CLAIMS, None)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_token_swaps_only_the_token() {
        let client = Client::new("http://127.0.0.1:1/", "a").with_reconnect(Arc::new(|| Box::pin(async { false })));
        let other = client.with_token("b");
        assert_eq!((other.base.as_str(), other.token.as_str()), ("http://127.0.0.1:1", "b"));
        assert_eq!(client.token, "a");
        assert!(other.reconnect.is_some());
    }
}
