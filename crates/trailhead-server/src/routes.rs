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
    CommentKind, EventFilter, NewComment, NewProject, NewTask, Principal, ProjectPatch, ProjectStatus, Status,
    TaskFilter, TaskPatch,
};
use uuid::Uuid;

use crate::AppState;
use crate::auth::{Auth, require_human};
use crate::error::{ApiResult, Json, Path, Query, invalid};

type S = State<Arc<AppState>>;

pub fn v1() -> Router<Arc<AppState>> {
    Router::new()
        .route("/health", get(health))
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
        .route("/tokens", get(list_tokens))
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

async fn health(State(s): S) -> Json<Value> {
    let mut v = json!(s.info);
    v["ok"] = json!(true);
    Json(v)
}

async fn me(State(s): S, Auth(ctx): Auth) -> ApiResult<Json<Value>> {
    let user = s.store.get_user(&ctx, ctx.user_id).await?;
    let agent = match ctx.agent_id {
        Some(id) => s.store.list_agents(&ctx).await?.into_iter().find(|a| a.id == id),
        None => None,
    };
    Ok(Json(json!({ "workspace_id": ctx.workspace_id, "user": user, "agent": agent })))
}

#[derive(Deserialize)]
struct ProjectListQuery {
    #[serde(default)]
    archived: bool,
}

async fn list_projects(State(s): S, Auth(ctx): Auth, Query(q): Query<ProjectListQuery>) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.list_projects(&ctx, q.archived).await?)))
}

#[derive(Deserialize)]
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

async fn get_project(State(s): S, Auth(ctx): Auth, Path(p): Path<String>) -> ApiResult<Response> {
    let project = s.store.get_project(&ctx, &p).await?;
    Ok(with_etag(project.version, project))
}

#[derive(Deserialize)]
struct UpdateProject {
    name: Option<String>,
    brief: Option<String>,
    status: Option<ProjectStatus>,
    require_review: Option<bool>,
    #[serde(default, deserialize_with = "nullable")]
    claim_ttl: Option<Option<i64>>,
    metadata: Option<Value>,
}

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

#[derive(Deserialize)]
struct TaskListQuery {
    project: Option<String>,
    /// Comma-separated. Defaults to everything not done or cancelled.
    status: Option<String>,
    parent: Option<String>,
    due_before: Option<String>,
    /// `key:value` pairs, comma-separated.
    metadata: Option<String>,
    limit: Option<i64>,
    fields: Option<String>,
}

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

#[derive(Deserialize)]
struct CreateTask {
    project: String,
    title: String,
    #[serde(default)]
    description: String,
    priority: Option<i64>,
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

async fn get_task(State(s): S, Auth(ctx): Auth, Path(t): Path<String>) -> ApiResult<Response> {
    let detail = s.store.task_detail(&ctx, &t).await?;
    Ok(with_etag(detail.task.version, detail))
}

#[derive(Deserialize)]
struct UpdateTask {
    title: Option<String>,
    description: Option<String>,
    status: Option<Status>,
    priority: Option<i64>,
    #[serde(default, deserialize_with = "nullable")]
    due_at: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    due_tz: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    parent: Option<Option<String>>,
    metadata: Option<Value>,
    /// Also accepted in the body for clients that can't set headers.
    version: Option<i64>,
}

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

async fn delete_task(State(s): S, Auth(ctx): Auth, Path(t): Path<String>, headers: HeaderMap) -> ApiResult<Response> {
    let task = s.store.delete_task(&ctx, &t, if_match(&headers)?).await?;
    Ok(Json(task).into_response())
}

#[derive(Deserialize, Default)]
struct ClaimBody {
    ttl: Option<i64>,
}

async fn claim(State(s): S, Auth(ctx): Auth, Path(t): Path<String>, b: axum::body::Bytes) -> ApiResult<Response> {
    let res = s.store.claim(&ctx, &t, optional_body::<ClaimBody>(&b)?.ttl).await?;
    Ok(with_etag(res.task.version, res))
}

async fn renew(State(s): S, Auth(ctx): Auth, Path(t): Path<String>, b: axum::body::Bytes) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.renew(&ctx, &t, optional_body::<ClaimBody>(&b)?.ttl).await?)))
}

async fn release(State(s): S, Auth(ctx): Auth, Path(t): Path<String>) -> ApiResult<Response> {
    let task = s.store.release(&ctx, &t).await?;
    Ok(with_etag(task.version, task))
}

async fn list_comments(State(s): S, Auth(ctx): Auth, Path(t): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.list_comments(&ctx, &t).await?)))
}

#[derive(Deserialize)]
struct AddComment {
    #[serde(default = "note")]
    kind: CommentKind,
    body: String,
    addressed_to: Option<Uuid>,
}

fn note() -> CommentKind {
    CommentKind::Note
}

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

#[derive(Deserialize, Default)]
struct ResolveBody {
    answer: Option<String>,
}

async fn resolve_comment(
    State(s): S,
    Auth(ctx): Auth,
    Path(id): Path<Uuid>,
    b: axum::body::Bytes,
) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.resolve_comment(&ctx, id, optional_body::<ResolveBody>(&b)?.answer).await?)))
}

async fn list_blockers(State(s): S, Auth(ctx): Auth, Path(t): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.blockers(&ctx, &t).await?)))
}

#[derive(Deserialize)]
struct AddBlocker {
    blocker: String,
}

async fn add_blocker(
    State(s): S,
    Auth(ctx): Auth,
    Path(t): Path<String>,
    Json(b): Json<AddBlocker>,
) -> ApiResult<Response> {
    Ok(created(s.store.add_dependency(&ctx, &b.blocker, &t).await?))
}

async fn remove_blocker(
    State(s): S,
    Auth(ctx): Auth,
    Path((t, blocker)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    s.store.remove_dependency(&ctx, &blocker, &t).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct Page {
    after: Option<i64>,
    limit: Option<i64>,
}

async fn task_history(
    State(s): S,
    Auth(ctx): Auth,
    Path(t): Path<String>,
    Query(p): Query<Page>,
) -> ApiResult<Json<Value>> {
    let filter = EventFilter { task: Some(t), after_id: p.after, limit: p.limit, ..Default::default() };
    Ok(Json(json!(s.store.list_events(&ctx, filter).await?)))
}

#[derive(Deserialize)]
struct ReadyQuery {
    project: Option<String>,
    limit: Option<i64>,
    fields: Option<String>,
}

async fn ready(State(s): S, Auth(ctx): Auth, Query(q): Query<ReadyQuery>) -> ApiResult<Json<Value>> {
    let tasks = s.store.ready(&ctx, q.project.as_deref(), q.limit).await?;
    Ok(Json(select_fields(&tasks, q.fields.as_deref())?))
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
    limit: Option<i64>,
    fields: Option<String>,
}

async fn search(State(s): S, Auth(ctx): Auth, Query(q): Query<SearchQuery>) -> ApiResult<Json<Value>> {
    let tasks = s.store.search(&ctx, &q.q, q.limit).await?;
    Ok(Json(select_fields(&tasks, q.fields.as_deref())?))
}

#[derive(Deserialize)]
struct EventQuery {
    after: Option<i64>,
    task: Option<String>,
    actor: Option<Uuid>,
    batch: Option<Uuid>,
    since: Option<String>,
    limit: Option<i64>,
    #[serde(default)]
    newest_first: bool,
}

async fn list_events(State(s): S, Auth(ctx): Auth, Query(q): Query<EventQuery>) -> ApiResult<Json<Value>> {
    let filter = EventFilter {
        after_id: q.after,
        task: q.task,
        actor_id: q.actor,
        batch_id: q.batch,
        since: parse_opt_time(q.since.as_deref())?,
        limit: q.limit,
        newest_first: q.newest_first,
    };
    Ok(Json(json!(s.store.list_events(&ctx, filter).await?)))
}

#[derive(Deserialize)]
struct UndoBody {
    event_id: Option<i64>,
    batch_id: Option<Uuid>,
}

async fn undo(State(s): S, Auth(ctx): Auth, Json(b): Json<UndoBody>) -> ApiResult<Json<Value>> {
    let report = match (b.event_id, b.batch_id) {
        (Some(e), None) => s.store.undo_event(&ctx, e).await?,
        (None, Some(batch)) => s.store.undo_batch(&ctx, batch).await?,
        _ => return Err(invalid("send exactly one of event_id or batch_id")),
    };
    Ok(Json(json!(report)))
}

#[derive(Deserialize)]
struct RevertBody {
    actor_id: Uuid,
    since: String,
}

async fn revert(State(s): S, Auth(ctx): Auth, Json(b): Json<RevertBody>) -> ApiResult<Json<Value>> {
    require_human(&ctx)?;
    Ok(Json(json!(s.store.revert_actor(&ctx, b.actor_id, parse_time(&b.since)?).await?)))
}

async fn list_agents(State(s): S, Auth(ctx): Auth) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.list_agents(&ctx).await?)))
}

#[derive(Deserialize)]
struct CreateAgent {
    name: String,
}

/// Creates an agent and its first token. The secret is only shown here.
async fn create_agent(State(s): S, Auth(ctx): Auth, Json(b): Json<CreateAgent>) -> ApiResult<Response> {
    require_human(&ctx)?;
    let agent = s.store.create_agent(&ctx, &b.name).await?;
    let (token, secret) = s.store.create_token(&ctx, Principal::Agent(agent.id), &agent.name).await?;
    Ok(created(json!({ "agent": agent, "token": token, "secret": secret })))
}

async fn list_tokens(State(s): S, Auth(ctx): Auth) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.list_tokens(&ctx).await?)))
}

async fn revoke_token(State(s): S, Auth(ctx): Auth, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.revoke_token(&ctx, id).await?)))
}
