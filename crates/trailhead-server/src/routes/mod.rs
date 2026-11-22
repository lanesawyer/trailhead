//! The REST API under `/v1`, one module per resource.

pub(crate) mod access;
pub(crate) mod apps;
pub(crate) mod attachments;
pub(crate) mod claims;
pub(crate) mod comments;
pub(crate) mod config;
pub(crate) mod dependencies;
pub(crate) mod events;
pub(crate) mod notifications;
pub(crate) mod projects;
pub(crate) mod shell;
pub(crate) mod tasks;
pub(crate) mod webhooks;

use std::sync::Arc;

use axum::Router;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::IntoParams;

use crate::AppState;
use crate::auth::Auth;
use crate::error::{ApiResult, Json, Query, invalid};
use crate::openapi::Health;
use crate::params::split_list;
use trailhead_core::Me;
use trailhead_core::shell::ShellBuild;

type S = State<Arc<AppState>>;

pub fn v1() -> Router<Arc<AppState>> {
    Router::new()
        .route("/health", get(health))
        .route("/openapi.json", get(crate::openapi::serve))
        .route("/me", get(me))
        .route("/config", get(config::show).patch(config::change))
        .route("/shell/build", get(shell_build))
        .route("/shell/source/{rev}", get(shell_source))
        .route("/shell/ask", get(shell::ask_targets).post(shell::ask))
        .route("/projects", get(projects::list_projects).post(projects::create_project))
        .route("/projects/{project}", get(projects::get_project).patch(projects::update_project))
        .route("/tasks", get(tasks::list_tasks).post(tasks::create_task))
        .route("/tasks/{task}", get(tasks::get_task).patch(tasks::update_task).delete(tasks::delete_task))
        .route("/tasks/{task}/claim", post(claims::claim))
        .route("/tasks/{task}/renew", post(claims::renew))
        .route("/tasks/{task}/release", post(claims::release))
        .route("/tasks/{task}/set-aside", post(tasks::set_aside))
        .route("/tasks/{task}/plan/approve", post(tasks::approve_plan))
        .route("/tasks/{task}/comments", get(comments::list_comments).post(comments::add_comment))
        .route(
            "/tasks/{task}/attachments",
            get(attachments::list_attachments)
                .post(attachments::add_attachment)
                .layer(DefaultBodyLimit::max(trailhead_core::MAX_ATTACHMENT_BYTES)),
        )
        .route("/attachments/{id}", get(attachments::read_attachment).delete(attachments::remove_attachment))
        .route("/tasks/{task}/blockers", get(dependencies::list_blockers).post(dependencies::add_blocker))
        .route("/tasks/{task}/blockers/{blocker}", delete(dependencies::remove_blocker))
        .route("/tasks/{task}/history", get(events::task_history))
        .route("/comments/{id}/resolve", post(comments::resolve_comment))
        .route("/notifications", get(notifications::list).post(notifications::raise))
        .route("/notifications/settings", get(notifications::settings).put(notifications::set_settings))
        .route("/notifications/read-all", post(notifications::read_all))
        .route("/notifications/resolve", post(notifications::resolve_about))
        .route("/notifications/{id}/read", post(notifications::read))
        .route("/notifications/{id}/unread", post(notifications::unread))
        .route("/notifications/{id}/resolve", post(notifications::resolve))
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
        .route("/apps", get(apps::list_apps))
        .route("/apps/{app}", get(apps::get_app))
        .route("/apps/{app}/declaration", put(apps::put_declaration))
        .route("/apps/{app}/records/{collection}", get(apps::list_records).post(apps::create_record))
        .route(
            "/apps/{app}/records/{collection}/{id}",
            get(apps::get_record).patch(apps::patch_record).put(apps::replace_record).delete(apps::delete_record),
        )
        .route("/apps/{app}/keys/{collection}/{key}", get(apps::get_by_key).put(apps::put_by_key))
        .route("/apps/{app}/batch", post(apps::batch))
        .route("/apps/{app}/events", get(apps::list_events))
        .route("/apps/{app}/events/stream", get(crate::stream::app_events))
        .route("/apps/{app}/undo", post(apps::undo))
}

// Distinguishes an absent field (`None`) from an explicit `null` (`Some(None)`): the one the task bodies use.
use trailhead_core::wire::nullable;

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
    s.parse().map(Some).map_err(|_| invalid("If-Match must be a version, like \"3\""))
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

#[utoipa::path(
    get, path = "/v1/shell/source/{rev}", tag = "meta",
    params(("rev" = String, Path, description = "A build the server keeps: `current` or `previous` of `GET /v1/shell/build`")),
    responses(
        (status = 200, description = "The apps repo at that build, as a gzipped tar, for a launcher on another machine to build the shell itself (`trailhead app --from`)", content_type = "application/gzip"),
        (status = 403, description = "The caller is an agent: the apps repo's source is a person's to fetch"),
        (status = 404, description = "No builder runs here, or it keeps no build of that commit")
    )
)]
async fn shell_source(
    State(s): S,
    Auth(ctx): Auth,
    axum::extract::Path(rev): axum::extract::Path<String>,
) -> ApiResult<Response> {
    use trailhead_core::Error;
    if !ctx.is_human() {
        return Err(Error::Forbidden("only a person can fetch the shell's source".into()).into());
    }
    let source = match s.shell.get() {
        Some(builds) => builds.source(&rev).await,
        None => None,
    };
    match source {
        Some(Ok(tar)) => Ok(([(header::CONTENT_TYPE, "application/gzip")], tar).into_response()),
        Some(Err(e)) => {
            tracing::warn!(target: "shell", rev, error = %e, "couldn't read a build's source");
            Ok((StatusCode::INTERNAL_SERVER_ERROR, format!("reading the build's source: {e}")).into_response())
        }
        None => Err(Error::NotFound(format!("the server keeps no build of {rev}")).into()),
    }
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct ShellBuildQuery {
    /// The build a shell runs (a commit hash, 7 to 40 hex digits): the answer's `changes` are the commits after it up
    /// to the newest build. Anything else, or a commit the builder doesn't have, leaves them empty.
    since: Option<String>,
}

#[utoipa::path(
    get, path = "/v1/shell/build", tag = "meta", params(ShellBuildQuery),
    responses((status = 200, description = "The shells the server's builder keeps: `configured` is false, and the rest empty, when the user config has no `[shell] repo`", body = ShellBuild))
)]
async fn shell_build(State(s): S, Auth(_): Auth, Query(q): Query<ShellBuildQuery>) -> Json<ShellBuild> {
    match s.shell.get() {
        Some(builds) => Json(builds.read(q.since.as_deref()).await),
        None => Json(ShellBuild::default()),
    }
}
