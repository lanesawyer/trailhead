//! The net thread (`docs/factory/ui.md#architecture`): a tokio runtime on its own thread that owns Lookout's iroh
//! endpoint, dials the control plane's `trailhead/ui/0`, serves the forwarded API on a local port, signs in, and
//! reports to Bevy over channels. Bevy sends [`NetCommand`]s and reads [`NetEvent`]s; neither side blocks the other.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::address_lookup::MemoryLookup;
use iroh::endpoint::{ConnectionError, presets};
use iroh::{Endpoint, EndpointId, RelayMode, SecretKey};
use tokio::sync::mpsc as tokio_mpsc;
use tokio::time::{MissedTickBehavior, interval, timeout};
use trailhead_client::Health;
use trailhead_client::creds::{Cred, CredStore};
use trailhead_factory::FactoryState;
use trailhead_factory::logs::LogLine;
use trailhead_wire::ui::{
    CLOSE_NOT_ALLOWED, LogFrame, LogStream, StatusFrame, StatusStream, UI_ALPN, UiTunnel, dial_ui, open_ui_tunnel,
};
use trailhead_wire::{parse_endpoint_id, parse_secret_key};

use crate::commands::{self, CommandId, Reply, Request};
use crate::detail::{self, DetailReply};
use crate::factory::FactoryUpdate;
use crate::search::{self, SearchReply};
use crate::sync::{self, Remote, Report, Resume, SyncItem, SyncTimings};

/// How the app is connected to its factory. The Bevy `States` type is in [`crate::connection`]; this is the same
/// three values as the net thread reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Link {
    Offline,
    Syncing,
    Live,
}

/// Why the link isn't [`Link::Live`], in words a person can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// No control plane endpoint id has been given yet.
    NoTarget,
    /// The pasted endpoint id doesn't parse.
    BadTarget(String),
    /// Dialing the control plane.
    Dialing,
    /// The control plane refused this endpoint: it isn't on its allowlist.
    NotAllowed,
    /// The control plane, or the Trailhead server behind it, can't be reached.
    Unreachable(String),
    /// Connected, and no saved or pasted token to sign in with.
    NeedsLogin,
    /// The server turned the token down.
    TokenRejected,
    /// The token belongs to an agent: Lookout acts only as the user.
    AgentToken,
    /// Confirming the token with `/v1/me`.
    SigningIn,
    /// Signed in, and loading the tasks and projects.
    Loading,
    /// The event stream dropped, and the board may be out of date until it is back.
    Reconnecting,
    /// A refusal that won't change by itself, so the probe waits this many seconds before asking again.
    Waiting(Box<Reason>, u64),
}

impl Reason {
    /// The reason itself, without the wait before the next try.
    pub fn cause(&self) -> &Reason {
        match self {
            Reason::Waiting(reason, _) => reason.cause(),
            reason => reason,
        }
    }
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Reason::NoTarget => write!(f, "paste your factory's endpoint id to connect"),
            Reason::BadTarget(e) => write!(f, "that isn't an endpoint id: {e}"),
            Reason::Dialing => write!(f, "connecting to the factory"),
            Reason::NotAllowed => {
                write!(
                    f,
                    "the factory doesn't allow this Lookout yet: run `trailhead factory allow` with the endpoint id shown here"
                )
            }
            Reason::Unreachable(e) => write!(f, "can't reach the factory: {e}"),
            Reason::NeedsLogin => write!(f, "no saved token for this server: log in with a user token"),
            Reason::TokenRejected => write!(f, "the server rejected the token: log in again"),
            Reason::AgentToken => write!(f, "that is an agent's token: log in with a user token"),
            Reason::SigningIn => write!(f, "signing in"),
            Reason::Loading => write!(f, "loading tasks and projects"),
            Reason::Reconnecting => write!(f, "the live feed dropped: reconnecting"),
            Reason::Waiting(reason, secs) => write!(f, "{reason}; trying again in {secs} s"),
        }
    }
}

/// What the app asks of the net thread.
#[derive(Debug, Clone)]
pub enum NetCommand {
    /// Dial this control plane (an endpoint id). Remembered for the next start.
    Connect(String),
    /// Sign in with a pasted user token; confirmed with `/v1/me`, and saved for this server if it holds.
    Login(String),
    /// Let go of the control plane and forget it, so the next start doesn't dial it either.
    Disconnect,
    /// Make this request (`crate::commands`); `attempt` comes back with the answer.
    Request { id: CommandId, attempt: u32, request: Request },
    /// Search tasks and comments for `query` (`GET /v1/search`); `seq` comes back with the answer.
    Search { seq: u64, query: String },
    /// Follow the control plane's log, or stop. Sent as the Logs page opens and closes.
    Logs(bool),
    /// Fetch the task `key` in full (`GET /v1/tasks/{key}`); `seq` comes back with the answer.
    Detail { seq: u64, key: String },
}

/// What the net thread tells the app.
#[derive(Debug, Clone, PartialEq)]
pub enum NetEvent {
    /// Lookout's own endpoint id, for `trailhead factory allow`.
    Endpoint(String),
    State(Link, Option<Reason>),
    /// The control plane the net thread is holding, as its parsed endpoint id; `None` once it has none (a failed
    /// Connect, or a disconnect). Sent on every Connect that parses and opens, so it never depends on a state change.
    Dialed(Option<String>),
    /// Signed in: where the forwarded API listens and who the user is. The token is the user's.
    Access(ApiAccess),
    /// The snapshot and then each event, for the mirror.
    Sync(SyncItem),
    /// What came of a [`NetCommand::Request`].
    Reply(Reply),
    /// The control plane's status snapshot and stream, or that it can't be reached.
    Factory(FactoryUpdate),
    /// What came of a [`NetCommand::Search`].
    Search(SearchReply),
    /// The control plane's log, while it is followed.
    Logs(LogUpdate),
    /// What came of a [`NetCommand::Detail`].
    Detail(DetailReply),
}

/// What the log stream brings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogUpdate {
    /// The lines the control plane has kept, replacing what was shown; first on every connection.
    Backlog(Vec<LogLine>),
    Line(LogLine),
    /// This many lines were missed because Lookout fell behind.
    Skipped(u64),
    /// The stream dropped; it is being retried.
    Dropped,
    /// The control plane doesn't serve logs, so it is older than this Lookout. Carries the message to show.
    TooOld(String),
}

/// Everything the HTTP client needs to talk to the Trailhead API through the control plane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiAccess {
    /// `http://127.0.0.1:<port>`, no trailing slash.
    pub base: String,
    pub token: String,
    pub user: String,
    /// The user's id, for naming what they wrote.
    pub user_id: String,
}

/// Where the net thread keeps things and how it reaches the network.
#[derive(Clone)]
pub struct NetConfig {
    /// Lookout's own config directory: the endpoint key and the last control plane dialed.
    pub config_dir: PathBuf,
    /// Trailhead's config and data directories, where the CLI saved its tokens and instance id.
    pub trailhead_config_dir: PathBuf,
    pub trailhead_data_dir: PathBuf,
    /// Whether tokens may use the OS keyring (off in tests; `TRAILHEAD_NO_KEYRING` turns it off too).
    pub keyring: bool,
    pub network: Network,
    /// How often the link is probed, how soon the status stream is retried after a dropped link, and the first gap
    /// after a refusal of either, which then doubles up to [`STATUS_BACKOFF_STEPS`] times.
    pub status_retry: Duration,
}

/// How the endpoint finds the control plane.
#[derive(Clone)]
pub enum Network {
    /// iroh's defaults: relays and discovery, so the factory can be anywhere.
    Internet,
    /// No relay; only the addresses in this lookup. For tests, and for a factory on the same machine or network.
    Direct(Arc<MemoryLookup>),
}

impl NetConfig {
    /// The OS locations: Lookout's own directory, and the same Trailhead directories the CLI uses
    /// (`TRAILHEAD_CONFIG_DIR` and `TRAILHEAD_DATA_DIR` override them, as for the CLI).
    pub fn from_env() -> Option<NetConfig> {
        let pick = |var: &str, default: Option<&Path>| {
            std::env::var_os(var).map(PathBuf::from).or_else(|| default.map(Path::to_path_buf))
        };
        let trailhead = directories::ProjectDirs::from("", "", "trailhead");
        let lookout = directories::ProjectDirs::from("", "", "lookout")?;
        let trailhead_config_dir = pick("TRAILHEAD_CONFIG_DIR", trailhead.as_ref().map(|d| d.config_dir()))?;
        Some(NetConfig {
            config_dir: pick("LOOKOUT_CONFIG_DIR", Some(lookout.config_dir()))?,
            trailhead_config_dir: trailhead_config_dir.clone(),
            trailhead_data_dir: pick("TRAILHEAD_DATA_DIR", trailhead.as_ref().map(|d| d.data_dir()))?,
            keyring: CredStore::new(PathBuf::new()).keyring,
            network: Network::Internet,
            status_retry: STATUS_RETRY,
        })
    }

    fn key_file(&self) -> PathBuf {
        self.config_dir.join("endpoint.key")
    }

    fn target_file(&self) -> PathBuf {
        self.config_dir.join("target")
    }

    /// The control plane dialed last time, if any.
    pub fn saved_target(&self) -> Option<String> {
        let text = std::fs::read_to_string(self.target_file()).ok()?;
        Some(text.trim().to_string()).filter(|t| !t.is_empty())
    }

    /// Where the user token for the server with this instance id is saved. This machine's own server keeps its
    /// tokens at the top of the config directory; any other server's are in a directory named after its instance.
    fn creds(&self, instance: Option<&str>) -> Option<CredStore> {
        let base = CredStore { config_dir: self.trailhead_config_dir.clone(), keyring: self.keyring };
        let ours = std::fs::read_to_string(self.trailhead_data_dir.join("instance")).ok();
        match instance {
            Some(id) if ours.as_deref().map(str::trim) != Some(id) => base.for_server(id),
            _ => Some(base),
        }
    }
}

/// The app's end of the channels. Dropping it stops the net thread.
pub struct NetHandle {
    commands: tokio_mpsc::UnboundedSender<NetCommand>,
    events: Mutex<mpsc::Receiver<NetEvent>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl NetHandle {
    /// Starts the net thread. `target` is dialed straight away; without one the link is `Offline` until a
    /// [`NetCommand::Connect`].
    pub fn spawn(config: NetConfig, target: Option<String>) -> NetHandle {
        let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("lookout-net".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = event_tx.send(NetEvent::State(Link::Offline, Some(Reason::Unreachable(e.to_string()))));
                        return;
                    }
                };
                runtime.block_on(run(config, target, cmd_rx, event_tx));
            })
            .expect("spawn the net thread");
        NetHandle { commands: cmd_tx, events: Mutex::new(event_rx), thread: Some(thread) }
    }

    pub fn send(&self, command: NetCommand) {
        // The thread only goes away with the handle (or if it failed to start, which it reported).
        let _ = self.commands.send(command);
    }

    /// A handle with no thread, whose events the test sends itself.
    #[cfg(test)]
    pub fn detached() -> (NetHandle, mpsc::Sender<NetEvent>) {
        let (commands, _) = tokio_mpsc::unbounded_channel();
        let (tx, rx) = mpsc::channel();
        (NetHandle { commands, events: Mutex::new(rx), thread: None }, tx)
    }

    /// Everything the net thread has reported since the last call.
    pub fn drain(&self) -> Vec<NetEvent> {
        let rx = self.events.lock().expect("net events lock");
        rx.try_iter().collect()
    }
}

impl Drop for NetHandle {
    fn drop(&mut self) {
        // Closing the command channel ends the loop; wait so the endpoint closes cleanly.
        let (closed, _) = tokio_mpsc::unbounded_channel();
        self.commands = closed;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

struct Target {
    id: EndpointId,
    /// Where the API answers: the tunnel's loopback address.
    api: SocketAddr,
    /// Held so replacing or dropping the target closes its tunnel (`None` for a test's stub).
    _tunnel: Option<UiTunnel>,
}

/// The default [`NetConfig::status_retry`].
pub const STATUS_RETRY: Duration = Duration::from_secs(2);
/// A refused request is asked again after 1x, 2x, 4x ... the retry, up to 2^5 = 32x (64 s by default). It is refused
/// again until something changes, so asking at the probe rate only fills the factory's logs; a factory that is
/// reinstalled is still found within the minute.
pub const STATUS_BACKOFF_STEPS: u32 = 5;
/// While `Live`, the token is checked with `/v1/me` on every this-many-th probe (30 s by default): a revoked token
/// doesn't need noticing within one, and the health request is all the other probes send.
const TOKEN_CHECK_EVERY: u32 = 15;

/// Why the factory won't serve the status stream.
enum Refusal {
    /// It doesn't know the request: older than this Lookout.
    TooOld(String),
    /// It knows the request and says no.
    Refused(String),
}

/// What the status task has seen of the current connection.
#[derive(Default)]
struct Progress {
    /// Whether the app was last told the factory is stopped; cleared by anything heard.
    stopped: bool,
    /// Whether the connection has served a frame, which means the factory isn't refusing us.
    heard: bool,
    /// Whether the last snapshot said the control plane isn't running. The server ends the stream then, having no
    /// repos to feed, and the link is still up: the next try picks up the change.
    waiting: bool,
}

/// The gap before the next try: the retry after a drop, and doubling from it after each refusal in a row.
struct Backoff {
    base: Duration,
    refusals: u32,
}

impl Backoff {
    fn new(base: Duration) -> Backoff {
        Backoff { base, refusals: 0 }
    }

    fn reset(&mut self) {
        self.refusals = 0;
    }

    /// A transient drop: back soon, without lengthening later gaps.
    fn dropped(&self) -> Duration {
        self.base
    }

    /// A refusal: this gap, and a longer one after the next.
    fn refused(&mut self) -> Duration {
        let wait = self.base * 2u32.pow(self.refusals);
        self.refusals = (self.refusals + 1).min(STATUS_BACKOFF_STEPS);
        wait
    }
}

struct Net {
    config: NetConfig,
    endpoint: Endpoint,
    http: reqwest::Client,
    events: mpsc::Sender<NetEvent>,
    target: Option<Target>,
    /// Follows the control plane's status stream, redialing until it is back.
    status: Option<tokio::task::JoinHandle<()>>,
    /// Follows the log while the Logs page is open.
    logs: Option<tokio::task::JoinHandle<()>>,
    /// Whether the Logs page is open, so a new control plane gets its log too.
    logs_wanted: bool,
    /// A token pasted in, not yet confirmed (and so not yet saved).
    pasted: Option<String>,
    /// The token that signed in, kept while `Live` so a dropped link signs back in without asking again.
    token: Option<String>,
    /// The `Live` probes since the token was last checked.
    live_probes: u32,
    state: Option<(Link, Option<Reason>)>,
    /// The gaps between probes of a refusal that will be repeated until something changes.
    probe_backoff: Backoff,
    /// When the probe may ask again, and the refusal it is waiting out.
    probe_hold: Option<(tokio::time::Instant, Reason)>,
    /// The running sync, and which run it is, so a report from one that was replaced is ignored.
    sync: Option<tokio::task::JoinHandle<()>>,
    sync_run: u64,
    /// Where the stream got to, kept across `Offline` so a short outage resumes instead of starting over.
    resume: Resume,
    reports: tokio_mpsc::UnboundedSender<(u64, Report)>,
    timings: SyncTimings,
}

async fn run(
    config: NetConfig,
    target: Option<String>,
    mut commands: tokio_mpsc::UnboundedReceiver<NetCommand>,
    events: mpsc::Sender<NetEvent>,
) {
    let endpoint = match bind(&config).await {
        Ok(e) => e,
        Err(e) => {
            let _ = events.send(NetEvent::State(Link::Offline, Some(Reason::Unreachable(format!("no endpoint: {e}")))));
            return;
        }
    };
    let _ = events.send(NetEvent::Endpoint(endpoint.id().to_string()));
    let http = reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build().expect("http client");
    let (reports, mut report_rx) = tokio_mpsc::unbounded_channel();
    let mut net = Net {
        probe_backoff: Backoff::new(config.status_retry),
        config,
        endpoint,
        http,
        events,
        target: None,
        status: None,
        logs: None,
        logs_wanted: false,
        pasted: None,
        token: None,
        live_probes: 0,
        state: None,
        probe_hold: None,
        sync: None,
        sync_run: 0,
        resume: Resume::default(),
        reports,
        timings: SyncTimings::default(),
    };
    match target {
        Some(t) => net.connect(&t).await,
        None => net.set(Link::Offline, Some(Reason::NoTarget)),
    }

    let mut tick = interval(net.config.status_retry);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(NetCommand::Connect(t)) => net.connect(&t).await,
                Some(NetCommand::Disconnect) => net.disconnect(),
                Some(NetCommand::Login(token)) => {
                    net.release_probe();
                    net.pasted = Some(token.trim().to_string());
                    net.check().await;
                }
                Some(NetCommand::Request { id, attempt, request }) => net.request(id, attempt, request),
                Some(NetCommand::Search { seq, query }) => net.search(seq, query),
                Some(NetCommand::Logs(on)) => net.set_logs(on),
                Some(NetCommand::Detail { seq, key }) => net.detail(seq, key),
                None => break,
            },
            Some((run, report)) = report_rx.recv() => net.on_report(run, report),
            _ = tick.tick() => {
                if net.probe_due() {
                    net.check().await;
                }
                // A slow probe mustn't be followed at once by the next, or an `Offline` is replaced before anyone sees it.
                tick.reset();
            }
        }
    }
    net.stop_sync();
    net.stop_logs();
    net.stop_status();
    net.endpoint.close().await;
}

async fn bind(config: &NetConfig) -> Result<Endpoint, String> {
    let key = load_or_create_key(&config.key_file())?;
    let builder = match &config.network {
        Network::Internet => Endpoint::builder(presets::N0),
        Network::Direct(lookup) => {
            Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled).address_lookup(lookup.clone())
        }
    };
    builder.secret_key(key).bind().await.map_err(|e| e.to_string())
}

/// The endpoint's identity is what a person adds to the allowlist, so it must survive restarts.
fn load_or_create_key(path: &Path) -> Result<SecretKey, String> {
    if let Ok(text) = std::fs::read_to_string(path) {
        return parse_secret_key(text.trim()).map_err(|e| format!("{}: {e}", path.display()));
    }
    let key = SecretKey::generate();
    trailhead_client::creds::write_secret(path, &hex_of(&key.to_bytes()))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(key)
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// One connection's worth of the status stream: the snapshot, then events until it ends. Returns why the factory
/// refused, if it did.
async fn follow_status(
    endpoint: &Endpoint,
    id: EndpointId,
    events: &mpsc::Sender<NetEvent>,
    progress: &mut Progress,
) -> Result<Option<Refusal>, trailhead_factory::Error> {
    let conn = dial_ui(endpoint, id).await?;
    match stream_status(conn.clone(), events, progress).await {
        Err(e) => match refusal_of(&conn, &e, progress.heard) {
            Some(refusal) => Ok(Some(refusal)),
            None => Err(e),
        },
        done => done,
    }
}

async fn stream_status(
    conn: iroh::endpoint::Connection,
    events: &mpsc::Sender<NetEvent>,
    progress: &mut Progress,
) -> Result<Option<Refusal>, trailhead_factory::Error> {
    let mut stream = StatusStream::open(conn).await?;
    while let Some(frame) = stream.next().await? {
        let update = match frame {
            StatusFrame::Snapshot { control, repos } => {
                progress.waiting = control.as_ref().is_some_and(|c| *c != FactoryState::Running);
                if events.send(NetEvent::Factory(FactoryUpdate::Control(control))).is_err() {
                    return Ok(None);
                }
                FactoryUpdate::Snapshot(repos)
            }
            StatusFrame::Event { repo, event } => FactoryUpdate::Event { repo, event },
            StatusFrame::Unsupported(unsupported) => return Ok(Some(Refusal::TooOld(too_old_message(&unsupported)))),
        };
        progress.stopped = false;
        progress.heard = true;
        if events.send(NetEvent::Factory(update)).is_err() {
            return Ok(None);
        }
    }
    Ok(None)
}

/// One connection's worth of the log stream. Returns the message to show when the factory doesn't serve logs.
async fn follow_logs(
    endpoint: &Endpoint,
    id: EndpointId,
    events: &mpsc::Sender<NetEvent>,
) -> Result<Option<String>, trailhead_factory::Error> {
    let mut stream = LogStream::open(dial_ui(endpoint, id).await?).await?;
    let mut heard = false;
    loop {
        let frame = match stream.next().await {
            Ok(Some(frame)) => frame,
            Ok(None) => return Ok(None),
            // Reset before anything was served: a control plane that has no log feed.
            Err(e) if !heard && e.to_string().contains("stream reset by peer") => return Ok(Some(NO_LOGS.into())),
            Err(e) => return Err(e),
        };
        heard = true;
        let update = match frame {
            LogFrame::Backlog { lines } => LogUpdate::Backlog(lines),
            LogFrame::Line(line) => LogUpdate::Line(line),
            LogFrame::Skipped { count } => LogUpdate::Skipped(count),
            LogFrame::Unsupported(_) => return Ok(Some(NO_LOGS.into())),
        };
        if events.send(NetEvent::Logs(update)).is_err() {
            return Ok(None);
        }
    }
}

/// What the Logs page says of a factory that doesn't send logs.
pub const NO_LOGS: &str = "this factory is too old to send logs";

/// Whether a failed stream was the factory saying no, rather than the link dropping: the connection closed because
/// this endpoint isn't allowed, or the stream reset before anything was served.
fn refusal_of(conn: &iroh::endpoint::Connection, error: &trailhead_factory::Error, heard: bool) -> Option<Refusal> {
    match conn.close_reason() {
        Some(ConnectionError::ApplicationClosed(c)) if c.error_code == CLOSE_NOT_ALLOWED.into() => {
            Some(Refusal::Refused(Reason::NotAllowed.to_string()))
        }
        Some(_) => None,
        None if !heard && error.to_string().contains("stream reset by peer") => {
            Some(Refusal::Refused("the factory refused the status request".into()))
        }
        None => None,
    }
}

/// What the map says of a factory that didn't know our request: what it was asked, what it serves, and its build.
fn too_old_message(u: &trailhead_wire::ui::Unsupported) -> String {
    // Only the op: a request that carries parameters would make the label long.
    let asked = u
        .got
        .get("op")
        .and_then(|op| op.as_str())
        .map_or_else(|| "an unreadable request".to_string(), |op| format!("\"{op}\""));
    format!(
        "the factory is older than this Lookout: reinstall trailhead and restart its server (it was asked {}, and \
         serves {}; version {}, build {})",
        asked,
        u.known.join(", "),
        u.version,
        u.build
    )
}

impl Net {
    /// Reports a change of link or reason, and only changes.
    fn set(&mut self, link: Link, reason: Option<Reason>) {
        if link == Link::Offline {
            self.stop_sync();
        }
        let next = (link, reason);
        if self.state.as_ref() != Some(&next) {
            let _ = self.events.send(NetEvent::State(next.0, next.1.clone()));
            self.state = Some(next);
        }
    }

    async fn connect(&mut self, text: &str) {
        let id = match parse_endpoint_id(text) {
            Ok(id) => id,
            Err(_) => {
                self.drop_target();
                return self.set(Link::Offline, Some(Reason::BadTarget(text.trim().to_string())));
            }
        };
        if self.target.as_ref().is_none_or(|t| t.id != id) {
            match open_ui_tunnel(self.endpoint.clone(), id).await {
                Ok(tunnel) => self.target = Some(Target { id, api: tunnel.addr(), _tunnel: Some(tunnel) }),
                Err(e) => {
                    self.drop_target();
                    return self.set(Link::Offline, Some(Reason::Unreachable(e.to_string())));
                }
            }
            self.start_status(id);
            self.restart_logs();
            self.token = None;
            self.state = None;
            self.stop_sync();
            self.resume.clear();
        }
        let _ = self.events.send(NetEvent::Dialed(Some(id.to_string())));
        if std::fs::create_dir_all(&self.config.config_dir).is_ok()
            && let Err(e) = std::fs::write(self.config.target_file(), format!("{id}\n"))
        {
            tracing::warn!(error = %e, "can't remember the control plane for next time");
        }
        self.release_probe();
        self.check().await;
    }

    /// Drops the control plane and the saved choice of it, back to waiting for an endpoint id.
    fn disconnect(&mut self) {
        self.drop_target();
        self.release_probe();
        if let Err(e) = std::fs::remove_file(self.config.target_file())
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(error = %e, "can't forget the saved control plane");
        }
        self.state = None;
        self.set(Link::Offline, Some(Reason::NoTarget));
    }

    /// Whether the probe may run: not while it waits out a refusal.
    fn probe_due(&self) -> bool {
        self.probe_hold.as_ref().is_none_or(|(until, _)| tokio::time::Instant::now() >= *until)
    }

    /// Forgets a refusal's wait, because the target, the login or the answer changed.
    fn release_probe(&mut self) {
        self.probe_backoff.reset();
        self.probe_hold = None;
    }

    /// Reports a refusal and holds the probe off for a growing gap, so a Lookout that is refused again doesn't ask
    /// (and fill the factory's log) at the probe rate.
    fn refused(&mut self, reason: Reason) {
        let wait = self.probe_backoff.refused();
        self.probe_hold = Some((tokio::time::Instant::now() + wait, reason.clone()));
        self.set(Link::Offline, Some(Reason::Waiting(Box::new(reason), wait.as_secs().max(1))));
    }

    /// Forgets the control plane a failed Connect was meant to replace, so the probe doesn't go back to it and bury
    /// the error.
    fn drop_target(&mut self) {
        let _ = self.events.send(NetEvent::Dialed(None));
        self.target = None;
        self.stop_status();
        self.stop_sync();
        self.token = None;
        self.pasted = None;
        self.resume.clear();
    }

    /// One probe of the whole path, and the climb back to `Live` from wherever it broke.
    async fn check(&mut self) {
        let Some(target) = &self.target else {
            // After a failed Connect the error stays up; there is nothing to probe.
            if self.state.is_none() {
                self.set(Link::Offline, Some(Reason::NoTarget));
            }
            return;
        };
        let (id, tunnel) = (target.id, format!("http://{}", target.api));
        // Signed in: live, or still loading the board or reconnecting its feed.
        let live =
            matches!(self.state, Some((Link::Live, _) | (Link::Syncing, Some(Reason::Loading | Reason::Reconnecting))))
                && self.pasted.is_none();
        // A token turned down stays on show until a new login, even through the probe's "connecting".
        // Bare cause: with no token left to try, nothing is retried, so no "trying again in n s".
        let refusal = match &self.state {
            Some((_, Some(reason))) if matches!(reason.cause(), Reason::TokenRejected | Reason::AgentToken) => {
                Some(reason.cause().clone())
            }
            _ => None,
        };
        if !live {
            self.live_probes = 0;
            self.set(Link::Syncing, Some(Reason::Dialing));
        }

        // The control plane forwards to the server, which answers /v1/health to anyone.
        let health = match self.http.get(format!("{tunnel}/v1/health")).send().await {
            Ok(r) if r.status().is_success() => r.json::<Health>().await.unwrap_or_default(),
            Ok(r) => {
                return self
                    .set(Link::Offline, Some(Reason::Unreachable(format!("the server answered {}", r.status()))));
            }
            Err(_) => {
                let reason = self.diagnose(id).await;
                // A control plane that refuses this Lookout is up, so that is told rather than hidden.
                match reason {
                    Reason::NotAllowed => return self.refused(reason),
                    _ => return self.set(Link::Offline, Some(reason)),
                }
            }
        };
        // Health answered, so the factory isn't refusing this Lookout any more.
        if matches!(&self.probe_hold, Some((_, Reason::NotAllowed))) {
            self.release_probe();
        }
        let base = tunnel;
        if live {
            self.release_probe();
            self.live_probes += 1;
            if self.live_probes < TOKEN_CHECK_EVERY {
                return;
            }
            self.live_probes = 0;
            // /v1/health is open to anyone, so a token revoked since sign-in shows only on an authenticated call.
            // Anything but a refusal is left to the sync, which has its own way of coping.
            if let Some(token) = &self.token
                && let Ok(r) = self.http.get(format!("{base}/v1/me")).bearer_auth(token).send().await
                && r.status() == reqwest::StatusCode::UNAUTHORIZED
            {
                self.token = None;
                self.resume.clear();
                self.set(Link::Offline, Some(Reason::TokenRejected));
            }
            return;
        }

        let store = self.config.creds(health.instance.as_deref());
        let saved = store.as_ref().and_then(|s| s.read(Cred::User)).map(|(t, _)| t);
        let pasted = self.pasted.take();
        let Some(token) = pasted.clone().or_else(|| self.token.clone()).or(saved) else {
            // A refusal stays on show until a new login, rather than decaying into "needs login".
            return match refusal {
                Some(reason) => self.set(Link::Offline, Some(reason)),
                None => self.set(Link::Offline, Some(Reason::NeedsLogin)),
            };
        };
        self.set(Link::Syncing, Some(Reason::SigningIn));
        let me = self.http.get(format!("{base}/v1/me")).bearer_auth(&token).send().await;
        let me = match me {
            Ok(r) if r.status().is_success() => r.json::<serde_json::Value>().await.unwrap_or_default(),
            Ok(r) if r.status() == reqwest::StatusCode::UNAUTHORIZED => {
                self.token = None;
                self.resume.clear();
                // A token just pasted is a new answer to wait for; one tried again will be rejected again.
                return match pasted {
                    Some(_) => self.set(Link::Offline, Some(Reason::TokenRejected)),
                    None => self.refused(Reason::TokenRejected),
                };
            }
            Ok(r) => {
                return self
                    .set(Link::Offline, Some(Reason::Unreachable(format!("the server answered {}", r.status()))));
            }
            Err(e) => return self.set(Link::Offline, Some(Reason::Unreachable(e.to_string()))),
        };
        // The user token only: an agent's would let Lookout act as one.
        if !me["agent_id"].is_null() {
            self.token = None;
            self.resume.clear();
            return match pasted {
                Some(_) => self.set(Link::Offline, Some(Reason::AgentToken)),
                None => self.refused(Reason::AgentToken),
            };
        }
        self.release_probe();
        if pasted.is_some()
            && let Some(Err(e)) = store.map(|s| s.save(Cred::User, &token))
        {
            tracing::warn!(error = %e, "can't save the token; it will be asked for again next start");
        }
        if pasted.is_some() {
            self.resume.clear();
        }
        let user = me["user"]["name"].as_str().unwrap_or("you").to_string();
        let user_id = me["user"]["id"].as_str().unwrap_or_default().to_string();
        self.token = Some(token.clone());
        let access = ApiAccess { base: base.clone(), token: token.clone(), user, user_id };
        let _ = self.events.send(NetEvent::Access(access));
        self.set(Link::Syncing, Some(Reason::Loading));
        self.start_sync(base, token);
    }

    /// Where the Trailhead API is reached right now.
    fn api_base(&self) -> Option<String> {
        self.target.as_ref().map(|t| format!("http://{}", t.api))
    }

    /// Follows the control plane's status stream on its own connection, and says when it can't be reached.
    fn start_status(&mut self, id: EndpointId) {
        self.stop_status();
        let (endpoint, events) = (self.endpoint.clone(), self.events.clone());
        let mut backoff = Backoff::new(self.config.status_retry);
        self.status = Some(tokio::spawn(async move {
            let mut progress = Progress::default();
            loop {
                progress.heard = false;
                progress.waiting = false;
                let followed = follow_status(&endpoint, id, &events, &mut progress).await;
                if progress.heard {
                    backoff.reset();
                }
                let ended_cleanly = followed.is_ok();
                let refusal = match followed {
                    Ok(refusal) => refusal,
                    Err(e) => {
                        tracing::debug!(error = %e, "the factory status stream ended");
                        None
                    }
                };
                if let Some(refusal) = refusal {
                    let wait = backoff.refused();
                    let retry = format!("trying again in {} s", wait.as_secs().max(1));
                    progress.stopped = false;
                    let update = match refusal {
                        Refusal::TooOld(why) => FactoryUpdate::TooOld(format!("{why}; {retry}")),
                        Refusal::Refused(why) => FactoryUpdate::Refused(format!("{why}; {retry}")),
                    };
                    if events.send(NetEvent::Factory(update)).is_err() {
                        return;
                    }
                    tokio::time::sleep(wait).await;
                    continue;
                }
                if !progress.stopped && !(ended_cleanly && progress.waiting) {
                    progress.stopped = true;
                    if events.send(NetEvent::Factory(FactoryUpdate::Unreachable)).is_err() {
                        return;
                    }
                }
                tokio::time::sleep(backoff.dropped()).await;
            }
        }));
    }

    /// Follows (or stops following) the control plane's log, for the Logs page.
    fn set_logs(&mut self, on: bool) {
        self.logs_wanted = on;
        self.restart_logs();
    }

    fn restart_logs(&mut self) {
        self.stop_logs();
        let (Some(Target { id, .. }), true) = (self.target.as_ref(), self.logs_wanted) else { return };
        let (endpoint, events, id) = (self.endpoint.clone(), self.events.clone(), *id);
        let retry = self.config.status_retry;
        self.logs = Some(tokio::spawn(async move {
            loop {
                match follow_logs(&endpoint, id, &events).await {
                    Ok(Some(why)) => {
                        let _ = events.send(NetEvent::Logs(LogUpdate::TooOld(why)));
                        return;
                    }
                    Ok(None) => {}
                    Err(e) => tracing::debug!(error = %e, "the factory log stream ended"),
                }
                if events.send(NetEvent::Logs(LogUpdate::Dropped)).is_err() {
                    return;
                }
                tokio::time::sleep(retry).await;
            }
        }));
    }

    fn stop_logs(&mut self) {
        if let Some(task) = self.logs.take() {
            task.abort();
        }
    }

    fn stop_status(&mut self) {
        if let Some(task) = self.status.take() {
            task.abort();
        }
    }

    /// Makes a command's request on its own task, so a slow one holds up neither the probe nor the others. It goes
    /// out only while `Live`; otherwise the app is told to hold it for later.
    fn request(&self, id: CommandId, attempt: u32, request: Request) {
        let events = self.events.clone();
        let reply = move |result| {
            let _ = events.send(NetEvent::Reply(Reply { id, attempt, result }));
        };
        let (Some(base), Some(token), Some((Link::Live, _))) = (self.api_base(), &self.token, &self.state) else {
            return reply(Err(commands::NOT_CONNECTED.into()));
        };
        let api = trailhead_client::http::Client::with_http(self.http.clone(), base, token.clone());
        tokio::spawn(async move { reply(commands::execute(&api, &request).await) });
    }

    /// Searches on its own task, like [`Self::request`], and says so if it can't.
    fn search(&self, seq: u64, query: String) {
        let events = self.events.clone();
        let (Some(base), Some(token), Some((Link::Live, _))) = (self.api_base(), &self.token, &self.state) else {
            let _ = events.send(NetEvent::Search(SearchReply { seq, result: Err("not connected".into()) }));
            return;
        };
        let api = trailhead_client::http::Client::with_http(self.http.clone(), base, token.clone());
        tokio::spawn(async move {
            let result = search::fetch(&api, &query).await;
            let _ = events.send(NetEvent::Search(SearchReply { seq, result }));
        });
    }

    fn detail(&self, seq: u64, key: String) {
        let events = self.events.clone();
        let (Some(base), Some(token), Some((Link::Live, _))) = (self.api_base(), &self.token, &self.state) else {
            let _ = events.send(NetEvent::Detail(DetailReply { seq, result: Err("not connected".into()) }));
            return;
        };
        let api = trailhead_client::http::Client::with_http(self.http.clone(), base, token.clone());
        tokio::spawn(async move {
            let result = detail::fetch(&api, &key).await;
            let _ = events.send(NetEvent::Detail(DetailReply { seq, result }));
        });
    }

    /// Starts the sync from the top, replacing any that runs. It brings the link to `Live` once the feed is open.
    fn start_sync(&mut self, base: String, token: String) {
        self.stop_sync();
        self.sync_run += 1;
        let (run, events, reports) = (self.sync_run, self.events.clone(), self.reports.clone());
        let remote = Remote::new(base, token, &self.timings);
        self.sync = Some(tokio::spawn(sync::run(
            remote,
            self.timings,
            self.resume.clone(),
            move |item| events.send(NetEvent::Sync(item)).is_ok(),
            move |report| {
                let _ = reports.send((run, report));
            },
        )));
    }

    fn stop_sync(&mut self) {
        self.resume.pause();
        if let Some(task) = self.sync.take() {
            task.abort();
        }
    }

    fn on_report(&mut self, run: u64, report: Report) {
        if run != self.sync_run || self.sync.is_none() {
            return;
        }
        match report {
            Report::Loading => self.set(Link::Syncing, Some(Reason::Loading)),
            Report::Reconnecting => self.set(Link::Syncing, Some(Reason::Reconnecting)),
            Report::Live => self.set(Link::Live, None),
        }
    }

    /// The request failed somewhere; dial the control plane directly to tell "refused" from "gone".
    async fn diagnose(&self, id: EndpointId) -> Reason {
        let conn = match timeout(REQUEST_TIMEOUT, self.endpoint.connect(id, UI_ALPN)).await {
            Ok(Ok(conn)) => conn,
            Ok(Err(e)) => return Reason::Unreachable(e.to_string()),
            Err(_) => return Reason::Unreachable("no answer from the control plane".into()),
        };
        // An endpoint that isn't allowed connects, and is then closed by the control plane with a reason.
        match timeout(Duration::from_secs(1), conn.closed()).await {
            Ok(ConnectionError::ApplicationClosed(c)) if c.error_code == CLOSE_NOT_ALLOWED.into() => Reason::NotAllowed,
            _ => Reason::Unreachable("the control plane is there, but the Trailhead server isn't answering".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn too_old_message_names_only_the_op() {
        let u = trailhead_wire::ui::Unsupported {
            got: serde_json::json!({"op": "logs", "task": "x".repeat(500)}),
            known: vec!["status".into()],
            version: "1".into(),
            build: "b".into(),
        };
        let message = too_old_message(&u);
        assert!(message.contains("asked \"logs\","), "{message}");
        assert!(message.len() < 300, "{message}");
    }

    #[test]
    fn refusals_back_off_to_a_cap_and_reset() {
        let mut backoff = Backoff::new(Duration::from_secs(2));
        let gaps: Vec<u64> = (0..8).map(|_| backoff.refused().as_secs()).collect();
        assert_eq!(gaps, [2, 4, 8, 16, 32, 64, 64, 64]);
        assert_eq!(backoff.dropped(), Duration::from_secs(2), "a drop is retried quickly even after refusals");
        backoff.reset();
        assert_eq!(backoff.refused(), Duration::from_secs(2));
    }

    #[test]
    fn a_waiting_refusal_shows_its_wait_but_its_cause_does_not() {
        let waiting = Reason::Waiting(Box::new(Reason::TokenRejected), 8);
        assert!(waiting.to_string().ends_with("trying again in 8 s"));
        assert_eq!(waiting.cause().to_string(), "the server rejected the token: log in again");
    }

    /// What `/v1/me` answers on the stub: `None` is a 401, `Some` the JSON of an identity.
    type Me = Arc<Mutex<Option<serde_json::Value>>>;

    /// A stub of the server behind the tunnel: `/v1/health` answers, `/v1/me` as `me` says.
    async fn stub(me: Me) -> SocketAddr {
        use axum::{Json, Router, http::StatusCode, routing::get};
        let app = Router::new().route("/v1/health", get(|| async { Json(serde_json::json!({})) })).route(
            "/v1/me",
            get(move || {
                let answer = me.lock().unwrap().clone();
                async move {
                    match answer {
                        Some(identity) => (StatusCode::OK, Json(identity)),
                        None => (StatusCode::UNAUTHORIZED, Json(serde_json::json!({}))),
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        addr
    }

    async fn net_for(dir: &Path, api: SocketAddr) -> (Net, mpsc::Receiver<NetEvent>) {
        let config = NetConfig {
            config_dir: dir.join("lookout"),
            trailhead_config_dir: dir.join("config"),
            trailhead_data_dir: dir.join("data"),
            keyring: false,
            network: Network::Direct(Arc::new(MemoryLookup::new())),
            status_retry: Duration::from_secs(10),
        };
        let endpoint = bind(&config).await.unwrap();
        let (events, rx) = mpsc::channel();
        let (reports, _) = tokio_mpsc::unbounded_channel();
        let net = Net {
            probe_backoff: Backoff::new(config.status_retry),
            target: Some(Target { id: endpoint.id(), api, _tunnel: None }),
            config,
            endpoint,
            http: reqwest::Client::new(),
            events,
            status: None,
            logs: None,
            logs_wanted: false,
            pasted: None,
            token: None,
            live_probes: 0,
            state: None,
            probe_hold: None,
            sync: None,
            sync_run: 0,
            resume: Resume::default(),
            reports,
            timings: SyncTimings::default(),
        };
        (net, rx)
    }

    fn states(rx: &mpsc::Receiver<NetEvent>) -> Vec<(Link, Option<Reason>)> {
        rx.try_iter()
            .filter_map(|e| match e {
                NetEvent::State(link, reason) => Some((link, reason)),
                _ => None,
            })
            .collect()
    }

    /// Runs the refusal sequence for a token the stub turns down in the way `refused` names.
    async fn refusal_sequence(me: Option<serde_json::Value>, refused: Reason) {
        let dir = tempfile::tempdir().unwrap();
        let addr = stub(Arc::new(Mutex::new(me))).await;
        let (mut net, rx) = net_for(dir.path(), addr).await;
        let offline = |reason: Reason| (Link::Offline, Some(reason));
        let connecting = (Link::Syncing, Some(Reason::Dialing));
        let signing_in = (Link::Syncing, Some(Reason::SigningIn));

        // A token just pasted is a new answer to wait for: the bare cause, and no hold on the probe.
        net.pasted = Some("pasted".into());
        net.check().await;
        assert_eq!(states(&rx), [connecting.clone(), signing_in.clone(), offline(refused.clone())]);
        assert!(net.probe_hold.is_none());
        assert!(net.probe_due());

        // No token left to try: the refusal stays up through "connecting", and nothing is waited for.
        net.check().await;
        assert_eq!(states(&rx), [connecting.clone(), offline(refused.clone())]);
        assert!(net.probe_hold.is_none());

        // A token tried again is refused again, so the probe backs off from the base gap, doubling.
        net.token = Some("saved".into());
        net.check().await;
        let waiting = |secs| offline(Reason::Waiting(Box::new(refused.clone()), secs));
        assert_eq!(states(&rx), [connecting.clone(), signing_in.clone(), waiting(10)]);
        assert!(!net.probe_due());
        assert_eq!(net.probe_hold.as_ref().map(|(_, r)| r), Some(&refused));
        net.token = Some("saved".into());
        net.check().await;
        assert_eq!(states(&rx), [connecting.clone(), signing_in, waiting(20)]);

        // Without a token the wait is dropped from the display, and the refusal is not forgotten.
        net.check().await;
        assert_eq!(states(&rx), [connecting, offline(refused.clone())]);
        assert_eq!(net.state.as_ref().and_then(|(_, r)| r.as_ref()), Some(&refused));
    }

    #[tokio::test]
    async fn a_rejected_token_shows_bare_then_waits_between_retries() {
        refusal_sequence(None, Reason::TokenRejected).await;
    }

    #[tokio::test]
    async fn an_agent_token_shows_bare_then_waits_between_retries() {
        refusal_sequence(Some(serde_json::json!({"agent_id": "claude"})), Reason::AgentToken).await;
    }

    #[tokio::test]
    async fn releasing_the_probe_resets_the_backoff() {
        let dir = tempfile::tempdir().unwrap();
        let addr = stub(Arc::new(Mutex::new(None))).await;
        let (mut net, _rx) = net_for(dir.path(), addr).await;
        net.token = Some("saved".into());
        net.check().await;
        assert!(net.probe_hold.is_some());
        net.release_probe();
        assert!(net.probe_due());
        assert_eq!(net.probe_backoff.refused(), Duration::from_secs(10), "back to the base gap");
    }
}
