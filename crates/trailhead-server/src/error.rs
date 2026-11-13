use crate::openapi::{ErrorBody, ErrorDetail};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use trailhead_core::Error;

/// Every error leaves the server as `{"error": {"code", "message", ...}}` with a
/// stable `code`.
pub struct ApiError(pub Error);

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        ApiError(e)
    }
}

macro_rules! rejection {
    ($($t:ty),+) => {$(
        impl From<$t> for ApiError {
            fn from(r: $t) -> Self {
                ApiError(Error::Invalid(r.body_text()))
            }
        }
    )+};
}

rejection!(
    axum::extract::rejection::JsonRejection,
    axum::extract::rejection::QueryRejection,
    axum::extract::rejection::PathRejection
);

/// Extractors whose rejections use the API's error envelope.
#[derive(axum::extract::FromRequest)]
#[from_request(via(axum::Json), rejection(ApiError))]
pub struct Json<T>(pub T);

impl<T: serde::Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

#[derive(axum::extract::FromRequestParts)]
#[from_request(via(axum::extract::Query), rejection(ApiError))]
pub struct Query<T>(pub T);

#[derive(axum::extract::FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(ApiError))]
pub struct Path<T>(pub T);

pub type ApiResult<T> = Result<T, ApiError>;

pub fn invalid(msg: impl Into<String>) -> ApiError {
    ApiError(Error::Invalid(msg.into()))
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let e = &self.0;
        let status = match e {
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            Error::VersionConflict { .. } => StatusCode::PRECONDITION_FAILED,
            Error::ClaimHeld { .. }
            | Error::NotClaimHolder
            | Error::ClaimLost(_)
            | Error::BlockedBy(_)
            | Error::InvalidTransition { .. }
            | Error::DependencyCycle
            | Error::UndoConflict { .. }
            | Error::UndoWindowExpired(_)
            | Error::AlreadyExists(_)
            | Error::IdempotencyKeyInFlight => StatusCode::CONFLICT,
            Error::IdempotencyKeyReused => StatusCode::UNPROCESSABLE_ENTITY,
            Error::ReviewRequired | Error::Forbidden(_) => StatusCode::FORBIDDEN,
            Error::Unauthorized => StatusCode::UNAUTHORIZED,
            Error::Invalid(_) => StatusCode::BAD_REQUEST,
            Error::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            Error::Db(_) | Error::Json(_) | Error::Corrupt(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let mut body = ErrorDetail { code: e.code().into(), message: e.to_string(), ..ErrorDetail::default() };
        match e {
            Error::BlockedBy(keys) => body.blocked_by = Some(keys.iter().map(|k| k.0.clone()).collect()),
            Error::VersionConflict { current, .. } => body.current_version = Some(*current),
            Error::ClaimHeld { holder, holder_id, expires_at } => {
                body.holder = Some(holder.clone());
                body.holder_id = Some(holder_id.clone());
                body.expires_at = Some(expires_at.clone());
            }
            Error::UndoConflict { later_event, .. } => body.later_event = Some(*later_event),
            Error::RateLimited { retry_after } => body.retry_after = Some(*retry_after),
            Error::InvalidTransition { from, to } => {
                body.from = Some(*from);
                body.to = Some(*to);
            }
            _ => {}
        }
        if status == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!(error = %e, "internal error");
        }
        let mut res = (status, axum::Json(ErrorBody { error: body })).into_response();
        if let Error::RateLimited { retry_after } = e {
            res.headers_mut().insert(axum::http::header::RETRY_AFTER, (*retry_after).into());
        }
        res
    }
}
