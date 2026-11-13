//! A typed client for the Trailhead API (`http` feature): base URL, bearer token, request bodies built from
//! `trailhead-model`'s types, and failed responses decoded into an [`Error`] that carries the stable code.
//!
//! It covers the endpoints `RestTracker` (`trailhead-tracker-rest`) uses; the rest follow as the CLI and Lookout move over.
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
    Claim, ClaimResult, Comment, CommentKind, CreatedTask, Dependency, Grade, HeldClaim, Priority, Project,
    ProjectStatus, SearchHit, Status, Task, TaskDetail, TaskKey, Verdict,
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

    /// The path with its `{task}`, `{project}` or `{id}` parameter filled in.
    fn url(&self, param: Option<&str>) -> String {
        match param {
            Some(p) => self.path.replace("{task}", p).replace("{project}", p).replace("{id}", p),
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
    pub const SEARCH: Route = Route::new(Method::GET, "/v1/search");
    pub const RESOLVE_COMMENT: Route = Route::new(Method::POST, "/v1/comments/{id}/resolve");
    pub const UNDO: Route = Route::new(Method::POST, "/v1/undo");
    pub const REVERT: Route = Route::new(Method::POST, "/v1/revert");
    pub const CREATE_PROJECT: Route = Route::new(Method::POST, "/v1/projects");
    pub const UPDATE_PROJECT: Route = Route::new(Method::PATCH, "/v1/projects/{project}");

    pub const DELETE_TASK: Route = Route::new(Method::DELETE, "/v1/tasks/{task}");
    pub const ADD_BLOCKER: Route = Route::new(Method::POST, "/v1/tasks/{task}/blockers");
    pub const REMOVE_BLOCKER: Route = Route::new(Method::DELETE, "/v1/tasks/{task}/blockers/{blocker}");
    pub const LIST_PROJECTS: Route = Route::new(Method::GET, "/v1/projects");
    pub const SHOW_PROJECT: Route = Route::new(Method::GET, "/v1/projects/{project}");

    /// All of them.
    pub fn all() -> [Route; 21] {
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
            UNDO,
            REVERT,
            MY_CLAIMS,
            SEARCH,
            RESOLVE_COMMENT,
            CREATE_PROJECT,
            UPDATE_PROJECT,
            DELETE_TASK,
            ADD_BLOCKER,
            REMOVE_BLOCKER,
            LIST_PROJECTS,
            SHOW_PROJECT,
        ]
    }
}

/// Why a call failed.
#[derive(Debug)]
pub enum Error {
    /// The request didn't complete, or the body wasn't readable.
    Transport(reqwest::Error),
    /// The server answered 2xx but the body didn't deserialize: a schema mismatch between client and server, not a network failure.
    Decode { status: u16, source: reqwest::Error },
    /// The server answered with an error: its HTTP status and the stable code and message.
    Api { status: u16, error: ApiError },
}

impl Error {
    /// The stable error code (`claim_held`, `not_found`, ...), when the server sent one.
    pub fn code(&self) -> Option<&str> {
        match self {
            Error::Api { error, .. } => Some(&error.code),
            Error::Transport(_) | Error::Decode { .. } => None,
        }
    }

    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Api { status, .. } | Error::Decode { status, .. } => Some(*status),
            Error::Transport(_) => None,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Transport(e) => write!(f, "{e}"),
            Error::Decode { status, source } => write!(f, "{status} response body didn't decode: {source}"),
            Error::Api { status, error } => write!(f, "{status} {error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Transport(e) | Error::Decode { source: e, .. } => Some(e),
            Error::Api { .. } => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// A failed response as an [`Error::Api`].
async fn api_error(res: reqwest::Response) -> Error {
    let status = res.status().as_u16();
    let body: Value = res.json().await.unwrap_or(Value::Null);
    Error::Api { status, error: ApiError::from_body(&body) }
}

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
    /// The day it's due, `YYYY-MM-DD`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_on: Option<String>,
    /// A time it's due by, RFC 3339 with the offset it was set in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_at: Option<String>,
    /// The IANA zone the deadline was set in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_tz: Option<String>,
    /// The day to work it, `YYYY-MM-DD`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_on: Option<String>,
    /// A time on that day, RFC 3339 with the offset it was planned in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<String>,
    /// The IANA zone `scheduled_at` was planned in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_tz: Option<String>,
    /// Keys of the tasks that must finish first, added with the task.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
    /// Keys of the tasks this one blocks, added the same way.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<String>,
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
    /// A handoff comment written with the update, or not at all if the update is refused.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff: Option<String>,
    /// The key of the task this one duplicates, which needs `cancelled`. `Some(None)` clears it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate_of: Option<Option<String>>,
    /// `YYYY-MM-DD`. A new day without `due_at` drops the time; `Some(None)` removes the deadline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_on: Option<Option<String>>,
    /// RFC 3339 with an offset; moves the day to match. `Some(None)` keeps the day without a time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_at: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_tz: Option<Option<String>>,
    /// `YYYY-MM-DD`. A new day without `scheduled_at` drops the time; `Some(None)` unschedules.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_on: Option<Option<String>>,
    /// RFC 3339 with an offset; moves the day to match. `Some(None)` keeps the day without a time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_tz: Option<Option<String>>,
    /// The key of the task to move this one under; `Some(None)` makes it top-level.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<Option<String>>,
    /// Refuses the update if the task has changed since this version, like `If-Match`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
}

/// `POST /v1/projects`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NewProject {
    pub key: String,
    pub name: String,
    pub brief: String,
    pub require_review: bool,
    /// The claim lease in seconds; unset takes the workspace's default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim_ttl: Option<i64>,
}

/// `PATCH /v1/projects/{project}`: only the fields set are sent.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct ProjectUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brief: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub require_review: Option<bool>,
    /// `Some(None)` sends `null`, which goes back to the workspace's default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim_ttl: Option<Option<i64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<ProjectStatus>,
    /// Names or ids of the agents that may approve review work here, replacing the list; empty clears it. Humans only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewers: Option<Vec<String>>,
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
    /// The user or agent the comment is for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub addressed_to: Option<Uuid>,
}

impl NewComment {
    pub fn new(kind: CommentKind, body: impl Into<String>) -> NewComment {
        NewComment { kind, body: body.into(), verdict: None, model: None, addressed_to: None }
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
    CreateProject(NewProject),
    UpdateProject {
        key: String,
        update: ProjectUpdate,
    },
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
            Call::CreateProject(_) => (routes::CREATE_PROJECT, None),
            Call::UpdateProject { key, .. } => (routes::UPDATE_PROJECT, Some(key)),
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
            Call::CreateProject(b) => Some(json(b)),
            Call::UpdateProject { update, .. } => Some(json(update)),
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
    /// `1` to `4`, `graded` or `ungraded`.
    pub grade: Option<String>,
    /// Only tasks scheduled on or before this day.
    pub scheduled_through: Option<jiff::civil::Date>,
    /// `priority` (the server's default), `updated_at` or `created_at`, with `-` for newest first, or `scheduled`.
    pub sort: Option<String>,
    /// Skips this many, to page with `limit`.
    pub offset: Option<usize>,
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

/// `GET /v1/ready` filters. Unset ones aren't sent.
#[derive(Debug, Default, Clone)]
pub struct ReadyQuery {
    pub project: Option<String>,
    /// `1` to `4`, `graded` or `ungraded`.
    pub grade: Option<String>,
    pub limit: Option<usize>,
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

    /// A client that sends over `http`, which the caller has set up (timeouts, TLS) its own way.
    pub fn with_http(http: reqwest::Client, base: impl Into<String>, token: impl Into<String>) -> Client {
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

    /// The request for a [`Call`], with its body, ready for the caller to add headers to and send.
    pub fn request_for(&self, call: &Call) -> RequestBuilder {
        let (route, param) = call.route();
        let req = self.request(route, param);
        match call.body() {
            Some(body) => req.json(&body),
            None => req,
        }
    }

    async fn send<T: DeserializeOwned>(&self, req: RequestBuilder) -> Result<T> {
        let res = self.dispatch(req).await?;
        let status = res.status();
        if status.is_success() {
            return match res.json().await {
                Ok(v) => Ok(v),
                Err(e) if e.is_decode() => Err(Error::Decode { status: status.as_u16(), source: e }),
                Err(e) => Err(Error::Transport(e)),
            };
        }
        Err(api_error(res).await)
    }

    /// Sends `req`, reconnecting and retrying once if it couldn't connect, and returns the response whatever its status.
    async fn dispatch(&self, req: RequestBuilder) -> Result<reqwest::Response> {
        let retry = req.try_clone();
        match req.send().await {
            Ok(res) => Ok(res),
            Err(e) => match (&self.reconnect, retry) {
                (Some(reconnect), Some(retry)) if e.is_connect() && reconnect().await => {
                    retry.send().await.map_err(Error::Transport)
                }
                _ => Err(Error::Transport(e)),
            },
        }
    }

    /// `GET /v1/ready`.
    pub async fn ready(&self, q: &ReadyQuery) -> Result<Vec<Task>> {
        let query =
            [("project", q.project.clone()), ("grade", q.grade.clone()), ("limit", q.limit.map(|l| l.to_string()))];
        let query: Vec<_> = query.into_iter().filter_map(|(k, v)| v.map(|v| (k, v))).collect();
        self.send(self.request(routes::READY, None).query(&query)).await
    }

    /// `GET /v1/search`: the tasks matching `query` in their title, description or comments, at most `limit` (or the
    /// server's default).
    pub async fn search(&self, query: &str, limit: Option<u32>) -> Result<Vec<SearchHit>> {
        let mut req = self.request(routes::SEARCH, None).query(&[("q", query)]);
        if let Some(l) = limit {
            req = req.query(&[("limit", l)]);
        }
        self.send(req).await
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
        if let Some(g) = &q.grade {
            query.push(("grade", g.clone()));
        }
        if let Some(d) = q.scheduled_through {
            query.push(("scheduled_through", d.to_string()));
        }
        if let Some(s) = &q.sort {
            query.push(("sort", s.clone()));
        }
        if let Some(o) = q.offset {
            query.push(("offset", o.to_string()));
        }
        self.send(self.request(routes::LIST_TASKS, None).query(&query)).await
    }

    /// `GET /v1/tasks/{task}`.
    pub async fn show_task(&self, key: &TaskKey) -> Result<TaskDetail> {
        self.send(self.request(routes::SHOW_TASK, Some(key.as_str()))).await
    }

    /// `POST /v1/tasks`.
    pub async fn create_task(&self, new: &NewTask) -> Result<CreatedTask> {
        self.send(self.request(routes::CREATE_TASK, None).json(new)).await
    }

    /// `PATCH /v1/tasks/{task}`.
    pub async fn update_task(&self, key: &TaskKey, update: &TaskUpdate) -> Result<Task> {
        self.send(self.request(routes::UPDATE_TASK, Some(key.as_str())).json(update)).await
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

impl Client {
    /// The request for a route whose parameters [`Route::url`] doesn't fill: each `(name, value)` replaces `{name}`.
    fn request_with(&self, route: Route, params: &[(&str, &str)]) -> RequestBuilder {
        let path = params
            .iter()
            .fold(route.path.to_string(), |path, (name, value)| path.replace(&format!("{{{name}}}"), value));
        self.http.request(route.method.clone(), format!("{}{path}", self.base)).bearer_auth(&self.token)
    }

    /// Sends `req` to an endpoint that answers `204 No Content`.
    async fn send_empty(&self, req: RequestBuilder) -> Result<()> {
        let res = self.dispatch(req).await?;
        if res.status().is_success() { Ok(()) } else { Err(api_error(res).await) }
    }

    /// `DELETE /v1/tasks/{task}`: the task, soft-deleted.
    pub async fn delete_task(&self, key: &TaskKey) -> Result<Task> {
        self.send(self.request(routes::DELETE_TASK, Some(key.as_str()))).await
    }

    /// `POST /v1/tasks/{task}/blockers`: `blocker` must finish before `blocked`.
    pub async fn add_blocker(&self, blocked: &TaskKey, blocker: &TaskKey) -> Result<Dependency> {
        let req = self.request(routes::ADD_BLOCKER, Some(blocked.as_str()));
        self.send(req.json(&serde_json::json!({ "blocker": blocker }))).await
    }

    /// `DELETE /v1/tasks/{task}/blockers/{blocker}`.
    pub async fn remove_blocker(&self, blocked: &TaskKey, blocker: &TaskKey) -> Result<()> {
        let req =
            self.request_with(routes::REMOVE_BLOCKER, &[("task", blocked.as_str()), ("blocker", blocker.as_str())]);
        self.send_empty(req).await
    }

    /// `GET /v1/projects`, with the archived ones too if `archived`.
    pub async fn list_projects(&self, archived: bool) -> Result<Vec<Project>> {
        self.send(self.request(routes::LIST_PROJECTS, None).query(&[("archived", archived)])).await
    }

    /// `POST /v1/projects`.
    pub async fn create_project(&self, new: &NewProject) -> Result<Project> {
        self.send(self.request(routes::CREATE_PROJECT, None).json(new)).await
    }

    /// `GET /v1/projects/{project}`.
    pub async fn show_project(&self, key: &str) -> Result<Project> {
        self.send(self.request(routes::SHOW_PROJECT, Some(key))).await
    }

    /// `PATCH /v1/projects/{project}`.
    pub async fn update_project(&self, key: &str, update: &ProjectUpdate) -> Result<Project> {
        self.send(self.request(routes::UPDATE_PROJECT, Some(key)).json(update)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// Serves one canned response on a local port and returns its base URL.
    fn serve_once(response: &'static str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            stream.write_all(response.as_bytes()).unwrap();
        });
        base
    }

    #[tokio::test]
    async fn an_undecodable_success_body_is_a_decode_error_not_transport() {
        let base = serve_once(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 7\r\nconnection: close\r\n\r\n{\"x\":1}",
        );
        let err = Client::new(base, "t").ready(&ReadyQuery::default()).await.unwrap_err();
        assert!(matches!(err, Error::Decode { status: 200, .. }), "{err}");
        assert_eq!(err.status(), Some(200));
        assert!(std::error::Error::source(&err).is_some_and(|s| s.is::<reqwest::Error>()));
    }

    #[tokio::test]
    async fn a_refused_connection_stays_transport() {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let err = Client::new(format!("http://127.0.0.1:{port}"), "t").ready(&ReadyQuery::default()).await.unwrap_err();
        assert!(matches!(err, Error::Transport(_)), "{err}");
        assert!(std::error::Error::source(&err).is_some_and(|s| s.is::<reqwest::Error>()));
    }

    #[test]
    fn with_token_swaps_only_the_token() {
        let client = Client::new("http://127.0.0.1:1/", "a").with_reconnect(Arc::new(|| Box::pin(async { false })));
        let other = client.with_token("b");
        assert_eq!((other.base.as_str(), other.token.as_str()), ("http://127.0.0.1:1", "b"));
        assert_eq!(client.token, "a");
        assert!(other.reconnect.is_some());
    }

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
