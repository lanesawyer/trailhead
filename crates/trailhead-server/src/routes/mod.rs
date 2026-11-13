//! The REST API under `/v1`, one module per resource.

pub(crate) mod access;
pub(crate) mod claims;
pub(crate) mod comments;
pub(crate) mod dependencies;
pub(crate) mod events;
pub(crate) mod projects;
pub(crate) mod tasks;
pub(crate) mod webhooks;

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::AppState;
use crate::auth::Auth;
use crate::error::{ApiResult, Json, invalid};
use crate::openapi::Health;
use crate::params::split_list;
use trailhead_core::Me;

type S = State<Arc<AppState>>;

pub fn v1() -> Router<Arc<AppState>> {
    Router::new()
        .route("/health", get(health))
        .route("/openapi.json", get(crate::openapi::serve))
        .route("/me", get(me))
        .route("/projects", get(projects::list_projects).post(projects::create_project))
        .route("/projects/{project}", get(projects::get_project).patch(projects::update_project))
        .route("/tasks", get(tasks::list_tasks).post(tasks::create_task))
        .route("/tasks/{task}", get(tasks::get_task).patch(tasks::update_task).delete(tasks::delete_task))
        .route("/tasks/{task}/claim", post(claims::claim))
        .route("/tasks/{task}/renew", post(claims::renew))
        .route("/tasks/{task}/release", post(claims::release))
        .route("/tasks/{task}/comments", get(comments::list_comments).post(comments::add_comment))
        .route("/tasks/{task}/blockers", get(dependencies::list_blockers).post(dependencies::add_blocker))
        .route("/tasks/{task}/blockers/{blocker}", delete(dependencies::remove_blocker))
        .route("/tasks/{task}/history", get(events::task_history))
        .route("/comments/{id}/resolve", post(comments::resolve_comment))
        .route("/ready", get(tasks::ready))
        .route("/claims", get(claims::my_claims))
        .route("/search", get(tasks::search))
        .route("/events", get(events::list_events))
        .route("/events/stream", get(crate::stream::events))
        .route("/undo", post(events::undo))
        .route("/revert", post(events::revert))
        .route("/users", get(access::list_users))
        .route("/agents", get(access::list_agents).post(access::create_agent))
        .route("/agents/{agent}/revoke", post(access::revoke_agent))
        .route("/tokens", get(access::list_tokens).post(access::create_token))
        .route("/tokens/{id}", delete(access::revoke_token).patch(access::update_token))
        .route("/webhooks", get(webhooks::list_webhooks).post(webhooks::create_webhook))
        .route("/webhooks/{id}", delete(webhooks::delete_webhook))
        .route("/webhooks/{id}/deliveries", get(webhooks::webhook_deliveries))
}

/// Distinguishes an absent field (`None`) from an explicit `null` (`Some(None)`).
fn nullable<'de, T: Deserialize<'de>, D: Deserializer<'de>>(d: D) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

/// `?fields=key,title` trims each object in a list to the named fields.
fn select_fields<T: Serialize>(items: &[T], fields: Option<&str>) -> ApiResult<Value> {
    let v = serde_json::to_value(items).map_err(trailhead_core::Error::from)?;
    let Some(fields) = fields else { return Ok(v) };
    let keep = split_list(Some(fields));
    let Value::Array(items) = v else { return Ok(v) };
    Ok(Value::Array(
        items
            .into_iter()
            .map(|item| match item {
                Value::Object(mut m) => {
                    Value::Object(keep.iter().filter_map(|k| m.remove(k).map(|v| (k.clone(), v))).collect())
                }
                other => other,
            })
            .collect(),
    ))
}

/// Bodies that may be empty, like `POST /claim` with no TTL.
fn optional_body<T: serde::de::DeserializeOwned + Default>(b: &[u8]) -> ApiResult<T> {
    if b.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(b).map_err(|e| invalid(format!("bad JSON body: {e}")))
}

fn if_match(headers: &HeaderMap) -> ApiResult<Option<i64>> {
    let Some(h) = headers.get(header::IF_MATCH) else { return Ok(None) };
    let s = h.to_str().map_err(|_| invalid("bad If-Match"))?.trim().trim_start_matches("W/").trim_matches('"');
    if s == "*" {
        return Ok(None);
    }
    s.parse().map(Some).map_err(|_| invalid("If-Match must be a task version"))
}

fn with_etag(version: i64, body: impl Serialize) -> Response {
    let mut res = Json(body).into_response();
    res.headers_mut().insert(header::ETAG, HeaderValue::from_str(&format!("\"{version}\"")).expect("ascii"));
    res
}

fn created(body: impl Serialize) -> Response {
    (StatusCode::CREATED, Json(body)).into_response()
}

#[utoipa::path(
    get, path = "/v1/health", tag = "meta", security(()),
    responses((status = 200, description = "The server is up", body = Health))
)]
async fn health(State(s): S, headers: HeaderMap) -> Json<Health> {
    // A store that is locked or stuck can't serve claims, so say so rather than reporting up. The status stays 200:
    // the CLI reads any other as no server running. The bound stays well under the CLI's 2s health timeout, or a slow
    // store would read as no server. The ping takes the writer lock and an immediate transaction, so it queues behind
    // real writes; this is unauthenticated and polled often, which a liveness check of the write path can afford.
    let store = tokio::time::timeout(std::time::Duration::from_millis(750), s.store.ping()).await;
    let store_error = match &store {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(e.to_string()),
        Err(_) => Some("the store did not answer in time".to_string()),
    };
    // Paths name the local user, so over a tunnel only token holders see them.
    let authed = headers.contains_key(header::AUTHORIZATION) && crate::auth::authenticate(&s, &headers).await.is_ok();
    let info = &s.info;
    let path = |p: &Option<std::path::PathBuf>| p.as_ref().filter(|_| authed).map(|p| p.to_string_lossy().into_owned());
    Json(Health {
        ok: store_error.is_none(),
        store_error,
        version: info.version.clone(),
        instance: info.instance.clone(),
        pid: authed.then_some(info.pid),
        exe: path(&info.exe),
        exe_modified_ms: info.exe_modified_ms.filter(|_| authed),
        db: path(&info.db),
        started_at: info.started_at,
        foreground: info.foreground,
        runs_in_flight: s.runs_in_flight.get().map(|runs| runs()),
        draining: s.draining.load(std::sync::atomic::Ordering::SeqCst),
        upgrade_pending: s.upgrade_pending.load(std::sync::atomic::Ordering::SeqCst),
    })
}

#[utoipa::path(
    get, path = "/v1/me", tag = "meta",
    responses((status = 200, description = "The caller", body = Me))
)]
async fn me(State(s): S, Auth(ctx): Auth) -> ApiResult<Json<Me>> {
    let user = s.store.get_user(&ctx, ctx.user_id).await?;
    let agent = match ctx.agent_id {
        Some(id) => s.store.list_agents(&ctx).await?.into_iter().find(|a| a.id == id),
        None => None,
    };
    Ok(Json(Me { workspace_id: ctx.workspace_id, user, agent_id: ctx.agent_id, agent }))
}
