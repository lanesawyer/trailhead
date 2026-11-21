//! Asking for work from the shell's window: [`crate::ask`] over HTTP.

use axum::extract::State;
use axum::response::Response;
use trailhead_core::shell::{Ask, AskTargets, Asked};

use crate::auth::Auth;
use crate::error::{ApiResult, Json};
use crate::openapi::ErrorBody;

use super::{S, created};

#[utoipa::path(
    get, path = "/v1/shell/ask", tag = "meta",
    responses((status = 200, description = "What can be asked for from the window: the active projects pointed at the apps repo. `configured` is false, and the rest empty, when the user config has no `[shell] repo`", body = AskTargets))
)]
pub(crate) async fn ask_targets(State(s): S, Auth(ctx): Auth) -> ApiResult<Json<AskTargets>> {
    let builds = s.shell.get();
    Ok(Json(crate::ask::targets(&s.store, &ctx, builds.is_some(), builds.and_then(|b| b.repo())).await?))
}

#[utoipa::path(
    post, path = "/v1/shell/ask", tag = "meta", request_body = Ask,
    responses(
        (status = 201, description = "A graded task in the app's project, or a new app's project pointed at the apps repo and its first graded task. A new app whose project was made but whose first task wasn't has `task: null` and says why in `notes`", body = Asked),
        (status = 403, description = "`forbidden`: only a person can ask; an agent's token is refused", body = ErrorBody),
    )
)]
pub(crate) async fn ask(State(s): S, Auth(ctx): Auth, Json(b): Json<Ask>) -> ApiResult<Response> {
    let builds = s.shell.get();
    let repo = builds.and_then(|b| b.repo());
    Ok(created(crate::ask::ask(&s.store, &ctx, builds.is_some(), repo.as_deref(), b).await?))
}
