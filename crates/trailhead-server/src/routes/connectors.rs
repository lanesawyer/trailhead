//! Connectors (`docs/app-platform.md`, "Connectors"): what the server knows of each and how a person set it, turning
//! one on or off and changing its settings, running one now, and the one route a connector's own run calls: handing
//! a feed over.

use axum::extract::State;
use axum::http::StatusCode;
use serde_json::Value;
use trailhead_core::connector::{ConnectorInfo, ConnectorPatch, FedCollection, FeedSet, HandedOver};
use trailhead_core::{Ctx, Error};
use utoipa::IntoParams;

use crate::auth::Auth;
use crate::error::{ApiResult, Json, Path};
use crate::openapi::ErrorBody;

use super::S;

/// The largest feed set a run may hand over, as JSON: a feed is handed over whole, so it is larger than any other
/// request.
pub(crate) const MAX_FEED_BYTES: usize = 64 * 1024 * 1024;

#[derive(IntoParams)]
#[into_params(parameter_in = Path)]
#[expect(dead_code, reason = "schema only; handlers take Path<String>")]
pub(crate) struct ConnectorParam {
    /// The connector's id, its directory under `connectors/` in the apps repo, e.g. `github`
    id: String,
}

#[derive(IntoParams)]
#[into_params(parameter_in = Path)]
#[expect(dead_code, reason = "schema only; handlers take a Path tuple")]
pub(crate) struct FeedParams {
    /// The connector's id, e.g. `github`
    id: String,
    /// One of its feeds, e.g. `pull_requests`
    feed: String,
}

fn person(ctx: &Ctx) -> ApiResult<()> {
    if ctx.is_human() && !ctx.is_scoped() {
        Ok(())
    } else {
        Err(Error::Forbidden("only a person sees and changes the connectors".into()).into())
    }
}

#[utoipa::path(
    get, path = "/v1/connectors", tag = "connectors",
    responses(
        (status = 200, description = "Every connector the apps repo's last build holds, in id order", body = Vec<ConnectorInfo>),
        (status = 403, description = "`forbidden`: only a person", body = ErrorBody),
    )
)]
pub(crate) async fn list(State(s): S, Auth(ctx): Auth) -> ApiResult<Json<Vec<ConnectorInfo>>> {
    person(&ctx)?;
    Ok(Json(s.connectors.list(&s.store, &ctx).await?))
}

#[utoipa::path(
    get, path = "/v1/connectors/{id}", tag = "connectors", params(ConnectorParam),
    responses(
        (status = 200, description = "What it declares, how it is set, what it still needs, and its runs. A secret's value is never shown, only whether it is set", body = ConnectorInfo),
        (status = 403, description = "`forbidden`: only a person", body = ErrorBody),
        (status = 404, description = "`not_found`: no connector by that id", body = ErrorBody),
    )
)]
pub(crate) async fn show(State(s): S, Auth(ctx): Auth, Path(id): Path<String>) -> ApiResult<Json<ConnectorInfo>> {
    person(&ctx)?;
    Ok(Json(s.connectors.info(&s.store, &ctx, &id).await?))
}

#[utoipa::path(
    patch, path = "/v1/connectors/{id}", tag = "connectors", params(ConnectorParam), request_body = ConnectorPatch,
    responses(
        (status = 200, description = "The connector as now set. Turning it on makes its agent, `connector/<id>`; nothing needs a restart", body = ConnectorInfo),
        (status = 400, description = "`invalid`: a setting it doesn't declare or of the wrong kind, named; or its connector.toml doesn't read", body = ErrorBody),
        (status = 403, description = "`forbidden`: only a person", body = ErrorBody),
        (status = 404, description = "`not_found`: no connector by that id", body = ErrorBody),
    )
)]
pub(crate) async fn change(
    State(s): S,
    Auth(ctx): Auth,
    Path(id): Path<String>,
    Json(patch): Json<ConnectorPatch>,
) -> ApiResult<Json<ConnectorInfo>> {
    person(&ctx)?;
    Ok(Json(s.connectors.change(&s.store, &ctx, &id, patch).await?))
}

#[utoipa::path(
    post, path = "/v1/connectors/{id}/run", tag = "connectors", params(ConnectorParam),
    responses(
        (status = 202, description = "The run has started; `GET /v1/connectors/{id}` shows it running, then its outcome as `last_run`", body = ConnectorInfo),
        (status = 400, description = "`invalid`: it is off, or can't run yet, saying what it needs", body = ErrorBody),
        (status = 403, description = "`forbidden`: only a person", body = ErrorBody),
        (status = 404, description = "`not_found`: no connector by that id", body = ErrorBody),
        (status = 409, description = "`already_exists`: it is running now", body = ErrorBody),
    )
)]
pub(crate) async fn run(
    State(s): S,
    Auth(ctx): Auth,
    Path(id): Path<String>,
) -> ApiResult<(StatusCode, Json<ConnectorInfo>)> {
    person(&ctx)?;
    crate::connectors::start_run(&s, &ctx, &id).await?;
    Ok((StatusCode::ACCEPTED, Json(s.connectors.info(&s.store, &ctx, &id).await?)))
}

#[utoipa::path(
    put, path = "/v1/connectors/{id}/feeds/{feed}", tag = "connectors", params(FeedParams), request_body = FeedSet,
    responses(
        (status = 200, description = "The set was written into every app collection filled from the feed (`from = \"<id>.<feed>\"`), each in its own app: what each write did, and why one failed, which stopped none of the others. A set that matches what an app holds writes nothing there", body = HandedOver),
        (status = 400, description = "`invalid`: a record that doesn't match the feed's fields, naming the feed, its key and the field", body = ErrorBody),
        (status = 403, description = "`forbidden`: only the connector's own agent, `connector/<id>`, hands its feeds over; a person can't", body = ErrorBody),
        (status = 404, description = "`not_found`: no such connector, or it declares no such feed", body = ErrorBody),
    )
)]
pub(crate) async fn hand_over(
    State(s): S,
    Auth(ctx): Auth,
    Path((id, feed)): Path<(String, String)>,
    Json(set): Json<FeedSet>,
) -> ApiResult<Json<HandedOver>> {
    if s.store.connector_of(&ctx).await?.as_deref() != Some(id.as_str()) {
        return Err(Error::Forbidden(format!("only connector/{id} hands over its feeds")).into());
    }
    let manifest = s.connectors.manifest(&id)?;
    let spec = manifest.feeds.get(&feed).ok_or_else(|| Error::NotFound(format!("feed {feed} of connector {id}")))?;
    spec.check(&feed, &set.records).map_err(|e| Error::Invalid(e.to_string()))?;
    let records = set.records.len();
    let rows: Vec<(String, Value)> = set.records.into_iter().map(|r| (r.key, Value::Object(r.data))).collect();
    let from = format!("{id}.{feed}");
    let mut apps = Vec::new();
    for (app, declaration) in s.store.list_apps(&Ctx { apps: None, ..ctx.clone() }).await? {
        for (collection, c) in declaration.collections {
            if c.from.as_deref() != Some(from.as_str()) {
                continue;
            }
            let mut fed = FedCollection { app: app.clone(), collection: collection.clone(), ..Default::default() };
            match s.store.replace_feed_set(&ctx.next_batch(), &app, &collection, rows.clone()).await {
                Ok(counts) => {
                    fed.created = counts.created;
                    fed.changed = counts.changed;
                    fed.removed = counts.removed;
                    fed.unchanged = counts.unchanged;
                }
                Err(e) => {
                    tracing::warn!(target: "connectors", id, feed, app, error = %e, "an app refused a feed's set");
                    fed.error = Some(e.to_string());
                }
            }
            apps.push(fed);
        }
    }
    let handed = HandedOver { feed, records, apps };
    s.connectors.record_handover(&id, &handed);
    Ok(Json(handed))
}
