//! Apps' data (`docs/app-platform.md`, "The app data store and connectors"): each app's declared collections, its
//! records, read and written by id or by key, batches, its events and undo. The store checks every write against the
//! declaration and the token's apps; a collection filled from a feed is never written here.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::Value;
use trailhead_core::app_data::{
    App, AppEvent, AppUndoReport, BatchWrite, BatchWritten, Declaration, Outcome, Record, Written,
};
use trailhead_core::{AppEventFilter, Ctx, Error, NewRecord, RecordChange, RecordPage};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::Auth;
use crate::error::{ApiResult, Json, Path, Query, invalid};
use crate::openapi::{ErrorBody, IfMatch};

use super::{S, if_match, with_etag};

/// On a page of records: the app's newest event as the page was read. A stream resumed after it (`?after=` or
/// `Last-Event-ID`) carries every change since the page, and none the page already holds.
pub const EVENT_CURSOR: HeaderName = HeaderName::from_static("trailhead-event-cursor");

/// The most writes one batch takes.
const MAX_BATCH: usize = 1000;

#[derive(IntoParams)]
#[into_params(parameter_in = Path)]
#[expect(dead_code, reason = "schema only; handlers take Path<String>")]
pub(crate) struct AppParam {
    /// The app's id, e.g. `cairn`
    app: String,
}

#[derive(IntoParams)]
#[into_params(parameter_in = Path)]
#[expect(dead_code, reason = "schema only; handlers take a Path tuple")]
pub(crate) struct CollectionParams {
    /// The app's id, e.g. `cairn`
    app: String,
    /// A collection its declaration names
    collection: String,
}

#[derive(IntoParams)]
#[into_params(parameter_in = Path)]
#[expect(dead_code, reason = "schema only; handlers take a Path tuple")]
pub(crate) struct RecordParams {
    /// The app's id, e.g. `cairn`
    app: String,
    /// The record's collection
    collection: String,
    /// The record's id
    id: Uuid,
}

#[derive(IntoParams)]
#[into_params(parameter_in = Path)]
#[expect(dead_code, reason = "schema only; handlers take a Path tuple")]
pub(crate) struct KeyParams {
    /// The app's id, e.g. `cairn`
    app: String,
    /// The record's collection
    collection: String,
    /// The record's key, percent-encoded as one path segment: `lanesawyer/trailhead#123` is
    /// `lanesawyer%2Ftrailhead%23123`
    key: String,
}

fn no_apps(ctx: &Ctx) -> ApiResult<()> {
    if ctx.apps.as_ref().is_some_and(Vec::is_empty) {
        return Err(Error::Forbidden("this token reaches no app's data".into()).into());
    }
    Ok(())
}

#[utoipa::path(
    get, path = "/v1/apps", tag = "apps",
    responses(
        (status = 200, description = "The apps with data that the token reaches, each with its collections", body = Vec<App>),
        (status = 403, description = "`forbidden`: the token reaches no app's data", body = ErrorBody),
    )
)]
pub(crate) async fn list_apps(State(s): S, Auth(ctx): Auth) -> ApiResult<Json<Vec<App>>> {
    no_apps(&ctx)?;
    let apps = s.store.list_apps(&ctx).await?;
    Ok(Json(apps.into_iter().map(|(id, declaration)| App { id, declaration }).collect()))
}

#[utoipa::path(
    get, path = "/v1/apps/{app}", tag = "apps", params(AppParam),
    responses(
        (status = 200, description = "The app's collections: each one's fields, and for one filled from a connector's feed, which (`from`)", body = App),
        (status = 403, description = "`forbidden`: the token doesn't reach this app", body = ErrorBody),
        (status = 404, description = "`not_found`: no app with data by that id", body = ErrorBody),
    )
)]
pub(crate) async fn get_app(State(s): S, Auth(ctx): Auth, Path(app): Path<String>) -> ApiResult<Json<App>> {
    let declaration = s.store.app_declaration(&ctx, &app).await?;
    Ok(Json(App { id: app, declaration }))
}

/// Registers the app's collections, as the server does from the apps repo it builds: for tests, a server with no apps
/// repo, and an app run from elsewhere. A collection that holds records changes only by addition (a new collection or
/// a new optional field); anything else is refused, saying what. A collection filled from a feed takes the feed's
/// fields from the connector's `connector.toml` in the server's last built apps repo, or, in the JSON form, from
/// `fields` sent with it.
#[utoipa::path(
    put, path = "/v1/apps/{app}/declaration", tag = "apps", params(AppParam),
    request_body(
        description = "The app's `collections.toml` as text, or its JSON form with `Content-Type: application/json`",
        content(
            (String = "application/toml", example = "[collections.snoozes]\nfields = { item = \"string\", until = \"timestamp\" }\n"),
            (Declaration = "application/json"),
        )
    ),
    responses(
        (status = 200, description = "The app as now declared", body = App),
        (status = 400, description = "`invalid`: the declaration, or a change to a collection that holds records, in plain words", body = ErrorBody),
        (status = 403, description = "`forbidden`: only a person declares an app", body = ErrorBody),
    )
)]
pub(crate) async fn put_declaration(
    State(s): S,
    Auth(ctx): Auth,
    Path(app): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Json<App>> {
    if !ctx.is_human() {
        return Err(Error::Forbidden("only a person declares an app's collections".into()).into());
    }
    let json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.trim_start().starts_with("application/json"));
    let declaration = if json {
        serde_json::from_slice::<Declaration>(&body).map_err(|e| invalid(format!("bad JSON declaration: {e}")))?
    } else {
        let text = std::str::from_utf8(&body).map_err(|_| invalid("collections.toml isn't UTF-8"))?;
        Declaration::parse(text).map_err(Error::from)?
    };
    let declaration = crate::declarations::fill_from_build(declaration, s.shell.get()).await?;
    s.store.register_app(&ctx, &app, declaration).await?;
    let declaration = s.store.app_declaration(&ctx, &app).await?;
    Ok(Json(App { id: app, declaration }))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct RecordListQuery {
    /// Only records after this id: the last id of the page before.
    after: Option<Uuid>,
    /// Defaults to 100, at most 1000.
    limit: Option<i64>,
}

#[utoipa::path(
    get, path = "/v1/apps/{app}/records/{collection}", tag = "apps", params(CollectionParams, RecordListQuery),
    responses(
        (status = 200, description = "A page of the collection's records in the order they were made", body = Vec<Record>,
            headers(("trailhead-event-cursor" = i64, description = "The app's newest event as the page was read: stream its events after it to follow on from this page"))),
        (status = 403, description = "`forbidden`: the token doesn't reach this app", body = ErrorBody),
        (status = 404, description = "`not_found`: no such app or collection", body = ErrorBody),
    )
)]
pub(crate) async fn list_records(
    State(s): S,
    Auth(ctx): Auth,
    Path((app, collection)): Path<(String, String)>,
    Query(q): Query<RecordListQuery>,
) -> ApiResult<Response> {
    let page = RecordPage { after: q.after, limit: q.limit };
    let (records, at) = s.store.list_records_at(&ctx, &app, &collection, page).await?;
    let mut res = Json(records).into_response();
    res.headers_mut().insert(EVENT_CURSOR, HeaderValue::from(at));
    Ok(res)
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct CreateRecord {
    /// The name something outside gave it, unique in the collection. `PUT /v1/apps/{app}/keys/{collection}/{key}`
    /// writes by key instead, creating or changing.
    key: Option<String>,
    /// The record's fields, as the collection declares them.
    #[schema(value_type = Object)]
    data: Value,
}

#[utoipa::path(
    post, path = "/v1/apps/{app}/records/{collection}", tag = "apps", params(CollectionParams), request_body = CreateRecord,
    responses(
        (status = 201, description = "Created", body = Record, headers(("ETag" = String, description = "The version, quoted, for `If-Match`"))),
        (status = 400, description = "`invalid`: a field missing, unknown or of the wrong type, named", body = ErrorBody),
        (status = 403, description = "`forbidden`: the token doesn't reach this app, or the collection is filled from a feed", body = ErrorBody),
        (status = 404, description = "`not_found`: no such app or collection", body = ErrorBody),
        (status = 409, description = "`already_exists`: a record in the collection has that key", body = ErrorBody),
    )
)]
pub(crate) async fn create_record(
    State(s): S,
    Auth(ctx): Auth,
    Path((app, collection)): Path<(String, String)>,
    Json(b): Json<CreateRecord>,
) -> ApiResult<Response> {
    let record = s.store.create_record(&ctx, &app, NewRecord { collection, key: b.key, data: b.data }).await?;
    let mut res = with_etag(record.version, record);
    *res.status_mut() = StatusCode::CREATED;
    Ok(res)
}

/// The record, if it is in `collection`: a record's collection is part of its address.
async fn in_collection(s: &S, ctx: &Ctx, app: &str, collection: &str, id: Uuid) -> ApiResult<Record> {
    let record = s.store.get_record(ctx, app, id).await?;
    if record.collection != collection {
        return Err(Error::NotFound(format!("record {id} in {collection}")).into());
    }
    Ok(record)
}

#[utoipa::path(
    get, path = "/v1/apps/{app}/records/{collection}/{id}", tag = "apps", params(RecordParams),
    responses(
        (status = 200, description = "The record", body = Record, headers(("ETag" = String, description = "The version, quoted, for `If-Match`"))),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
pub(crate) async fn get_record(
    s: S,
    Auth(ctx): Auth,
    Path((app, collection, id)): Path<(String, String, Uuid)>,
) -> ApiResult<Response> {
    let record = in_collection(&s, &ctx, &app, &collection, id).await?;
    Ok(with_etag(record.version, record))
}

fn written(w: Written) -> Response {
    let created = w.outcome == Outcome::Created;
    let mut res = with_etag(w.record.version, w);
    if created {
        *res.status_mut() = StatusCode::CREATED;
    }
    res
}

#[utoipa::path(
    patch, path = "/v1/apps/{app}/records/{collection}/{id}", tag = "apps", params(RecordParams, IfMatch),
    request_body(content = Object, description = "A JSON merge patch (RFC 7396) of the record's fields: those given are set, those given as `null` removed, the rest kept"),
    responses(
        (status = 200, description = "`changed`, or `unchanged` when the record already looked like that (nothing written, same version)", body = Written, headers(("ETag" = String, description = "The version, quoted, for `If-Match`"))),
        (status = 400, description = "`invalid`: the result has a field missing, unknown or of the wrong type", body = ErrorBody),
        (status = 403, description = "`forbidden`: the token doesn't reach this app, or the collection is filled from a feed", body = ErrorBody),
        (status = 404, description = "`not_found`", body = ErrorBody),
        (status = 412, description = "`version_conflict`", body = ErrorBody),
    )
)]
pub(crate) async fn patch_record(
    s: S,
    Auth(ctx): Auth,
    Path((app, collection, id)): Path<(String, String, Uuid)>,
    headers: HeaderMap,
    Json(patch): Json<Value>,
) -> ApiResult<Response> {
    in_collection(&s, &ctx, &app, &collection, id).await?;
    let w = s.store.update_record(&ctx, &app, id, RecordChange::Merge(patch), if_match(&headers)?).await?;
    Ok(written(w))
}

#[utoipa::path(
    put, path = "/v1/apps/{app}/records/{collection}/{id}", tag = "apps", params(RecordParams, IfMatch),
    request_body(content = Object, description = "The record's fields, whole; its key stays"),
    responses(
        (status = 200, description = "`changed`, or `unchanged` when the record already looked like that (nothing written, same version)", body = Written, headers(("ETag" = String, description = "The version, quoted, for `If-Match`"))),
        (status = 400, description = "`invalid`: a field missing, unknown or of the wrong type", body = ErrorBody),
        (status = 403, description = "`forbidden`: the token doesn't reach this app, or the collection is filled from a feed", body = ErrorBody),
        (status = 404, description = "`not_found`", body = ErrorBody),
        (status = 412, description = "`version_conflict`", body = ErrorBody),
    )
)]
pub(crate) async fn replace_record(
    s: S,
    Auth(ctx): Auth,
    Path((app, collection, id)): Path<(String, String, Uuid)>,
    headers: HeaderMap,
    Json(data): Json<Value>,
) -> ApiResult<Response> {
    in_collection(&s, &ctx, &app, &collection, id).await?;
    let w = s.store.update_record(&ctx, &app, id, RecordChange::Replace(data), if_match(&headers)?).await?;
    Ok(written(w))
}

#[utoipa::path(
    delete, path = "/v1/apps/{app}/records/{collection}/{id}", tag = "apps", params(RecordParams, IfMatch),
    responses(
        (status = 200, description = "Removed; the record as it was. Undo puts it back", body = Record),
        (status = 403, description = "`forbidden`: the token doesn't reach this app, or the collection is filled from a feed", body = ErrorBody),
        (status = 404, description = "`not_found`", body = ErrorBody),
        (status = 412, description = "`version_conflict`", body = ErrorBody),
    )
)]
pub(crate) async fn delete_record(
    s: S,
    Auth(ctx): Auth,
    Path((app, collection, id)): Path<(String, String, Uuid)>,
    headers: HeaderMap,
) -> ApiResult<Json<Record>> {
    in_collection(&s, &ctx, &app, &collection, id).await?;
    Ok(Json(s.store.delete_record(&ctx, &app, id, if_match(&headers)?).await?))
}

#[utoipa::path(
    get, path = "/v1/apps/{app}/keys/{collection}/{key}", tag = "apps", params(KeyParams),
    responses(
        (status = 200, description = "The record with that key", body = Record, headers(("ETag" = String, description = "The version, quoted"))),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
pub(crate) async fn get_by_key(
    State(s): S,
    Auth(ctx): Auth,
    Path((app, collection, key)): Path<(String, String, String)>,
) -> ApiResult<Response> {
    let record = s.store.get_record_by_key(&ctx, &app, &collection, &key).await?;
    Ok(with_etag(record.version, record))
}

/// Makes the record with this key look like the body: creates it, changes it, or, when it already does, writes
/// nothing (no event, same version). Writing the same thing twice leaves one record.
#[utoipa::path(
    put, path = "/v1/apps/{app}/keys/{collection}/{key}", tag = "apps", params(KeyParams),
    request_body(content = Object, description = "The record's fields, whole"),
    responses(
        (status = 200, description = "`changed` or `unchanged`", body = Written, headers(("ETag" = String, description = "The version, quoted"))),
        (status = 201, description = "`created`", body = Written, headers(("ETag" = String, description = "The version, quoted"))),
        (status = 400, description = "`invalid`: a field missing, unknown or of the wrong type, or a bad key", body = ErrorBody),
        (status = 403, description = "`forbidden`: the token doesn't reach this app, or the collection is filled from a feed", body = ErrorBody),
        (status = 404, description = "`not_found`: no such app or collection", body = ErrorBody),
    )
)]
pub(crate) async fn put_by_key(
    State(s): S,
    Auth(ctx): Auth,
    Path((app, collection, key)): Path<(String, String, String)>,
    Json(data): Json<Value>,
) -> ApiResult<Response> {
    Ok(written(s.store.put_record(&ctx, &app, &collection, &key, data).await?))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct BatchBody {
    /// 1 to 1000 writes, applied in order.
    writes: Vec<BatchWrite>,
}

/// Several writes in one transaction: all of them, or, if one is refused, none, with the refusal's error.
#[utoipa::path(
    post, path = "/v1/apps/{app}/batch", tag = "apps", params(AppParam), request_body = BatchBody,
    responses(
        (status = 200, description = "Every write applied, each one's outcome in order, and the batch they are in, to undo them together", body = BatchWritten),
        (status = 400, description = "`invalid`: a write's data, or no writes, or more than 1000", body = ErrorBody),
        (status = 403, description = "`forbidden`: the token doesn't reach this app, or a write is to a collection filled from a feed", body = ErrorBody),
        (status = 404, description = "`not_found`: a collection or record", body = ErrorBody),
        (status = 409, description = "`already_exists`: a create's key is taken", body = ErrorBody),
        (status = 412, description = "`version_conflict`: a write's `version`", body = ErrorBody),
    )
)]
pub(crate) async fn batch(
    State(s): S,
    Auth(ctx): Auth,
    Path(app): Path<String>,
    Json(b): Json<BatchBody>,
) -> ApiResult<Json<BatchWritten>> {
    if b.writes.is_empty() || b.writes.len() > MAX_BATCH {
        return Err(invalid(format!("a batch is 1 to {MAX_BATCH} writes, not {}", b.writes.len())));
    }
    let results = s.store.write_records(&ctx, &app, b.writes.into_iter().map(Into::into).collect()).await?;
    Ok(Json(BatchWritten { batch_id: ctx.batch_id, results }))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct AppEventQuery {
    /// Only events with a greater id.
    after: Option<i64>,
    collection: Option<String>,
    /// Only events on this record.
    record: Option<Uuid>,
    /// Defaults to 100.
    limit: Option<i64>,
}

#[utoipa::path(
    get, path = "/v1/apps/{app}/events", tag = "apps", params(AppParam, AppEventQuery),
    responses(
        (status = 200, description = "The app's events, oldest first. Each app numbers its own", body = Vec<AppEvent>),
        (status = 403, description = "`forbidden`: the token doesn't reach this app", body = ErrorBody),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
pub(crate) async fn list_events(
    State(s): S,
    Auth(ctx): Auth,
    Path(app): Path<String>,
    Query(q): Query<AppEventQuery>,
) -> ApiResult<Json<Vec<AppEvent>>> {
    let filter = AppEventFilter { collection: q.collection, record: q.record, after_id: q.after, limit: q.limit };
    Ok(Json(s.store.list_app_events(&ctx, &app, filter).await?))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct AppUndoBody {
    event_id: Option<i64>,
    batch_id: Option<Uuid>,
}

#[utoipa::path(
    post, path = "/v1/apps/{app}/undo", tag = "apps", params(AppParam), request_body = AppUndoBody,
    responses(
        (status = 200, description = "What was reverted. A collection filled from a feed is left for its next run to set", body = AppUndoReport),
        (status = 404, description = "`not_found`: no such event or batch in this app", body = ErrorBody),
        (status = 409, description = "`undo_conflict`, or `undo_window_expired` for events older than 90 days", body = ErrorBody),
    )
)]
pub(crate) async fn undo(
    State(s): S,
    Auth(ctx): Auth,
    Path(app): Path<String>,
    Json(b): Json<AppUndoBody>,
) -> ApiResult<Json<AppUndoReport>> {
    let report = match (b.event_id, b.batch_id) {
        (Some(e), None) => s.store.undo_app_event(&ctx, &app, e).await?,
        (None, Some(batch)) => s.store.undo_app_batch(&ctx, &app, batch).await?,
        _ => return Err(invalid("send exactly one of event_id or batch_id")),
    };
    Ok(Json(report))
}
