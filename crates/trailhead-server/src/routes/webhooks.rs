//! Outbound webhooks.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;
use serde_json::{Value, json};
use trailhead_core::{Webhook, WebhookDelivery};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::Auth;
use crate::error::{ApiResult, Json, Path, Query};
use crate::openapi::{CreatedWebhook, ErrorBody};

use super::{S, created};

#[derive(Deserialize, ToSchema)]
pub(crate) struct CreateWebhook {
    /// Where to POST events.
    url: String,
    /// Event kind prefixes, e.g. `["task.", "comment.created"]`. Omitted or empty means every event.
    #[serde(default)]
    event_kinds: Vec<String>,
}

/// Registers a webhook for events from now on. Each delivery is a POST of the
/// event JSON with `trailhead-signature: t=<unix>,v1=<hex HMAC-SHA256 of "<t>.<body>">`
/// keyed by the secret, which is shown only here. Delivery is ordered and
/// at-least-once; failures back off exponentially and 20 in a row disable the
/// webhook. Humans with unscoped tokens only.
#[utoipa::path(
    post, path = "/v1/webhooks", tag = "webhooks", request_body = CreateWebhook,
    responses(
        (status = 201, description = "The webhook and its signing secret, shown once", body = CreatedWebhook),
        (status = 403, description = "`forbidden`: agents and scoped tokens can't manage webhooks", body = ErrorBody),
    )
)]
pub(crate) async fn create_webhook(State(s): S, Auth(ctx): Auth, Json(b): Json<CreateWebhook>) -> ApiResult<Response> {
    let (webhook, secret) = s.store.create_webhook(&ctx, &b.url, b.event_kinds).await?;
    Ok(created(json!({ "webhook": webhook, "secret": secret })))
}

#[utoipa::path(
    get, path = "/v1/webhooks", tag = "webhooks",
    responses((status = 200, description = "Webhooks, without secrets", body = Vec<Webhook>))
)]
pub(crate) async fn list_webhooks(State(s): S, Auth(ctx): Auth) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.list_webhooks(&ctx).await?)))
}

#[utoipa::path(
    delete, path = "/v1/webhooks/{id}", tag = "webhooks", params(("id" = Uuid, Path, description = "Webhook id")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
pub(crate) async fn delete_webhook(State(s): S, Auth(ctx): Auth, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    s.store.delete_webhook(&ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, IntoParams)]
pub(crate) struct DeliveryQuery {
    /// Defaults to 50.
    limit: Option<i64>,
}

#[utoipa::path(
    get, path = "/v1/webhooks/{id}/deliveries", tag = "webhooks",
    params(("id" = Uuid, Path, description = "Webhook id"), DeliveryQuery),
    responses((status = 200, description = "Delivery attempts, newest first", body = Vec<WebhookDelivery>))
)]
pub(crate) async fn webhook_deliveries(
    State(s): S,
    Auth(ctx): Auth,
    Path(id): Path<Uuid>,
    Query(q): Query<DeliveryQuery>,
) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.webhook_deliveries(&ctx, id, q.limit).await?)))
}
