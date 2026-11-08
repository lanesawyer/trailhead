//! The daemon's side of the daemon contract over iroh (`trailhead_wire` has the framing and the control plane's
//! side), and what keeps a sandbox's iroh endpoint connectable.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointId};
use tokio::sync::{Notify, mpsc, watch};
use trailhead_factory::{
    DaemonVersion, Error, FromDaemon, PrepareRequest, Result, RunEvent, RunReport, RunRequest, ToDaemon,
};
use trailhead_wire::daemon::ATTACH_PATIENCE;
use trailhead_wire::{frames, io_err, read_frame, write_frame};

use crate::Runner;
use crate::awake::SpriteHolds;

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

/// Hosts whose last lookup failed, so a host that never resolves (Docker sandboxes have no DNS) is warned about once,
/// not on every pass.
static UNRESOLVED: Mutex<std::collections::BTreeSet<String>> = Mutex::new(std::collections::BTreeSet::new());

/// Records a lookup's outcome in `unresolved` and says whether that is news: a host that starts failing or recovers.
fn note_lookup_in(unresolved: &mut std::collections::BTreeSet<String>, host: &str, failed: bool) -> bool {
    if failed { unresolved.insert(host.to_string()) } else { unresolved.remove(host) }
}

fn note_lookup(host: &str, failed: bool) -> bool {
    note_lookup_in(&mut UNRESOLVED.lock().unwrap(), host, failed)
}

/// A Sprite lets a raw IP through only after its own DNS has resolved it from an allowed name, and a policy change
/// resets that. iroh dials relays by IPs it resolved and cached earlier, so after a lockdown every relay reconnect
/// was refused. Looking the names up again through the system resolver re-allows their IPs.
pub async fn resolve_iroh_hosts() {
    let extra: Vec<String> = KEEP_RESOLVED.lock().unwrap().iter().cloned().collect();
    for host in IROH_HOSTS.iter().map(|h| h.to_string()).chain(extra) {
        let host = host.trim_end_matches('.');
        match tokio::net::lookup_host((host, 443)).await {
            Err(e) => {
                if note_lookup(host, true) {
                    tracing::warn!(%host, error = %e, "can't resolve an iroh host; not warning again until it does");
                } else {
                    tracing::debug!(%host, error = %e, "can't resolve an iroh host");
                }
            }
            Ok(_) => {
                if note_lookup(host, false) {
                    tracing::info!(%host, "an iroh host resolves again");
                }
            }
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
            version: DaemonVersion {
                version: env!("CARGO_PKG_VERSION").into(),
                build: "unknown".into(),
                secrets: String::new(),
            },
        }
    }

    /// Sets the build the daemon reports: [`build_id`](trailhead_wire::build_id) of its own executable.
    pub fn build(mut self, build: String) -> DaemonProtocol {
        self.version.build = build;
        self
    }

    /// Sets the secrets the daemon reports: [`build_id`](trailhead_wire::build_id) of the secrets file it started with.
    pub fn secrets(mut self, secrets: String) -> DaemonProtocol {
        self.version.secrets = secrets;
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
            ToDaemon::InstallHosts(req) => {
                let reply = match self.runner.install_hosts(&req).await {
                    Ok(hosts) => FromDaemon::InstallHosts(hosts),
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
            ToDaemon::Unpublished { remote_branch } => {
                let reply = match self.runner.unpublished(&remote_branch).await {
                    Ok(unpublished) => FromDaemon::Unpublished(unpublished),
                    Err(e) => FromDaemon::Failed(e),
                };
                return reply_with(&mut send, &reply).await;
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failing_host_is_news_only_when_it_starts_or_stops_failing() {
        let mut set = std::collections::BTreeSet::new();
        assert!(note_lookup_in(&mut set, "relay", true));
        assert!(!note_lookup_in(&mut set, "relay", true));
        assert!(note_lookup_in(&mut set, "relay", false));
        assert!(!note_lookup_in(&mut set, "relay", false));
        assert!(note_lookup_in(&mut set, "relay", true));
    }
}
