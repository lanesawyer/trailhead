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

pub fn require_human(ctx: &Ctx) -> Result<(), ApiError> {
    if ctx.is_human() { Ok(()) } else { Err(ApiError(Error::Forbidden("only a human can do this".into()))) }
}
