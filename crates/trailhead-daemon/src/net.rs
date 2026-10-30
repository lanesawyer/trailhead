//! The daemon contract over iroh. The control plane dials each sandbox's daemon by an endpoint id it derives from
//! the sandbox name, so it needs no address book and can reconnect after a restart. One bidirectional stream
//! carries one request; frames are a 4-byte big-endian length and a JSON message.
//!
//! The other direction is the API tunnel: a daemon reaches Trailhead by dialing the control plane on
//! [`API_ALPN`], which forwards each stream to the server's loopback port. Only daemons this control plane started
//! get in, and the server still checks every request's bearer token.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use hmac::{Hmac, KeyInit, Mac};
use iroh::endpoint::{Connection, ReadExactError, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointId, SecretKey};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, mpsc, watch};
use trailhead_factory::{
    Compute, Daemon, DaemonSecrets, DaemonVersion, Error, EventSink, FromDaemon, PrepareRequest, Program, Result,
    RunEvent, RunReport, RunRequest, SandboxFile, SandboxId, ToDaemon,
};

use crate::Runner;
use crate::awake::SpriteHolds;

pub const ALPN: &[u8] = b"trailhead/daemon/0";

/// Daemon to control plane: a byte stream to the Trailhead server.
pub const API_ALPN: &[u8] = b"trailhead/api/0";

/// The daemons allowed through the API tunnel: the ones this control plane started or dialed.
pub type Peers = Arc<Mutex<HashSet<EndpointId>>>;

const MAX_FRAME: usize = 16 * 1024 * 1024;

/// How long the control plane keeps dialing a daemon that was just started: it has to boot and publish its address.
const CONNECT_PATIENCE: Duration = Duration::from_secs(90);

/// How long it keeps dialing a running daemon, which may need to resume from a pause and rebuild its connections.
const RECONNECT_PATIENCE: Duration = Duration::from_secs(60);

/// How many times one run or prepare may lose its connection before the control plane gives up on it.
const MAX_REATTACH: u32 = 10;

/// How long a daemon keeps a run going with no control plane following it before cancelling it. Covers QUIC noticing
/// the drop on both sides plus the control plane's [`RECONNECT_PATIENCE`].
pub const ATTACH_PATIENCE: Duration = Duration::from_secs(5 * 60);

/// A sandbox daemon's iroh key, derived from the control plane's master key and the sandbox name.
pub fn sandbox_key(master: &[u8; 32], sandbox: &SandboxId) -> SecretKey {
    let mut mac = Hmac::<Sha256>::new_from_slice(master).expect("HMAC takes any key length");
    mac.update(b"trailhead daemon key\0");
    mac.update(sandbox.0.as_bytes());
    SecretKey::from_bytes(&mac.finalize().into_bytes().into())
}

pub fn parse_secret_key(hex_key: &str) -> Result<SecretKey> {
    let bytes: [u8; 32] = hex::decode(hex_key.trim())
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| Error::Config("secret_key must be 32 bytes of hex".into()))?;
    Ok(SecretKey::from_bytes(&bytes))
}

pub fn parse_endpoint_id(hex_id: &str) -> Result<EndpointId> {
    hex_id.trim().parse().map_err(|e| Error::Config(format!("endpoint id {hex_id}: {e}")))
}

/// The identity of a daemon binary: the hex SHA-256 of its bytes.
pub fn build_id(binary: &[u8]) -> String {
    hex::encode(Sha256::digest(binary))
}

fn io_err(e: impl std::fmt::Display) -> Error {
    Error::Other(format!("daemon connection: {e}"))
}

pub async fn write_frame<T: Serialize>(send: &mut SendStream, msg: &T) -> Result<()> {
    let body = serde_json::to_vec(msg).map_err(io_err)?;
    let len = u32::try_from(body.len()).map_err(io_err)?;
    send.write_all(&len.to_be_bytes()).await.map_err(io_err)?;
    send.write_all(&body).await.map_err(io_err)
}

/// The next frame, or `None` when the peer finished the stream cleanly.
pub async fn read_frame<T: DeserializeOwned>(recv: &mut RecvStream) -> Result<Option<T>> {
    let mut len = [0u8; 4];
    match recv.read_exact(&mut len).await {
        Ok(_) => {}
        Err(ReadExactError::FinishedEarly(0)) => return Ok(None),
        Err(e) => return Err(io_err(e)),
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(io_err(format!("frame of {len} bytes")));
    }
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body).await.map_err(io_err)?;
    serde_json::from_slice(&body).map(Some).map_err(io_err)
}

/// Reads frames on their own task, since `read_frame` isn't safe to cancel inside a `select!`.
fn frames<T: DeserializeOwned + Send + 'static>(mut recv: RecvStream) -> mpsc::UnboundedReceiver<Result<T>> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            match read_frame(&mut recv).await {
                Ok(Some(msg)) => {
                    if tx.send(Ok(msg)).is_err() {
                        return;
                    }
                }
                Ok(None) => return,
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            }
        }
    });
    rx
}

/// Hosts iroh reaches by name: n0's relays and its DNS discovery.
const IROH_HOSTS: &[&str] = &[
    iroh::defaults::prod::NA_EAST_RELAY_HOSTNAME,
    iroh::defaults::prod::NA_WEST_RELAY_HOSTNAME,
    iroh::defaults::prod::EU_RELAY_HOSTNAME,
    iroh::defaults::prod::AP_RELAY_HOSTNAME,
    "dns.iroh.link",
];

/// Other hosts runs need, such as the model API, kept resolved alongside iroh's (`RunRequest::allowed_hosts`).
static KEEP_RESOLVED: Mutex<std::collections::BTreeSet<String>> = Mutex::new(std::collections::BTreeSet::new());

/// A Sprite lets a raw IP through only after its own DNS has resolved it from an allowed name, and a policy change
/// resets that. iroh dials relays by IPs it resolved and cached earlier, so after a lockdown every relay reconnect
/// was refused. Looking the names up again through the system resolver re-allows their IPs.
pub async fn resolve_iroh_hosts() {
    let extra: Vec<String> = KEEP_RESOLVED.lock().unwrap().iter().cloned().collect();
    for host in IROH_HOSTS.iter().map(|h| h.to_string()).chain(extra) {
        let host = host.trim_end_matches('.');
        if let Err(e) = tokio::net::lookup_host((host, 443)).await {
            tracing::warn!(%host, error = %e, "can't resolve an iroh host");
        }
    }
}

/// Well inside QUIC's 30-second idle timeout, so a relay connection cut by an egress change comes back in time.
pub const RESOLVE_EVERY: Duration = Duration::from_secs(10);

/// Keeps iroh connectable: its hosts are re-resolved every [`RESOLVE_EVERY`], since any egress change resets which
/// relay IPs the Sprite allows, and can land before a run request reaches the daemon (TRAIL-105's attempt 4 lost its
/// connection that way). They're also re-resolved right away after the Sprite resumes from a pause, which drops every
/// connection. A resume shows as the
/// wall clock jumping between ticks; then iroh is told the network changed so it rebuilds its relay connection.
pub fn watch_network(endpoint: Endpoint) {
    tokio::spawn(async move {
        let tick = Duration::from_secs(5);
        let mut last = std::time::SystemTime::now();
        let mut since_resolve = Duration::MAX;
        loop {
            let now = std::time::SystemTime::now();
            let gap = now.duration_since(last).unwrap_or_default();
            let resumed = gap > tick * 3;
            if resumed || since_resolve >= RESOLVE_EVERY {
                resolve_iroh_hosts().await;
                since_resolve = Duration::ZERO;
            }
            if resumed {
                tracing::info!(paused_for = ?gap, "resumed from a pause; reconnecting");
                endpoint.network_change().await;
            }
            last = now;
            tokio::time::sleep(tick).await;
            since_resolve = since_resolve.saturating_add(tick);
        }
    });
}

/// A run on the daemon, which goes on whether or not a control plane is following it.
struct LiveRun {
    log: watch::Sender<RunLog>,
    cancel: Arc<Notify>,
    /// Counts the streams that have followed the run, so a detached stream's watchdog can tell if another came.
    followers: AtomicU64,
    /// The latest heartbeat's quiet seconds. Heartbeats stay out of the replay log: one replayed after a reattach would
    /// be stale, so the connected follower gets them live and a new follower waits for the next.
    beat: watch::Sender<u64>,
}

impl LiveRun {
    fn record(&self, ev: RunEvent) {
        match ev {
            RunEvent::Heartbeat { quiet_secs } => {
                self.beat.send_replace(quiet_secs);
            }
            ev => self.log.send_modify(|l| l.events.push(ev)),
        }
    }
}

/// Everything a run has produced, kept so a control plane that reattaches can pick up where it lost the stream.
#[derive(Default)]
struct RunLog {
    events: Vec<RunEvent>,
    result: Option<Result<RunReport>>,
}

/// The daemon's side: accepts connections from the one control plane it was told about and serves its requests.
#[derive(Clone)]
pub struct DaemonProtocol {
    runner: Arc<Runner>,
    control_plane: EndpointId,
    holds: Option<Arc<SpriteHolds>>,
    endpoint: Option<Endpoint>,
    runs: Arc<Mutex<HashMap<String, Arc<LiveRun>>>>,
    preparing: Arc<Mutex<Option<Preparing>>>,
    attach_patience: Duration,
    version: DaemonVersion,
}

/// The latest prepare, so a control plane that lost its connection and asks again joins it rather than racing it in
/// the same checkout.
struct Preparing {
    req: PrepareRequest,
    done: watch::Receiver<Option<Result<()>>>,
}

impl std::fmt::Debug for DaemonProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonProtocol").field("control_plane", &self.control_plane).finish_non_exhaustive()
    }
}

impl DaemonProtocol {
    pub fn new(runner: Arc<Runner>, control_plane: EndpointId) -> DaemonProtocol {
        DaemonProtocol {
            runner,
            control_plane,
            holds: None,
            endpoint: None,
            runs: Arc::default(),
            preparing: Arc::default(),
            attach_patience: ATTACH_PATIENCE,
            version: DaemonVersion { version: env!("CARGO_PKG_VERSION").into(), build: "unknown".into() },
        }
    }

    /// Sets the build the daemon reports: [`build_id`] of its own executable.
    pub fn build(mut self, build: String) -> DaemonProtocol {
        self.version.build = build;
        self
    }

    /// How long a run may go unfollowed before it's cancelled; [`ATTACH_PATIENCE`] unless a test wants less.
    pub fn attach_patience(mut self, patience: Duration) -> DaemonProtocol {
        self.attach_patience = patience;
        self
    }

    /// The daemon's own endpoint, to tell iroh when a lockdown has cut its connections.
    pub fn on(mut self, endpoint: Endpoint) -> DaemonProtocol {
        self.endpoint = Some(endpoint);
        self
    }

    /// Holds the Sprite awake while a request is in flight.
    pub fn holding(mut self, holds: Arc<SpriteHolds>) -> DaemonProtocol {
        self.holds = Some(holds);
        self
    }

    async fn serve_stream(&self, mut send: SendStream, mut recv: RecvStream) -> Result<()> {
        let Some(first) = read_frame::<ToDaemon>(&mut recv).await? else { return Ok(()) };
        let _hold = self.holds.as_ref().map(|h| h.hold());
        let (live, seen) = match first {
            ToDaemon::Prepare(req) => {
                let reply = match self.prepare(req).await {
                    Ok(()) => FromDaemon::Prepared,
                    Err(e) => FromDaemon::Failed(e),
                };
                return reply_with(&mut send, &reply).await;
            }
            ToDaemon::Run(req) => match self.start(*req).await {
                Ok(live) => (live, 0),
                Err(e) => return reply_with(&mut send, &FromDaemon::Failed(e)).await,
            },
            ToDaemon::Attach { run, seen } => match self.find(&run) {
                Some(live) => {
                    tracing::info!(%run, seen, "control plane reattached");
                    (live, seen)
                }
                None => {
                    let e = Error::NotFound(format!("run {run} on this daemon"));
                    return reply_with(&mut send, &FromDaemon::Failed(e)).await;
                }
            },
            ToDaemon::Version => return reply_with(&mut send, &FromDaemon::Version(self.version.clone())).await,
            ToDaemon::Cancel => {
                return reply_with(&mut send, &FromDaemon::Failed(Error::Other("cancel without a run".into()))).await;
            }
        };
        let follower = live.followers.fetch_add(1, Ordering::SeqCst) + 1;
        // The result stays in the run's log, so if this reply doesn't arrive, a reattach gets it again.
        let followed = match self.follow(&live, seen, &mut send, recv).await {
            Ok(result) => reply_with(&mut send, &result.map_or_else(FromDaemon::Failed, FromDaemon::Report)).await,
            Err(e) => Err(e),
        };
        if followed.is_err() {
            self.watch_detached(live, follower);
        }
        followed
    }

    /// Prepares on its own task, so a dropped connection doesn't stop it. The same request while it's still going
    /// waits for it instead of starting another.
    async fn prepare(&self, req: PrepareRequest) -> Result<()> {
        let mut done = {
            let mut preparing = self.preparing.lock().unwrap();
            match preparing.as_ref().filter(|p| p.req == req && p.done.borrow().is_none()) {
                Some(p) => {
                    tracing::info!("joining the prepare in flight");
                    p.done.clone()
                }
                None => {
                    let (tx, rx) = watch::channel(None);
                    let runner = self.runner.clone();
                    let hold = self.holds.as_ref().map(|h| h.hold());
                    tokio::spawn({
                        let req = req.clone();
                        async move {
                            let _hold = hold;
                            let _ = tx.send(Some(runner.prepare(&req).await));
                        }
                    });
                    *preparing = Some(Preparing { req, done: rx.clone() });
                    rx
                }
            }
        };
        let finished = done.wait_for(Option::is_some).await.map_err(|_| Error::Other("prepare panicked".into()))?;
        finished.clone().expect("waited for a result")
    }

    fn find(&self, run: &str) -> Option<Arc<LiveRun>> {
        self.runs.lock().unwrap().get(run).cloned()
    }

    /// Starts a run on its own task, so it outlives the stream that asked for it.
    async fn start(&self, mut req: RunRequest) -> Result<Arc<LiveRun>> {
        if self.runs.lock().unwrap().contains_key(&req.run) {
            return Err(Error::Conflict(format!("run {} already started; attach to it", req.run)));
        }
        // Narrowing a Sprite's egress resets which relay IPs it lets through, so once the lockdown is confirmed, the
        // relay names are resolved again (see `resolve_iroh_hosts`) and iroh is told to reconnect.
        KEEP_RESOLVED.lock().unwrap().extend(req.allowed_hosts.iter().cloned());
        if !req.blocked_hosts.is_empty() {
            self.runner.workspace.confirm_blocked(&req.blocked_hosts).await?;
            req.blocked_hosts.clear();
            resolve_iroh_hosts().await;
            if let Some(ep) = &self.endpoint {
                ep.network_change().await;
            }
        }
        let live = Arc::new(LiveRun {
            log: watch::Sender::new(RunLog::default()),
            cancel: Arc::default(),
            followers: AtomicU64::new(0),
            beat: watch::Sender::new(0),
        });
        {
            let mut runs = self.runs.lock().unwrap();
            // A sandbox runs one step at a time, so once a new run starts, nobody will ask for a finished one.
            runs.retain(|_, r| r.log.borrow().result.is_none());
            runs.insert(req.run.clone(), live.clone());
        }
        let (events, mut rx) = mpsc::unbounded_channel();
        let runner = self.runner.clone();
        let hold = self.holds.as_ref().map(|h| h.hold());
        let mut run = tokio::spawn({
            let cancel = live.cancel.clone();
            async move { runner.run(req, events, cancel).await }
        });
        tokio::spawn({
            let live = live.clone();
            async move {
                let _hold = hold;
                let result = loop {
                    tokio::select! {
                        Some(ev) = rx.recv() => live.record(ev),
                        done = &mut run => break done.unwrap_or_else(|e| Err(Error::Other(format!("run panicked: {e}")))),
                    }
                };
                live.log.send_modify(|l| {
                    while let Ok(ev) = rx.try_recv() {
                        if !matches!(ev, RunEvent::Heartbeat { .. }) {
                            l.events.push(ev);
                        }
                    }
                    l.result = Some(result);
                });
            }
        });
        Ok(live)
    }

    /// Streams a run's events after the first `seen`, passing on cancels, until it finishes. Fails if the control
    /// plane goes away; the run carries on regardless.
    async fn follow(
        &self,
        live: &LiveRun,
        seen: u64,
        send: &mut SendStream,
        recv: RecvStream,
    ) -> Result<Result<RunReport>> {
        let mut seen = usize::try_from(seen).unwrap_or(usize::MAX);
        let mut log = live.log.subscribe();
        let mut beat = live.beat.subscribe();
        let mut incoming = frames::<ToDaemon>(recv);
        loop {
            let (new, result) = {
                let l = log.borrow_and_update();
                (l.events.get(seen..).unwrap_or_default().to_vec(), l.result.clone())
            };
            for ev in new {
                write_frame(send, &FromDaemon::Event(ev)).await?;
                seen += 1;
            }
            if let Some(result) = result {
                return Ok(result);
            }
            tokio::select! {
                _ = log.changed() => {}
                Ok(()) = beat.changed() => {
                    let quiet_secs = *beat.borrow_and_update();
                    write_frame(send, &FromDaemon::Event(RunEvent::Heartbeat { quiet_secs })).await?;
                }
                msg = incoming.recv() => match msg {
                    Some(Ok(ToDaemon::Cancel)) => live.cancel.notify_one(),
                    Some(Ok(other)) => tracing::warn!(?other, "unexpected message during a run"),
                    Some(Err(e)) => return Err(e),
                    None => return Err(io_err("the control plane went away")),
                },
            }
        }
    }

    /// After a stream following a run drops, cancels the run unless a control plane reattaches in time, so an
    /// abandoned run doesn't go on working unobserved.
    fn watch_detached(&self, live: Arc<LiveRun>, follower: u64) {
        let patience = self.attach_patience;
        tokio::spawn(async move {
            tokio::time::sleep(patience).await;
            let unfinished = live.log.borrow().result.is_none();
            if unfinished && live.followers.load(Ordering::SeqCst) == follower {
                tracing::warn!(?patience, "no control plane reattached to the run; cancelling it");
                live.cancel.notify_one();
            }
        });
    }
}

async fn reply_with(send: &mut SendStream, reply: &FromDaemon) -> Result<()> {
    write_frame(send, reply).await?;
    send.finish().map_err(io_err)
}

impl ProtocolHandler for DaemonProtocol {
    async fn accept(&self, conn: Connection) -> std::result::Result<(), AcceptError> {
        if conn.remote_id() != self.control_plane {
            tracing::warn!(peer = %conn.remote_id(), "refusing a connection from an unknown peer");
            conn.close(1u32.into(), b"unknown peer");
            return Ok(());
        }
        loop {
            let (send, recv) = match conn.accept_bi().await {
                Ok(streams) => streams,
                Err(_) => return Ok(()),
            };
            let this = self.clone();
            tokio::spawn(async move {
                if let Err(e) = this.serve_stream(send, recv).await {
                    tracing::warn!(error = %e, "daemon stream failed");
                }
            });
        }
    }
}

/// Copies bytes both ways between an iroh stream and a TCP connection until both sides finish.
async fn pipe(mut send: SendStream, mut recv: RecvStream, tcp: TcpStream) {
    let (mut tcp_read, mut tcp_write) = tcp.into_split();
    let inbound = async {
        let _ = tokio::io::copy(&mut recv, &mut tcp_write).await;
        let _ = tcp_write.shutdown().await;
    };
    let outbound = async {
        let _ = tokio::io::copy(&mut tcp_read, &mut send).await;
        let _ = send.finish();
    };
    tokio::join!(inbound, outbound);
}

/// The control plane's end of the API tunnel.
#[derive(Debug, Clone)]
pub struct ApiTunnel {
    server: SocketAddr,
    peers: Peers,
}

impl ApiTunnel {
    pub fn new(server: SocketAddr, peers: Peers) -> ApiTunnel {
        ApiTunnel { server, peers }
    }
}

impl ProtocolHandler for ApiTunnel {
    async fn accept(&self, conn: Connection) -> std::result::Result<(), AcceptError> {
        if !self.peers.lock().unwrap().contains(&conn.remote_id()) {
            tracing::warn!(peer = %conn.remote_id(), "refusing an API tunnel from an unknown peer");
            conn.close(1u32.into(), b"unknown peer");
            return Ok(());
        }
        while let Ok((send, recv)) = conn.accept_bi().await {
            let server = self.server;
            tokio::spawn(async move {
                match TcpStream::connect(server).await {
                    Ok(tcp) => pipe(send, recv, tcp).await,
                    Err(e) => tracing::warn!(%server, error = %e, "can't reach the Trailhead server"),
                }
            });
        }
        Ok(())
    }
}

/// The daemon's end of the API tunnel: a loopback port whose connections reach the Trailhead server, each as a
/// stream on one long-lived connection to the control plane, redialed if it drops.
pub async fn open_api_tunnel(endpoint: Endpoint, control_plane: EndpointId) -> Result<SocketAddr> {
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
                            let c = endpoint.connect(control_plane, API_ALPN).await.map_err(io_err)?;
                            *guard = Some(c.clone());
                            c
                        }
                    };
                    drop(guard);
                    c.open_bi().await.map_err(io_err)
                };
                match streams.await {
                    Ok((send, recv)) => pipe(send, recv, tcp).await,
                    Err(e) => tracing::warn!(error = %e, "API tunnel to the control plane failed"),
                }
            });
        }
    });
    Ok(addr)
}

/// Where the daemon lives inside a sandbox and how it's started.
#[derive(Debug, Clone)]
pub struct Launch {
    /// The daemon binary for the sandbox's platform.
    pub binary: Arc<Vec<u8>>,
    pub dir: String,
    pub workdir: String,
    /// A `tracing` filter for the daemon's log, such as `info,iroh=debug`.
    pub log: Option<String>,
    /// Subscription mode: the Claude Code OAuth token handed to each daemon in its secrets file.
    pub claude_oauth_token: Option<String>,
}

impl Launch {
    pub fn new(binary: Vec<u8>) -> Launch {
        Launch {
            binary: Arc::new(binary),
            dir: "/home/sprite/.trailhead".into(),
            workdir: "/home/sprite/work".into(),
            log: None,
            claude_oauth_token: None,
        }
    }
}

/// The control plane's side: starts daemons in sandboxes through `Compute` and talks to them over iroh.
pub struct RemoteDaemon {
    endpoint: Endpoint,
    master: [u8; 32],
    compute: Arc<dyn Compute>,
    launch: Launch,
    /// [`build_id`] of `launch.binary`, hashed once here rather than on every prepare.
    build: String,
    conns: Mutex<HashMap<SandboxId, Connection>>,
    cancels: Mutex<HashMap<SandboxId, mpsc::UnboundedSender<()>>>,
    peers: Peers,
}

impl RemoteDaemon {
    pub fn new(endpoint: Endpoint, master: [u8; 32], compute: Arc<dyn Compute>, launch: Launch) -> RemoteDaemon {
        let build = build_id(&launch.binary);
        RemoteDaemon {
            endpoint,
            master,
            compute,
            launch,
            build,
            conns: Mutex::default(),
            cancels: Mutex::default(),
            peers: Peers::default(),
        }
    }

    pub fn daemon_id(&self, sandbox: &SandboxId) -> EndpointId {
        sandbox_key(&self.master, sandbox).public()
    }

    /// The daemons let through the API tunnel; share it with [`ApiTunnel`].
    pub fn peers(&self) -> Peers {
        self.peers.clone()
    }

    /// The program that runs a sandbox's daemon, with its secrets file.
    pub fn program(&self, sandbox: &SandboxId) -> Program {
        let secrets = DaemonSecrets {
            secret_key: hex::encode(sandbox_key(&self.master, sandbox).to_bytes()),
            control_plane: self.endpoint.id().to_string(),
            claude_oauth_token: self.launch.claude_oauth_token.clone(),
        };
        let dir = &self.launch.dir;
        let bin = format!("{dir}/trailhead-daemon");
        let secrets_path = format!("{dir}/secrets.json");
        Program {
            name: "trailhead-daemon".into(),
            files: vec![
                SandboxFile { path: bin.clone(), contents: self.launch.binary.to_vec(), mode: 0o755 },
                SandboxFile {
                    path: secrets_path.clone(),
                    contents: serde_json::to_vec(&secrets).expect("secrets serialize"),
                    mode: 0o600,
                },
            ],
            cmd: bin,
            args: {
                let mut args = vec!["--secrets".into(), secrets_path, "--workdir".into(), self.launch.workdir.clone()];
                if let Some(log) = &self.launch.log {
                    args.extend(["--log".into(), log.clone()]);
                }
                args
            },
        }
    }

    async fn connection(&self, sandbox: &SandboxId, patience: Duration) -> Result<Connection> {
        if let Some(conn) = self.conns.lock().unwrap().get(sandbox).filter(|c| c.close_reason().is_none()) {
            return Ok(conn.clone());
        }
        let deadline = tokio::time::Instant::now() + patience;
        let mut wait = Duration::from_millis(250);
        // The sandbox may have paused since the last request, and a dial won't wake it. The compute API can blip
        // as well, so a failed wake is retried like a failed dial, unless retrying can't help (any error but `Other`).
        loop {
            // Bounded by the deadline, since a hanging compute API plus its own retries would outlast the patience.
            let woke = tokio::time::timeout_at(deadline, self.compute.wake(sandbox))
                .await
                .unwrap_or_else(|_| Err(Error::Other(format!("waking {} timed out", sandbox.0))));
            match woke {
                Ok(()) => break,
                Err(e @ Error::Other(_)) if tokio::time::Instant::now() < deadline => {
                    tracing::warn!(sandbox = %sandbox.0, error = %e, "couldn't wake the sandbox yet");
                    tokio::time::sleep(wait).await;
                    wait = (wait * 2).min(Duration::from_secs(5));
                }
                Err(e) => return Err(e),
            }
        }
        let id = self.daemon_id(sandbox);
        self.peers.lock().unwrap().insert(id);
        wait = Duration::from_millis(250);
        let conn = loop {
            match self.endpoint.connect(id, ALPN).await {
                Ok(conn) => break conn,
                Err(e) if tokio::time::Instant::now() < deadline => {
                    tracing::debug!(sandbox = %sandbox.0, error = %e, "daemon not reachable yet");
                    tokio::time::sleep(wait).await;
                    wait = (wait * 2).min(Duration::from_secs(5));
                }
                Err(e) => return Err(Error::Other(format!("can't reach the daemon in {}: {e}", sandbox.0))),
            }
        };
        self.conns.lock().unwrap().insert(sandbox.clone(), conn.clone());
        Ok(conn)
    }

    /// The version the daemon in the sandbox reports, or `None` when it predates version reporting (or hung up).
    async fn running_version(&self, sandbox: &SandboxId) -> Result<Option<DaemonVersion>> {
        let (mut send, mut recv) = self.open(sandbox, CONNECT_PATIENCE).await?;
        write_frame(&mut send, &ToDaemon::Version).await?;
        Ok(match read_frame::<FromDaemon>(&mut recv).await {
            Ok(Some(FromDaemon::Version(v))) => Some(v),
            _ => None,
        })
    }

    /// Makes sure the daemon that runs is the binary this control plane uploads. A sandbox restored from a
    /// checkpoint can come back with the previous build's process still running, which would silently drop
    /// the fields of any newer request. A stale daemon is restarted onto the current binary; one still stale
    /// after that is an error.
    async fn ensure_current(&self, sandbox: &SandboxId) -> Result<()> {
        let want = self.build.as_str();
        let running = self.running_version(sandbox).await?;
        if let Some(v) = running.as_ref().filter(|v| v.build == want) {
            tracing::info!(sandbox = %sandbox.0, version = %v.version, build = %v.build, "daemon version");
            return Ok(());
        }
        tracing::warn!(
            sandbox = %sandbox.0,
            running = ?running.as_ref().map(|v| (&v.version, &v.build)),
            %want,
            "the daemon in the sandbox is not the current build; restarting it"
        );
        self.disconnect(sandbox);
        self.compute.start(sandbox, &self.program(sandbox)).await?;
        match self.running_version(sandbox).await? {
            Some(v) if v.build == want => {
                tracing::info!(sandbox = %sandbox.0, version = %v.version, build = %v.build, "daemon restarted onto the current build");
                Ok(())
            }
            other => Err(Error::Other(format!(
                "the daemon in {} is still not the current build after a restart (running {:?}, want {want})",
                sandbox.0,
                other.map(|v| v.build)
            ))),
        }
    }

    async fn open(&self, sandbox: &SandboxId, patience: Duration) -> Result<(SendStream, RecvStream)> {
        let conn = self.connection(sandbox, patience).await?;
        conn.open_bi().await.map_err(io_err)
    }
    /// Starts a run and follows it to its report. A dropped connection doesn't end the run: the daemon keeps it
    /// going, and this redials and reattaches, picking up the events after the last one it saw.
    async fn follow(
        &self,
        sandbox: &SandboxId,
        run: String,
        mut hello: ToDaemon,
        events: &EventSink,
        cancels: &mut mpsc::UnboundedReceiver<()>,
    ) -> Result<RunReport> {
        let mut seen = 0u64;
        let mut cancelled = false;
        let mut reattached = 0;
        loop {
            let (mut send, recv) = self.open(sandbox, RECONNECT_PATIENCE).await?;
            let mut sent = write_frame(&mut send, &hello).await;
            if cancelled && sent.is_ok() {
                sent = write_frame(&mut send, &ToDaemon::Cancel).await;
            }
            let lost = match sent {
                Err(e) => e,
                Ok(()) => {
                    if reattached > 0 {
                        tracing::info!(sandbox = %sandbox.0, %run, seen, "reattached to the run");
                    }
                    let mut incoming = frames::<FromDaemon>(recv);
                    loop {
                        tokio::select! {
                            Some(()) = cancels.recv() => {
                                cancelled = true;
                                if let Err(e) = write_frame(&mut send, &ToDaemon::Cancel).await {
                                    break e;
                                }
                            }
                            msg = incoming.recv() => match msg {
                                Some(Ok(FromDaemon::Event(ev))) => {
                                    // Heartbeats aren't in the daemon's log, so they don't count towards `seen`.
                                    if !matches!(ev, RunEvent::Heartbeat { .. }) {
                                        seen += 1;
                                    }
                                    let _ = events.send(ev);
                                }
                                Some(Ok(FromDaemon::Report(report))) => return Ok(report),
                                Some(Ok(FromDaemon::Failed(e))) => return Err(e),
                                Some(Ok(FromDaemon::Prepared | FromDaemon::Version(_))) => {
                                    return Err(io_err("unexpected reply during a run"));
                                }
                                Some(Err(e)) => break e,
                                None => break io_err(format!("the daemon in {} went away", sandbox.0)),
                            },
                        }
                    }
                }
            };
            reattached += 1;
            if reattached > MAX_REATTACH {
                return Err(io_err(format!("lost run {run} in {} {MAX_REATTACH} times: {lost}", sandbox.0)));
            }
            tracing::warn!(sandbox = %sandbox.0, %run, seen, error = %lost, "lost the daemon mid-run; reattaching");
            self.disconnect(sandbox);
            hello = ToDaemon::Attach { run: run.clone(), seen };
        }
    }

    fn disconnect(&self, sandbox: &SandboxId) {
        if let Some(conn) = self.conns.lock().unwrap().remove(sandbox) {
            conn.close(0u32.into(), b"reattaching");
        }
    }
}

#[async_trait]
impl Daemon for RemoteDaemon {
    /// Starts the daemon unless this control plane already has a live connection to it. A connection that drops
    /// while the daemon prepares is redialed and the request sent again; the daemon joins the prepare in flight.
    async fn prepare(&self, sandbox: &SandboxId, req: PrepareRequest) -> Result<()> {
        let connected = self.conns.lock().unwrap().get(sandbox).is_some_and(|c| c.close_reason().is_none());
        if !connected {
            self.compute.start(sandbox, &self.program(sandbox)).await?;
        }
        self.ensure_current(sandbox).await?;
        let mut patience = CONNECT_PATIENCE;
        let mut lost = 0;
        loop {
            let (mut send, mut recv) = self.open(sandbox, patience).await?;
            let reply = match write_frame(&mut send, &ToDaemon::Prepare(req.clone())).await {
                Ok(()) => read_frame::<FromDaemon>(&mut recv).await,
                Err(e) => Err(e),
            };
            let error = match reply {
                Ok(Some(FromDaemon::Prepared)) => return Ok(()),
                Ok(Some(FromDaemon::Failed(e))) => return Err(e),
                Ok(Some(other)) => return Err(io_err(format!("unexpected reply to prepare: {other:?}"))),
                Ok(None) => io_err(format!("the daemon in {} went away", sandbox.0)),
                Err(e) => e,
            };
            lost += 1;
            if lost > MAX_REATTACH {
                return Err(io_err(format!("lost the prepare in {} {MAX_REATTACH} times: {error}", sandbox.0)));
            }
            tracing::warn!(sandbox = %sandbox.0, %error, "lost the daemon while it prepared; asking again");
            self.disconnect(sandbox);
            patience = RECONNECT_PATIENCE;
        }
    }

    async fn run(&self, sandbox: &SandboxId, req: RunRequest, events: EventSink) -> Result<RunReport> {
        // Registered before connecting, so a cancel that arrives while dialing is kept and sent once the run starts.
        let (cancel_tx, mut cancel_rx) = mpsc::unbounded_channel();
        self.cancels.lock().unwrap().insert(sandbox.clone(), cancel_tx);
        let result = self.follow(sandbox, req.run.clone(), ToDaemon::Run(Box::new(req)), &events, &mut cancel_rx).await;
        self.cancels.lock().unwrap().remove(sandbox);
        result
    }

    async fn attach(&self, sandbox: &SandboxId, run: &str, events: EventSink) -> Result<RunReport> {
        let (cancel_tx, mut cancel_rx) = mpsc::unbounded_channel();
        self.cancels.lock().unwrap().insert(sandbox.clone(), cancel_tx);
        let hello = ToDaemon::Attach { run: run.to_string(), seen: 0 };
        let result = self.follow(sandbox, run.to_string(), hello, &events, &mut cancel_rx).await;
        self.cancels.lock().unwrap().remove(sandbox);
        result
    }

    async fn forget(&self, sandbox: &SandboxId) {
        if let Some(conn) = self.conns.lock().unwrap().remove(sandbox) {
            conn.close(0u32.into(), b"restarted");
        }
    }

    async fn cancel(&self, sandbox: &SandboxId) -> Result<()> {
        if let Some(tx) = self.cancels.lock().unwrap().get(sandbox) {
            let _ = tx.send(());
        }
        Ok(())
    }

    async fn shutdown(&self) {
        for tx in self.cancels.lock().unwrap().values() {
            let _ = tx.send(());
        }
    }
}
