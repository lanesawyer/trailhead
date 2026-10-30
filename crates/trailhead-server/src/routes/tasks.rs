//! Tasks: listing, reading, writing, the ready queue and search.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use trailhead_core::{GradeFilter, NewTask, SearchFilter, SearchHit, Status, Task, TaskDetail, TaskFilter, TaskPatch};
use utoipa::{IntoParams, ToSchema};

use crate::auth::Auth;
use crate::error::{ApiResult, Json, Path, Query, invalid};
use crate::openapi::{ErrorBody, IfMatch, TaskParam};
use crate::params::{parse_list, parse_opt_at, parse_opt_date, parse_opt_time, split_list};

use super::{S, if_match, nullable, select_fields, with_etag};

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct TaskListQuery {
    project: Option<String>,
    /// Comma-separated. Defaults to everything not done or cancelled.
    status: Option<String>,
    parent: Option<String>,
    #[param(format = DateTime)]
    due_before: Option<String>,
    /// Tasks scheduled on or before this day (`YYYY-MM-DD`): pass today for today's plan,
    /// including anything planned earlier that's still unfinished.
    #[param(format = Date)]
    scheduled_through: Option<String>,
    /// `1` to `4`, `graded` or `ungraded`.
    grade: Option<String>,
    /// `key:value` pairs, comma-separated.
    metadata: Option<String>,
    /// `priority` (the default: priority, then due date, then oldest), `updated_at` or
    /// `created_at`, with `-` for newest first, e.g. `-updated_at`; or `scheduled`: by
    /// scheduled day, timed tasks in time order before untimed ones, unscheduled last.
    sort: Option<String>,
    /// Defaults to 100.
    limit: Option<i64>,
    /// Skip this many tasks, to page with `limit`.
    offset: Option<i64>,
    fields: Option<String>,
    /// Comma-separated extras per task: `claim` (the live claim or `null`, with the holder's
    /// name as `holder`), `open_questions` (unresolved `question` comments, oldest first) and
    /// `last_handoff` (the newest `handoff` comment or `null`).
    include: Option<String>,
}

#[utoipa::path(
    get, path = "/v1/tasks", tag = "tasks", params(TaskListQuery),
    responses((status = 200, description = "Tasks; trimmed to the requested `fields` if set", body = Vec<crate::openapi::ListedTask>))
)]
pub(crate) async fn list_tasks(State(s): S, Auth(ctx): Auth, Query(q): Query<TaskListQuery>) -> ApiResult<Json<Value>> {
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
        scheduled_through: parse_opt_date(q.scheduled_through.as_deref())?,
        grade: parse_grade_filter(q.grade.as_deref())?,
        metadata,
        sort: q.sort.as_deref().map(str::parse).transpose()?.unwrap_or_default(),
        limit: q.limit,
        offset: q.offset,
    };
    let includes = split_list(q.include.as_deref());
    if let Some(other) = includes.iter().find(|i| !["claim", "open_questions", "last_handoff"].contains(&i.as_str())) {
        return Err(invalid(format!("unknown include: {other}")));
    }
    let wants = |name: &str| includes.iter().any(|i| i == name);
    let tasks = s.store.list_tasks(&ctx, filter).await?;
    let mut out = select_fields(&tasks, q.fields.as_deref())?;
    let ids: Vec<_> = tasks.iter().map(|t| t.id).collect();
    let mut claims = if wants("claim") { Some(s.store.live_claims(&ctx, &ids).await?) } else { None };
    let mut questions = if wants("open_questions") { Some(s.store.open_questions(&ctx, &ids).await?) } else { None };
    let mut handoffs = if wants("last_handoff") { Some(s.store.last_handoffs(&ctx, &ids).await?) } else { None };
    if let Value::Array(items) = &mut out {
        for (item, task) in items.iter_mut().zip(&tasks) {
            if let Some(c) = &mut claims {
                item["claim"] = json!(c.remove(&task.id));
            }
            if let Some(q) = &mut questions {
                item["open_questions"] = json!(q.remove(&task.id).unwrap_or_default());
            }
            if let Some(h) = &mut handoffs {
                item["last_handoff"] = json!(h.remove(&task.id));
            }
        }
    }
    Ok(Json(out))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct CreateTask {
    project: String,
    title: String,
    #[serde(default)]
    description: String,
    priority: Option<i64>,
    /// The day it's due. Optional when `due_at` is set.
    #[schema(format = Date)]
    due_on: Option<String>,
    /// A time it's due by. Without one, it's due by the end of `due_on` in `due_tz`.
    #[schema(format = DateTime)]
    due_at: Option<String>,
    /// IANA zone the deadline was set in; decides its day and when that day ends. Without one, UTC.
    due_tz: Option<String>,
    /// The day to work the task (`YYYY-MM-DD`), separate from its due date.
    /// Optional when `scheduled_at` is set.
    #[schema(format = Date)]
    scheduled_on: Option<String>,
    /// A time on the scheduled day, e.g. 1pm.
    #[schema(format = DateTime)]
    scheduled_at: Option<String>,
    /// IANA zone `scheduled_at` was planned in; decides its day. Without one, the UTC day.
    scheduled_tz: Option<String>,
    parent: Option<String>,
    /// 1 to 4. Only a human can create a task with a grade.
    grade: Option<i64>,
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
        (status = 201, description = "Created. `possible_duplicates` lists keys of open tasks with a close title, when there are any; it's a warning, not an error.", body = Task, headers(("ETag" = String, description = "The version, quoted, for `If-Match`"))),
        (status = 404, description = "`not_found`: project, parent or dependency", body = ErrorBody),
        (status = 409, description = "`dependency_cycle` from `blocked_by`/`blocks`. Repeating a `source` + `external_id` returns the existing task instead of an error.", body = ErrorBody),
    )
)]
pub(crate) async fn create_task(State(s): S, Auth(ctx): Auth, Json(b): Json<CreateTask>) -> ApiResult<Response> {
    let new = NewTask {
        project: b.project,
        title: b.title,
        description: b.description,
        priority: b.priority,
        grade: b.grade,
        due_on: parse_opt_date(b.due_on.as_deref())?,
        due_at: parse_opt_at(b.due_at.as_deref())?,
        due_tz: b.due_tz,
        scheduled_on: parse_opt_date(b.scheduled_on.as_deref())?,
        scheduled_at: parse_opt_at(b.scheduled_at.as_deref())?,
        scheduled_tz: b.scheduled_tz,
        parent: b.parent,
        metadata: b.metadata,
        source: b.source,
        external_id: b.external_id,
    };
    let project = new.project.clone();
    let task = s.store.create_task(&ctx, new).await?;
    let similar = s.store.similar_tasks(&ctx, &project, &task.title, Some(task.id)).await.unwrap_or_default();
    for blocker in &b.blocked_by {
        s.store.add_dependency(&ctx, blocker, &task.key).await?;
    }
    for blocked in &b.blocks {
        s.store.add_dependency(&ctx, &task.key, blocked).await?;
    }
    let version = task.version;
    let mut body = serde_json::to_value(task).expect("tasks serialize");
    if !similar.is_empty() {
        body["possible_duplicates"] = json!(similar.iter().map(|t| &t.key).collect::<Vec<_>>());
    }
    let mut res = with_etag(version, body);
    *res.status_mut() = StatusCode::CREATED;
    Ok(res)
}

#[utoipa::path(
    get, path = "/v1/tasks/{task}", tag = "tasks", params(TaskParam),
    responses(
        (status = 200, description = "The task with its claim, dependencies and comments", body = TaskDetail, headers(("ETag" = String, description = "The version, quoted, for `If-Match`"))),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
pub(crate) async fn get_task(State(s): S, Auth(ctx): Auth, Path(t): Path<String>) -> ApiResult<Response> {
    let detail = s.store.task_detail(&ctx, &t).await?;
    Ok(with_etag(detail.task.version, detail))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct UpdateTask {
    title: Option<String>,
    description: Option<String>,
    status: Option<Status>,
    priority: Option<i64>,
    /// 1 to 4. A human can set, raise, lower or `null` it; an agent can only raise an existing grade.
    #[serde(default, deserialize_with = "nullable")]
    grade: Option<Option<i64>>,
    /// Key of the task this one duplicates. Needs `status` `cancelled` (in this request or already);
    /// `null` clears it, and so does leaving `cancelled`.
    #[serde(default, deserialize_with = "nullable")]
    duplicate_of: Option<Option<String>>,
    /// A new day without `due_at` drops the time; `null` removes the deadline.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(format = Date)]
    due_on: Option<Option<String>>,
    /// Moves the day to match; `null` keeps the day without a time.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(format = DateTime)]
    due_at: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    due_tz: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    /// A new day without `scheduled_at` drops the time; `null` unschedules.
    #[schema(format = Date)]
    scheduled_on: Option<Option<String>>,
    /// Moves the day to match; `null` keeps the day without a time.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(format = DateTime)]
    scheduled_at: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    scheduled_tz: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    parent: Option<Option<String>>,
    metadata: Option<Value>,
    /// Also accepted in the body for clients that can't set headers.
    version: Option<i64>,
}

#[utoipa::path(
    patch, path = "/v1/tasks/{task}", tag = "tasks", params(TaskParam, IfMatch),
    request_body = UpdateTask,
    responses(
        (status = 200, description = "Updated", body = Task, headers(("ETag" = String, description = "The version, quoted, for `If-Match`"))),
        (status = 403, description = "`review_required` or `forbidden`", body = ErrorBody),
        (status = 404, description = "`not_found`", body = ErrorBody),
        (status = 409, description = "`invalid_transition`, `claim_held` or `not_claim_holder`", body = ErrorBody),
        (status = 412, description = "`version_conflict`", body = ErrorBody),
    )
)]
pub(crate) async fn update_task(
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
        grade: b.grade,
        duplicate_of: b.duplicate_of,
        due_on: b.due_on.map(|d| parse_opt_date(d.as_deref())).transpose()?,
        due_at: b.due_at.map(|d| parse_opt_at(d.as_deref())).transpose()?,
        due_tz: b.due_tz,
        scheduled_on: b.scheduled_on.map(|d| parse_opt_date(d.as_deref())).transpose()?,
        scheduled_at: b.scheduled_at.map(|t| parse_opt_at(t.as_deref())).transpose()?,
        scheduled_tz: b.scheduled_tz,
        parent: b.parent,
        metadata: b.metadata,
    };
    let version = if_match(&headers)?.or(b.version);
    let task = s.store.update_task(&ctx, &t, patch, version).await?;
    Ok(with_etag(task.version, task))
}

#[utoipa::path(
    delete, path = "/v1/tasks/{task}", tag = "tasks", params(TaskParam, IfMatch),
    responses(
        (status = 200, description = "Soft-deleted; undo restores it", body = Task),
        (status = 404, description = "`not_found`", body = ErrorBody),
        (status = 409, description = "`claim_held` or `not_claim_holder`", body = ErrorBody),
        (status = 412, description = "`version_conflict`", body = ErrorBody),
    )
)]
pub(crate) async fn delete_task(
    State(s): S,
    Auth(ctx): Auth,
    Path(t): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let task = s.store.delete_task(&ctx, &t, if_match(&headers)?).await?;
    Ok(Json(task).into_response())
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct ReadyQuery {
    project: Option<String>,
    limit: Option<i64>,
    /// `1` to `4`, `graded` or `ungraded`.
    grade: Option<String>,
    fields: Option<String>,
}

#[utoipa::path(
    get, path = "/v1/ready", tag = "tasks", params(ReadyQuery),
    responses((status = 200, description = "Open, unclaimed, unblocked tasks by priority then due date; trimmed to `fields` if set", body = Vec<Task>))
)]
pub(crate) async fn ready(State(s): S, Auth(ctx): Auth, Query(q): Query<ReadyQuery>) -> ApiResult<Json<Value>> {
    let tasks = s.store.ready(&ctx, q.project.as_deref(), q.limit, parse_grade_filter(q.grade.as_deref())?).await?;
    Ok(Json(select_fields(&tasks, q.fields.as_deref())?))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct SearchQuery {
    q: String,
    project: Option<String>,
    /// Comma-separated. Defaults to every status, open work first.
    status: Option<String>,
    limit: Option<i64>,
    fields: Option<String>,
}

#[utoipa::path(
    get, path = "/v1/search", tag = "tasks", params(SearchQuery),
    responses((status = 200, description = "Matching tasks, each with `match` saying where it matched; tasks are trimmed to `fields` if set", body = Vec<SearchHit>))
)]
pub(crate) async fn search(State(s): S, Auth(ctx): Auth, Query(q): Query<SearchQuery>) -> ApiResult<Json<Value>> {
    let filter =
        SearchFilter { query: q.q, project: q.project, statuses: parse_list(q.status.as_deref())?, limit: q.limit };
    let hits = s.store.search_hits(&ctx, filter).await?;
    let tasks: Vec<_> = hits.iter().map(|h| &h.task).collect();
    let mut out = select_fields(&tasks, q.fields.as_deref())?;
    if let Value::Array(items) = &mut out {
        for (item, hit) in items.iter_mut().zip(&hits) {
            item["match"] = json!(hit.matched);
        }
    }
    Ok(Json(out))
}

fn parse_grade_filter(s: Option<&str>) -> ApiResult<GradeFilter> {
    Ok(s.map(GradeFilter::parse).transpose()?.unwrap_or_default())
}
