use std::sync::Arc;

use axum::extract::{FromRequestParts, Request};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
use trailhead_core::{Ctx, Error, Token};
use uuid::Uuid;

use crate::AppState;
use crate::error::ApiError;

pub const BATCH_HEADER: HeaderName = HeaderName::from_static("trailhead-batch");

#[derive(Clone, Copy)]
struct Batch(Uuid);

/// One batch per request, so everything a request did can be undone together.
/// The id is echoed back in the `trailhead-batch` response header.
pub async fn batch(mut req: Request, next: Next) -> Response {
    let id = Uuid::now_v7();
    req.extensions_mut().insert(Batch(id));
    let mut res = next.run(req).await;
    res.headers_mut().insert(BATCH_HEADER, HeaderValue::from_str(&id.to_string()).expect("uuid is ascii"));
    res
}

/// Checks the bearer token and its rate limit. REST and MCP share this.
pub async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<(Ctx, Token), Error> {
    let header = headers.get(axum::http::header::AUTHORIZATION).and_then(|h| h.to_str().ok());
    let secret = header.and_then(|h| h.strip_prefix("Bearer ")).ok_or(Error::Unauthorized)?;
    let (ctx, token) = state.store.authenticate_token(secret.trim()).await?;
    state.limits.check(&token)?;
    Ok((ctx, token))
}

/// The authenticated caller.
pub struct Auth(pub Ctx);

impl FromRequestParts<Arc<AppState>> for Auth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &Arc<AppState>) -> Result<Self, ApiError> {
        let (mut ctx, _) = authenticate(state, &parts.headers).await?;
        if let Some(Batch(id)) = parts.extensions.get::<Batch>() {
            ctx.batch_id = *id;
        }
        Ok(Auth(ctx))
    }
}

/// Lets only a human's token through: the factory's status routes, whose event stream names commands and paths from
/// every run. Agent tokens, a sandbox's sub-agents included (they reach the server through the API tunnel), get 403.
#[cfg(feature = "factory")]
pub async fn human_only(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    use axum::response::IntoResponse;
    match authenticate(&state, req.headers()).await {
        Ok((ctx, _)) if ctx.is_human() => next.run(req).await,
        Ok(_) => ApiError(Error::Forbidden("only the user can read the factory's status".into())).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}
