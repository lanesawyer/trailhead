use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use trailhead_core::{
    Agent, Claim, ClaimResult, Comment, CommentKind, Dependency, Error, Event, EventFilter, NewComment, NewProject,
    NewTask, Principal, Project, ProjectPatch, ProjectStatus, Status, Task, TaskDetail, TaskFilter, TaskPatch, Token,
    TokenOptions, UndoReport,
};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::AppState;
use crate::auth::{Auth, require_human};
use crate::error::{ApiError, ApiResult, Json, Path, Query, invalid};
use crate::openapi::{CreatedAgent, CreatedToken, ErrorBody, Health, Me};

type S = State<Arc<AppState>>;

pub fn v1() -> Router<Arc<AppState>> {
    Router::new()
        .route("/health", get(health))
        .route("/openapi.json", get(crate::openapi::serve))
        .route("/me", get(me))
        .route("/projects", get(list_projects).post(create_project))
        .route("/projects/{project}", get(get_project).patch(update_project))
        .route("/tasks", get(list_tasks).post(create_task))
        .route("/tasks/{task}", get(get_task).patch(update_task).delete(delete_task))
        .route("/tasks/{task}/claim", post(claim))
        .route("/tasks/{task}/renew", post(renew))
        .route("/tasks/{task}/release", post(release))
        .route("/tasks/{task}/comments", get(list_comments).post(add_comment))
        .route("/tasks/{task}/blockers", get(list_blockers).post(add_blocker))
        .route("/tasks/{task}/blockers/{blocker}", delete(remove_blocker))
        .route("/tasks/{task}/history", get(task_history))
        .route("/comments/{id}/resolve", post(resolve_comment))
        .route("/ready", get(ready))
        .route("/search", get(search))
        .route("/events", get(list_events))
        .route("/events/stream", get(crate::stream::events))
        .route("/undo", post(undo))
        .route("/revert", post(revert))
        .route("/agents", get(list_agents).post(create_agent))
        .route("/tokens", get(list_tokens).post(create_token))
        .route("/tokens/{id}", delete(revoke_token))
}

/// Distinguishes an absent field (`None`) from an explicit `null` (`Some(None)`).
fn nullable<'de, T: Deserialize<'de>, D: Deserializer<'de>>(d: D) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

fn parse_time(s: &str) -> ApiResult<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339).map_err(|_| invalid(format!("{s:?} is not an RFC 3339 timestamp")))
}

fn parse_opt_time(s: Option<&str>) -> ApiResult<Option<OffsetDateTime>> {
    s.map(parse_time).transpose()
}

fn parse_list<T: std::str::FromStr<Err = trailhead_core::Error>>(s: Option<&str>) -> ApiResult<Vec<T>> {
    s.map(|s| s.split(',').filter(|p| !p.is_empty()).map(|p| p.trim().parse()).collect::<Result<Vec<_>, _>>())
        .transpose()
        .map(Option::unwrap_or_default)
        .map_err(Into::into)
}

/// `?fields=key,title` trims each object in a list to the named fields.
fn select_fields<T: Serialize>(items: &[T], fields: Option<&str>) -> ApiResult<Value> {
    let v = serde_json::to_value(items).map_err(trailhead_core::Error::from)?;
    let Some(fields) = fields else { return Ok(v) };
    let keep: Vec<&str> = fields.split(',').map(str::trim).filter(|f| !f.is_empty()).collect();
    let Value::Array(items) = v else { return Ok(v) };
    Ok(Value::Array(
        items
            .into_iter()
            .map(|item| match item {
                Value::Object(mut m) => {
                    Value::Object(keep.iter().filter_map(|k| m.remove(*k).map(|v| (k.to_string(), v))).collect())
                }
                other => other,
            })
            .collect(),
    ))
}

/// Bodies that may be empty, like `POST /claim` with no TTL.
fn optional_body<T: serde::de::DeserializeOwned + Default>(b: &[u8]) -> ApiResult<T> {
    if b.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(b).map_err(|e| invalid(format!("bad JSON body: {e}")))
}

fn if_match(headers: &HeaderMap) -> ApiResult<Option<i64>> {
    let Some(h) = headers.get(header::IF_MATCH) else { return Ok(None) };
    let s = h.to_str().map_err(|_| invalid("bad If-Match"))?.trim().trim_start_matches("W/").trim_matches('"');
    if s == "*" {
        return Ok(None);
    }
    s.parse().map(Some).map_err(|_| invalid("If-Match must be a task version"))
}

fn with_etag(version: i64, body: impl Serialize) -> Response {
    let mut res = Json(body).into_response();
    res.headers_mut().insert(header::ETAG, HeaderValue::from_str(&format!("\"{version}\"")).expect("ascii"));
    res
}

fn created(body: impl Serialize) -> Response {
    (StatusCode::CREATED, Json(body)).into_response()
}

#[utoipa::path(
    get, path = "/v1/health", tag = "meta", security(()),
    responses((status = 200, description = "The server is up", body = Health))
)]
async fn health(State(s): S) -> Json<Value> {
    let mut v = json!(s.info);
    v["ok"] = json!(true);
    Json(v)
}

#[utoipa::path(
    get, path = "/v1/me", tag = "meta",
    responses((status = 200, description = "The caller", body = Me))
)]
async fn me(State(s): S, Auth(ctx): Auth) -> ApiResult<Json<Value>> {
    let user = s.store.get_user(&ctx, ctx.user_id).await?;
    let agent = match ctx.agent_id {
        Some(id) => s.store.list_agents(&ctx).await?.into_iter().find(|a| a.id == id),
        None => None,
    };
    Ok(Json(json!({ "workspace_id": ctx.workspace_id, "user": user, "agent": agent })))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct ProjectListQuery {
    #[serde(default)]
    archived: bool,
}

#[utoipa::path(
    get, path = "/v1/projects", tag = "projects", params(ProjectListQuery),
    responses((status = 200, description = "Projects", body = Vec<Project>))
)]
async fn list_projects(State(s): S, Auth(ctx): Auth, Query(q): Query<ProjectListQuery>) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.list_projects(&ctx, q.archived).await?)))
}

#[derive(Deserialize, ToSchema)]
struct CreateProject {
    key: String,
    name: String,
    #[serde(default)]
    brief: String,
    #[serde(default)]
    require_review: bool,
    claim_ttl: Option<i64>,
    metadata: Option<Value>,
}

#[utoipa::path(
    post, path = "/v1/projects", tag = "projects", request_body = CreateProject,
    responses(
        (status = 201, description = "Created", body = Project),
        (status = 409, description = "`already_exists`", body = ErrorBody),
    )
)]
async fn create_project(State(s): S, Auth(ctx): Auth, Json(b): Json<CreateProject>) -> ApiResult<Response> {
    let new = NewProject {
        key: b.key,
        name: b.name,
        brief: b.brief,
        require_review: b.require_review,
        claim_ttl: b.claim_ttl,
        metadata: b.metadata,
    };
    Ok(created(s.store.create_project(&ctx, new).await?))
}

#[utoipa::path(
    get, path = "/v1/projects/{project}", tag = "projects", params(("project" = String, Path, description = "Project key, e.g. `AUTH`, or id")),
    responses(
        (status = 200, description = "The project", body = Project, headers(("ETag" = String, description = "The version, quoted, for `If-Match`"))),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
async fn get_project(State(s): S, Auth(ctx): Auth, Path(p): Path<String>) -> ApiResult<Response> {
    let project = s.store.get_project(&ctx, &p).await?;
    Ok(with_etag(project.version, project))
}

#[derive(Deserialize, ToSchema)]
struct UpdateProject {
    name: Option<String>,
    brief: Option<String>,
    status: Option<ProjectStatus>,
    require_review: Option<bool>,
    #[serde(default, deserialize_with = "nullable")]
    claim_ttl: Option<Option<i64>>,
    metadata: Option<Value>,
}

#[utoipa::path(
    patch, path = "/v1/projects/{project}", tag = "projects", params(("project" = String, Path, description = "Project key, e.g. `AUTH`, or id"), ("If-Match" = Option<String>, Header, description = "Expected version, e.g. `\"3\"`. A stale version gets 412 `version_conflict`.")),
    request_body = UpdateProject,
    responses(
        (status = 200, description = "Updated", body = Project, headers(("ETag" = String, description = "The version, quoted, for `If-Match`"))),
        (status = 404, description = "`not_found`", body = ErrorBody),
        (status = 412, description = "`version_conflict`", body = ErrorBody),
    )
)]
async fn update_project(
    State(s): S,
    Auth(ctx): Auth,
    Path(p): Path<String>,
    headers: HeaderMap,
    Json(b): Json<UpdateProject>,
) -> ApiResult<Response> {
    let patch = ProjectPatch {
        name: b.name,
        brief: b.brief,
        status: b.status,
        require_review: b.require_review,
        claim_ttl: b.claim_ttl,
        metadata: b.metadata,
    };
    let project = s.store.update_project(&ctx, &p, patch, if_match(&headers)?).await?;
    Ok(with_etag(project.version, project))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct TaskListQuery {
    project: Option<String>,
    /// Comma-separated. Defaults to everything not done or cancelled.
    status: Option<String>,
    parent: Option<String>,
    #[param(format = DateTime)]
    due_before: Option<String>,
    /// `key:value` pairs, comma-separated.
    metadata: Option<String>,
    limit: Option<i64>,
    fields: Option<String>,
}

#[utoipa::path(
    get, path = "/v1/tasks", tag = "tasks", params(TaskListQuery),
    responses((status = 200, description = "Tasks; trimmed to the requested `fields` if set", body = Vec<Task>))
)]
async fn list_tasks(State(s): S, Auth(ctx): Auth, Query(q): Query<TaskListQuery>) -> ApiResult<Json<Value>> {
    let metadata = q
        .metadata
        .as_deref()
        .map(|m| {
            m.split(',')
                .map(|pair| {
                    pair.split_once(':')
                        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                        .ok_or_else(|| invalid("metadata filter must be key:value"))
                })
                .collect::<ApiResult<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    let filter = TaskFilter {
        project: q.project,
        statuses: parse_list(q.status.as_deref())?,
        parent: q.parent,
        due_before: parse_opt_time(q.due_before.as_deref())?,
        metadata,
        limit: q.limit,
    };
    let tasks = s.store.list_tasks(&ctx, filter).await?;
    Ok(Json(select_fields(&tasks, q.fields.as_deref())?))
}

#[derive(Deserialize, ToSchema)]
struct CreateTask {
    project: String,
    title: String,
    #[serde(default)]
    description: String,
    priority: Option<i64>,
    #[schema(format = DateTime)]
    due_at: Option<String>,
    due_tz: Option<String>,
    parent: Option<String>,
    metadata: Option<Value>,
    source: Option<String>,
    external_id: Option<String>,
    /// Tasks that must finish before this one.
    #[serde(default)]
    blocked_by: Vec<String>,
    /// Tasks this one blocks.
    #[serde(default)]
    blocks: Vec<String>,
}

#[utoipa::path(
    post, path = "/v1/tasks", tag = "tasks", request_body = CreateTask,
    responses(
        (status = 201, description = "Created", body = Task, headers(("ETag" = String, description = "The version, quoted, for `If-Match`"))),
        (status = 404, description = "`not_found`: project, parent or dependency", body = ErrorBody),
        (status = 409, description = "`already_exists` (same `source` and `external_id`) or `dependency_cycle`", body = ErrorBody),
    )
)]
async fn create_task(State(s): S, Auth(ctx): Auth, Json(b): Json<CreateTask>) -> ApiResult<Response> {
    let new = NewTask {
        project: b.project,
        title: b.title,
        description: b.description,
        priority: b.priority,
        due_at: parse_opt_time(b.due_at.as_deref())?,
        due_tz: b.due_tz,
        parent: b.parent,
        metadata: b.metadata,
        source: b.source,
        external_id: b.external_id,
    };
    let task = s.store.create_task(&ctx, new).await?;
    for blocker in &b.blocked_by {
        s.store.add_dependency(&ctx, blocker, &task.key).await?;
    }
    for blocked in &b.blocks {
        s.store.add_dependency(&ctx, &task.key, blocked).await?;
    }
    let mut res = created(&task);
    res.headers_mut().insert(header::ETAG, HeaderValue::from_str(&format!("\"{}\"", task.version)).expect("ascii"));
    Ok(res)
}

#[utoipa::path(
    get, path = "/v1/tasks/{task}", tag = "tasks", params(("task" = String, Path, description = "Task key, e.g. `AUTH-42`, or id")),
    responses(
        (status = 200, description = "The task with its claim, dependencies and comments", body = TaskDetail, headers(("ETag" = String, description = "The version, quoted, for `If-Match`"))),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
async fn get_task(State(s): S, Auth(ctx): Auth, Path(t): Path<String>) -> ApiResult<Response> {
    let detail = s.store.task_detail(&ctx, &t).await?;
    Ok(with_etag(detail.task.version, detail))
}

#[derive(Deserialize, ToSchema)]
struct UpdateTask {
    title: Option<String>,
    description: Option<String>,
    status: Option<Status>,
    priority: Option<i64>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(format = DateTime)]
    due_at: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    due_tz: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    parent: Option<Option<String>>,
    metadata: Option<Value>,
    /// Also accepted in the body for clients that can't set headers.
    version: Option<i64>,
}

#[utoipa::path(
    patch, path = "/v1/tasks/{task}", tag = "tasks", params(("task" = String, Path, description = "Task key, e.g. `AUTH-42`, or id"), ("If-Match" = Option<String>, Header, description = "Expected version, e.g. `\"3\"`. A stale version gets 412 `version_conflict`.")),
    request_body = UpdateTask,
    responses(
        (status = 200, description = "Updated", body = Task, headers(("ETag" = String, description = "The version, quoted, for `If-Match`"))),
        (status = 403, description = "`review_required` or `forbidden`", body = ErrorBody),
        (status = 404, description = "`not_found`", body = ErrorBody),
        (status = 409, description = "`invalid_transition`, `blocked_by`, `claim_held` or `not_claim_holder`", body = ErrorBody),
        (status = 412, description = "`version_conflict`", body = ErrorBody),
    )
)]
async fn update_task(
    State(s): S,
    Auth(ctx): Auth,
    Path(t): Path<String>,
    headers: HeaderMap,
    Json(b): Json<UpdateTask>,
) -> ApiResult<Response> {
    let patch = TaskPatch {
        title: b.title,
        description: b.description,
        status: b.status,
        priority: b.priority,
        due_at: b.due_at.map(|d| parse_opt_time(d.as_deref())).transpose()?,
        due_tz: b.due_tz,
        parent: b.parent,
        metadata: b.metadata,
    };
    let version = if_match(&headers)?.or(b.version);
    let task = s.store.update_task(&ctx, &t, patch, version).await?;
    Ok(with_etag(task.version, task))
}

#[utoipa::path(
    delete, path = "/v1/tasks/{task}", tag = "tasks", params(("task" = String, Path, description = "Task key, e.g. `AUTH-42`, or id"), ("If-Match" = Option<String>, Header, description = "Expected version, e.g. `\"3\"`. A stale version gets 412 `version_conflict`.")),
    responses(
        (status = 200, description = "Soft-deleted; undo restores it", body = Task),
        (status = 404, description = "`not_found`", body = ErrorBody),
        (status = 409, description = "`claim_held` or `not_claim_holder`", body = ErrorBody),
        (status = 412, description = "`version_conflict`", body = ErrorBody),
    )
)]
async fn delete_task(State(s): S, Auth(ctx): Auth, Path(t): Path<String>, headers: HeaderMap) -> ApiResult<Response> {
    let task = s.store.delete_task(&ctx, &t, if_match(&headers)?).await?;
    Ok(Json(task).into_response())
}

#[derive(Deserialize, Default, ToSchema)]
struct ClaimBody {
    ttl: Option<i64>,
}

#[utoipa::path(
    post, path = "/v1/tasks/{task}/claim", tag = "claims", params(("task" = String, Path, description = "Task key, e.g. `AUTH-42`, or id")),
    request_body(content = Option<ClaimBody>, description = "Optional; an empty body uses the default TTL"),
    responses(
        (status = 200, description = "Claimed, with the last handoff", body = ClaimResult, headers(("ETag" = String, description = "The version, quoted, for `If-Match`"))),
        (status = 404, description = "`not_found`", body = ErrorBody),
        (status = 409, description = "`claim_held`, `blocked_by` or `invalid_transition`", body = ErrorBody),
    )
)]
async fn claim(State(s): S, Auth(ctx): Auth, Path(t): Path<String>, b: axum::body::Bytes) -> ApiResult<Response> {
    let res = s.store.claim(&ctx, &t, optional_body::<ClaimBody>(&b)?.ttl).await?;
    Ok(with_etag(res.task.version, res))
}

#[utoipa::path(
    post, path = "/v1/tasks/{task}/renew", tag = "claims", params(("task" = String, Path, description = "Task key, e.g. `AUTH-42`, or id")),
    request_body(content = Option<ClaimBody>, description = "Optional; an empty body uses the default TTL"),
    responses(
        (status = 200, description = "The extended claim", body = Claim),
        (status = 404, description = "`not_found`", body = ErrorBody),
        (status = 409, description = "`not_claim_holder`", body = ErrorBody),
    )
)]
async fn renew(State(s): S, Auth(ctx): Auth, Path(t): Path<String>, b: axum::body::Bytes) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.renew(&ctx, &t, optional_body::<ClaimBody>(&b)?.ttl).await?)))
}

#[utoipa::path(
    post, path = "/v1/tasks/{task}/release", tag = "claims", params(("task" = String, Path, description = "Task key, e.g. `AUTH-42`, or id")),
    responses(
        (status = 200, description = "Released; the task is open again", body = Task, headers(("ETag" = String, description = "The version, quoted, for `If-Match`"))),
        (status = 404, description = "`not_found`", body = ErrorBody),
        (status = 409, description = "`not_claim_holder`", body = ErrorBody),
    )
)]
async fn release(State(s): S, Auth(ctx): Auth, Path(t): Path<String>) -> ApiResult<Response> {
    let task = s.store.release(&ctx, &t).await?;
    Ok(with_etag(task.version, task))
}

#[utoipa::path(
    get, path = "/v1/tasks/{task}/comments", tag = "comments", params(("task" = String, Path, description = "Task key, e.g. `AUTH-42`, or id")),
    responses(
        (status = 200, description = "Comments, oldest first", body = Vec<Comment>),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
async fn list_comments(State(s): S, Auth(ctx): Auth, Path(t): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.list_comments(&ctx, &t).await?)))
}

#[derive(Deserialize, ToSchema)]
struct AddComment {
    #[serde(default = "note")]
    kind: CommentKind,
    body: String,
    addressed_to: Option<Uuid>,
}

fn note() -> CommentKind {
    CommentKind::Note
}

#[utoipa::path(
    post, path = "/v1/tasks/{task}/comments", tag = "comments", params(("task" = String, Path, description = "Task key, e.g. `AUTH-42`, or id")), request_body = AddComment,
    responses(
        (status = 201, description = "Created", body = Comment),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
async fn add_comment(
    State(s): S,
    Auth(ctx): Auth,
    Path(t): Path<String>,
    Json(b): Json<AddComment>,
) -> ApiResult<Response> {
    let c =
        s.store.add_comment(&ctx, &t, NewComment { kind: b.kind, body: b.body, addressed_to: b.addressed_to }).await?;
    Ok(created(c))
}

#[derive(Deserialize, Default, ToSchema)]
struct ResolveBody {
    answer: Option<String>,
}

#[utoipa::path(
    post, path = "/v1/comments/{id}/resolve", tag = "comments",
    params(("id" = Uuid, Path, description = "Comment id")),
    request_body(content = Option<ResolveBody>, description = "Optional"),
    responses(
        (status = 200, description = "The resolved comment", body = Comment),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
async fn resolve_comment(
    State(s): S,
    Auth(ctx): Auth,
    Path(id): Path<Uuid>,
    b: axum::body::Bytes,
) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.resolve_comment(&ctx, id, optional_body::<ResolveBody>(&b)?.answer).await?)))
}

#[utoipa::path(
    get, path = "/v1/tasks/{task}/blockers", tag = "dependencies", params(("task" = String, Path, description = "Task key, e.g. `AUTH-42`, or id")),
    responses(
        (status = 200, description = "Tasks blocking this one", body = Vec<Task>),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
async fn list_blockers(State(s): S, Auth(ctx): Auth, Path(t): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.blockers(&ctx, &t).await?)))
}

#[derive(Deserialize, ToSchema)]
struct AddBlocker {
    blocker: String,
}

#[utoipa::path(
    post, path = "/v1/tasks/{task}/blockers", tag = "dependencies", params(("task" = String, Path, description = "Task key, e.g. `AUTH-42`, or id")), request_body = AddBlocker,
    responses(
        (status = 201, description = "Created", body = Dependency),
        (status = 404, description = "`not_found`", body = ErrorBody),
        (status = 409, description = "`dependency_cycle` or `already_exists`", body = ErrorBody),
    )
)]
async fn add_blocker(
    State(s): S,
    Auth(ctx): Auth,
    Path(t): Path<String>,
    Json(b): Json<AddBlocker>,
) -> ApiResult<Response> {
    Ok(created(s.store.add_dependency(&ctx, &b.blocker, &t).await?))
}

#[utoipa::path(
    delete, path = "/v1/tasks/{task}/blockers/{blocker}", tag = "dependencies",
    params(("task" = String, Path, description = "Task key, e.g. `AUTH-42`, or id"), ("blocker" = String, Path, description = "Blocking task key or id")),
    responses(
        (status = 204, description = "Removed"),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
async fn remove_blocker(
    State(s): S,
    Auth(ctx): Auth,
    Path((t, blocker)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    s.store.remove_dependency(&ctx, &blocker, &t).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct HistoryQuery {
    after: Option<i64>,
    limit: Option<i64>,
    /// Include events older than 90 days, which have moved to the archive.
    #[serde(default)]
    archived: bool,
}

#[utoipa::path(
    get, path = "/v1/tasks/{task}/history", tag = "events", params(("task" = String, Path, description = "Task key, e.g. `AUTH-42`, or id"), HistoryQuery),
    responses(
        (status = 200, description = "The task's events", body = Vec<Event>),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
async fn task_history(
    State(s): S,
    Auth(ctx): Auth,
    Path(t): Path<String>,
    Query(p): Query<HistoryQuery>,
) -> ApiResult<Json<Value>> {
    let filter = EventFilter {
        task: Some(t),
        after_id: p.after,
        limit: p.limit,
        include_archived: p.archived,
        ..Default::default()
    };
    Ok(Json(json!(s.store.list_events(&ctx, filter).await?)))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct ReadyQuery {
    project: Option<String>,
    limit: Option<i64>,
    fields: Option<String>,
}

#[utoipa::path(
    get, path = "/v1/ready", tag = "tasks", params(ReadyQuery),
    responses((status = 200, description = "Open, unclaimed, unblocked tasks by priority then due date; trimmed to `fields` if set", body = Vec<Task>))
)]
async fn ready(State(s): S, Auth(ctx): Auth, Query(q): Query<ReadyQuery>) -> ApiResult<Json<Value>> {
    let tasks = s.store.ready(&ctx, q.project.as_deref(), q.limit).await?;
    Ok(Json(select_fields(&tasks, q.fields.as_deref())?))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct SearchQuery {
    q: String,
    limit: Option<i64>,
    fields: Option<String>,
}

#[utoipa::path(
    get, path = "/v1/search", tag = "tasks", params(SearchQuery),
    responses((status = 200, description = "Matching tasks; trimmed to `fields` if set", body = Vec<Task>))
)]
async fn search(State(s): S, Auth(ctx): Auth, Query(q): Query<SearchQuery>) -> ApiResult<Json<Value>> {
    let tasks = s.store.search(&ctx, &q.q, q.limit).await?;
    Ok(Json(select_fields(&tasks, q.fields.as_deref())?))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct EventQuery {
    after: Option<i64>,
    task: Option<String>,
    actor: Option<Uuid>,
    batch: Option<Uuid>,
    #[param(format = DateTime)]
    since: Option<String>,
    limit: Option<i64>,
    #[serde(default)]
    newest_first: bool,
}

#[utoipa::path(
    get, path = "/v1/events", tag = "events", params(EventQuery),
    responses((status = 200, description = "Events", body = Vec<Event>))
)]
async fn list_events(State(s): S, Auth(ctx): Auth, Query(q): Query<EventQuery>) -> ApiResult<Json<Value>> {
    let filter = EventFilter {
        after_id: q.after,
        task: q.task,
        actor_id: q.actor,
        batch_id: q.batch,
        since: parse_opt_time(q.since.as_deref())?,
        limit: q.limit,
        newest_first: q.newest_first,
        include_archived: false,
    };
    Ok(Json(json!(s.store.list_events(&ctx, filter).await?)))
}

#[derive(Deserialize, ToSchema)]
struct UndoBody {
    event_id: Option<i64>,
    batch_id: Option<Uuid>,
}

#[utoipa::path(
    post, path = "/v1/undo", tag = "undo", request_body = UndoBody,
    responses(
        (status = 200, description = "What was reverted", body = UndoReport),
        (status = 404, description = "`not_found`", body = ErrorBody),
        (status = 409, description = "`undo_conflict`, or `undo_window_expired` for events older than 90 days", body = ErrorBody),
    )
)]
async fn undo(State(s): S, Auth(ctx): Auth, Json(b): Json<UndoBody>) -> ApiResult<Json<Value>> {
    let report = match (b.event_id, b.batch_id) {
        (Some(e), None) => s.store.undo_event(&ctx, e).await?,
        (None, Some(batch)) => s.store.undo_batch(&ctx, batch).await?,
        _ => return Err(invalid("send exactly one of event_id or batch_id")),
    };
    Ok(Json(json!(report)))
}

#[derive(Deserialize, ToSchema)]
struct RevertBody {
    actor_id: Uuid,
    #[schema(format = DateTime)]
    since: String,
}

#[utoipa::path(
    post, path = "/v1/revert", tag = "undo", request_body = RevertBody,
    responses(
        (status = 200, description = "What was reverted and what was skipped because someone else changed it later. A `since` older than 90 days is clamped to the undo window", body = UndoReport),
        (status = 403, description = "`forbidden`: humans only", body = ErrorBody),
    )
)]
async fn revert(State(s): S, Auth(ctx): Auth, Json(b): Json<RevertBody>) -> ApiResult<Json<Value>> {
    require_human(&ctx)?;
    Ok(Json(json!(s.store.revert_actor(&ctx, b.actor_id, parse_time(&b.since)?).await?)))
}

#[utoipa::path(
    get, path = "/v1/agents", tag = "agents",
    responses((status = 200, description = "Agents", body = Vec<Agent>))
)]
async fn list_agents(State(s): S, Auth(ctx): Auth) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.list_agents(&ctx).await?)))
}

/// A token's optional project scope and rate limit.
#[derive(Deserialize, Default, ToSchema)]
struct TokenLimits {
    /// Project keys or ids. Omitted or empty means every project.
    projects: Option<Vec<String>>,
    rate_limit_per_minute: Option<i64>,
}

impl From<TokenLimits> for TokenOptions {
    fn from(l: TokenLimits) -> Self {
        TokenOptions { projects: l.projects, rate_limit_per_minute: l.rate_limit_per_minute }
    }
}

#[derive(Deserialize, ToSchema)]
struct CreateAgent {
    name: String,
    #[serde(flatten)]
    limits: TokenLimits,
}

/// Creates an agent and its first token. The secret is only shown here.
#[utoipa::path(
    post, path = "/v1/agents", tag = "agents", request_body = CreateAgent,
    responses(
        (status = 201, description = "The agent, its token and the token's secret", body = CreatedAgent),
        (status = 403, description = "`forbidden`: humans only", body = ErrorBody),
        (status = 409, description = "`already_exists`", body = ErrorBody),
    )
)]
async fn create_agent(State(s): S, Auth(ctx): Auth, Json(b): Json<CreateAgent>) -> ApiResult<Response> {
    require_human(&ctx)?;
    let agent = s.store.create_agent(&ctx, &b.name).await?;
    let (token, secret) =
        s.store.create_token_with(&ctx, Principal::Agent(agent.id), &agent.name, b.limits.into()).await?;
    Ok(created(json!({ "agent": agent, "token": token, "secret": secret })))
}

#[derive(Deserialize, ToSchema)]
struct CreateToken {
    name: String,
    /// Agent name or id. Omitted means a token for the caller.
    agent: Option<String>,
    #[serde(flatten)]
    limits: TokenLimits,
}

/// Issues a token for the caller or one of their agents. The secret is only shown here.
#[utoipa::path(
    post, path = "/v1/tokens", tag = "tokens", request_body = CreateToken,
    responses(
        (status = 201, description = "The token and its secret, shown once", body = CreatedToken),
        (status = 403, description = "forbidden: agents and scoped tokens can't issue tokens", body = ErrorBody),
        (status = 404, description = "not_found: no such agent", body = ErrorBody),
    )
)]
async fn create_token(State(s): S, Auth(ctx): Auth, Json(b): Json<CreateToken>) -> ApiResult<Response> {
    require_human(&ctx)?;
    let principal = match &b.agent {
        None => Principal::User(ctx.user_id),
        Some(a) => {
            let agents = s.store.list_agents(&ctx).await?;
            let agent = agents
                .iter()
                .find(|x| x.owner_user_id == ctx.user_id && (x.name == *a || x.id.to_string() == *a))
                .ok_or_else(|| ApiError(Error::NotFound(format!("agent {a}"))))?;
            Principal::Agent(agent.id)
        }
    };
    let (token, secret) = s.store.create_token_with(&ctx, principal, &b.name, b.limits.into()).await?;
    Ok(created(json!({ "token": token, "secret": secret })))
}

#[utoipa::path(
    get, path = "/v1/tokens", tag = "tokens",
    responses((status = 200, description = "Tokens, without secrets", body = Vec<Token>))
)]
async fn list_tokens(State(s): S, Auth(ctx): Auth) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.list_tokens(&ctx).await?)))
}

#[utoipa::path(
    delete, path = "/v1/tokens/{id}", tag = "tokens", params(("id" = Uuid, Path, description = "Token id")),
    responses(
        (status = 200, description = "The revoked token", body = Token),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
async fn revoke_token(State(s): S, Auth(ctx): Auth, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.revoke_token(&ctx, id).await?)))
}
