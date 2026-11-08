//! The factory status surface (`docs/factory/ui.md#factory-status`): the control plane's in-memory snapshot and event
//! stream, as one [`Feed`] behind `trailhead/ui/0`'s status request and the server's `/v1/factory` routes
//! ([`routes`]), which `status` and `logs --follow` read.
//!
//! The stream carries tool calls, which can name commands and paths, so the server mounts the routes
//! behind the user's token (agent tokens, a sandbox's included, are refused).

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use trailhead_control::StatusBoard;
use trailhead_factory::{FactorySnapshot, FactoryState, Sequenced};
use trailhead_wire::ui::{RepoFeed, StatusFeed};

/// The control plane's state and each repo's board. It exists before the repos do, so a client asking early sees the
/// state (`starting`, say) and no repos.
#[derive(Clone)]
pub struct Feed(Arc<Inner>);

struct Inner {
    state: Mutex<FactoryState>,
    boards: Mutex<Boards>,
}

/// A repo's slug and its board.
type Boards = Vec<(String, Arc<StatusBoard>)>;

impl Feed {
    pub fn new(state: FactoryState) -> Feed {
        Feed(Arc::new(Inner { state: Mutex::new(state), boards: Mutex::default() }))
    }

    pub fn set_state(&self, state: FactoryState) {
        *self.0.state.lock().unwrap_or_else(|e| e.into_inner()) = state;
    }

    pub fn state(&self) -> FactoryState {
        self.0.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_boards(&self, boards: Boards) {
        *self.0.boards.lock().unwrap_or_else(|e| e.into_inner()) = boards;
    }

    pub fn boards(&self) -> Boards {
        self.0.boards.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The state and every repo's snapshot.
    pub fn reply(&self) -> StatusReply {
        let repos = self
            .boards()
            .iter()
            .map(|(repo, board)| RepoStatus { repo: repo.clone(), snapshot: board.snapshot() })
            .collect();
        StatusReply { control: Some(self.state()), repos }
    }
}

impl StatusFeed for Feed {
    fn subscribe(&self) -> Vec<RepoFeed> {
        self.boards()
            .iter()
            .map(|(repo, board)| {
                let (snapshot, events) = board.subscribe();
                RepoFeed { repo: repo.clone(), snapshot, events }
            })
            .collect()
    }

    fn state(&self) -> Option<FactoryState> {
        Some(Feed::state(self))
    }
}

/// How long a client waits to connect and to hear a snapshot.
const CONNECT: Duration = Duration::from_secs(2);

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
    /// Whether the control plane runs at all; a server from before this was added sends none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<FactoryState>,
    pub repos: Vec<RepoStatus>,
}

/// One line of `GET /events`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoEvent {
    pub repo: String,
    #[serde(flatten)]
    pub event: Sequenced,
}

/// `GET /status` and `GET /events` for `feed`, with no authentication of their own: the server mounts them under
/// `/v1/factory` behind its bearer token.
pub fn routes(feed: Feed) -> Router {
    Router::new().route("/status", get(status)).route("/events", get(events)).with_state(feed)
}

async fn status(State(feed): State<Feed>) -> Response {
    axum::Json(feed.reply()).into_response()
}

#[derive(Deserialize)]
struct EventsQuery {
    /// Only the events about this task.
    task: Option<String>,
}

/// Newline-delimited JSON of every event from now on, for one task when asked.
async fn events(State(feed): State<Feed>, Query(q): Query<EventsQuery>) -> Response {
    let task = q.task.map(|t| t.to_ascii_uppercase());
    let streams = feed.boards().into_iter().map(|(repo, board)| {
        let (_, rx) = board.subscribe();
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

/// Where `status` and `logs --follow` reach the control plane: the server's `/v1/factory`.
#[derive(Debug, Clone)]
pub struct Remote {
    base: String,
    token: String,
}

impl Remote {
    /// The server at `addr`, with the user's token.
    pub fn server(addr: SocketAddr, token: String) -> Remote {
        Remote { base: format!("http://{addr}/v1/factory"), token }
    }
}

/// The server's snapshot, when it runs a control plane (one that is off or disabled doesn't count).
pub async fn find(server: Remote) -> Option<(Remote, StatusReply)> {
    let reply = fetch(&server).await?;
    (!matches!(reply.control, None | Some(FactoryState::Off | FactoryState::Disabled))).then_some((server, reply))
}

fn client(timeout: Option<Duration>) -> reqwest::Client {
    let mut b = reqwest::Client::builder().connect_timeout(CONNECT).no_proxy();
    if let Some(t) = timeout {
        b = b.timeout(t);
    }
    b.build().expect("an HTTP client")
}

/// `remote`'s snapshot, or `None` when it doesn't answer with one.
pub async fn fetch(remote: &Remote) -> Option<StatusReply> {
    let reply = client(Some(CONNECT * 2))
        .get(format!("{}/status", remote.base))
        .bearer_auth(&remote.token)
        .send()
        .await
        .ok()?;
    if !reply.status().is_success() {
        return None;
    }
    reply.json().await.ok()
}

/// Calls `on_event` for each event `remote` streams (about `task` when given) until it stops or `stop` resolves.
/// `Err` when it doesn't answer.
pub async fn follow(
    remote: &Remote,
    task: Option<&str>,
    mut on_event: impl FnMut(RepoEvent),
    stop: impl std::future::Future<Output = ()>,
) -> Result<(), String> {
    let mut req = client(None).get(format!("{}/events", remote.base)).bearer_auth(&remote.token);
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

    async fn server(feed: Feed) -> Remote {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().nest("/v1/factory", routes(feed));
        tokio::spawn(async move { axum::serve(listener, app).await });
        Remote::server(addr, "t".into())
    }

    #[tokio::test]
    async fn a_client_reads_the_snapshot_and_follows_one_tasks_events() {
        let board = StatusBoard::new();
        board.control(ControlState::Up);
        let feed = Feed::new(FactoryState::Running);
        feed.set_boards(vec![("a/b".into(), board.clone())]);
        let remote = server(feed).await;

        let (_, reply) = find(remote.clone()).await.expect("a snapshot");
        assert_eq!(reply.control, Some(FactoryState::Running));
        assert_eq!(reply.repos[0].repo, "a/b");
        assert_eq!(reply.repos[0].snapshot.control, ControlState::Up);

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let following = remote.clone();
        let follower = tokio::spawn(async move {
            follow(&following, Some("trail-1"), |e| drop(tx.send(e)), std::future::pending()).await
        });
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
        follower.abort();
    }

    #[tokio::test]
    async fn a_server_without_a_running_control_plane_has_none_to_ask() {
        let feed = Feed::new(FactoryState::Disabled);
        let remote = server(feed.clone()).await;
        assert!(find(remote.clone()).await.is_none());

        feed.set_state(FactoryState::Starting { reason: Some("keyring locked".into()) });
        let (_, reply) = find(remote).await.unwrap();
        assert_eq!(reply.control, Some(FactoryState::Starting { reason: Some("keyring locked".into()) }));
    }
}
