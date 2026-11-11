//! The control plane's side of Lookout's transport (`docs/factory/ui.md#transport`): the `trailhead/ui/0` protocol on
//! the control plane's iroh endpoint, separate from the daemons' [`API_ALPN`](crate::API_ALPN) tunnel.
//!
//! Each stream opens with one framed [`UiRequest`] saying what it wants. [`UiRequest::Api`] turns the stream into a
//! byte pipe to the Trailhead server's loopback port, so the UI's HTTP client (and the `/v1/events/stream` event
//! stream) talks to a local port. [`UiRequest::Status`] answers with the factory status snapshot and then its stream
//! (TRAIL-692, TRAIL-701) as framed [`StatusFrame`]s, from whatever [`StatusFeed`] the control plane gave the protocol.
//! [`UiRequest::Logs`] answers with the control plane's last log lines and then each new one as framed [`LogFrame`]s, from
//! a [`LogFeed`] (TRAIL-885).
//!
//! Admission is a persisted [`UiAllowlist`] of endpoint ids a person added; it decides who can connect. What they can
//! do is decided by the user's bearer token, which the server checks on every forwarded request.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointId};
use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};
use trailhead_factory::logs::LogLine;
use trailhead_factory::{Error, FactorySnapshot, FactoryState, Result, Sequenced};

use crate::{io_err, parse_endpoint_id, pipe, read_frame, write_frame};

pub const UI_ALPN: &[u8] = b"trailhead/ui/0";

/// The application close code on a connection from an endpoint that isn't on the allowlist, so a UI can say so.
pub const CLOSE_NOT_ALLOWED: u32 = 1;

/// How long a UI waits to reach the control plane before a request fails.
const DIAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// The first frame on every stream a UI opens.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum UiRequest {
    /// The rest of the stream is raw bytes to and from the Trailhead server.
    Api,
    /// The stream carries [`StatusFrame`]s: a snapshot of every repo, then each event after it, until either side
    /// closes it. The control plane ends it when the UI falls behind and misses events, so a UI that sees it end
    /// redials for a fresh snapshot.
    Status,
    /// The stream carries [`LogFrame`]s: the control plane's last log lines, then each new one.
    Logs,
}

/// One repo's snapshot in a [`StatusFrame::Snapshot`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepoSnapshot {
    pub repo: String,
    #[serde(flatten)]
    pub snapshot: FactorySnapshot,
}

/// A frame the control plane sends on a [`UiRequest::Status`] stream.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "frame", rename_all = "snake_case")]
pub enum StatusFrame {
    /// Always first. Each repo's `seq` is the last event it includes. `control` says whether the server's control
    /// plane is running at all; a control plane from before it was added sends none.
    Snapshot {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        control: Option<FactoryState>,
        repos: Vec<RepoSnapshot>,
    },
    Event {
        repo: String,
        #[serde(flatten)]
        event: Sequenced,
    },
    /// The only frame on a stream whose request the control plane doesn't know, which is then closed.
    Unsupported(Unsupported),
}

/// What a control plane says to a request it can't parse: the request it got, the ones it knows, and its build, so a
/// newer UI can tell its factory is older.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Unsupported {
    /// The request as it arrived.
    pub got: serde_json::Value,
    /// The `op` of every request this control plane serves.
    pub known: Vec<String>,
    pub version: String,
    pub build: String,
}

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the control plane doesn't know the request {} (it serves {}; version {}, build {})",
            self.got,
            self.known.join(", "),
            self.version,
            self.build
        )
    }
}

/// The `op` of every [`UiRequest`], for [`Unsupported::known`].
const KNOWN_OPS: [&str; 3] = ["api", "status", "logs"];

/// One repo's status as the control plane keeps it: a snapshot and the events after it.
pub struct RepoFeed {
    pub repo: String,
    pub snapshot: FactorySnapshot,
    pub events: broadcast::Receiver<Sequenced>,
}

/// Where the control plane's status comes from, so this crate needn't know the control plane's types.
pub trait StatusFeed: Send + Sync + 'static {
    /// Every repo's snapshot with a receiver for the events after it.
    fn subscribe(&self) -> Vec<RepoFeed>;

    /// Whether the control plane is running at all, sent with the snapshot.
    fn state(&self) -> Option<FactoryState> {
        None
    }
}

impl std::fmt::Debug for dyn StatusFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StatusFeed")
    }
}

/// A frame the control plane sends on a [`UiRequest::Logs`] stream.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "frame", rename_all = "snake_case")]
pub enum LogFrame {
    /// Always first: the lines the control plane has kept, oldest first.
    Backlog { lines: Vec<LogLine> },
    /// One new line.
    Line(LogLine),
    /// What a control plane that doesn't know the request answers with (see [`StatusFrame::Unsupported`]).
    Unsupported(Unsupported),
}

/// Where the control plane's log comes from, so this crate needn't know the control plane's types.
pub trait LogFeed: Send + Sync + 'static {
    /// The lines kept so far, oldest first, and a receiver for each one after them.
    fn subscribe(&self) -> (Vec<LogLine>, broadcast::Receiver<LogLine>);
}

impl std::fmt::Debug for dyn LogFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LogFeed")
    }
}

/// Why a stream was reset: the first frame wasn't a request this control plane knows.
pub const BAD_REQUEST: u32 = 2;

/// Why an API stream was reset: the request was fine, but the control plane couldn't reach the Trailhead server.
/// Distinct from [`BAD_REQUEST`] so a UI can tell a server that's down from an op that's unknown.
pub const SERVER_UNREACHABLE: u32 = 3;

/// How often an open UI connection is checked against the allowlist, so a disallowed endpoint loses the streams it
/// already has open (the event and status feeds), not just new ones.
const RECHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

/// The endpoint ids allowed to use `trailhead/ui/0`, one per line in a file. It is read on every connection and
/// stream (on the blocking pool), and an open connection is rechecked every second, so `trailhead factory allow` and `disallow` take
/// effect without restarting the control plane.
#[derive(Debug, Clone)]
pub struct UiAllowlist {
    path: PathBuf,
}

impl UiAllowlist {
    pub fn new(path: PathBuf) -> UiAllowlist {
        UiAllowlist { path }
    }

    pub fn list(&self) -> Result<Vec<EndpointId>> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(parse_endpoint_id)
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(Error::Other(format!("{}: {e}", self.path.display()))),
        }
    }

    /// Whether `id` is listed. The file is read on the blocking pool so it never stalls an async worker; a list
    /// that can't be read allows no one.
    pub async fn contains(&self, id: &EndpointId) -> bool {
        let this = self.clone();
        let listed =
            tokio::task::spawn_blocking(move || this.list()).await.unwrap_or_else(|e| Err(Error::Other(e.to_string())));
        match listed {
            Ok(ids) => ids.contains(id),
            Err(e) => {
                tracing::error!(error = %e, "can't read the UI allowlist; refusing");
                false
            }
        }
    }

    /// Whether `id` was newly added.
    pub fn add(&self, id: EndpointId) -> Result<bool> {
        let _lock = self.lock()?;
        let mut ids = self.list()?;
        if ids.contains(&id) {
            return Ok(false);
        }
        ids.push(id);
        self.save(&ids)?;
        Ok(true)
    }

    /// Whether `id` was there to remove.
    pub fn remove(&self, id: &EndpointId) -> Result<bool> {
        let _lock = self.lock()?;
        let mut ids = self.list()?;
        let before = ids.len();
        ids.retain(|i| i != id);
        if ids.len() == before {
            return Ok(false);
        }
        self.save(&ids)?;
        Ok(true)
    }

    /// Hold an exclusive lock on a sibling file for a read-modify-write, so edits from concurrent processes
    /// can't lose each other's entries. The lock is on a separate file because `save` replaces the list's inode.
    /// It is released when the returned file is dropped.
    fn lock(&self) -> Result<std::fs::File> {
        let fail = |e: std::io::Error| Error::Other(format!("{}: {e}", self.path.display()));
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(fail)?;
        }
        let mut name = self.path.as_os_str().to_owned();
        name.push(".lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(PathBuf::from(name))
            .map_err(fail)?;
        file.lock().map_err(fail)?;
        Ok(file)
    }

    fn save(&self, ids: &[EndpointId]) -> Result<()> {
        let fail = |e: std::io::Error| Error::Other(format!("{}: {e}", self.path.display()));
        let text: String = ids.iter().map(|i| format!("{i}\n")).collect();
        // Write beside, then rename, so a crash can't leave half a list. Callers hold the lock.
        let tmp = self.path.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&tmp, text).map_err(fail)?;
        std::fs::rename(&tmp, &self.path).map_err(fail)
    }
}

/// The control plane's end of `trailhead/ui/0`.
#[derive(Debug, Clone)]
pub struct UiProtocol {
    server: SocketAddr,
    allowed: Arc<UiAllowlist>,
    status: Option<Arc<dyn StatusFeed>>,
    logs: Option<Arc<dyn LogFeed>>,
    /// The build reported to a UI that sent a request this control plane doesn't know.
    build: String,
    /// The (peer, request kind) pairs already logged as unknown, so a UI retrying doesn't fill the log.
    unknown: Arc<std::sync::Mutex<std::collections::HashSet<(EndpointId, String)>>>,
}

impl UiProtocol {
    pub fn new(server: SocketAddr, allowed: UiAllowlist) -> UiProtocol {
        UiProtocol {
            server,
            allowed: Arc::new(allowed),
            status: None,
            logs: None,
            build: "unknown".into(),
            unknown: Arc::default(),
        }
    }

    /// Sets the build reported with an unknown request: [`build_id`](crate::build_id) of the executable.
    pub fn build(mut self, build: String) -> UiProtocol {
        self.build = build;
        self
    }

    /// Serves [`UiRequest::Status`] from `feed`; without one that request is refused.
    pub fn with_status(mut self, feed: Arc<dyn StatusFeed>) -> UiProtocol {
        self.status = Some(feed);
        self
    }

    /// Serves [`UiRequest::Logs`] from `feed`; without one that request is refused.
    pub fn with_logs(mut self, feed: Arc<dyn LogFeed>) -> UiProtocol {
        self.logs = Some(feed);
        self
    }
}

/// Sends the snapshot, then every event, until the UI goes away or the feed ends.
async fn serve_status(feed: &dyn StatusFeed, mut send: iroh::endpoint::SendStream) {
    let feeds = feed.subscribe();
    let repos = feeds.iter().map(|f| RepoSnapshot { repo: f.repo.clone(), snapshot: f.snapshot.clone() }).collect();
    if write_frame(&mut send, &StatusFrame::Snapshot { control: feed.state(), repos }).await.is_err() {
        return;
    }
    let (tx, mut rx) = mpsc::channel(256);
    // Dropped with this function, which stops the forwarding.
    let mut forwards = tokio::task::JoinSet::new();
    for RepoFeed { repo, mut events, .. } in feeds {
        let tx = tx.clone();
        forwards.spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => {
                        if tx.send(Some(StatusFrame::Event { repo: repo.clone(), event })).await.is_err() {
                            return;
                        }
                    }
                    // Too slow to keep up: it missed events and the UI's mirror can't repair a gap, so
                    // end the stream and let the UI redial for a fresh snapshot.
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let _ = tx.send(None).await;
                        return;
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });
    }
    drop(tx);
    loop {
        tokio::select! {
            frame = rx.recv() => match frame {
                Some(Some(frame)) => {
                    if write_frame(&mut send, &frame).await.is_err() {
                        return;
                    }
                }
                // A forward lagged, or every feed ended.
                Some(None) | None => break,
            },
            _ = send.stopped() => return,
        }
    }
    let _ = send.finish();
}

/// Sends the kept lines, then every new one, until the UI goes away.
async fn serve_logs(feed: &dyn LogFeed, mut send: iroh::endpoint::SendStream) {
    let (lines, mut live) = feed.subscribe();
    if write_frame(&mut send, &LogFrame::Backlog { lines }).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            line = live.recv() => match line {
                Ok(line) => {
                    if write_frame(&mut send, &LogFrame::Line(line)).await.is_err() {
                        return;
                    }
                }
                // Too slow to keep up: it misses some lines.
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = send.stopped() => return,
        }
    }
    let _ = send.finish();
}

impl UiProtocol {
    /// Closes `conn` if its peer has left the allowlist since it connected; says whether it did.
    async fn close_if_disallowed(&self, conn: &Connection) -> bool {
        if self.allowed.contains(&conn.remote_id()).await {
            return false;
        }
        tracing::warn!(peer = %conn.remote_id(), "closing a UI connection from an endpoint that was disallowed");
        conn.close(CLOSE_NOT_ALLOWED.into(), b"not allowed");
        true
    }
}

impl ProtocolHandler for UiProtocol {
    async fn accept(&self, conn: Connection) -> std::result::Result<(), AcceptError> {
        if !self.allowed.contains(&conn.remote_id()).await {
            tracing::warn!(peer = %conn.remote_id(), "refusing a UI connection from an endpoint that isn't allowed");
            conn.close(CLOSE_NOT_ALLOWED.into(), b"not allowed");
            return Ok(());
        }
        let mut recheck = tokio::time::interval(RECHECK_EVERY);
        loop {
            let (mut send, mut recv) = tokio::select! {
                stream = conn.accept_bi() => match stream {
                    Ok(stream) => stream,
                    Err(_) => break,
                },
                _ = recheck.tick() => {
                    if self.close_if_disallowed(&conn).await {
                        return Ok(());
                    }
                    continue;
                }
            };
            if self.close_if_disallowed(&conn).await {
                return Ok(());
            }
            let (server, status, logs) = (self.server, self.status.clone(), self.logs.clone());
            let (peer, build, unknown) = (conn.remote_id(), self.build.clone(), self.unknown.clone());
            tokio::spawn(async move {
                let request = match read_frame::<serde_json::Value>(&mut recv).await {
                    Ok(Some(raw)) => match serde_json::from_value::<UiRequest>(raw.clone()) {
                        Ok(request) => Ok(Some(request)),
                        Err(e) => Err((raw, e.to_string())),
                    },
                    Ok(None) => Ok(None),
                    Err(e) => {
                        tracing::warn!(error = %e, "bad UI request");
                        let _ = send.reset(BAD_REQUEST.into());
                        return;
                    }
                };
                match request {
                    Ok(Some(UiRequest::Api)) => match TcpStream::connect(server).await {
                        Ok(tcp) => pipe(send, recv, tcp).await,
                        Err(e) => {
                            tracing::warn!(%server, error = %e, "can't reach the Trailhead server");
                            let _ = send.reset(SERVER_UNREACHABLE.into());
                        }
                    },
                    Ok(Some(UiRequest::Status)) => match status {
                        Some(feed) => serve_status(feed.as_ref(), send).await,
                        None => {
                            let _ = send.reset(BAD_REQUEST.into());
                        }
                    },
                    Ok(Some(UiRequest::Logs)) => match logs {
                        Some(feed) => serve_logs(feed.as_ref(), send).await,
                        None => {
                            let _ = send.reset(BAD_REQUEST.into());
                        }
                    },
                    Ok(None) => {}
                    Err((got, error)) => {
                        let kind = got.get("op").and_then(|op| op.as_str()).unwrap_or("(no op)").to_string();
                        let first = unknown.lock().unwrap().insert((peer, kind.clone()));
                        if first {
                            tracing::warn!(
                                %peer, request = %kind, %error, version = env!("CARGO_PKG_VERSION"), %build,
                                "a UI sent a request this factory doesn't know; it is probably newer"
                            );
                        }
                        let known = KNOWN_OPS.iter().map(|op| op.to_string()).collect();
                        let reply = Unsupported { got, known, version: env!("CARGO_PKG_VERSION").into(), build };
                        if write_frame(&mut send, &StatusFrame::Unsupported(reply)).await.is_ok() {
                            let _ = send.finish();
                            // Held until the UI has read it, so closing the connection doesn't cut the frame short.
                            let _ = send.stopped().await;
                        } else {
                            let _ = send.reset(BAD_REQUEST.into());
                        }
                    }
                }
            });
        }
        Ok(())
    }
}

/// Dials the control plane's `trailhead/ui/0`, giving up after a while.
pub async fn dial_ui(endpoint: &Endpoint, control_plane: EndpointId) -> Result<Connection> {
    let dial = endpoint.connect(control_plane, UI_ALPN);
    tokio::time::timeout(DIAL_TIMEOUT, dial)
        .await
        .map_err(|_| Error::Other("dialing the control plane timed out".into()))?
        .map_err(io_err)
}

/// A UI's end of a [`UiRequest::Status`] stream.
pub struct StatusStream {
    recv: iroh::endpoint::RecvStream,
    // Held so the connection lives as long as the stream is read.
    _conn: Connection,
}

impl StatusStream {
    /// Asks for the status on a new stream. A control plane with none to serve resets it, which shows up as an error
    /// from [`next`](Self::next).
    pub async fn open(conn: Connection) -> Result<StatusStream> {
        let (mut send, recv) = conn.open_bi().await.map_err(io_err)?;
        write_frame(&mut send, &UiRequest::Status).await?;
        Ok(StatusStream { recv, _conn: conn })
    }

    /// The next frame, or `None` when the control plane ended the stream cleanly.
    pub async fn next(&mut self) -> Result<Option<StatusFrame>> {
        read_frame(&mut self.recv).await
    }
}

/// A UI's end of a [`UiRequest::Logs`] stream.
pub struct LogStream {
    recv: iroh::endpoint::RecvStream,
    // Held so the connection lives as long as the stream is read.
    _conn: Connection,
}

impl LogStream {
    /// Asks for the log on a new stream. A control plane without the op resets it, which shows up as an error from
    /// [`next`](Self::next), or answers [`LogFrame::Unsupported`].
    pub async fn open(conn: Connection) -> Result<LogStream> {
        let (mut send, recv) = conn.open_bi().await.map_err(io_err)?;
        write_frame(&mut send, &UiRequest::Logs).await?;
        Ok(LogStream { recv, _conn: conn })
    }

    /// The next frame, or `None` when the control plane ended the stream cleanly.
    pub async fn next(&mut self) -> Result<Option<LogFrame>> {
        read_frame(&mut self.recv).await
    }
}

/// A UI's end of the API forwarding: a loopback port whose connections reach the Trailhead server through the control
/// plane, each as a stream on one long-lived connection, redialed if it drops.
pub async fn open_ui_tunnel(endpoint: Endpoint, control_plane: EndpointId) -> Result<SocketAddr> {
    let listener = TcpListener::bind("127.0.0.1:0").await.map_err(io_err)?;
    let addr = listener.local_addr().map_err(io_err)?;
    let conn: Arc<tokio::sync::Mutex<Option<Connection>>> = Arc::default();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let (endpoint, conn) = (endpoint.clone(), conn.clone());
            tokio::spawn(async move {
                let streams = async {
                    let mut guard = conn.lock().await;
                    let live = guard.as_ref().filter(|c| c.close_reason().is_none()).cloned();
                    let c = match live {
                        Some(c) => c,
                        None => {
                            // Bounded, so a dial to a control plane that has gone away doesn't hold the lock (and every
                            // request behind it) until QUIC gives up.
                            let dial = endpoint.connect(control_plane, UI_ALPN);
                            let c = tokio::time::timeout(DIAL_TIMEOUT, dial)
                                .await
                                .map_err(|_| {
                                    Error::Other("daemon connection: dialing the control plane timed out".into())
                                })?
                                .map_err(io_err)?;
                            *guard = Some(c.clone());
                            c
                        }
                    };
                    drop(guard);
                    let (mut send, recv) = c.open_bi().await.map_err(io_err)?;
                    write_frame(&mut send, &UiRequest::Api).await?;
                    Ok::<_, Error>((send, recv))
                };
                match streams.await {
                    Ok((send, recv)) => pipe(send, recv, tcp).await,
                    Err(e) => tracing::warn!(error = %e, "UI tunnel to the control plane failed"),
                }
            });
        }
    });
    Ok(addr)
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;

    use super::*;

    #[tokio::test]
    async fn the_allowlist_persists_and_dedupes() {
        let dir = tempfile::tempdir().unwrap();
        let list = UiAllowlist::new(dir.path().join("factory").join("ui-allowed"));
        let (a, b) = (SecretKey::generate().public(), SecretKey::generate().public());
        assert!(list.list().unwrap().is_empty());
        assert!(list.add(a).unwrap());
        assert!(!list.add(a).unwrap());
        assert!(list.add(b).unwrap());
        let reread = UiAllowlist::new(dir.path().join("factory").join("ui-allowed"));
        assert_eq!(reread.list().unwrap(), vec![a, b]);
        assert!(reread.remove(&a).unwrap());
        assert!(!reread.remove(&a).unwrap());
        assert!(!reread.contains(&a).await && reread.contains(&b).await);
    }

    #[test]
    fn concurrent_adds_keep_every_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("factory").join("ui-allowed");
        let ids: Vec<_> = (0..16).map(|_| SecretKey::generate().public()).collect();
        let threads: Vec<_> = ids
            .iter()
            .map(|id| {
                let (list, id) = (UiAllowlist::new(path.clone()), *id);
                std::thread::spawn(move || list.add(id).unwrap())
            })
            .collect();
        for t in threads {
            assert!(t.join().unwrap());
        }
        let mut got = UiAllowlist::new(path).list().unwrap();
        let mut want = ids;
        got.sort();
        want.sort();
        assert_eq!(got, want);
    }

    #[test]
    fn requests_are_tagged_so_status_can_join_them() {
        assert_eq!(serde_json::to_string(&UiRequest::Api).unwrap(), r#"{"op":"api"}"#);
        assert_eq!(serde_json::to_string(&UiRequest::Status).unwrap(), r#"{"op":"status"}"#);
        assert_eq!(serde_json::to_string(&UiRequest::Logs).unwrap(), r#"{"op":"logs"}"#);
    }

    #[test]
    fn known_ops_are_the_requests_variants() {
        for request in [UiRequest::Api, UiRequest::Status, UiRequest::Logs] {
            let op = serde_json::to_value(&request).unwrap()["op"].as_str().unwrap().to_string();
            assert!(KNOWN_OPS.contains(&op.as_str()), "{op}");
        }
        assert_eq!(KNOWN_OPS.len(), 3);
    }

    #[test]
    fn status_frames_round_trip() {
        use trailhead_factory::{ControlState, FactoryEvent, TaskKey};
        let snapshot = FactorySnapshot {
            control: ControlState::Up,
            last_error: None,
            sandboxes: vec![],
            runs: vec![],
            holding: None,
            targets: Vec::new(),
            human_grades: Vec::new(),
            queue: Vec::new(),
            usage: Vec::new(),
            seq: 4,
        };
        let frames = [
            StatusFrame::Snapshot {
                control: None,
                repos: vec![RepoSnapshot { repo: "a/b".into(), snapshot: snapshot.clone() }],
            },
            StatusFrame::Snapshot {
                control: Some(FactoryState::Starting { reason: Some("keyring locked".into()) }),
                repos: vec![RepoSnapshot { repo: "a/b".into(), snapshot }],
            },
            StatusFrame::Event {
                repo: "a/b".into(),
                event: Sequenced { seq: 5, event: FactoryEvent::RunEnded { task: TaskKey("TRAIL-1".into()) } },
            },
            StatusFrame::Unsupported(Unsupported {
                got: serde_json::json!({"op": "later"}),
                known: vec!["api".into()],
                version: "0.1.0".into(),
                build: "abc".into(),
            }),
        ];
        for frame in frames {
            let text = serde_json::to_string(&frame).unwrap();
            assert_eq!(serde_json::from_str::<StatusFrame>(&text).unwrap(), frame, "{text}");
        }
    }
}
