//! Notifications (`docs/app-platform.md#notifications`): what a person is told, listed, raised, read and resolved.
//! Each change is also an event on the stream, so a window keeps its list without asking again.

use axum::extract::State;
use axum::response::Response;
use serde::{Deserialize, Serialize};
use trailhead_core::{NewNotification, Notification, NotificationAction, NotificationFilter};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::Auth;
use crate::error::{ApiResult, Json, Path, Query};
use crate::openapi::ErrorBody;

use super::{S, created};

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct ListQuery {
    /// Only those neither read nor resolved.
    #[serde(default)]
    unread: bool,
    /// Only those raised before this one, to page back.
    before: Option<Uuid>,
    /// At most this many: 50 unless given, never more than 200.
    limit: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub(crate) struct NotificationList {
    /// Newest first.
    notifications: Vec<Notification>,
    /// How many are neither read nor resolved, in all, whatever this page holds.
    unread: i64,
}

#[utoipa::path(
    get, path = "/v1/notifications", tag = "notifications", params(ListQuery),
    responses((status = 200, description = "Notifications, newest first, and how many are unread. A token limited to some projects sees none", body = NotificationList))
)]
pub(crate) async fn list(
    State(s): S,
    Auth(ctx): Auth,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<NotificationList>> {
    let filter = NotificationFilter { unread: q.unread, before: q.before, limit: q.limit };
    let notifications = s.store.list_notifications(&ctx, filter).await?;
    Ok(Json(NotificationList { notifications, unread: s.store.unread_notifications(&ctx).await? }))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct Raise {
    /// What raises it: an app's id, `factory` or `server`. 1 to 64 characters.
    source: String,
    /// The sort of thing it is, as a short name such as `ticklist.streak`. 1 to 64 characters.
    kind: String,
    /// One line, at most 200 characters.
    title: String,
    /// Plain text, cut to 2000 characters.
    #[serde(default)]
    body: String,
    /// What it is about, as the raiser names it, to resolve by later.
    subject: Option<String>,
    /// The key or id of the task it is about. It is resolved when that task is finished or deleted.
    task: Option<String>,
    /// At most 4 links, each a label and an `http` or `https` address.
    #[serde(default)]
    actions: Vec<NotificationAction>,
}

#[utoipa::path(
    post, path = "/v1/notifications", tag = "notifications", request_body = Raise,
    responses(
        (status = 201, description = "Raised", body = Notification),
        (status = 400, description = "`invalid`: an empty or long source, kind, title or subject, or an action that isn't a web link", body = ErrorBody),
        (status = 404, description = "`not_found`: the task", body = ErrorBody),
    )
)]
pub(crate) async fn raise(State(s): S, Auth(ctx): Auth, Json(b): Json<Raise>) -> ApiResult<Response> {
    let new = NewNotification {
        source: b.source,
        kind: b.kind,
        title: b.title,
        body: b.body,
        subject: b.subject,
        task: b.task,
        actions: b.actions,
    };
    Ok(created(s.store.raise_notification(&ctx, new).await?))
}

#[utoipa::path(
    post, path = "/v1/notifications/{id}/read", tag = "notifications",
    params(("id" = Uuid, Path, description = "Notification id")),
    responses(
        (status = 200, description = "The notification, read", body = Notification),
        (status = 403, description = "`forbidden`: only a person marks one read", body = ErrorBody),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
pub(crate) async fn read(State(s): S, Auth(ctx): Auth, Path(id): Path<Uuid>) -> ApiResult<Json<Notification>> {
    Ok(Json(s.store.mark_notification(&ctx, id, true).await?))
}

#[utoipa::path(
    post, path = "/v1/notifications/{id}/unread", tag = "notifications",
    params(("id" = Uuid, Path, description = "Notification id")),
    responses(
        (status = 200, description = "The notification, unread again", body = Notification),
        (status = 403, description = "`forbidden`: only a person marks one unread", body = ErrorBody),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
pub(crate) async fn unread(State(s): S, Auth(ctx): Auth, Path(id): Path<Uuid>) -> ApiResult<Json<Notification>> {
    Ok(Json(s.store.mark_notification(&ctx, id, false).await?))
}

#[utoipa::path(
    post, path = "/v1/notifications/{id}/resolve", tag = "notifications",
    params(("id" = Uuid, Path, description = "Notification id")),
    responses(
        (status = 200, description = "The notification, resolved: what it is about no longer needs the person", body = Notification),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
pub(crate) async fn resolve(State(s): S, Auth(ctx): Auth, Path(id): Path<Uuid>) -> ApiResult<Json<Notification>> {
    Ok(Json(s.store.resolve_notification(&ctx, id).await?))
}

#[derive(Serialize, ToSchema)]
pub(crate) struct Count {
    /// How many notifications the call changed.
    count: usize,
}

#[utoipa::path(
    post, path = "/v1/notifications/read-all", tag = "notifications",
    responses(
        (status = 200, description = "Every unread one is now read", body = Count),
        (status = 403, description = "`forbidden`: only a person marks them read", body = ErrorBody),
    )
)]
pub(crate) async fn read_all(State(s): S, Auth(ctx): Auth) -> ApiResult<Json<Count>> {
    Ok(Json(Count { count: s.store.mark_all_notifications_read(&ctx).await? }))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct About {
    source: String,
    subject: String,
}

#[utoipa::path(
    post, path = "/v1/notifications/resolve", tag = "notifications", request_body = About,
    responses((status = 200, description = "Every open one that source raised about that subject is now resolved", body = Count))
)]
pub(crate) async fn resolve_about(State(s): S, Auth(ctx): Auth, Json(b): Json<About>) -> ApiResult<Json<Count>> {
    Ok(Json(Count { count: s.store.resolve_notifications_about(&ctx, &b.source, &b.subject).await? }))
}
