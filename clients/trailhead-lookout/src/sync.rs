//! The three-step sync (`docs/factory/ui.md#sync`) and the event stream, run on the net thread.
//!
//! 1. Note the newest event id.
//! 2. Load the snapshot: projects, agents, and tasks with their claims, open questions and last handoff, paged.
//! 3. Open `/v1/events/stream` with `Last-Event-ID` set to the id from step 1. Whatever was written during step 2
//!    arrives again, and the mirror ignores it by version.
//!
//! A dropped stream is reopened from the last id received. Past [`SyncTimings::long_disconnect`], or when the
//! server refuses to resume, all three steps run again: the stream reads only `events`, not the archive, so a
//! long-lost client can't trust a resume.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use trailhead_model::Status;

/// What the sync hands to the app, in order. The mirror (`crate::mirror`) is the only thing that applies them.
#[derive(bevy::prelude::Message, Debug, Clone, PartialEq, Eq)]
pub enum SyncItem {
    /// Everything as of step 2. It replaces the whole mirror.
    Snapshot(Snapshot),
    /// One event off the stream, as the server sent it (`id`, `entity`, `entity_id`, `kind`, `after`, ...).
    Event(Value),
}

/// The world at step 2, as the API's JSON.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    /// The newest event id noted in step 1: the stream resumes after it.
    pub at: i64,
    pub projects: Vec<Value>,
    pub agents: Vec<Value>,
    /// Tasks with `claim`, `open_questions`, `last_handoff` and `last_review` on each.
    pub tasks: Vec<Value>,
}

/// Where the sync is, for the connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Report {
    /// Running the snapshot (steps 1 and 2).
    Loading,
    /// The stream is open: the mirror follows the server.
    Live,
    /// The stream dropped; trying to resume, or about to start over.
    Reconnecting,
}

#[derive(Debug, Clone, Copy)]
pub struct SyncTimings {
    /// Wait between attempts at the stream, and at the snapshot.
    pub retry: Duration,
    /// A stream down this long is not resumed: the whole sync runs again.
    pub long_disconnect: Duration,
    /// A stream silent this long is dead. The server sends a keep-alive every 15 seconds.
    pub silence: Duration,
}

impl Default for SyncTimings {
    fn default() -> Self {
        SyncTimings {
            retry: Duration::from_secs(1),
            long_disconnect: Duration::from_secs(300),
            silence: Duration::from_secs(45),
        }
    }
}

const PAGE: usize = 100;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The API as the sync reads it: a base URL and the user's token.
pub struct Remote {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl Remote {
    pub fn new(base: String, token: String, timings: &SyncTimings) -> Remote {
        // No total timeout: the stream lives as long as the connection. A read timeout catches a dead one.
        let http = reqwest::Client::builder()
            .connect_timeout(REQUEST_TIMEOUT)
            .read_timeout(timings.silence)
            .build()
            .expect("http client");
        Remote { http, base, token }
    }

    async fn get(&self, path: &str) -> Result<Value, String> {
        let res = self
            .http
            .get(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            return Err(format!("{path}: the server answered {}", res.status()));
        }
        res.json().await.map_err(|e| format!("{path}: {e}"))
    }

    async fn list(&self, path: &str) -> Result<Vec<Value>, String> {
        match self.get(path).await? {
            Value::Array(items) => Ok(items),
            _ => Err(format!("{path}: expected a list")),
        }
    }

    /// Step 1: the newest event id, or 0 when there are no events.
    pub async fn newest_event_id(&self) -> Result<i64, String> {
        let newest = self.list("/v1/events?newest_first=true&limit=1").await?;
        Ok(newest.first().and_then(|e| e["id"].as_i64()).unwrap_or(0))
    }

    /// Step 2. `at` is what step 1 found.
    pub async fn load_snapshot(&self, at: i64) -> Result<Snapshot, String> {
        let projects = self.list("/v1/projects").await?;
        let agents = self.list("/v1/agents").await?;
        // Pages are by offset, and a task deleted from an earlier page moves the rest up one, so the one at the
        // boundary is skipped. Nothing after `at` need bring it back, so page again if anything was deleted meanwhile.
        let tasks = loop {
            let from = self.newest_event_id().await?.max(at);
            let tasks = self.load_tasks().await?;
            let deleted = self.list(&format!("/v1/events?after={from}&kinds=task.deleted&limit=1")).await?;
            if deleted.is_empty() {
                break tasks;
            }
        };
        Ok(Snapshot { at, projects, agents, tasks })
    }

    async fn load_tasks(&self) -> Result<Vec<Value>, String> {
        // Every status, so a task finishing while the pages load can't shift the next page's first task out of view.
        let all_statuses = Status::ALL.map(Status::as_str).join(",");
        let mut tasks = Vec::new();
        loop {
            let page = self
                .list(&format!(
                    "/v1/tasks?include=claim,open_questions,last_handoff,last_review&status={all_statuses}\
                     &sort=created_at&limit={PAGE}&offset={}",
                    tasks.len()
                ))
                .await?;
            let last = page.len() < PAGE;
            tasks.extend(page);
            if last {
                return Ok(tasks);
            }
        }
    }

    /// Step 3, until it ends. Each event is passed to `emit` and `last` moves to its id; `emit` returning false
    /// (the app is gone) ends it. `on_open` runs once the server has accepted the stream.
    pub async fn stream(
        &self,
        last: &mut i64,
        on_open: impl FnOnce(),
        mut emit: impl FnMut(SyncItem) -> bool,
    ) -> StreamEnd {
        let res = match self
            .http
            .get(format!("{}/v1/events/stream", self.base))
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
                let Ok(event) = serde_json::from_str::<Value>(&frame.data) else { continue };
                if !emit(SyncItem::Event(event)) {
                    return StreamEnd::AppGone;
                }
                *last = id;
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum StreamEnd {
    /// The connection ended or failed; the last id is still good to resume from.
    Dropped(String),
    /// The server wouldn't open the stream (not a network failure): resuming is off, start over.
    Refused(String),
    /// Nobody is listening any more.
    AppGone,
}

/// Where the stream got to, kept outside [`run`] so a sync that is stopped (the link went down) can be picked up
/// again from the same place if it's soon enough.
#[derive(Clone, Default)]
pub struct Resume(Arc<Mutex<Option<ResumePoint>>>);

#[derive(Clone, Copy)]
struct ResumePoint {
    /// The id of the last event handed on, or the snapshot's.
    last: i64,
    /// Since when the stream has not been open; `None` while it is.
    down_since: Option<Instant>,
}

impl Resume {
    fn point(&self) -> Option<ResumePoint> {
        *self.0.lock().expect("resume lock")
    }

    fn set(&self, point: Option<ResumePoint>) {
        *self.0.lock().expect("resume lock") = point;
    }

    /// Forgets the position: the next sync starts from a snapshot.
    pub fn clear(&self) {
        self.set(None);
    }

    /// Notes that the stream is no longer being read, as of now.
    pub fn pause(&self) {
        if let Some(p) = self.0.lock().expect("resume lock").as_mut() {
            p.down_since.get_or_insert_with(Instant::now);
        }
    }

    fn advance(&self, id: i64) {
        if let Some(p) = self.0.lock().expect("resume lock").as_mut() {
            p.last = p.last.max(id);
        }
    }

    fn opened(&self) {
        if let Some(p) = self.0.lock().expect("resume lock").as_mut() {
            p.down_since = None;
        }
    }
}

/// Runs the sync until the app goes away: the snapshot, then the stream, over and over. `report` says where it is.
/// With a `resume` position that's younger than [`SyncTimings::long_disconnect`], it starts at the stream.
pub async fn run(
    remote: Remote,
    timings: SyncTimings,
    resume: Resume,
    mut emit: impl FnMut(SyncItem) -> bool,
    report: impl Fn(Report),
) {
    let mut emit = |item: SyncItem| {
        let id = match &item {
            SyncItem::Event(e) => e["id"].as_i64(),
            SyncItem::Snapshot(_) => None,
        };
        let sent = emit(item);
        if let (true, Some(id)) = (sent, id) {
            resume.advance(id);
        }
        sent
    };
    'sync: loop {
        let kept = resume.point().filter(|p| p.down_since.is_none_or(|d| d.elapsed() < timings.long_disconnect));
        let mut last = match kept {
            Some(p) => {
                report(Report::Reconnecting);
                p.last
            }
            None => {
                resume.clear();
                report(Report::Loading);
                match snapshot(&remote, &mut emit).await {
                    Ok(Some(at)) => {
                        resume.set(Some(ResumePoint { last: at, down_since: Some(Instant::now()) }));
                        at
                    }
                    Ok(None) => return,
                    Err(e) => {
                        tracing::warn!(error = %e, "can't load the snapshot");
                        tokio::time::sleep(timings.retry).await;
                        continue;
                    }
                }
            }
        };
        loop {
            let end = remote
                .stream(
                    &mut last,
                    || {
                        resume.opened();
                        report(Report::Live);
                    },
                    &mut emit,
                )
                .await;
            match end {
                StreamEnd::AppGone => return,
                StreamEnd::Refused(why) => {
                    tracing::warn!(%why, "can't resume the event stream; syncing from scratch");
                    resume.clear();
                    tokio::time::sleep(timings.retry).await;
                    continue 'sync;
                }
                StreamEnd::Dropped(why) => {
                    tracing::debug!(%why, last, "event stream dropped");
                    report(Report::Reconnecting);
                    resume.pause();
                    let down = resume.point().and_then(|p| p.down_since);
                    if down.is_some_and(|d| d.elapsed() >= timings.long_disconnect) {
                        continue 'sync;
                    }
                    tokio::time::sleep(timings.retry).await;
                }
            }
        }
    }
}

/// Steps 1 and 2, handed to `emit`. `None` when the app is gone.
async fn snapshot(remote: &Remote, emit: &mut impl FnMut(SyncItem) -> bool) -> Result<Option<i64>, String> {
    let at = remote.newest_event_id().await?;
    let snapshot = remote.load_snapshot(at).await?;
    Ok(emit(SyncItem::Snapshot(snapshot)).then_some(at))
}

#[derive(Debug, Default, PartialEq)]
struct Frame {
    id: Option<String>,
    data: String,
}

/// Removes the first complete server-sent-events frame (ended by a blank line) from `buf`. Frames are decoded
/// whole, so a multi-byte character split across chunks survives.
fn take_frame(buf: &mut Vec<u8>) -> Option<Frame> {
    let (pos, len) = [&b"\r\n\r\n"[..], b"\n\n", b"\r\r"]
        .into_iter()
        .filter_map(|sep| buf.windows(sep.len()).position(|w| w == sep).map(|p| (p, sep.len())))
        .min_by_key(|&(p, _)| p)?;
    let raw: Vec<u8> = buf.drain(..pos + len).collect();
    let text = String::from_utf8_lossy(&raw[..pos]).replace("\r\n", "\n").replace('\r', "\n");
    let mut frame = Frame::default();
    let mut data = Vec::new();
    for line in text.split('\n') {
        let value = |v: &str| v.strip_prefix(' ').unwrap_or(v).to_string();
        if let Some(id) = line.strip_prefix("id:") {
            frame.id = Some(value(id));
        } else if let Some(d) = line.strip_prefix("data:") {
            data.push(value(d));
        }
    }
    frame.data = data.join("\n");
    Some(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_survive_splits_and_skip_keepalives() {
        let mut buf = b": keep-alive\n\nid: 7\nevent: task.updated\ndata: {\"t\":\"h\xc3".to_vec();
        // The keep-alive comment is a frame with nothing in it.
        assert_eq!(take_frame(&mut buf), Some(Frame::default()));
        assert_eq!(take_frame(&mut buf), None);
        buf.extend_from_slice(b"\xa9llo\"}\r\n\r\n");
        let f = take_frame(&mut buf).unwrap();
        assert_eq!((f.id.as_deref(), f.data.as_str()), (Some("7"), "{\"t\":\"héllo\"}"));
        assert!(buf.is_empty());
    }
}
