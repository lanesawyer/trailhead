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

/// A connector's agent (`connector/<id>`) hands its own feeds over and does nothing else: its token reaches no other
/// route, REST or MCP. `feed_of` is the connector whose feed route this request is, if it is one.
pub async fn refuse_connectors(state: &AppState, ctx: &Ctx, feed_of: Option<&str>) -> Result<(), Error> {
    if ctx.agent_id.is_none() {
        return Ok(());
    }
    match state.store.connector_of(ctx).await? {
        Some(id) if feed_of != Some(id.as_str()) => {
            Err(Error::Forbidden(format!("connector/{id} may only hand over its own feeds")))
        }
        _ => Ok(()),
    }
}

/// The connector of a `/v1/connectors/{id}/feeds/{feed}` path.
fn feed_route(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/v1/connectors/")?;
    let (id, rest) = rest.split_once('/')?;
    let feed = rest.strip_prefix("feeds/")?;
    (!feed.is_empty() && !feed.contains('/')).then_some(id)
}

/// The authenticated caller.
pub struct Auth(pub Ctx);

impl FromRequestParts<Arc<AppState>> for Auth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &Arc<AppState>) -> Result<Self, ApiError> {
        let (mut ctx, _) = authenticate(state, &parts.headers).await?;
        let path = parts.extensions.get::<axum::extract::OriginalUri>().map_or(&parts.uri, |o| &o.0).path();
        let feed_of = (parts.method == axum::http::Method::PUT).then(|| feed_route(path)).flatten();
        refuse_connectors(state, &ctx, feed_of).await?;
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

#[cfg(test)]
mod tests {
    #[test]
    fn only_a_feed_route_names_a_connector() {
        assert_eq!(super::feed_route("/v1/connectors/github/feeds/pull_requests"), Some("github"));
        for other in ["/v1/connectors/github", "/v1/connectors/github/run", "/v1/connectors/github/feeds/", "/v1/tasks"]
        {
            assert_eq!(super::feed_route(other), None, "{other}");
        }
    }
}
