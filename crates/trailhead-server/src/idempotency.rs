//! `Idempotency-Key` on `/v1` writes. The first request with a key runs and,
//! if it succeeds, its response is stored; a retry with the same key and the
//! same request gets that response back instead of running again.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderName, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use trailhead_core::{Ctx, Error, StoredResponse, Token, idempotency_fingerprint};

use crate::AppState;
use crate::auth::BATCH_HEADER;
use crate::error::{ApiError, invalid};

pub const IDEMPOTENCY_KEY: HeaderName = HeaderName::from_static("idempotency-key");
/// Set on a response that was replayed from an earlier request.
pub const REPLAYED_HEADER: HeaderName = HeaderName::from_static("idempotent-replayed");

/// Matches axum's default `Json` limit, so buffering here rejects nothing the
/// handler would have accepted.
const BODY_LIMIT: usize = 2 * 1024 * 1024;
const MAX_KEY_LEN: usize = 255;

/// Response headers worth replaying. `trailhead-batch` lets a retrying caller
/// undo the original request.
const KEPT: [HeaderName; 4] = [header::CONTENT_TYPE, header::ETAG, header::LOCATION, BATCH_HEADER];

pub async fn layer(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let write = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if !write || !req.uri().path().starts_with("/v1/") || !req.headers().contains_key(IDEMPOTENCY_KEY) {
        return next.run(req).await;
    }
    let key = match req.headers().get(IDEMPOTENCY_KEY).map(HeaderValue::to_str) {
        Some(Ok(k)) if !k.trim().is_empty() && k.len() <= MAX_KEY_LEN => k.trim().to_string(),
        _ => {
            return invalid(format!("Idempotency-Key must be 1-{MAX_KEY_LEN} visible ASCII characters"))
                .into_response();
        }
    };
    // Unauthenticated requests fall through so the handler answers with its usual 401.
    let Some((ctx, token)) = authenticate(&state, bearer(&req)).await else { return next.run(req).await };
    // Keys belong to the token, not just the principal, so a project-scoped token
    // can't replay a response stored under the same agent's broader token.
    let key = format!("{}:{key}", token.id);

    let (parts, body) = req.into_parts();
    let Ok(bytes) = to_bytes(body, BODY_LIMIT).await else {
        return invalid("request body is too large or unreadable").into_response();
    };
    let path = parts.uri.path_and_query().map_or(parts.uri.path(), |p| p.as_str());
    let fingerprint = idempotency_fingerprint(parts.method.as_str(), path, &bytes);
    match state.store.idempotency_begin(&ctx, &key, &fingerprint).await {
        Ok(Some(stored)) => {
            // Replays are cheap, but they still count against the token's limit.
            if let Err(e) = state.limits.check(&token) {
                return ApiError(e).into_response();
            }
            return replay(stored);
        }
        Ok(None) => {}
        Err(e) => return ApiError(e).into_response(),
    }

    let mut reservation = Reservation { state: state.clone(), ctx, key, open: true };
    let res = next.run(Request::from_parts(parts, Body::from(bytes))).await;
    if !res.status().is_success() {
        reservation.abandon().await;
        return res;
    }
    let (parts, body) = res.into_parts();
    let Ok(bytes) = to_bytes(body, usize::MAX).await else {
        reservation.abandon().await;
        return ApiError(Error::Corrupt("response body could not be read".into())).into_response();
    };
    let stored = StoredResponse {
        status: parts.status.as_u16(),
        headers: KEPT
            .iter()
            .filter_map(|h| Some((h.as_str().to_string(), parts.headers.get(h)?.to_str().ok()?.to_string())))
            .collect(),
        body: bytes.to_vec(),
    };
    if let Err(e) = state.store.idempotency_complete(&reservation.ctx, &reservation.key, &stored).await {
        // The write itself succeeded; failing the request now would invite a duplicate.
        tracing::warn!(error = %e, "saving idempotent response failed");
        reservation.abandon().await;
    }
    reservation.open = false;
    Response::from_parts(parts, Body::from(bytes))
}

fn bearer(req: &Request) -> Option<String> {
    let h = req.headers().get(header::AUTHORIZATION)?.to_str().ok()?;
    Some(h.strip_prefix("Bearer ")?.trim().to_string())
}

async fn authenticate(state: &AppState, secret: Option<String>) -> Option<(Ctx, Token)> {
    state.store.authenticate_token(&secret?).await.ok()
}

fn replay(stored: StoredResponse) -> Response {
    let mut res = Response::new(Body::from(stored.body));
    *res.status_mut() = StatusCode::from_u16(stored.status).unwrap_or(StatusCode::OK);
    for (k, v) in &stored.headers {
        if let (Ok(k), Ok(v)) = (HeaderName::try_from(k.as_str()), HeaderValue::from_str(v)) {
            res.headers_mut().insert(k, v);
        }
    }
    res.headers_mut().insert(REPLAYED_HEADER, HeaderValue::from_static("true"));
    res
}

/// A key held by an in-flight request. Released if the request is dropped
/// midway (say, the client disconnects), so a retry isn't stuck on `in_flight`.
struct Reservation {
    state: Arc<AppState>,
    ctx: Ctx,
    key: String,
    open: bool,
}

impl Reservation {
    async fn abandon(&mut self) {
        self.open = false;
        if let Err(e) = self.state.store.idempotency_abandon(&self.ctx, &self.key).await {
            tracing::warn!(error = %e, "releasing idempotency key failed");
        }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.open {
            return;
        }
        let (state, ctx, key) = (self.state.clone(), self.ctx.clone(), std::mem::take(&mut self.key));
        tokio::spawn(async move {
            state.store.idempotency_abandon(&ctx, &key).await.ok();
        });
    }
}
