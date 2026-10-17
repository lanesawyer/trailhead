//! Claims: taking, renewing and releasing leases.

use axum::extract::State;
use axum::response::Response;
use serde::Deserialize;
use serde_json::{Value, json};
use trailhead_core::{Claim, ClaimResult, HeldClaim, Task};
use utoipa::ToSchema;

use crate::auth::Auth;
use crate::error::{ApiResult, Json, Path};
use crate::openapi::{ErrorBody, TaskParam};

use super::{S, optional_body, with_etag};

#[derive(Deserialize, Default, ToSchema)]
pub(crate) struct ClaimBody {
    ttl: Option<i64>,
}

#[utoipa::path(
    post, path = "/v1/tasks/{task}/claim", tag = "claims", params(TaskParam),
    request_body(content = Option<ClaimBody>, description = "Optional; an empty body uses the default TTL"),
    responses(
        (status = 200, description = "Claimed, with the last handoff", body = ClaimResult, headers(("ETag" = String, description = "The version, quoted, for `If-Match`"))),
        (status = 404, description = "`not_found`", body = ErrorBody),
        (status = 409, description = "`claim_held`, `blocked_by` or `invalid_transition`", body = ErrorBody),
    )
)]
pub(crate) async fn claim(
    State(s): S,
    Auth(ctx): Auth,
    Path(t): Path<String>,
    b: axum::body::Bytes,
) -> ApiResult<Response> {
    let res = s.store.claim(&ctx, &t, optional_body::<ClaimBody>(&b)?.ttl).await?;
    Ok(with_etag(res.task.version, res))
}

#[utoipa::path(
    post, path = "/v1/tasks/{task}/renew", tag = "claims", params(TaskParam),
    request_body(content = Option<ClaimBody>, description = "Optional; an empty body uses the default TTL"),
    responses(
        (status = 200, description = "The extended claim", body = Claim),
        (status = 404, description = "`not_found`", body = ErrorBody),
        (status = 409, description = "`claim_lost` (you held it, but it expired or someone else took it) or `not_claim_holder`", body = ErrorBody),
    )
)]
pub(crate) async fn renew(
    State(s): S,
    Auth(ctx): Auth,
    Path(t): Path<String>,
    b: axum::body::Bytes,
) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.renew(&ctx, &t, optional_body::<ClaimBody>(&b)?.ttl).await?)))
}

#[utoipa::path(
    post, path = "/v1/tasks/{task}/release", tag = "claims", params(TaskParam),
    responses(
        (status = 200, description = "Released; the task is open again", body = Task, headers(("ETag" = String, description = "The version, quoted, for `If-Match`"))),
        (status = 404, description = "`not_found`", body = ErrorBody),
        (status = 409, description = "`not_claim_holder`", body = ErrorBody),
    )
)]
pub(crate) async fn release(State(s): S, Auth(ctx): Auth, Path(t): Path<String>) -> ApiResult<Response> {
    let task = s.store.release(&ctx, &t).await?;
    Ok(with_etag(task.version, task))
}

#[utoipa::path(
    get, path = "/v1/claims", tag = "claims",
    responses((status = 200, description = "Live claims you hold, soonest to expire first", body = Vec<HeldClaim>))
)]
pub(crate) async fn my_claims(State(s): S, Auth(ctx): Auth) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.my_claims(&ctx).await?)))
}
