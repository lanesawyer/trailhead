//! Comments and answering questions.

use axum::extract::State;
use axum::response::Response;
use serde::Deserialize;
use serde_json::{Value, json};
use trailhead_core::{Comment, CommentKind, NewComment};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::Auth;
use crate::error::{ApiResult, Json, Path};
use crate::openapi::{ErrorBody, TaskParam};

use super::{S, created, optional_body};

#[utoipa::path(
    get, path = "/v1/tasks/{task}/comments", tag = "comments", params(TaskParam),
    responses(
        (status = 200, description = "Comments, oldest first", body = Vec<Comment>),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
pub(crate) async fn list_comments(State(s): S, Auth(ctx): Auth, Path(t): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.list_comments(&ctx, &t).await?)))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct AddComment {
    #[serde(default = "note")]
    kind: CommentKind,
    body: String,
    addressed_to: Option<Uuid>,
}

fn note() -> CommentKind {
    CommentKind::Note
}

#[utoipa::path(
    post, path = "/v1/tasks/{task}/comments", tag = "comments", params(TaskParam), request_body = AddComment,
    responses(
        (status = 201, description = "Created", body = Comment),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
pub(crate) async fn add_comment(
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
pub(crate) struct ResolveBody {
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
pub(crate) async fn resolve_comment(
    State(s): S,
    Auth(ctx): Auth,
    Path(id): Path<Uuid>,
    b: axum::body::Bytes,
) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.resolve_comment(&ctx, id, optional_body::<ResolveBody>(&b)?.answer).await?)))
}
