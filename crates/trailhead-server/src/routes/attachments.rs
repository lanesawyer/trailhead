//! Pictures on a task: uploaded as the raw body, listed as metadata, read back as bytes by id.

use axum::body::Bytes;
use axum::extract::State;
use axum::extract::rejection::BytesRejection;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use trailhead_core::{Attachment, AttachmentRole, MAX_ATTACHMENT_BYTES, NewAttachment};
use utoipa::IntoParams;
use uuid::Uuid;

use crate::auth::Auth;
use crate::error::{ApiResult, Json, Path, Query, invalid};
use crate::openapi::{ErrorBody, TaskParam};

use super::{S, created};

#[utoipa::path(
    get, path = "/v1/tasks/{task}/attachments", tag = "attachments", params(TaskParam),
    responses(
        (status = 200, description = "The task's pictures, oldest first, without their bytes", body = Vec<Attachment>),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
pub(crate) async fn list_attachments(
    State(s): S,
    Auth(ctx): Auth,
    Path(t): Path<String>,
) -> ApiResult<Json<Vec<Attachment>>> {
    Ok(Json(s.store.list_attachments(&ctx, &t).await?))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct AddAttachmentQuery {
    /// A short label, 1 to 64 letters, digits, `.`, `_` or `-`, such as `ticklist-today-dark`. A before and an after
    /// of the same view share it.
    name: String,
    /// `before` or `after`.
    role: Option<AttachmentRole>,
    /// Groups pictures taken together, such as a run id or a commit: 1 to 64 characters.
    set: Option<String>,
}

#[utoipa::path(
    post, path = "/v1/tasks/{task}/attachments", tag = "attachments", params(TaskParam, AddAttachmentQuery),
    request_body(content = Vec<u8>, content_type = "image/png", description = "The picture's bytes: a PNG of at most 2 MiB"),
    responses(
        (status = 201, description = "Attached", body = Attachment),
        (status = 400, description = "`invalid`: not a PNG, too large, a bad name or set, or the task's pictures are at their limit (64, or 32 MiB together)", body = ErrorBody),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
pub(crate) async fn add_attachment(
    State(s): S,
    Auth(ctx): Auth,
    Path(t): Path<String>,
    Query(q): Query<AddAttachmentQuery>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> ApiResult<Response> {
    let bytes = body.map_err(|_| invalid(format!("the picture must be at most {MAX_ATTACHMENT_BYTES} bytes")))?;
    let content_type = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or_default();
    let new = NewAttachment {
        name: q.name,
        content_type: content_type.to_string(),
        role: q.role,
        set: q.set,
        bytes: bytes.to_vec(),
    };
    Ok(created(s.store.add_attachment(&ctx, &t, new).await?))
}

#[utoipa::path(
    get, path = "/v1/attachments/{id}", tag = "attachments",
    params(("id" = Uuid, Path, description = "Attachment id")),
    responses(
        (status = 200, description = "The picture's bytes. They never change, so they may be cached for good.", content_type = "image/png", body = Vec<u8>,
         headers(("ETag" = String, description = "The bytes' SHA-256, quoted"), ("Cache-Control" = String, description = "`private, max-age=31536000, immutable`"))),
        (status = 304, description = "`If-None-Match` names these bytes"),
        (status = 404, description = "`not_found`, also once it is removed", body = ErrorBody),
    )
)]
pub(crate) async fn read_attachment(
    State(s): S,
    Auth(ctx): Auth,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let (a, bytes) = s.store.attachment_bytes(&ctx, id).await?;
    let etag = HeaderValue::from_str(&format!("\"{}\"", a.sha256)).expect("hex is a header value");
    let cache = HeaderValue::from_static("private, max-age=31536000, immutable");
    let fresh = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|tag| tag.trim().trim_start_matches("W/") == etag));
    if fresh {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, etag), (header::CACHE_CONTROL, cache)]).into_response());
    }
    let content_type = HeaderValue::from_str(&a.content_type).unwrap_or(HeaderValue::from_static("image/png"));
    Ok(([(header::CONTENT_TYPE, content_type), (header::ETAG, etag), (header::CACHE_CONTROL, cache)], bytes)
        .into_response())
}

#[utoipa::path(
    delete, path = "/v1/attachments/{id}", tag = "attachments",
    params(("id" = Uuid, Path, description = "Attachment id")),
    responses(
        (status = 200, description = "Removed (a soft delete, which undo restores); the attachment as it is now", body = Attachment),
        (status = 403, description = "`forbidden`: an agent removing a picture it or its sub-agents didn't add", body = ErrorBody),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
pub(crate) async fn remove_attachment(
    State(s): S,
    Auth(ctx): Auth,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Attachment>> {
    Ok(Json(s.store.remove_attachment(&ctx, id).await?))
}
