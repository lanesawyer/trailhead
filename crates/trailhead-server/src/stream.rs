use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use futures_util::Stream;
use serde::Deserialize;
use trailhead_core::app_data::AppEvent;
use trailhead_core::{AppEventFilter, Ctx, Event, EventFilter};

use crate::AppState;
use crate::auth::Auth;
use crate::error::{ApiResult, Query};

/// Other processes can write too (a second server on Turso, say), so streams
/// also poll rather than relying only on this process's commit signal.
const POLL: Duration = Duration::from_secs(2);
const PAGE: i64 = 100;

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct StreamQuery {
    /// Start after this event id. `Last-Event-ID` wins when both are sent.
    /// Without either, the stream starts at the newest event.
    after: Option<i64>,
    task: Option<String>,
    /// Project key: events on its tasks and on the project itself.
    project: Option<String>,
    /// Comma-separated kind prefixes, e.g. `task.,comment.created`.
    kinds: Option<String>,
}

struct Cursor {
    state: Arc<AppState>,
    ctx: Ctx,
    after: i64,
    task: Option<String>,
    project: Option<String>,
    kinds: Vec<String>,
    changes: tokio::sync::watch::Receiver<u64>,
    shutdown: tokio::sync::watch::Receiver<bool>,
    buffered: VecDeque<Event>,
}

fn to_sse(e: &Event) -> SseEvent {
    SseEvent::default()
        .id(e.id.to_string())
        .event(e.kind.clone())
        .data(serde_json::to_string(e).expect("events serialize"))
}

/// `GET /v1/events/stream`: every event as Server-Sent Events, read from the
/// events table so a reconnect with `Last-Event-ID` misses nothing.
#[utoipa::path(
    get,
    path = "/v1/events/stream",
    tag = "events",
    params(
        StreamQuery,
        ("Last-Event-ID" = Option<i64>, Header, description = "Resume after this event id"),
    ),
    responses(
        (status = 200, description = "Server-Sent Events. Each has `id` (the event id), `event` (the kind, e.g. `task.updated`) and `data` (the event as JSON).",
            content_type = "text/event-stream", body = Event),
        (status = 404, description = "`not_found`: the `task` or `project` filter", body = crate::openapi::ErrorBody),
    )
)]
pub async fn events(
    State(state): State<Arc<AppState>>,
    Auth(ctx): Auth,
    headers: HeaderMap,
    Query(q): Query<StreamQuery>,
) -> ApiResult<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>> {
    let last_id = headers.get("last-event-id").and_then(|h| h.to_str().ok()).and_then(|s| s.trim().parse().ok());
    let after = match last_id.or(q.after) {
        Some(id) => id,
        None => state.store.latest_event_id(&ctx).await?,
    };
    if q.task.is_some() || q.project.is_some() {
        // Fail now on an unknown task or project rather than inside the stream.
        let check = EventFilter {
            task: q.task.clone().map(Into::into),
            project: q.project.clone(),
            limit: Some(0),
            ..Default::default()
        };
        state.store.list_events(&ctx, check).await?;
    }
    let cursor = Cursor {
        changes: state.store.changes(),
        shutdown: state.shutdown.subscribe(),
        state,
        ctx,
        after,
        task: q.task,
        project: q.project,
        kinds: crate::params::split_list(q.kinds.as_deref()),
        buffered: VecDeque::new(),
    };
    let stream = futures_util::stream::unfold(cursor, |mut c| async move {
        loop {
            if *c.shutdown.borrow() {
                return None;
            }
            if let Some(e) = c.buffered.pop_front() {
                return Some((Ok(to_sse(&e)), c));
            }
            c.changes.mark_unchanged();
            let filter = EventFilter {
                after_id: Some(c.after),
                task: c.task.clone().map(Into::into),
                project: c.project.clone(),
                limit: Some(PAGE),
                ..Default::default()
            };
            match c.state.store.list_events(&c.ctx, filter).await {
                Ok(page) if !page.is_empty() => {
                    c.after = page.last().map_or(c.after, |e| e.id);
                    let wanted =
                        |e: &Event| c.kinds.is_empty() || c.kinds.iter().any(|k| e.kind.starts_with(k.as_str()));
                    let keep: Vec<_> = page.into_iter().filter(wanted).collect();
                    c.buffered.extend(keep);
                }
                Ok(_) => {
                    tokio::select! {
                        changed = c.changes.changed() => if changed.is_err() { return None },
                        _ = c.shutdown.wait_for(|s| *s) => return None,
                        _ = tokio::time::sleep(POLL) => {}
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "event stream query failed");
                    tokio::time::sleep(POLL).await;
                }
            }
        }
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AppStreamQuery {
    /// Start after this event id, such as a page of records' `trailhead-event-cursor`. `Last-Event-ID` wins when both
    /// are sent. Without either, the stream starts at the app's newest event.
    after: Option<i64>,
    collection: Option<String>,
    /// Only events on this record.
    record: Option<uuid::Uuid>,
}

struct AppCursor {
    state: Arc<AppState>,
    ctx: Ctx,
    app: String,
    filter: AppEventFilter,
    changes: tokio::sync::watch::Receiver<u64>,
    shutdown: tokio::sync::watch::Receiver<bool>,
    buffered: VecDeque<AppEvent>,
}

/// `GET /v1/apps/{app}/events/stream`: the app's events as Server-Sent Events, read from its file by id as the task
/// stream reads the events table, so a reconnect with `Last-Event-ID` misses nothing.
#[utoipa::path(
    get,
    path = "/v1/apps/{app}/events/stream",
    tag = "apps",
    params(
        crate::routes::apps::AppParam,
        AppStreamQuery,
        ("Last-Event-ID" = Option<i64>, Header, description = "Resume after this event id"),
    ),
    responses(
        (status = 200, description = "Server-Sent Events. Each has `id` (the event id in this app), `event` (the kind, e.g. `record.updated`) and `data` (the event as JSON).",
            content_type = "text/event-stream", body = AppEvent),
        (status = 403, description = "`forbidden`: the token doesn't reach this app", body = crate::openapi::ErrorBody),
        (status = 404, description = "`not_found`: no such app", body = crate::openapi::ErrorBody),
    )
)]
pub async fn app_events(
    State(state): State<Arc<AppState>>,
    Auth(ctx): Auth,
    crate::error::Path(app): crate::error::Path<String>,
    headers: HeaderMap,
    Query(q): Query<AppStreamQuery>,
) -> ApiResult<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>> {
    // Subscribed before the cursor is read, so a write between the two still wakes the stream.
    let changes = state.store.app_changes(&ctx, &app).await?;
    let last_id = headers.get("last-event-id").and_then(|h| h.to_str().ok()).and_then(|s| s.trim().parse().ok());
    let after = match last_id.or(q.after) {
        Some(id) => id,
        None => state.store.latest_app_event_id(&ctx, &app).await?,
    };
    let filter =
        AppEventFilter { collection: q.collection, record: q.record, after_id: Some(after), limit: Some(PAGE) };
    let cursor =
        AppCursor { changes, shutdown: state.shutdown.subscribe(), state, ctx, app, filter, buffered: VecDeque::new() };
    let stream = futures_util::stream::unfold(cursor, |mut c| async move {
        loop {
            if *c.shutdown.borrow() {
                return None;
            }
            if let Some(e) = c.buffered.pop_front() {
                let sse = SseEvent::default()
                    .id(e.id.to_string())
                    .event(e.kind.clone())
                    .data(serde_json::to_string(&e).expect("events serialize"));
                return Some((Ok(sse), c));
            }
            c.changes.mark_unchanged();
            match c.state.store.list_app_events(&c.ctx, &c.app, c.filter.clone()).await {
                Ok(page) if !page.is_empty() => {
                    c.filter.after_id = page.last().map(|e| e.id).or(c.filter.after_id);
                    c.buffered.extend(page);
                }
                Ok(_) => {
                    tokio::select! {
                        changed = c.changes.changed() => if changed.is_err() { return None },
                        _ = c.shutdown.wait_for(|s| *s) => return None,
                        _ = tokio::time::sleep(POLL) => {}
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, app = c.app, "app event stream query failed");
                    tokio::time::sleep(POLL).await;
                }
            }
        }
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
