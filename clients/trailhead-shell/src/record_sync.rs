//! The record sync, run on the net thread beside the task sync ([`crate::sync`]): for each app that keeps
//! collections ([`crate::records::Collections`]), a snapshot of those collections and then the app's own event
//! stream, with a cursor of its own, since each app's file numbers its own events.
//!
//! 1. Read the app (`GET /v1/apps/{app}`): its declaration, and whether the server knows it and the token reaches it.
//! 2. Page through each kept collection (`GET /v1/apps/{app}/records/{collection}`). The first page's
//!    `trailhead-event-cursor` is where the stream starts: it is the oldest cursor read, so nothing written while the
//!    pages load is missed. What arrives twice is ignored by the mirror by version (the rule in `llms.txt`).
//! 3. Follow `GET /v1/apps/{app}/events/stream` from that cursor.
//!
//! A dropped stream is resumed from the last event handed on, as the task stream is; past
//! [`SyncTimings::long_disconnect`], or when the server won't open it, the snapshot is loaded again. An app the server
//! doesn't know yet (`404`), or that the token doesn't reach (`403`), is not an error to retry at once: it is said once
//! in the log and asked again every [`RecordTimings::unregistered`].

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use trailhead_client::http::{Client, app_event_stream_path};
use trailhead_model::app_data::{AppEvent, Declaration, Record};
use uuid::Uuid;

use crate::sync::{Resume, ResumePoint, StreamEnd, SyncTimings, take_frame};

/// What the window keeps of one app: its id and the collections it reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSpec {
    pub app: String,
    pub collections: Vec<String>,
}

/// What the record sync hands to the window, in order. The record mirror ([`crate::records`]) applies them.
#[derive(bevy::prelude::Message, Debug, Clone, PartialEq)]
pub enum RecordItem {
    /// An app's kept collections as of the snapshot. It replaces everything the mirror holds of that app.
    Snapshot(RecordSnapshot),
    /// One event off an app's stream.
    Event { app: String, event: AppEvent },
    /// Where an app's sync is, when that changes.
    Status { app: String, status: RecordStatus },
}

/// An app's kept collections, read page by page.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RecordSnapshot {
    pub app: String,
    /// The first page's event cursor: the stream resumes after it.
    pub at: i64,
    /// The app's collections as the server last had them declared, when the snapshot read it (a test's fixtures
    /// leave it out).
    pub declaration: Option<Declaration>,
    /// Each kept collection the server declares, with its records in the order they were made.
    pub collections: Vec<(String, Vec<Record>)>,
}

/// Where one app's record sync is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecordStatus {
    /// Reading the snapshot.
    Loading,
    /// The stream is open: the mirror follows the server.
    Live,
    /// The stream dropped; resuming it, or about to load the snapshot again.
    Reconnecting,
    /// The server has no app by this id yet: its `collections.toml` isn't registered. Asked again slowly.
    NotRegistered,
    /// The token doesn't reach this app's data, in the server's words. Asked again slowly.
    Forbidden(String),
}

/// How the record sync waits.
#[derive(Debug, Clone, Copy)]
pub struct RecordTimings {
    /// The task sync's: the gap between tries, the disconnect past which a stream isn't resumed, and the silence
    /// that means a stream is dead.
    pub sync: SyncTimings,
    /// How often an app the server doesn't know, or the token doesn't reach, is asked about again.
    pub unregistered: Duration,
}

/// How many records a page asks for. The server allows 1000.
const PAGE: i64 = 500;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Why a snapshot couldn't be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// `404` on the app: it isn't registered.
    NotRegistered,
    /// `403`: the token doesn't reach the app.
    Forbidden(String),
    /// Anything else (the link, a server error): tried again soon.
    Failed(String),
}

/// An app's data as the record sync reads it: a base URL and the user's token.
pub struct RecordRemote {
    http: reqwest::Client,
    api: Client,
    base: String,
    token: String,
}

impl RecordRemote {
    pub fn new(base: String, token: String, timings: &SyncTimings) -> RecordRemote {
        // No total timeout: the stream lives as long as the connection. A read timeout catches a dead one.
        let http = reqwest::Client::builder()
            .connect_timeout(REQUEST_TIMEOUT)
            .read_timeout(timings.silence)
            .build()
            .expect("http client");
        let api = Client::with_http(http.clone(), base.clone(), token.clone());
        RecordRemote { http, api, base, token }
    }

    /// Steps 1 and 2 for `spec`. A kept collection the server doesn't declare is left out, with a warning: the
    /// window was built against a `collections.toml` the server hasn't been given.
    pub async fn load(&self, spec: &AppSpec) -> Result<RecordSnapshot, LoadError> {
        let declared = tokio::time::timeout(REQUEST_TIMEOUT, self.api.show_app(&spec.app))
            .await
            .map_err(|_| LoadError::Failed("no answer".into()))?
            .map_err(|e| match e.status() {
                Some(404) => LoadError::NotRegistered,
                Some(403) => LoadError::Forbidden(e.to_string()),
                _ => LoadError::Failed(e.chain()),
            })?;
        let mut snapshot = RecordSnapshot { app: spec.app.clone(), ..RecordSnapshot::default() };
        let mut first = None;
        for collection in &spec.collections {
            if !declared.declaration.collections.contains_key(collection) {
                tracing::warn!(app = spec.app, collection, "not in the app's declaration on the server; left out");
                continue;
            }
            let mut records = Vec::new();
            let mut after: Option<Uuid> = None;
            loop {
                let page = self.page(&spec.app, collection, after).await?;
                first.get_or_insert(page.event_cursor);
                let last = page.records.len() < PAGE as usize;
                after = page.records.last().map(|r| r.id);
                records.extend(page.records);
                if last {
                    break;
                }
            }
            snapshot.collections.push((collection.clone(), records));
        }
        // With no collection read, the cursor is the app's newest event, which the app's own page says.
        snapshot.at = match first {
            Some(at) => at,
            None => self.newest_event(&spec.app).await?,
        };
        snapshot.declaration = Some(declared.declaration);
        Ok(snapshot)
    }

    async fn page(
        &self,
        app: &str,
        collection: &str,
        after: Option<Uuid>,
    ) -> Result<trailhead_client::http::RecordsPage, LoadError> {
        tokio::time::timeout(REQUEST_TIMEOUT, self.api.list_records(app, collection, after, Some(PAGE)))
            .await
            .map_err(|_| LoadError::Failed("no answer".into()))?
            .map_err(|e| match e.status() {
                Some(404) => LoadError::NotRegistered,
                Some(403) => LoadError::Forbidden(e.to_string()),
                _ => LoadError::Failed(e.chain()),
            })
    }

    /// The newest event in the app's file, or 0 with none.
    async fn newest_event(&self, app: &str) -> Result<i64, LoadError> {
        let mut newest = 0;
        loop {
            let events = tokio::time::timeout(REQUEST_TIMEOUT, self.api.app_events(app, Some(newest), Some(1000)))
                .await
                .map_err(|_| LoadError::Failed("no answer".into()))?
                .map_err(|e| LoadError::Failed(e.chain()))?;
            match events.last() {
                Some(e) if events.len() == 1000 => newest = e.id,
                Some(e) => return Ok(e.id),
                None => return Ok(newest),
            }
        }
    }

    /// Step 3, until it ends: each event is passed to `emit` and `last` moves to its id; `emit` returning false
    /// (the window is gone) ends it. `on_open` runs once the server has accepted the stream.
    pub async fn stream(
        &self,
        app: &str,
        last: &mut i64,
        on_open: impl FnOnce(),
        mut emit: impl FnMut(AppEvent) -> bool,
    ) -> StreamEnd {
        let res = match self
            .http
            .get(format!("{}{}", self.base, app_event_stream_path(app, Some(*last))))
            .bearer_auth(&self.token)
            .header("last-event-id", last.to_string())
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return StreamEnd::Dropped(e.to_string()),
        };
        if !res.status().is_success() {
            return StreamEnd::Refused(format!("the stream answered {}", res.status()));
        }
        on_open();
        let mut res = res;
        let mut buf = Vec::new();
        loop {
            match res.chunk().await {
                Ok(Some(chunk)) => buf.extend_from_slice(&chunk),
                Ok(None) => return StreamEnd::Dropped("the stream closed".into()),
                Err(e) => return StreamEnd::Dropped(e.to_string()),
            }
            while let Some(frame) = take_frame(&mut buf) {
                let (Some(id), false) = (frame.id.and_then(|i| i.trim().parse::<i64>().ok()), frame.data.is_empty())
                else {
                    continue;
                };
                let Ok(event) = serde_json::from_str::<AppEvent>(&frame.data) else { continue };
                if !emit(event) {
                    return StreamEnd::AppGone;
                }
                *last = id;
            }
        }
    }
}

/// Hands items on for one app, and each status only when it changes.
struct Out<F> {
    app: String,
    emit: F,
    status: Option<RecordStatus>,
}

impl<F: FnMut(RecordItem) -> bool> Out<F> {
    fn item(&mut self, item: RecordItem) -> bool {
        (self.emit)(item)
    }

    fn status(&mut self, status: RecordStatus) -> bool {
        if self.status.as_ref() == Some(&status) {
            return true;
        }
        self.status = Some(status.clone());
        (self.emit)(RecordItem::Status { app: self.app.clone(), status })
    }
}

/// Runs one app's record sync until the window goes away: the snapshot, then the stream, over and over. With a
/// `resume` position younger than [`SyncTimings::long_disconnect`], it starts at the stream.
pub async fn run(
    remote: RecordRemote,
    timings: RecordTimings,
    resume: Resume,
    spec: AppSpec,
    emit: impl FnMut(RecordItem) -> bool,
) {
    let mut out = Out { app: spec.app.clone(), emit, status: None };
    // Whether the log has said the app is missing or out of reach, so a slow retry doesn't say it every time.
    let mut said = false;
    'sync: loop {
        let kept = resume.point().filter(|p| p.down_since.is_none_or(|d| d.elapsed() < timings.sync.long_disconnect));
        let mut last = match kept {
            Some(p) => {
                if !out.status(RecordStatus::Reconnecting) {
                    return;
                }
                p.last
            }
            None => {
                resume.clear();
                // Asking again about an app that was refused isn't news: the refusal stays said until it changes.
                if !said && !out.status(RecordStatus::Loading) {
                    return;
                }
                match remote.load(&spec).await {
                    Ok(snapshot) => {
                        said = false;
                        let at = snapshot.at;
                        if !out.item(RecordItem::Snapshot(snapshot)) {
                            return;
                        }
                        resume.set(Some(ResumePoint { last: at, down_since: Some(Instant::now()) }));
                        at
                    }
                    Err(LoadError::Failed(e)) => {
                        tracing::warn!(app = spec.app, error = %e, "can't load the app's records");
                        tokio::time::sleep(timings.sync.retry).await;
                        continue;
                    }
                    Err(LoadError::NotRegistered) => {
                        if !said {
                            said = true;
                            let every = timings.unregistered.as_secs_f32();
                            tracing::info!(app = spec.app, "the server has no such app yet; asking every {every} s");
                        }
                        if !out.status(RecordStatus::NotRegistered) {
                            return;
                        }
                        tokio::time::sleep(timings.unregistered).await;
                        continue;
                    }
                    Err(LoadError::Forbidden(why)) => {
                        let status = RecordStatus::Forbidden(why);
                        if !said {
                            said = true;
                            let every = timings.unregistered.as_secs_f32();
                            tracing::info!(
                                app = spec.app,
                                ?status,
                                "the token doesn't reach this app; asking every {every} s"
                            );
                        }
                        if !out.status(status) {
                            return;
                        }
                        tokio::time::sleep(timings.unregistered).await;
                        continue;
                    }
                }
            }
        };
        loop {
            // A mutex, not a `RefCell`: the future is sent to the net thread's runtime. The two closures never hold
            // it at once.
            let shared = std::sync::Mutex::new(&mut out);
            let end = remote
                .stream(
                    &spec.app,
                    &mut last,
                    || {
                        resume.opened();
                        shared.lock().expect("record sync lock").status(RecordStatus::Live);
                    },
                    |event| {
                        let id = event.id;
                        let sent = shared
                            .lock()
                            .expect("record sync lock")
                            .item(RecordItem::Event { app: spec.app.clone(), event });
                        if sent {
                            resume.advance(id);
                        }
                        sent
                    },
                )
                .await;
            match end {
                StreamEnd::AppGone => return,
                StreamEnd::Refused(why) => {
                    tracing::warn!(app = spec.app, %why, "can't follow the app's events; loading its records again");
                    resume.clear();
                    tokio::time::sleep(timings.sync.retry).await;
                    continue 'sync;
                }
                StreamEnd::Dropped(why) => {
                    tracing::debug!(app = spec.app, %why, last, "app event stream dropped");
                    if !out.status(RecordStatus::Reconnecting) {
                        return;
                    }
                    resume.pause();
                    let down = resume.point().and_then(|p| p.down_since);
                    if down.is_some_and(|d| d.elapsed() >= timings.sync.long_disconnect) {
                        continue 'sync;
                    }
                    tokio::time::sleep(timings.sync.retry).await;
                }
            }
        }
    }
}
