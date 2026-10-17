//! The event log, undo and revert.

use axum::extract::State;
use serde::Deserialize;
use serde_json::{Value, json};
use trailhead_core::{Event, EventFilter, UndoReport};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::{Auth, require_human};
use crate::error::{ApiResult, Json, Path, Query, invalid};
use crate::openapi::{ErrorBody, TaskParam};
use crate::params::{parse_opt_time, parse_time, split_list};

use super::S;

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct HistoryQuery {
    after: Option<i64>,
    limit: Option<i64>,
    /// Include events older than 90 days, which have moved to the archive.
    #[serde(default)]
    archived: bool,
}

#[utoipa::path(
    get, path = "/v1/tasks/{task}/history", tag = "events", params(TaskParam, HistoryQuery),
    responses(
        (status = 200, description = "The task's events", body = Vec<Event>),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
pub(crate) async fn task_history(
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
pub(crate) struct EventQuery {
    after: Option<i64>,
    /// Only events with a smaller id; with `newest_first`, pass the last id you got to page back.
    before: Option<i64>,
    task: Option<String>,
    /// Project key: events on its tasks and on the project itself.
    project: Option<String>,
    actor: Option<Uuid>,
    batch: Option<Uuid>,
    #[param(format = DateTime)]
    since: Option<String>,
    limit: Option<i64>,
    #[serde(default)]
    newest_first: bool,
    /// Leave out notifications (`task.due_soon`, `task.overdue`).
    #[serde(default)]
    changes_only: bool,
    /// Comma-separated kind prefixes, e.g. `claim.,comment.created`. Applied before `limit`.
    kinds: Option<String>,
}

#[utoipa::path(
    get, path = "/v1/events", tag = "events", params(EventQuery),
    responses((status = 200, description = "Events", body = Vec<Event>))
)]
pub(crate) async fn list_events(State(s): S, Auth(ctx): Auth, Query(q): Query<EventQuery>) -> ApiResult<Json<Value>> {
    let filter = EventFilter {
        after_id: q.after,
        before_id: q.before,
        task: q.task,
        project: q.project,
        actor_id: q.actor,
        batch_id: q.batch,
        since: parse_opt_time(q.since.as_deref())?,
        limit: q.limit,
        newest_first: q.newest_first,
        include_archived: false,
        changes_only: q.changes_only,
        kinds: split_list(q.kinds.as_deref()),
    };
    Ok(Json(json!(s.store.list_events(&ctx, filter).await?)))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct UndoBody {
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
pub(crate) async fn undo(State(s): S, Auth(ctx): Auth, Json(b): Json<UndoBody>) -> ApiResult<Json<Value>> {
    let report = match (b.event_id, b.batch_id) {
        (Some(e), None) => s.store.undo_event(&ctx, e).await?,
        (None, Some(batch)) => s.store.undo_batch(&ctx, batch).await?,
        _ => return Err(invalid("send exactly one of event_id or batch_id")),
    };
    Ok(Json(json!(report)))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct RevertBody {
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
pub(crate) async fn revert(State(s): S, Auth(ctx): Auth, Json(b): Json<RevertBody>) -> ApiResult<Json<Value>> {
    require_human(&ctx)?;
    Ok(Json(json!(s.store.revert_actor(&ctx, b.actor_id, parse_time(&b.since)?).await?)))
}
