//! The server's settings in its user config, read and changed from the window: [`crate::user_config`] over HTTP.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use trailhead_core::config::{ConfigChange, ServerConfig};
use trailhead_core::{Ctx, Error};

use crate::auth::Auth;
use crate::error::{ApiResult, Json};
use crate::openapi::{ErrorBody, ErrorDetail};
use crate::user_config::{ConfigFile, Refused};

use super::S;

#[utoipa::path(
    get, path = "/v1/config", tag = "meta",
    responses(
        (status = 200, description = "The server's settings in its user config (`[factory]` and `[shell]`), read with the parsers it starts with, and which of them wait for a restart. Never the file's text", body = ServerConfig),
        (status = 403, description = "`forbidden`: only a person reads the server's settings; an agent's token is refused", body = ErrorBody),
        (status = 404, description = "`not_found`: this server reads no user config", body = ErrorBody),
    )
)]
pub(crate) async fn show(State(s): S, Auth(ctx): Auth) -> ApiResult<Response> {
    let file = config_file(&s, &ctx)?;
    Ok(answer(file.show(s.info.foreground)))
}

#[utoipa::path(
    patch, path = "/v1/config", tag = "meta", request_body = ConfigChange,
    responses(
        (status = 200, description = "The change is written, and the file as it now is. The running server is unchanged: what it waits for is in `waiting`", body = ServerConfig),
        (status = 400, description = "`invalid`: the file the change would leave is one the server or the CLI would refuse, in their words. Nothing was written", body = ErrorBody),
        (status = 403, description = "`forbidden`: only a person changes the server's settings; an agent's token is refused", body = ErrorBody),
        (status = 404, description = "`not_found`: this server reads no user config", body = ErrorBody),
        (status = 412, description = "`version_conflict`: the file changed on disk since `version` was read. Nothing was written; read it again", body = ErrorBody),
    )
)]
pub(crate) async fn change(State(s): S, Auth(ctx): Auth, Json(change): Json<ConfigChange>) -> ApiResult<Response> {
    let file = config_file(&s, &ctx)?;
    Ok(answer(file.change(change, s.info.foreground).await))
}

fn config_file<'a>(s: &'a crate::AppState, ctx: &Ctx) -> ApiResult<&'a ConfigFile> {
    if !ctx.is_human() {
        return Err(Error::Forbidden("only a person can read or change the server's settings".into()).into());
    }
    Ok(s.config.get().ok_or_else(|| Error::NotFound("this server reads no user config".into()))?)
}

fn answer(result: Result<ServerConfig, Refused>) -> Response {
    let (status, code, message) = match result {
        Ok(config) => return Json(config).into_response(),
        Err(Refused::Stale) => (
            StatusCode::PRECONDITION_FAILED,
            "version_conflict",
            "the config file changed on disk since it was read; read it again".to_string(),
        ),
        Err(Refused::Invalid(message)) => (StatusCode::BAD_REQUEST, "invalid", message),
        Err(Refused::Io(message)) => {
            tracing::error!(error = %message, "the user config couldn't be read or written");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal", message)
        }
    };
    let error = ErrorDetail { code: code.into(), message, ..ErrorDetail::default() };
    (status, axum::Json(ErrorBody { error })).into_response()
}
