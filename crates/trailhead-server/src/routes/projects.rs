//! Projects.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use serde::Deserialize;
use serde_json::{Value, json};
use trailhead_core::{NewProject, Project, ProjectPatch, ProjectStatus};
use utoipa::{IntoParams, ToSchema};

use crate::auth::Auth;
use crate::error::{ApiResult, Json, Path, Query};
use crate::openapi::{ErrorBody, IfMatch, ProjectParam};

use super::{S, created, if_match, nullable, with_etag};

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct ProjectListQuery {
    #[serde(default)]
    archived: bool,
}

#[utoipa::path(
    get, path = "/v1/projects", tag = "projects", params(ProjectListQuery),
    responses((status = 200, description = "Projects", body = Vec<Project>))
)]
pub(crate) async fn list_projects(
    State(s): S,
    Auth(ctx): Auth,
    Query(q): Query<ProjectListQuery>,
) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.list_projects(&ctx, q.archived).await?)))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct CreateProject {
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
pub(crate) async fn create_project(State(s): S, Auth(ctx): Auth, Json(b): Json<CreateProject>) -> ApiResult<Response> {
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
    get, path = "/v1/projects/{project}", tag = "projects", params(ProjectParam),
    responses(
        (status = 200, description = "The project", body = Project, headers(("ETag" = String, description = "The version, quoted, for `If-Match`"))),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
pub(crate) async fn get_project(State(s): S, Auth(ctx): Auth, Path(p): Path<String>) -> ApiResult<Response> {
    let project = s.store.get_project(&ctx, &p).await?;
    Ok(with_etag(project.version, project))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct UpdateProject {
    name: Option<String>,
    brief: Option<String>,
    status: Option<ProjectStatus>,
    require_review: Option<bool>,
    /// Agent names or ids that may approve review work in this project (`in_review` → `done`).
    /// Replaces the list; `[]` clears it. Humans only.
    reviewers: Option<Vec<String>>,
    #[serde(default, deserialize_with = "nullable")]
    claim_ttl: Option<Option<i64>>,
    metadata: Option<Value>,
}

#[utoipa::path(
    patch, path = "/v1/projects/{project}", tag = "projects", params(ProjectParam, IfMatch),
    request_body = UpdateProject,
    responses(
        (status = 200, description = "Updated", body = Project, headers(("ETag" = String, description = "The version, quoted, for `If-Match`"))),
        (status = 404, description = "`not_found`", body = ErrorBody),
        (status = 412, description = "`version_conflict`", body = ErrorBody),
    )
)]
pub(crate) async fn update_project(
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
        reviewers: b.reviewers,
        claim_ttl: b.claim_ttl,
        metadata: b.metadata,
    };
    let project = s.store.update_project(&ctx, &p, patch, if_match(&headers)?).await?;
    Ok(with_etag(project.version, project))
}
