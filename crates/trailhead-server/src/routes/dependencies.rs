//! Blockers between tasks.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;
use serde_json::{Value, json};
use trailhead_core::{Dependency, Task};
use utoipa::ToSchema;

use crate::auth::Auth;
use crate::error::{ApiResult, Json, Path};
use crate::openapi::{ErrorBody, TaskParam};

use super::{S, created};

#[utoipa::path(
    get, path = "/v1/tasks/{task}/blockers", tag = "dependencies", params(TaskParam),
    responses(
        (status = 200, description = "Tasks blocking this one", body = Vec<Task>),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
pub(crate) async fn list_blockers(State(s): S, Auth(ctx): Auth, Path(t): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.blockers(&ctx, &t).await?)))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct AddBlocker {
    blocker: String,
}

#[utoipa::path(
    post, path = "/v1/tasks/{task}/blockers", tag = "dependencies", params(TaskParam), request_body = AddBlocker,
    responses(
        (status = 201, description = "Created", body = Dependency),
        (status = 404, description = "`not_found`", body = ErrorBody),
        (status = 409, description = "`dependency_cycle`. Adding an existing dependency returns it unchanged.", body = ErrorBody),
    )
)]
pub(crate) async fn add_blocker(
    State(s): S,
    Auth(ctx): Auth,
    Path(t): Path<String>,
    Json(b): Json<AddBlocker>,
) -> ApiResult<Response> {
    Ok(created(s.store.add_dependency(&ctx, &b.blocker, &t).await?))
}

#[utoipa::path(
    delete, path = "/v1/tasks/{task}/blockers/{blocker}", tag = "dependencies",
    params(TaskParam, ("blocker" = String, Path, description = "Blocking task key or id")),
    responses(
        (status = 204, description = "Removed"),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
pub(crate) async fn remove_blocker(
    State(s): S,
    Auth(ctx): Auth,
    Path((t, blocker)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    s.store.remove_dependency(&ctx, &blocker, &t).await?;
    Ok(StatusCode::NO_CONTENT)
}
