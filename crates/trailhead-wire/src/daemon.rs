//! The control plane's side of the daemon contract ([`ALPN`]): starts daemons in sandboxes through `Compute` and
//! talks to them over iroh.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use iroh::Endpoint;
use iroh::EndpointId;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use tokio::sync::mpsc;
use trailhead_factory::{
    Compute, Daemon, DaemonSecrets, DaemonVersion, Error, EventSink, FromDaemon, HarnessSecrets, InstallHost,
    PrepareRequest, Program, Result, RunEvent, RunReport, RunRequest, SandboxFile, SandboxId, Target, ToDaemon,
};

use crate::{ALPN, Peers, build_id, frames, io_err, read_frame, sandbox_key, write_frame};

/// How long the control plane keeps dialing a daemon that was just started: it has to boot and publish its address.
const CONNECT_PATIENCE: Duration = Duration::from_secs(90);

/// How long it keeps dialing a running daemon, which may need to resume from a pause and rebuild its connections.
const RECONNECT_PATIENCE: Duration = Duration::from_secs(60);

/// How long the control plane waits for the daemon a restored container starts by itself before starting one.
const RESTORE_PATIENCE: Duration = Duration::from_secs(20);

/// How many times one run or prepare may lose its connection before the control plane gives up on it.
const MAX_REATTACH: u32 = 10;

/// How long a daemon keeps a run going with no control plane following it before cancelling it. Covers QUIC noticing
/// the drop on both sides plus the control plane's [`RECONNECT_PATIENCE`].
pub const ATTACH_PATIENCE: Duration = Duration::from_secs(5 * 60);

/// Lets an `Arc<Vec<u8>>` back a [`Bytes`], so each program shares the binary rather than copying it.
struct SharedBinary(Arc<Vec<u8>>);

impl AsRef<[u8]> for SharedBinary {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Where the daemon lives inside a sandbox and how it's started.
#[derive(Debug, Clone)]
pub struct Launch {
    /// The daemon binary for the sandbox's platform.
    pub binary: Bytes,
    pub dir: String,
    pub workdir: String,
    /// A `tracing` filter for the daemon's log, such as `info,iroh=debug`.
    pub log: Option<String>,
    /// The `adapter` each daemon builds its harness from.
    pub harness: String,
    /// That harness's credentials (subscription mode's token, say), handed to each daemon in its secrets file.
    pub harness_secrets: HarnessSecrets,
    /// The connector list URL for each daemon's secrets.
    pub gateway_list: String,
    /// The socket each daemon holds its sandbox awake through, if the compute pauses idle ones.
    pub hold_socket: Option<String>,
}

impl Launch {
    pub fn new(binary: impl Into<Arc<Vec<u8>>>, target: &Target, harness: impl Into<String>) -> Launch {
        Launch {
            binary: Bytes::from_owner(SharedBinary(binary.into())),
            dir: target.dir.clone(),
            workdir: target.workdir.clone(),
            log: None,
            harness: harness.into(),
            harness_secrets: HarnessSecrets::new(),
            gateway_list: target.gateway_list.clone(),
            hold_socket: target.hold_socket.clone(),
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
    /// Sandboxes restored since the daemon was last started in them: their image has a daemon that starts with the
    /// container, which is probably the current build with the secrets it was last started with.
    restored: Mutex<HashSet<SandboxId>>,
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
            restored: Mutex::default(),
            cancels: Mutex::default(),
            peers: Peers::default(),
        }
    }

    /// Lets through the same peers as another target's daemon, so one [`ApiTunnel`] serves them all.
    pub fn with_peers(mut self, peers: Peers) -> RemoteDaemon {
        self.peers = peers;
        self
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
            harness: self.launch.harness.clone(),
            harness_secrets: self.launch.harness_secrets.clone(),
            gateway_list: self.launch.gateway_list.clone(),
        };
        let dir = &self.launch.dir;
        let bin = format!("{dir}/trailhead-daemon");
        let secrets_path = format!("{dir}/secrets.json");
        Program {
            name: "trailhead-daemon".into(),
            files: vec![
                SandboxFile { path: bin.clone(), contents: self.launch.binary.clone(), mode: 0o755 },
                SandboxFile {
                    path: secrets_path.clone(),
                    contents: serde_json::to_vec(&secrets).expect("secrets serialize").into(),
                    mode: 0o600,
                },
            ],
            cmd: bin,
            args: {
                let mut args = vec!["--secrets".into(), secrets_path, "--workdir".into(), self.launch.workdir.clone()];
                if let Some(log) = &self.launch.log {
                    args.extend(["--log".into(), log.clone()]);
                }
                if let Some(socket) = &self.launch.hold_socket {
                    args.extend(["--hold-socket".into(), socket.clone()]);
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
        // as well, so a failed wake is retried like a failed dial, unless retrying can't help (any error that isn't transient).
        loop {
            // Bounded by the deadline, since a hanging compute API plus its own retries would outlast the patience.
            let woke = tokio::time::timeout_at(deadline, self.compute.wake(sandbox))
                .await
                .unwrap_or_else(|_| Err(Error::Other(format!("waking {} timed out", sandbox.0))));
            match woke {
                Ok(()) => break,
                Err(e) if e.is_transient() && tokio::time::Instant::now() < deadline => {
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
    async fn running_version(&self, sandbox: &SandboxId, patience: Duration) -> Result<Option<DaemonVersion>> {
        let (mut send, mut recv) = self.open(sandbox, patience).await?;
        write_frame(&mut send, &ToDaemon::Version).await?;
        Ok(match read_frame::<FromDaemon>(&mut recv).await {
            Ok(Some(FromDaemon::Version(v))) => Some(v),
            _ => None,
        })
    }

    /// Starts the daemon unless this control plane already has a live connection to it, and makes sure it's the
    /// current build.
    async fn start_current(&self, sandbox: &SandboxId) -> Result<()> {
        let connected = self.conns.lock().unwrap().get(sandbox).is_some_and(|c| c.close_reason().is_none());
        // A restored container starts the daemon in its image by itself. When that is the current build, starting
        // another would only upload the same files, kill it and wait for its replacement to publish its address.
        if !connected && self.restored.lock().unwrap().remove(sandbox) {
            // The daemon reads its secrets file once, at start: reuse it only when that file is what we'd write now.
            let secrets = self.program(sandbox).files.get(1).map(|f| build_id(&f.contents)).unwrap_or_default();
            match self.running_version(sandbox, RESTORE_PATIENCE).await {
                Ok(Some(v)) if v.build == self.build && v.secrets == secrets => {
                    tracing::info!(sandbox = %sandbox.0, version = %v.version, build = %v.build, "daemon version (started with the restored sandbox)");
                    return Ok(());
                }
                other => {
                    tracing::info!(sandbox = %sandbox.0, found = ?other.map(|v| v.map(|v| (v.build, v.secrets == secrets))), "the restored sandbox has no current daemon with the current secrets; starting one");
                    self.disconnect(sandbox);
                }
            }
        }
        if !connected {
            self.compute.start(sandbox, &self.program(sandbox)).await?;
        }
        self.ensure_current(sandbox).await
    }

    /// Makes sure the daemon that runs is the binary this control plane uploads. A sandbox restored from a
    /// checkpoint can come back with the previous build's process still running, which would silently drop
    /// the fields of any newer request. A stale daemon is restarted onto the current binary; one still stale
    /// after that is an error.
    async fn ensure_current(&self, sandbox: &SandboxId) -> Result<()> {
        let want = self.build.as_str();
        let running = self.running_version(sandbox, CONNECT_PATIENCE).await?;
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
        match self.running_version(sandbox, CONNECT_PATIENCE).await? {
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
                                Some(Ok(
                                FromDaemon::Prepared
                                | FromDaemon::InstallHosts(_)
                                | FromDaemon::Version(_)
                                | FromDaemon::Unpublished(_),
                            )) => {
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
    /// Asks the daemon which hosts the install needs, starting it first as `prepare` does.
    async fn install_hosts(&self, sandbox: &SandboxId, req: &PrepareRequest) -> Result<Vec<InstallHost>> {
        self.start_current(sandbox).await?;
        let mut patience = CONNECT_PATIENCE;
        let mut lost = 0;
        loop {
            let (mut send, mut recv) = self.open(sandbox, patience).await?;
            let reply = match write_frame(&mut send, &ToDaemon::InstallHosts(req.clone())).await {
                Ok(()) => read_frame::<FromDaemon>(&mut recv).await,
                Err(e) => Err(e),
            };
            let error = match reply {
                Ok(Some(FromDaemon::InstallHosts(hosts))) => return Ok(hosts),
                Ok(Some(FromDaemon::Failed(e))) => return Err(e),
                Ok(Some(other)) => return Err(io_err(format!("unexpected reply to install_hosts: {other:?}"))),
                Ok(None) => io_err(format!("the daemon in {} went away", sandbox.0)),
                Err(e) => e,
            };
            lost += 1;
            if lost > MAX_REATTACH {
                return Err(io_err(format!("lost the daemon in {} {MAX_REATTACH} times: {error}", sandbox.0)));
            }
            tracing::warn!(sandbox = %sandbox.0, %error, "lost the daemon while it listed install hosts; asking again");
            self.disconnect(sandbox);
            patience = RECONNECT_PATIENCE;
        }
    }

    /// Asks the daemon whether the checkout has work that exists nowhere else. One attempt: the caller keeps the
    /// sandbox when this fails.
    async fn unpublished(&self, sandbox: &SandboxId, remote_branch: &str) -> Result<bool> {
        self.start_current(sandbox).await?;
        let (mut send, mut recv) = self.open(sandbox, CONNECT_PATIENCE).await?;
        write_frame(&mut send, &ToDaemon::Unpublished { remote_branch: remote_branch.into() }).await?;
        match read_frame::<FromDaemon>(&mut recv).await? {
            Some(FromDaemon::Unpublished(unpublished)) => Ok(unpublished),
            Some(FromDaemon::Failed(e)) => Err(e),
            Some(other) => Err(io_err(format!("unexpected reply to unpublished: {other:?}"))),
            None => Err(io_err(format!("the daemon in {} went away", sandbox.0))),
        }
    }

    /// A connection that drops while the daemon prepares is redialed and the request sent again; the daemon joins the
    /// prepare in flight.
    async fn prepare(&self, sandbox: &SandboxId, req: PrepareRequest) -> Result<()> {
        self.start_current(sandbox).await?;
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
        self.restored.lock().unwrap().remove(sandbox);
        if let Some(conn) = self.conns.lock().unwrap().remove(sandbox) {
            conn.close(0u32.into(), b"restarted");
        }
    }

    async fn restored(&self, sandbox: &SandboxId) {
        self.forget(sandbox).await;
        self.restored.lock().unwrap().insert(sandbox.clone());
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
