//! A typed client for the Trailhead API (`http` feature): base URL, bearer token, request bodies built from
//! `trailhead-model`'s types, and failed responses decoded into an [`Error`] that carries the stable code.
//!
//! It covers the endpoints the CLI and `RestTracker` (`trailhead-tracker-rest`) use; the rest follow as Lookout moves over.
//! Every endpoint is a [`Route`] in [`routes`], so a test can check them against the server's OpenAPI document.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use reqwest::{Method, RequestBuilder};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use trailhead_model::{
    Agent, Attachment, AttachmentRole, Claim, ClaimResult, Comment, CommentKind, CreatedAgent, CreatedTask,
    CreatedToken, CreatedWebhook, Dependency, Event, Grade, HeldClaim, Me, Notification, NotificationAction,
    NotificationSettings, Priority, Project, ProjectStatus, SearchHit, Status, Task, TaskDetail, TaskKey, Token,
    UndoReport, Verdict, Webhook, WebhookDelivery,
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
    pub const SET_ASIDE: Route = Route::new(Method::POST, "/v1/tasks/{task}/set-aside");
    pub const APPROVE_PLAN: Route = Route::new(Method::POST, "/v1/tasks/{task}/plan/approve");
    pub const MY_CLAIMS: Route = Route::new(Method::GET, "/v1/claims");
    pub const SEARCH: Route = Route::new(Method::GET, "/v1/search");
    pub const RESOLVE_COMMENT: Route = Route::new(Method::POST, "/v1/comments/{id}/resolve");
    pub const LIST_ATTACHMENTS: Route = Route::new(Method::GET, "/v1/tasks/{task}/attachments");
    pub const ADD_ATTACHMENT: Route = Route::new(Method::POST, "/v1/tasks/{task}/attachments");
    pub const READ_ATTACHMENT: Route = Route::new(Method::GET, "/v1/attachments/{id}");
    pub const REMOVE_ATTACHMENT: Route = Route::new(Method::DELETE, "/v1/attachments/{id}");
    pub const LIST_NOTIFICATIONS: Route = Route::new(Method::GET, "/v1/notifications");
    pub const RAISE_NOTIFICATION: Route = Route::new(Method::POST, "/v1/notifications");
    pub const READ_ALL_NOTIFICATIONS: Route = Route::new(Method::POST, "/v1/notifications/read-all");
    pub const RESOLVE_NOTIFICATIONS: Route = Route::new(Method::POST, "/v1/notifications/resolve");
    pub const READ_NOTIFICATION: Route = Route::new(Method::POST, "/v1/notifications/{id}/read");
    pub const UNREAD_NOTIFICATION: Route = Route::new(Method::POST, "/v1/notifications/{id}/unread");
    pub const RESOLVE_NOTIFICATION: Route = Route::new(Method::POST, "/v1/notifications/{id}/resolve");
    pub const NOTIFICATION_SETTINGS: Route = Route::new(Method::GET, "/v1/notifications/settings");
    pub const SET_NOTIFICATION_SETTINGS: Route = Route::new(Method::PUT, "/v1/notifications/settings");
    pub const UNDO: Route = Route::new(Method::POST, "/v1/undo");
    pub const REVERT: Route = Route::new(Method::POST, "/v1/revert");
    pub const CREATE_PROJECT: Route = Route::new(Method::POST, "/v1/projects");
    pub const UPDATE_PROJECT: Route = Route::new(Method::PATCH, "/v1/projects/{project}");

    pub const DELETE_TASK: Route = Route::new(Method::DELETE, "/v1/tasks/{task}");
    pub const ADD_BLOCKER: Route = Route::new(Method::POST, "/v1/tasks/{task}/blockers");
    pub const REMOVE_BLOCKER: Route = Route::new(Method::DELETE, "/v1/tasks/{task}/blockers/{blocker}");
    pub const LIST_PROJECTS: Route = Route::new(Method::GET, "/v1/projects");
    pub const SHOW_PROJECT: Route = Route::new(Method::GET, "/v1/projects/{project}");
    pub const ME: Route = Route::new(Method::GET, "/v1/me");
    pub const CONFIG: Route = Route::new(Method::GET, "/v1/config");
    pub const UPDATE_CONFIG: Route = Route::new(Method::PATCH, "/v1/config");
    pub const SHELL_BUILD: Route = Route::new(Method::GET, "/v1/shell/build");
    pub const SHELL_ASK_TARGETS: Route = Route::new(Method::GET, "/v1/shell/ask");
    pub const SHELL_ASK: Route = Route::new(Method::POST, "/v1/shell/ask");
    pub const LIST_AGENTS: Route = Route::new(Method::GET, "/v1/agents");
    pub const CREATE_AGENT: Route = Route::new(Method::POST, "/v1/agents");
    pub const REVOKE_AGENT: Route = Route::new(Method::POST, "/v1/agents/{agent}/revoke");
    pub const LIST_TOKENS: Route = Route::new(Method::GET, "/v1/tokens");
    pub const CREATE_TOKEN: Route = Route::new(Method::POST, "/v1/tokens");
    pub const REVOKE_TOKEN: Route = Route::new(Method::DELETE, "/v1/tokens/{id}");
    pub const UPDATE_TOKEN: Route = Route::new(Method::PATCH, "/v1/tokens/{id}");
    pub const LIST_EVENTS: Route = Route::new(Method::GET, "/v1/events");
    pub const LIST_WEBHOOKS: Route = Route::new(Method::GET, "/v1/webhooks");
    pub const CREATE_WEBHOOK: Route = Route::new(Method::POST, "/v1/webhooks");
    pub const DELETE_WEBHOOK: Route = Route::new(Method::DELETE, "/v1/webhooks/{id}");
    pub const WEBHOOK_DELIVERIES: Route = Route::new(Method::GET, "/v1/webhooks/{id}/deliveries");
    /// These two have no method on [`Client`]: Lookout's sync reads them its own way (a snapshot it takes record by
    /// record, and a stream), and names them here so the paths are checked with the rest.
    pub const LIST_USERS: Route = Route::new(Method::GET, "/v1/users");
    pub const EVENT_STREAM: Route = Route::new(Method::GET, "/v1/events/stream");

    /// All of them.
    pub fn all() -> [Route; 51] {
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
            SET_ASIDE,
            APPROVE_PLAN,
            UNDO,
            REVERT,
            MY_CLAIMS,
            SEARCH,
            RESOLVE_COMMENT,
            LIST_ATTACHMENTS,
            ADD_ATTACHMENT,
            READ_ATTACHMENT,
            REMOVE_ATTACHMENT,
            CREATE_PROJECT,
            UPDATE_PROJECT,
            DELETE_TASK,
            ADD_BLOCKER,
            REMOVE_BLOCKER,
            LIST_PROJECTS,
            SHOW_PROJECT,
            ME,
            CONFIG,
            UPDATE_CONFIG,
            SHELL_BUILD,
            SHELL_ASK_TARGETS,
            SHELL_ASK,
            LIST_AGENTS,
            CREATE_AGENT,
            REVOKE_AGENT,
            LIST_TOKENS,
            CREATE_TOKEN,
            REVOKE_TOKEN,
            UPDATE_TOKEN,
            LIST_EVENTS,
            LIST_WEBHOOKS,
            CREATE_WEBHOOK,
            DELETE_WEBHOOK,
            WEBHOOK_DELIVERIES,
            LIST_USERS,
            EVENT_STREAM,
            RAISE_NOTIFICATION,
            RESOLVE_NOTIFICATIONS,
            NOTIFICATION_SETTINGS,
            SET_NOTIFICATION_SETTINGS,
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

    /// The error and its sources, `a: b: c`, for a place that shows one string: `Display` names only this layer.
    pub fn chain(&self) -> String {
        let mut out = self.to_string();
        let mut next = std::error::Error::source(self);
        while let Some(s) = next {
            out.push_str(": ");
            out.push_str(&s.to_string());
            next = s.source();
        }
        out
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
            // The reqwest error is the `source()`, so each layer says only its own part.
            Error::Transport(_) => write!(f, "request to Trailhead failed"),
            Error::Decode { status, .. } => write!(f, "{status} response body didn't decode"),
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

/// `POST /v1/tasks/{task}/attachments`: what goes with the bytes.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct NewAttachment {
    pub name: String,
    pub role: Option<AttachmentRole>,
    pub set: Option<String>,
}

/// `GET /v1/notifications`: a page of them, newest first, and how many are unread in all.
#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
pub struct NotificationList {
    pub notifications: Vec<Notification>,
    pub unread: i64,
}

/// `POST /v1/notifications`.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct NewNotification {
    /// What raises it: an app's id, `factory` or `server`.
    pub source: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    /// What it is about, as the raiser names it, to resolve by later.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// The key or id of the task it is about.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<NotificationAction>,
}

/// How many notifications a call changed.
#[derive(Debug, Deserialize)]
struct Count {
    count: usize,
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
    /// The task's metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
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
    /// An RFC 7386 merge patch to the project's metadata: `null` removes a key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
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
    /// Sets the task aside as a person, with what they wrote, if anything.
    SetAside {
        key: TaskKey,
        note: Option<String>,
    },
    /// Approves the plan under the spec task `key`, grading each of its open, ungraded subtasks (by key).
    ApprovePlan {
        key: TaskKey,
        grades: BTreeMap<String, Grade>,
    },
    /// `blocker` must finish before `blocked`.
    AddBlocker {
        blocked: TaskKey,
        blocker: TaskKey,
    },
    /// `blocked` no longer waits on `blocker`.
    RemoveBlocker {
        blocked: TaskKey,
        blocker: TaskKey,
    },
    Undo(Undo),
    Revert(Revert),
    CreateProject(NewProject),
    UpdateProject {
        key: String,
        update: ProjectUpdate,
    },
    /// Marks the notification with this id read.
    ReadNotification(String),
    /// Marks the notification with this id unread.
    UnreadNotification(String),
    /// Marks every notification read.
    ReadAllNotifications,
    /// Raises a notification.
    RaiseNotification(NewNotification),
    /// Resolves every open notification `source` raised about `subject`.
    ResolveNotifications {
        source: String,
        subject: String,
    },
    /// Replaces how the person wants to be told with these settings, whole. Only a person may.
    SetNotificationSettings(NotificationSettings),
}

impl Call {
    /// The endpoint, and the value of each of its path's parameters.
    fn route(&self) -> (Route, Vec<(&'static str, &str)>) {
        match self {
            Call::CreateTask(_) => (routes::CREATE_TASK, vec![]),
            Call::UpdateTask { key, .. } => (routes::UPDATE_TASK, vec![("task", &key.0)]),
            Call::AddComment { key, .. } => (routes::ADD_COMMENT, vec![("task", &key.0)]),
            Call::ResolveComment { id, .. } => (routes::RESOLVE_COMMENT, vec![("id", id)]),
            Call::Release(key) => (routes::RELEASE, vec![("task", &key.0)]),
            Call::SetAside { key, .. } => (routes::SET_ASIDE, vec![("task", &key.0)]),
            Call::ApprovePlan { key, .. } => (routes::APPROVE_PLAN, vec![("task", &key.0)]),
            Call::AddBlocker { blocked, .. } => (routes::ADD_BLOCKER, vec![("task", &blocked.0)]),
            Call::RemoveBlocker { blocked, blocker } => {
                (routes::REMOVE_BLOCKER, vec![("task", &blocked.0), ("blocker", &blocker.0)])
            }
            Call::Undo(_) => (routes::UNDO, vec![]),
            Call::Revert(_) => (routes::REVERT, vec![]),
            Call::CreateProject(_) => (routes::CREATE_PROJECT, vec![]),
            Call::UpdateProject { key, .. } => (routes::UPDATE_PROJECT, vec![("project", key)]),
            Call::ReadNotification(id) => (routes::READ_NOTIFICATION, vec![("id", id)]),
            Call::UnreadNotification(id) => (routes::UNREAD_NOTIFICATION, vec![("id", id)]),
            Call::ReadAllNotifications => (routes::READ_ALL_NOTIFICATIONS, vec![]),
            Call::RaiseNotification(_) => (routes::RAISE_NOTIFICATION, vec![]),
            Call::ResolveNotifications { .. } => (routes::RESOLVE_NOTIFICATIONS, vec![]),
            Call::SetNotificationSettings(_) => (routes::SET_NOTIFICATION_SETTINGS, vec![]),
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
            Call::ResolveComment { answer: None, .. }
            | Call::Release(_)
            | Call::RemoveBlocker { .. }
            | Call::ReadNotification(_)
            | Call::UnreadNotification(_)
            | Call::ReadAllNotifications => None,
            Call::AddBlocker { blocker, .. } => Some(serde_json::json!({ "blocker": blocker })),
            Call::SetAside { note, .. } => Some(serde_json::json!({ "note": note })),
            Call::ApprovePlan { grades, .. } => Some(serde_json::json!({ "grades": grades })),
            Call::Undo(b) => Some(json(b)),
            Call::Revert(b) => Some(json(b)),
            Call::CreateProject(b) => Some(json(b)),
            Call::UpdateProject { update, .. } => Some(json(update)),
            Call::RaiseNotification(b) => Some(json(b)),
            Call::ResolveNotifications { source, subject } => {
                Some(serde_json::json!({ "source": source, "subject": subject }))
            }
            Call::SetNotificationSettings(b) => Some(json(b)),
        }
    }

    /// The path this call goes to, e.g. `/v1/tasks/TRAIL-1`.
    pub fn path(&self) -> String {
        let (route, params) = self.route();
        params.iter().fold(route.path.to_string(), |path, (name, value)| path.replace(&format!("{{{name}}}"), value))
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

/// `POST /v1/agents`: an agent and its first token.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct NewAgent {
    pub name: String,
    /// Project keys or ids to limit the token to; empty for every project.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit_per_minute: Option<i64>,
}

/// `POST /v1/tokens`.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct NewToken {
    /// What the token is for.
    pub name: String,
    /// The name or id of the caller's agent to issue it for; unset for the caller.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Project keys or ids to limit the token to; empty for every project.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit_per_minute: Option<i64>,
    /// Seconds until it stops working; unset for never.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in_seconds: Option<i64>,
}

/// `PATCH /v1/tokens/{id}`: only the fields set are sent.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct TokenUpdate {
    /// Project keys or ids. `Some(None)` opens the token to every project.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projects: Option<Option<Vec<String>>>,
    /// Requests per minute. `Some(None)` removes the limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit_per_minute: Option<Option<i64>>,
}

/// `POST /v1/undo` for a whole batch: everything one request did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UndoBatch {
    pub batch_id: Uuid,
}

/// `GET /v1/events` filters. Unset ones aren't sent.
#[derive(Debug, Default, Clone)]
pub struct EventQuery {
    /// A task's key or id.
    pub task: Option<String>,
    /// The user or agent that acted.
    pub actor: Option<Uuid>,
    /// Leaves out notifications (`task.due_soon`, `task.overdue`).
    pub changes_only: bool,
    pub newest_first: bool,
    pub limit: Option<usize>,
}

/// `POST /v1/webhooks`.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct NewWebhook {
    /// Where to POST events.
    pub url: String,
    /// Event kind prefixes, e.g. `task.` or `comment.created`; empty for every event.
    pub event_kinds: Vec<String>,
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
        let req = self.http.request(call.method(), format!("{}{}", self.base, call.path())).bearer_auth(&self.token);
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

    /// `GET /v1/tasks/{task}/attachments`: the task's pictures, without their bytes.
    pub async fn list_attachments(&self, key: &TaskKey) -> Result<Vec<Attachment>> {
        self.send(self.request(routes::LIST_ATTACHMENTS, Some(key.as_str()))).await
    }

    /// `POST /v1/tasks/{task}/attachments`: attaches `png` (its bytes) to the task.
    pub async fn add_attachment(&self, key: &TaskKey, new: &NewAttachment, png: Vec<u8>) -> Result<Attachment> {
        let mut query = vec![("name", new.name.clone())];
        query.extend(new.role.map(|r| ("role", r.to_string())));
        query.extend(new.set.clone().map(|s| ("set", s)));
        let req = self.request(routes::ADD_ATTACHMENT, Some(key.as_str())).query(&query);
        self.send(req.header(reqwest::header::CONTENT_TYPE, trailhead_model::PNG).body(png)).await
    }

    /// `GET /v1/attachments/{id}`: the picture's bytes.
    pub async fn attachment_bytes(&self, id: Uuid) -> Result<Vec<u8>> {
        let res = self.dispatch(self.request(routes::READ_ATTACHMENT, Some(&id.to_string()))).await?;
        if !res.status().is_success() {
            return Err(api_error(res).await);
        }
        Ok(res.bytes().await.map_err(Error::Transport)?.to_vec())
    }

    /// `DELETE /v1/attachments/{id}`: the picture, removed (undo restores it).
    pub async fn remove_attachment(&self, id: Uuid) -> Result<Attachment> {
        self.send(self.request(routes::REMOVE_ATTACHMENT, Some(&id.to_string()))).await
    }

    /// `GET /v1/notifications`: newest first, only the unread ones if asked, those raised before `before` to page back.
    pub async fn list_notifications(&self, unread: bool, before: Option<Uuid>, limit: i64) -> Result<NotificationList> {
        let mut query = vec![("unread", unread.to_string()), ("limit", limit.to_string())];
        query.extend(before.map(|b| ("before", b.to_string())));
        self.send(self.request(routes::LIST_NOTIFICATIONS, None).query(&query)).await
    }

    /// `POST /v1/notifications`.
    pub async fn raise_notification(&self, new: &NewNotification) -> Result<Notification> {
        self.send(self.request(routes::RAISE_NOTIFICATION, None).json(new)).await
    }

    /// `POST /v1/notifications/{id}/read` or `/unread`. Only a person may.
    pub async fn mark_notification(&self, id: Uuid, read: bool) -> Result<Notification> {
        let route = if read { routes::READ_NOTIFICATION } else { routes::UNREAD_NOTIFICATION };
        self.send(self.request(route, Some(&id.to_string()))).await
    }

    /// `POST /v1/notifications/read-all`: how many were unread. Only a person may.
    pub async fn mark_all_notifications_read(&self) -> Result<usize> {
        self.send::<Count>(self.request(routes::READ_ALL_NOTIFICATIONS, None)).await.map(|c| c.count)
    }

    /// `POST /v1/notifications/{id}/resolve`: what it is about no longer needs the person.
    pub async fn resolve_notification(&self, id: Uuid) -> Result<Notification> {
        self.send(self.request(routes::RESOLVE_NOTIFICATION, Some(&id.to_string()))).await
    }

    /// `POST /v1/notifications/resolve`: resolves every open one `source` raised about `subject`.
    pub async fn resolve_notifications_about(&self, source: &str, subject: &str) -> Result<usize> {
        let body = serde_json::json!({ "source": source, "subject": subject });
        self.send::<Count>(self.request(routes::RESOLVE_NOTIFICATIONS, None).json(&body)).await.map(|c| c.count)
    }

    /// `GET /v1/notifications/settings`: how the person wants to be told, the defaults until they set any.
    pub async fn notification_settings(&self) -> Result<NotificationSettings> {
        self.send(self.request(routes::NOTIFICATION_SETTINGS, None)).await
    }

    /// `PUT /v1/notifications/settings`: replaces them whole, and answers them as kept. Only a person may.
    pub async fn set_notification_settings(&self, settings: &NotificationSettings) -> Result<NotificationSettings> {
        self.send(self.request(routes::SET_NOTIFICATION_SETTINGS, None).json(settings)).await
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

    /// `POST /v1/tasks/{task}/plan/approve`, as a person: each of the spec task's open, ungraded subtasks gets its grade
    /// from `grades` (by key, naming every one and nothing else), and the spec task comes back with its plan approved.
    pub async fn approve_plan(&self, key: &TaskKey, grades: &BTreeMap<String, Grade>) -> Result<Task> {
        let body = serde_json::json!({ "grades": grades });
        self.send(self.request(routes::APPROVE_PLAN, Some(key.as_str())).json(&body)).await
    }

    /// `POST /v1/tasks/{task}/set-aside`: the task, open and ungraded, its questions resolved with a note saying so.
    pub async fn set_aside(&self, key: &TaskKey, note: Option<&str>) -> Result<Task> {
        self.send(self.request(routes::SET_ASIDE, Some(key.as_str())).json(&serde_json::json!({ "note": note }))).await
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

    /// `GET /v1/me`: who the token acts as.
    pub async fn me(&self) -> Result<Me> {
        self.send(self.request(routes::ME, None)).await
    }

    /// `GET /v1/config`: the server's settings in its user config, and which wait for a restart. Only a person's
    /// token reads them.
    pub async fn config(&self) -> Result<trailhead_model::config::ServerConfig> {
        self.send(self.request(routes::CONFIG, None)).await
    }

    /// `PATCH /v1/config`: changes the settings in the server's user config, refused (`412`) when the file has
    /// changed since `change.version` was read and (`400`) when the server would refuse the file it leaves.
    pub async fn update_config(
        &self,
        change: &trailhead_model::config::ConfigChange,
    ) -> Result<trailhead_model::config::ServerConfig> {
        self.send(self.request(routes::UPDATE_CONFIG, None).json(change)).await
    }

    /// `GET /v1/shell/build`: the shells the server's builder keeps, and with `since` (the build a shell runs) the
    /// commits after it up to the newest build.
    pub async fn shell_build(&self, since: Option<&str>) -> Result<trailhead_model::shell::ShellBuild> {
        let mut req = self.request(routes::SHELL_BUILD, None);
        if let Some(since) = since {
            req = req.query(&[("since", since)]);
        }
        self.send(req).await
    }

    /// `GET /v1/shell/ask`: what can be asked for from the window, the projects pointed at the apps repo.
    pub async fn shell_ask_targets(&self) -> Result<trailhead_model::shell::AskTargets> {
        self.send(self.request(routes::SHELL_ASK_TARGETS, None)).await
    }

    /// `POST /v1/shell/ask`: a change to an app or a new app, as the person this client acts for. A retry with the
    /// same `idempotency_key` and ask gets the first answer back instead of asking twice.
    pub async fn shell_ask(
        &self,
        ask: &trailhead_model::shell::Ask,
        idempotency_key: Option<&str>,
    ) -> Result<trailhead_model::shell::Asked> {
        let mut req = self.request(routes::SHELL_ASK, None).json(ask);
        if let Some(key) = idempotency_key {
            req = req.header("Idempotency-Key", key);
        }
        self.send(req).await
    }

    /// `GET /v1/agents`.
    pub async fn list_agents(&self) -> Result<Vec<Agent>> {
        self.send(self.request(routes::LIST_AGENTS, None)).await
    }

    /// `POST /v1/agents`: the agent, its token and the token's secret.
    pub async fn create_agent(&self, new: &NewAgent) -> Result<CreatedAgent> {
        self.send(self.request(routes::CREATE_AGENT, None).json(new)).await
    }

    /// `POST /v1/agents/{agent}/revoke`: revokes the agent named `name` (or with that id) and its sub-agents.
    pub async fn revoke_agent(&self, name: &str) -> Result<Agent> {
        let req = self.request_with(routes::REVOKE_AGENT, &[("agent", &name.replace('/', "%2F"))]);
        self.send(req.json(&serde_json::json!({}))).await
    }

    /// `GET /v1/tokens`, without their secrets.
    pub async fn list_tokens(&self) -> Result<Vec<Token>> {
        self.send(self.request(routes::LIST_TOKENS, None)).await
    }

    /// `POST /v1/tokens`: the token and its secret.
    pub async fn create_token(&self, new: &NewToken) -> Result<CreatedToken> {
        self.send(self.request(routes::CREATE_TOKEN, None).json(new)).await
    }

    /// `DELETE /v1/tokens/{id}`: the revoked token.
    pub async fn revoke_token(&self, id: &str) -> Result<Token> {
        self.send(self.request(routes::REVOKE_TOKEN, Some(id))).await
    }

    /// `PATCH /v1/tokens/{id}`.
    pub async fn update_token(&self, id: &str, update: &TokenUpdate) -> Result<Token> {
        self.send(self.request(routes::UPDATE_TOKEN, Some(id)).json(update)).await
    }

    /// `GET /v1/events`.
    pub async fn list_events(&self, q: &EventQuery) -> Result<Vec<Event>> {
        let mut query: Vec<(&str, String)> = Vec::new();
        if let Some(t) = &q.task {
            query.push(("task", t.clone()));
        }
        if let Some(a) = q.actor {
            query.push(("actor", a.to_string()));
        }
        if q.changes_only {
            query.push(("changes_only", "true".into()));
        }
        if q.newest_first {
            query.push(("newest_first", "true".into()));
        }
        if let Some(l) = q.limit {
            query.push(("limit", l.to_string()));
        }
        self.send(self.request(routes::LIST_EVENTS, None).query(&query)).await
    }

    /// `POST /v1/undo`: reverts one event.
    pub async fn undo(&self, undo: &Undo) -> Result<UndoReport> {
        self.send(self.request(routes::UNDO, None).json(undo)).await
    }

    /// `POST /v1/undo`: reverts a batch.
    pub async fn undo_batch(&self, undo: &UndoBatch) -> Result<UndoReport> {
        self.send(self.request(routes::UNDO, None).json(undo)).await
    }

    /// `POST /v1/revert`: reverts what one actor did since a time, skipping what someone else changed later.
    pub async fn revert(&self, revert: &Revert) -> Result<UndoReport> {
        self.send(self.request(routes::REVERT, None).json(revert)).await
    }

    /// `GET /v1/webhooks`, without their secrets.
    pub async fn list_webhooks(&self) -> Result<Vec<Webhook>> {
        self.send(self.request(routes::LIST_WEBHOOKS, None)).await
    }

    /// `POST /v1/webhooks`: the webhook and its signing secret.
    pub async fn create_webhook(&self, new: &NewWebhook) -> Result<CreatedWebhook> {
        self.send(self.request(routes::CREATE_WEBHOOK, None).json(new)).await
    }

    /// `DELETE /v1/webhooks/{id}`.
    pub async fn delete_webhook(&self, id: &str) -> Result<()> {
        self.send_empty(self.request(routes::DELETE_WEBHOOK, Some(id))).await
    }

    /// `GET /v1/webhooks/{id}/deliveries`: the latest `limit` delivery attempts, newest first.
    pub async fn webhook_deliveries(&self, id: &str, limit: usize) -> Result<Vec<WebhookDelivery>> {
        self.send(self.request(routes::WEBHOOK_DELIVERIES, Some(id)).query(&[("limit", limit)])).await
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
        assert_eq!(err.to_string(), "200 response body didn't decode");
        assert!(std::error::Error::source(&err).is_some_and(|s| s.is::<reqwest::Error>()));
    }

    #[tokio::test]
    async fn a_refused_connection_stays_transport() {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let err = Client::new(format!("http://127.0.0.1:{port}"), "t").ready(&ReadyQuery::default()).await.unwrap_err();
        assert!(matches!(err, Error::Transport(_)), "{err}");
        assert_eq!(err.to_string(), "request to Trailhead failed");
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
        let aside = Call::SetAside { key: TaskKey("TRAIL-2".into()), note: Some("later".into()) };
        assert_eq!((aside.method(), aside.path()), (Method::POST, "/v1/tasks/TRAIL-2/set-aside".to_string()));
        assert_eq!(aside.body(), Some(serde_json::json!({"note": "later"})));
        let read = Call::ReadNotification("n1".into());
        assert_eq!((read.method(), read.path(), read.body()), (Method::POST, "/v1/notifications/n1/read".into(), None));
        assert_eq!(Call::UnreadNotification("n1".into()).path(), "/v1/notifications/n1/unread");
        assert_eq!(Call::ReadAllNotifications.path(), "/v1/notifications/read-all");
        let raise = Call::RaiseNotification(NewNotification {
            source: "example".into(),
            kind: "example.milestone".into(),
            title: "Ten presses".into(),
            subject: Some("count".into()),
            ..Default::default()
        });
        assert_eq!((raise.method(), raise.path()), (Method::POST, "/v1/notifications".into()));
        assert_eq!(
            raise.body(),
            Some(serde_json::json!({"source": "example", "kind": "example.milestone", "title": "Ten presses",
                                    "body": "", "subject": "count"}))
        );
        let resolve = Call::ResolveNotifications { source: "example".into(), subject: "count".into() };
        assert_eq!((resolve.method(), resolve.path()), (Method::POST, "/v1/notifications/resolve".into()));
        assert_eq!(resolve.body(), Some(serde_json::json!({"source": "example", "subject": "count"})));
        let settings = Call::SetNotificationSettings(NotificationSettings { sound: false, ..Default::default() });
        assert_eq!((settings.method(), settings.path()), (Method::PUT, "/v1/notifications/settings".into()));
        assert_eq!(settings.body().unwrap()["sound"], serde_json::json!(false));
        let grades = BTreeMap::from([("TRAIL-3".to_string(), Grade::new(2).unwrap())]);
        let approve = Call::ApprovePlan { key: TaskKey("TRAIL-2".into()), grades };
        assert_eq!((approve.method(), approve.path()), (Method::POST, "/v1/tasks/TRAIL-2/plan/approve".to_string()));
        assert_eq!(approve.body(), Some(serde_json::json!({"grades": {"TRAIL-3": 2}})));
        let (blocked, blocker) = (TaskKey("TRAIL-3".into()), TaskKey("TRAIL-4".into()));
        let add = Call::AddBlocker { blocked: blocked.clone(), blocker: blocker.clone() };
        assert_eq!((add.method(), add.path()), (Method::POST, "/v1/tasks/TRAIL-3/blockers".to_string()));
        assert_eq!(add.body(), Some(serde_json::json!({"blocker": "TRAIL-4"})));
        let remove = Call::RemoveBlocker { blocked, blocker };
        assert_eq!(
            (remove.method(), remove.path()),
            (Method::DELETE, "/v1/tasks/TRAIL-3/blockers/TRAIL-4".to_string())
        );
        assert_eq!(remove.body(), None);
    }
}
