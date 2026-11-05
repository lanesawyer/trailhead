//! The factory status surface (`docs/factory/ui.md#factory-status`): the control plane serves its in-memory snapshot
//! and event stream on a loopback port, and `status` and `logs --follow` read them. Nothing is stored but a small file
//! saying where to connect, removed on a clean stop.
//!
//! The stream carries tool calls, which can name commands and paths, so the port needs a bearer token that only the
//! user who runs the control plane can read (the file is `0600`).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use trailhead_control::StatusBoard;
use trailhead_daemon::ui::{RepoFeed, StatusFeed};
use trailhead_factory::{FactorySnapshot, Sequenced};

/// The boards behind `trailhead/ui/0`'s status request. The protocol is up before the repos are, so the boards are
/// filled in once they exist; until then a UI sees no repos.
#[derive(Clone, Default)]
pub struct BoardFeed(Arc<OnceLock<Boards>>);

/// A repo's slug and its board.
type Boards = Vec<(String, Arc<StatusBoard>)>;

impl BoardFeed {
    pub fn set(&self, boards: Vec<(String, Arc<StatusBoard>)>) {
        let _ = self.0.set(boards);
    }
}

impl StatusFeed for BoardFeed {
    fn subscribe(&self) -> Vec<RepoFeed> {
        let boards = self.0.get().map(Vec::as_slice).unwrap_or_default();
        boards
            .iter()
            .map(|(repo, board)| {
                let (snapshot, events) = board.subscribe();
                RepoFeed { repo: repo.clone(), snapshot, events }
            })
            .collect()
    }
}

/// Where the control plane says how to reach it, inside `<data dir>/factory`.
pub const FILE: &str = "status.json";

/// How long a client waits to connect and to hear a snapshot, so a dead control plane's leftover file costs little.
const CONNECT: Duration = Duration::from_secs(2);

/// What `status.json` holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub addr: SocketAddr,
    pub token: String,
    pub pid: u32,
}

/// One repo's snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoStatus {
    pub repo: String,
    #[serde(flatten)]
    pub snapshot: FactorySnapshot,
}

/// The reply to `GET /status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusReply {
    pub repos: Vec<RepoStatus>,
}

/// One line of `GET /events`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoEvent {
    pub repo: String,
    #[serde(flatten)]
    pub event: Sequenced,
}

struct Shared {
    token: String,
    boards: Vec<(String, Arc<StatusBoard>)>,
}

/// A running status server. Dropping it stops serving and removes the file.
pub struct Serving {
    task: tokio::task::JoinHandle<()>,
    file: PathBuf,
    pub addr: SocketAddr,
}

impl Drop for Serving {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.file);
    }
}

/// Serves `boards` (a repo's slug and its board) on a loopback port and records where in `dir`.
pub async fn serve(dir: &Path, boards: Vec<(String, Arc<StatusBoard>)>) -> Result<Serving, String> {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).map_err(|e| format!("no randomness for the status token: {e}"))?;
    let token = hex::encode(bytes);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.map_err(|e| format!("status port: {e}"))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let file = dir.join(FILE);
    let endpoint = Endpoint { addr, token: token.clone(), pid: std::process::id() };
    write_private(&file, &serde_json::to_vec(&endpoint).map_err(|e| e.to_string())?)?;
    let app = Router::new()
        .route("/status", get(status))
        .route("/events", get(events))
        .with_state(Arc::new(Shared { token, boards }));
    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::warn!(error = %e, "the status server stopped");
        }
    });
    Ok(Serving { task, file, addr })
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let _ = std::fs::remove_file(path);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    file.write_all(bytes).map_err(|e| format!("{}: {e}", path.display()))
}

fn authorized(shared: &Shared, headers: &HeaderMap) -> bool {
    let given =
        headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer "));
    // Compared in full whatever the first mismatch, so the time taken says nothing about the token.
    given.is_some_and(|g| {
        g.len() == shared.token.len() && g.bytes().zip(shared.token.bytes()).fold(0u8, |a, (x, y)| a | (x ^ y)) == 0
    })
}

async fn status(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    if !authorized(&shared, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let repos = shared
        .boards
        .iter()
        .map(|(repo, board)| RepoStatus { repo: repo.clone(), snapshot: board.snapshot() })
        .collect();
    axum::Json(StatusReply { repos }).into_response()
}

#[derive(Deserialize)]
struct EventsQuery {
    /// Only the events about this task.
    task: Option<String>,
}

/// Newline-delimited JSON of every event from now on, for one task when asked.
async fn events(State(shared): State<Arc<Shared>>, headers: HeaderMap, Query(q): Query<EventsQuery>) -> Response {
    if !authorized(&shared, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let task = q.task.map(|t| t.to_ascii_uppercase());
    let streams = shared.boards.iter().map(|(repo, board)| {
        let (_, rx) = board.subscribe();
        let repo = repo.clone();
        futures_util::stream::unfold(rx, move |mut rx| {
            let repo = repo.clone();
            async move {
                loop {
                    match rx.recv().await {
                        Ok(event) => return Some((RepoEvent { repo, event }, rx)),
                        // A client too slow to keep up misses some, which it can see in the numbering.
                        Err(RecvError::Lagged(_)) => continue,
                        Err(RecvError::Closed) => return None,
                    }
                }
            }
        })
        .boxed()
    });
    let lines = futures_util::stream::select_all(streams)
        .filter(move |e| {
            let keep = match &task {
                Some(t) => e.event.event.task().is_some_and(|k| &k.0 == t),
                None => true,
            };
            std::future::ready(keep)
        })
        .map(|e| {
            let mut line = serde_json::to_vec(&e).unwrap_or_default();
            line.push(b'\n');
            Ok::<_, std::convert::Infallible>(Bytes::from(line))
        });
    Response::builder()
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .body(Body::from_stream(lines))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn read_endpoint(dir: &Path) -> Option<Endpoint> {
    serde_json::from_slice(&std::fs::read(dir.join(FILE)).ok()?).ok()
}

fn client(timeout: Option<Duration>) -> reqwest::Client {
    let mut b = reqwest::Client::builder().connect_timeout(CONNECT).no_proxy();
    if let Some(t) = timeout {
        b = b.timeout(t);
    }
    b.build().expect("an HTTP client")
}

/// The running control plane's snapshot, or `None` when there isn't one to reach (not running, or not answering).
pub async fn fetch(dir: &Path) -> Option<StatusReply> {
    let endpoint = read_endpoint(dir)?;
    let reply = client(Some(CONNECT * 2))
        .get(format!("http://{}/status", endpoint.addr))
        .bearer_auth(&endpoint.token)
        .send()
        .await
        .ok()?;
    if !reply.status().is_success() {
        return None;
    }
    reply.json().await.ok()
}

/// Calls `on_event` for each event the running control plane streams (about `task` when given) until it stops or
/// `stop` resolves. `Err` when there's no control plane to follow.
pub async fn follow(
    dir: &Path,
    task: Option<&str>,
    mut on_event: impl FnMut(RepoEvent),
    stop: impl std::future::Future<Output = ()>,
) -> Result<(), String> {
    let endpoint = read_endpoint(dir).ok_or("no control plane is running (no status file)")?;
    let mut req = client(None).get(format!("http://{}/events", endpoint.addr)).bearer_auth(&endpoint.token);
    if let Some(task) = task {
        req = req.query(&[("task", task)]);
    }
    let mut reply = req.send().await.map_err(|e| format!("the control plane doesn't answer: {e}"))?;
    if !reply.status().is_success() {
        return Err(format!("the control plane refused the stream: {}", reply.status()));
    }
    tokio::pin!(stop);
    let mut buf: Vec<u8> = Vec::new();
    loop {
        tokio::select! {
            _ = &mut stop => return Ok(()),
            chunk = reply.chunk() => match chunk.map_err(|e| e.to_string())? {
                Some(chunk) => {
                    buf.extend_from_slice(&chunk);
                    while let Some(at) = buf.iter().position(|b| *b == b'\n') {
                        let line: Vec<u8> = buf.drain(..=at).collect();
                        if let Ok(event) = serde_json::from_slice(&line) {
                            on_event(event);
                        }
                    }
                }
                None => return Err("the control plane stopped".into()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use trailhead_factory::{ControlState, RunPhase, TaskKey};

    use super::*;

    #[tokio::test]
    async fn a_client_reads_the_snapshot_and_follows_one_tasks_events() {
        let dir = tempfile::tempdir().unwrap();
        let board = StatusBoard::new();
        board.control(ControlState::Up);
        let serving = serve(dir.path(), vec![("a/b".into(), board.clone())]).await.unwrap();

        let reply = fetch(dir.path()).await.expect("a snapshot");
        assert_eq!(reply.repos[0].repo, "a/b");
        assert_eq!(reply.repos[0].snapshot.control, ControlState::Up);

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let path = dir.path().to_path_buf();
        let follower =
            tokio::spawn(
                async move { follow(&path, Some("trail-1"), |e| drop(tx.send(e)), std::future::pending()).await },
            );
        // The stream starts when the client subscribes, so give it a moment to get there.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let other = board.begin(&TaskKey("TRAIL-2".into()), RunPhase::Agent);
        let mine = board.begin(&TaskKey("TRAIL-1".into()), RunPhase::Agent);
        board.tool_call(&TaskKey("TRAIL-1".into()), "Bash", Some("ls"));
        let first = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
        assert!(first.event.event.line().contains("TRAIL-1 agent"), "{first:?}");
        let second = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
        assert!(second.event.event.line().contains("tool Bash ls"), "{second:?}");
        drop((other, mine));

        // Without the token the port says no.
        let endpoint = read_endpoint(dir.path()).unwrap();
        let bare = client(None).get(format!("http://{}/status", endpoint.addr)).send().await.unwrap();
        assert_eq!(bare.status(), 401);

        follower.abort();
        drop(serving);
        assert!(fetch(dir.path()).await.is_none(), "the file goes with the server");
    }

    #[tokio::test]
    async fn no_file_means_no_control_plane() {
        let dir = tempfile::tempdir().unwrap();
        assert!(fetch(dir.path()).await.is_none());
        assert!(follow(dir.path(), None, |_| {}, std::future::pending()).await.is_err());
    }
}
