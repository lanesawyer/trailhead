//! A typed client for the Trailhead API (`http` feature): base URL, bearer token, request bodies built from
//! `trailhead-model`'s types, and failed responses decoded into an [`Error`] that carries the stable code.
//!
//! It covers the endpoints the daemon's `RestTracker` uses; the rest follow as the CLI and Lookout move over.
//! Every endpoint is a [`Route`] in [`routes`], so a test can check them against the server's OpenAPI document.

use std::time::Duration;

use reqwest::{Method, RequestBuilder};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use trailhead_model::{CommentKind, Grade, Priority, Status, Task, TaskDetail, TaskKey, Verdict};
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

    /// The path with its one parameter (`{task}` or `{id}`) filled in.
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
    pub const RESOLVE_COMMENT: Route = Route::new(Method::POST, "/v1/comments/{id}/resolve");
    pub const UNDO: Route = Route::new(Method::POST, "/v1/undo");
    pub const REVERT: Route = Route::new(Method::POST, "/v1/revert");

    /// All of them.
    pub fn all() -> [Route; 12] {
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
            RESOLVE_COMMENT,
            UNDO,
            REVERT,
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
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct NewTask {
    pub project: String,
    pub title: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,
    /// The key of the task this one goes under.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Only a human can create a task with a grade.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grade: Option<Grade>,
}

/// `PATCH /v1/tasks/{task}`: only the fields set are sent.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct TaskUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,
    /// `Some(None)` sends `null`, which clears the grade.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grade: Option<Option<Grade>>,
    /// Merged into the task's metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

/// `POST /v1/tasks/{task}/comments`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NewComment {
    pub kind: CommentKind,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl NewComment {
    pub fn new(kind: CommentKind, body: impl Into<String>) -> NewComment {
        NewComment { kind, body: body.into(), verdict: None, model: None }
    }
}

/// `POST /v1/undo`: reverts one event.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Undo {
    pub event_id: i64,
}

/// `POST /v1/revert`: reverts what one actor did since a time (humans only).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Revert {
    pub actor_id: Uuid,
    /// RFC 3339.
    pub since: String,
}

/// A write as a value a caller can hold, compare and send later: Lookout queues these as commands. [`Client::request_for`]
/// builds its request, for the caller to add headers to (`If-Match`, `Idempotency-Key`) and send.
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    CreateTask(NewTask),
    UpdateTask {
        key: TaskKey,
        update: TaskUpdate,
    },
    AddComment {
        key: TaskKey,
        comment: NewComment,
    },
    /// Resolves the comment with this id, with the answer when it is a question.
    ResolveComment {
        id: String,
        answer: Option<String>,
    },
    Release(TaskKey),
    Undo(Undo),
    Revert(Revert),
}

impl Call {
    fn route(&self) -> (Route, Option<&str>) {
        match self {
            Call::CreateTask(_) => (routes::CREATE_TASK, None),
            Call::UpdateTask { key, .. } => (routes::UPDATE_TASK, Some(&key.0)),
            Call::AddComment { key, .. } => (routes::ADD_COMMENT, Some(&key.0)),
            Call::ResolveComment { id, .. } => (routes::RESOLVE_COMMENT, Some(id)),
            Call::Release(key) => (routes::RELEASE, Some(&key.0)),
            Call::Undo(_) => (routes::UNDO, None),
            Call::Revert(_) => (routes::REVERT, None),
        }
    }

    /// The JSON body, if the endpoint takes one.
    pub fn body(&self) -> Option<Value> {
        fn json<T: Serialize>(v: &T) -> Value {
            serde_json::to_value(v).expect("a request body serializes")
        }
        match self {
            Call::CreateTask(b) => Some(json(b)),
            Call::UpdateTask { update, .. } => Some(json(update)),
            Call::AddComment { comment, .. } => Some(json(comment)),
            Call::ResolveComment { answer: Some(a), .. } => Some(serde_json::json!({ "answer": a })),
            Call::ResolveComment { answer: None, .. } | Call::Release(_) => None,
            Call::Undo(b) => Some(json(b)),
            Call::Revert(b) => Some(json(b)),
        }
    }

    /// The path this call goes to, e.g. `/v1/tasks/TRAIL-1`.
    pub fn path(&self) -> String {
        let (route, param) = self.route();
        route.url(param)
    }

    /// The method this call uses.
    pub fn method(&self) -> Method {
        self.route().0.method
    }
}

/// `GET /v1/tasks` filters. Unset ones aren't sent.
#[derive(Debug, Default, Clone)]
pub struct TaskQuery {
    pub project: Option<String>,
    /// Statuses to list; empty for the server's default (everything unfinished).
    pub status: Vec<Status>,
    pub parent: Option<TaskKey>,
    pub title: Option<String>,
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
}

impl Client {
    /// `base` is the server's root URL, e.g. `http://127.0.0.1:7241`.
    pub fn new(base: impl Into<String>, token: impl Into<String>) -> Client {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(60)).build().expect("http client");
        Client { http, base: base.into().trim_end_matches('/').to_string(), token: token.into() }
    }

    /// A client that sends over `http`, which the caller has set up (timeouts, TLS) its own way.
    pub fn with_http(http: reqwest::Client, base: impl Into<String>, token: impl Into<String>) -> Client {
        Client { http, base: base.into().trim_end_matches('/').to_string(), token: token.into() }
    }

    fn request(&self, route: Route, task: Option<&TaskKey>) -> RequestBuilder {
        let url = format!("{}{}", self.base, route.url(task.map(|t| t.0.as_str())));
        self.http.request(route.method.clone(), url).bearer_auth(&self.token)
    }

    /// The request for a [`Call`], with its body, ready for the caller to add headers to and send.
    pub fn request_for(&self, call: &Call) -> RequestBuilder {
        let (route, param) = call.route();
        let req = self.http.request(route.method.clone(), format!("{}{}", self.base, route.url(param)));
        let req = req.bearer_auth(&self.token);
        match call.body() {
            Some(body) => req.json(&body),
            None => req,
        }
    }

    async fn send<T: DeserializeOwned>(&self, req: RequestBuilder) -> Result<T> {
        let res = req.send().await.map_err(Error::Transport)?;
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
        self.send(self.request(routes::SHOW_TASK, Some(key))).await
    }

    /// `POST /v1/tasks`; the new task's key.
    pub async fn create_task(&self, new: &NewTask) -> Result<TaskKey> {
        let task: Task = self.send(self.request(routes::CREATE_TASK, None).json(new)).await?;
        Ok(task.key)
    }

    /// `PATCH /v1/tasks/{task}`.
    pub async fn update_task(&self, key: &TaskKey, update: &TaskUpdate) -> Result<()> {
        self.send::<Value>(self.request(routes::UPDATE_TASK, Some(key)).json(update)).await.map(drop)
    }

    /// `POST /v1/tasks/{task}/comments`.
    pub async fn add_comment(&self, key: &TaskKey, comment: &NewComment) -> Result<()> {
        self.send::<Value>(self.request(routes::ADD_COMMENT, Some(key)).json(comment)).await.map(drop)
    }

    /// `POST /v1/tasks/{task}/claim`.
    pub async fn claim(&self, key: &TaskKey) -> Result<()> {
        self.send::<Value>(self.request(routes::CLAIM, Some(key)).json(&serde_json::json!({}))).await.map(drop)
    }

    /// `POST /v1/tasks/{task}/renew`.
    pub async fn renew(&self, key: &TaskKey) -> Result<()> {
        self.send::<Value>(self.request(routes::RENEW, Some(key)).json(&serde_json::json!({}))).await.map(drop)
    }

    /// `POST /v1/tasks/{task}/release`.
    pub async fn release(&self, key: &TaskKey) -> Result<()> {
        self.send::<Value>(self.request(routes::RELEASE, Some(key)).json(&serde_json::json!({}))).await.map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_names_its_endpoint_and_body() {
        let key = TaskKey("TRAIL-1".into());
        let clear = TaskUpdate { grade: Some(None), ..Default::default() };
        let call = Call::UpdateTask { key: key.clone(), update: clear };
        assert_eq!((call.method(), call.path()), (Method::PATCH, "/v1/tasks/TRAIL-1".to_string()));
        assert_eq!(call.body(), Some(serde_json::json!({"grade": null})));
        assert_eq!(Call::Release(key).body(), None);
        let resolve = Call::ResolveComment { id: "c1".into(), answer: Some("yes".into()) };
        assert_eq!(resolve.path(), "/v1/comments/c1/resolve");
        assert_eq!(resolve.body(), Some(serde_json::json!({"answer": "yes"})));
    }
}
