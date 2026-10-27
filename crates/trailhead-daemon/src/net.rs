//! The daemon contract over iroh. The control plane dials each sandbox's daemon by an endpoint id it derives from
//! the sandbox name, so it needs no address book and can reconnect after a restart. One bidirectional stream
//! carries one request; frames are a 4-byte big-endian length and a JSON message.
//!
//! The other direction is the API tunnel: a daemon reaches Trailhead by dialing the control plane on
//! [`API_ALPN`], which forwards each stream to the server's loopback port. Only daemons this control plane started
//! get in, and the server still checks every request's bearer token.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use hmac::{Hmac, KeyInit, Mac};
use iroh::endpoint::{Connection, ReadExactError, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointId, SecretKey};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::Sha256;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, mpsc};
use trailhead_factory::{
    Compute, Daemon, DaemonSecrets, Error, EventSink, FromDaemon, PrepareRequest, Program, Result, RunReport,
    RunRequest, SandboxFile, SandboxId, ToDaemon,
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

/// A Sprite lets a raw IP through only after its own DNS has resolved it from an allowed name, and a policy change
/// resets that. iroh dials relays by IPs it resolved and cached earlier, so after a lockdown every relay reconnect
/// was refused. Looking the names up again through the system resolver re-allows their IPs.
pub async fn resolve_iroh_hosts() {
    for host in IROH_HOSTS {
        let host = host.trim_end_matches('.');
        if let Err(e) = tokio::net::lookup_host((host, 443)).await {
            tracing::warn!(%host, error = %e, "can't resolve an iroh host");
        }
    }
}

/// Keeps iroh's hosts resolved through the system resolver, in case the Sprite's allowance lapses with the DNS TTL.
pub fn keep_iroh_hosts_resolved() {
    tokio::spawn(async {
        loop {
            resolve_iroh_hosts().await;
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });
}

/// The daemon's side: accepts connections from the one control plane it was told about and serves its requests.
#[derive(Clone)]
pub struct DaemonProtocol {
    runner: Arc<Runner>,
    control_plane: EndpointId,
    holds: Option<Arc<SpriteHolds>>,
    endpoint: Option<Endpoint>,
}

impl std::fmt::Debug for DaemonProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonProtocol").field("control_plane", &self.control_plane).finish_non_exhaustive()
    }
}

impl DaemonProtocol {
    pub fn new(runner: Arc<Runner>, control_plane: EndpointId) -> DaemonProtocol {
        DaemonProtocol { runner, control_plane, holds: None, endpoint: None }
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
        let reply = match first {
            ToDaemon::Prepare(req) => match self.runner.prepare(&req).await {
                Ok(()) => FromDaemon::Prepared,
                Err(e) => FromDaemon::Failed(e),
            },
            ToDaemon::Run(req) => match self.serve_run(*req, &mut send, recv).await {
                Ok(report) => FromDaemon::Report(report),
                Err(e) => FromDaemon::Failed(e),
            },
            ToDaemon::Cancel => FromDaemon::Failed(Error::Other("cancel without a run".into())),
        };
        write_frame(&mut send, &reply).await?;
        send.finish().map_err(io_err)
    }

    async fn serve_run(&self, mut req: RunRequest, send: &mut SendStream, recv: RecvStream) -> Result<RunReport> {
        // Narrowing a Sprite's egress resets which relay IPs it lets through, so once the lockdown is confirmed, the
        // relay names are resolved again (see `resolve_iroh_hosts`) and iroh is told to reconnect.
        if !req.blocked_hosts.is_empty() {
            self.runner.workspace.confirm_blocked(&req.blocked_hosts).await?;
            req.blocked_hosts.clear();
            resolve_iroh_hosts().await;
            if let Some(ep) = &self.endpoint {
                ep.network_change().await;
            }
        }
        let cancel = Arc::new(Notify::new());
        let (events, mut rx) = mpsc::unbounded_channel();
        let runner = self.runner.clone();
        let mut run = tokio::spawn({
            let cancel = cancel.clone();
            async move { runner.run(req, events, cancel).await }
        });
        let mut incoming = frames::<ToDaemon>(recv);
        let mut control_open = true;
        let report = loop {
            tokio::select! {
                Some(ev) = rx.recv() => write_frame(send, &FromDaemon::Event(ev)).await?,
                msg = incoming.recv(), if control_open => match msg {
                    Some(Ok(ToDaemon::Cancel)) => cancel.notify_one(),
                    Some(Ok(other)) => tracing::warn!(?other, "unexpected message during a run"),
                    // The control plane went away: stop rather than work unobserved; it requeues the task.
                    Some(Err(_)) | None => {
                        control_open = false;
                        cancel.notify_one();
                    }
                },
                done = &mut run => break done.map_err(|e| Error::Other(format!("run panicked: {e}")))?,
            }
        };
        while let Ok(ev) = rx.try_recv() {
            write_frame(send, &FromDaemon::Event(ev)).await?;
        }
        report
    }
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
}

impl Launch {
    pub fn new(binary: Vec<u8>) -> Launch {
        Launch { binary: Arc::new(binary), dir: "/home/sprite/.trailhead".into(), workdir: "/home/sprite/work".into() }
    }
}

/// The control plane's side: starts daemons in sandboxes through `Compute` and talks to them over iroh.
pub struct RemoteDaemon {
    endpoint: Endpoint,
    master: [u8; 32],
    compute: Arc<dyn Compute>,
    launch: Launch,
    conns: Mutex<HashMap<SandboxId, Connection>>,
    cancels: Mutex<HashMap<SandboxId, mpsc::UnboundedSender<()>>>,
    peers: Peers,
}

impl RemoteDaemon {
    pub fn new(endpoint: Endpoint, master: [u8; 32], compute: Arc<dyn Compute>, launch: Launch) -> RemoteDaemon {
        RemoteDaemon {
            endpoint,
            master,
            compute,
            launch,
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
            args: vec!["--secrets".into(), secrets_path, "--workdir".into(), self.launch.workdir.clone()],
        }
    }

    async fn connection(&self, sandbox: &SandboxId, patience: Duration) -> Result<Connection> {
        if let Some(conn) = self.conns.lock().unwrap().get(sandbox).filter(|c| c.close_reason().is_none()) {
            return Ok(conn.clone());
        }
        let id = self.daemon_id(sandbox);
        self.peers.lock().unwrap().insert(id);
        let deadline = tokio::time::Instant::now() + patience;
        let mut wait = Duration::from_millis(250);
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

    async fn open(&self, sandbox: &SandboxId, patience: Duration) -> Result<(SendStream, RecvStream)> {
        let conn = self.connection(sandbox, patience).await?;
        conn.open_bi().await.map_err(io_err)
    }
}

#[async_trait]
impl Daemon for RemoteDaemon {
    /// Starts the daemon unless this control plane already has a live connection to it.
    async fn prepare(&self, sandbox: &SandboxId, req: PrepareRequest) -> Result<()> {
        let connected = self.conns.lock().unwrap().get(sandbox).is_some_and(|c| c.close_reason().is_none());
        if !connected {
            self.compute.start(sandbox, &self.program(sandbox)).await?;
        }
        let (mut send, mut recv) = self.open(sandbox, CONNECT_PATIENCE).await?;
        write_frame(&mut send, &ToDaemon::Prepare(req)).await?;
        match read_frame::<FromDaemon>(&mut recv).await? {
            Some(FromDaemon::Prepared) => Ok(()),
            Some(FromDaemon::Failed(e)) => Err(e),
            other => Err(io_err(format!("unexpected reply to prepare: {other:?}"))),
        }
    }

    async fn run(&self, sandbox: &SandboxId, req: RunRequest, events: EventSink) -> Result<RunReport> {
        // Registered before connecting, so a cancel that arrives while dialing is kept and sent once the run starts.
        let (cancel_tx, mut cancel_rx) = mpsc::unbounded_channel();
        self.cancels.lock().unwrap().insert(sandbox.clone(), cancel_tx);
        let opened = self.open(sandbox, Duration::from_secs(15)).await;
        let (mut send, recv) = match opened {
            Ok(streams) => streams,
            Err(e) => {
                self.cancels.lock().unwrap().remove(sandbox);
                return Err(e);
            }
        };
        write_frame(&mut send, &ToDaemon::Run(Box::new(req))).await?;
        let mut incoming = frames::<FromDaemon>(recv);
        let result = loop {
            tokio::select! {
                Some(()) = cancel_rx.recv() => write_frame(&mut send, &ToDaemon::Cancel).await?,
                msg = incoming.recv() => match msg {
                    Some(Ok(FromDaemon::Event(ev))) => {
                        let _ = events.send(ev);
                    }
                    Some(Ok(FromDaemon::Report(report))) => break Ok(report),
                    Some(Ok(FromDaemon::Failed(e))) => break Err(e),
                    Some(Ok(FromDaemon::Prepared)) => break Err(io_err("unexpected Prepared during a run")),
                    Some(Err(e)) => break Err(e),
                    None => break Err(io_err(format!("the daemon in {} went away", sandbox.0))),
                },
            }
        };
        self.cancels.lock().unwrap().remove(sandbox);
        result
    }

    async fn cancel(&self, sandbox: &SandboxId) -> Result<()> {
        if let Some(tx) = self.cancels.lock().unwrap().get(sandbox) {
            let _ = tx.send(());
        }
        Ok(())
    }
}
