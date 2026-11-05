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
use trailhead_daemon::net::{parse_endpoint_id, parse_secret_key};
use trailhead_daemon::ui::{CLOSE_NOT_ALLOWED, StatusFrame, StatusStream, UI_ALPN, dial_ui, open_ui_tunnel};

use crate::commands::{self, CommandId, Reply, Request};
use crate::factory::FactoryUpdate;
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
    /// Make this request (`crate::commands`); `attempt` comes back with the answer.
    Request { id: CommandId, attempt: u32, request: Request },
}

/// What the net thread tells the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetEvent {
    /// Lookout's own endpoint id, for `trailhead factory allow`.
    Endpoint(String),
    State(Link, Option<Reason>),
    /// Signed in: where the forwarded API listens and who the user is. The token is the user's.
    Access(ApiAccess),
    /// The snapshot and then each event, for the mirror.
    Sync(SyncItem),
    /// What came of a [`NetCommand::Request`].
    Reply(Reply),
    /// The control plane's status snapshot and stream, or that it can't be reached.
    Factory(FactoryUpdate),
}

/// Everything the HTTP client needs to talk to the Trailhead API through the control plane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiAccess {
    /// `http://127.0.0.1:<port>`, no trailing slash.
    pub base: String,
    pub token: String,
    pub user: String,
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
    /// The server URL the CLI uses. When the control plane can't be reached, the board talks to the server here
    /// instead of through the tunnel. `None` keeps the tunnel as the only way in.
    pub server_url: Option<String>,
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
            server_url: Some(cli_server_url(&trailhead_config_dir, std::env::var("TRAILHEAD_URL").ok())),
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

/// The server the CLI would use: `TRAILHEAD_URL`, else `url` in `<config dir>/config.toml` (which `trailhead login`
/// sets), else the default.
pub fn cli_server_url(config_dir: &Path, env: Option<String>) -> String {
    let from_file = || {
        let text = std::fs::read_to_string(config_dir.join("config.toml")).ok()?;
        text.parse::<toml::Table>().ok()?.get("url")?.as_str().map(String::from)
    };
    env.filter(|u| !u.is_empty())
        .or_else(from_file)
        .unwrap_or_else(|| DEFAULT_SERVER_URL.to_string())
        .trim_end_matches('/')
        .to_string()
}

const DEFAULT_SERVER_URL: &str = "http://127.0.0.1:7241";

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

const PROBE_EVERY: Duration = Duration::from_secs(2);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

struct Target {
    id: EndpointId,
    api: SocketAddr,
}

/// Which way the Trailhead API is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Via {
    /// Through the control plane's tunnel.
    Tunnel,
    /// Straight to the server the CLI uses, because the control plane can't be reached.
    Direct,
}

const STATUS_RETRY: Duration = Duration::from_secs(2);
/// How often to ask a factory that is too old, so a reinstall takes effect without restarting Lookout.
const STATUS_RETRY_OLD: Duration = Duration::from_secs(30);

struct Net {
    config: NetConfig,
    endpoint: Endpoint,
    http: reqwest::Client,
    events: mpsc::Sender<NetEvent>,
    target: Option<Target>,
    via: Via,
    /// Follows the control plane's status stream, redialing until it is back.
    status: Option<tokio::task::JoinHandle<()>>,
    /// A token pasted in, not yet confirmed (and so not yet saved).
    pasted: Option<String>,
    /// The token that signed in, kept while `Live` so a dropped link signs back in without asking again.
    token: Option<String>,
    state: Option<(Link, Option<Reason>)>,
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
        config,
        endpoint,
        http,
        events,
        target: None,
        via: Via::Tunnel,
        status: None,
        pasted: None,
        token: None,
        state: None,
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

    let mut tick = interval(PROBE_EVERY);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(NetCommand::Connect(t)) => net.connect(&t).await,
                Some(NetCommand::Login(token)) => {
                    net.pasted = Some(token.trim().to_string());
                    net.check().await;
                }
                Some(NetCommand::Request { id, attempt, request }) => net.request(id, attempt, request),
                None => break,
            },
            Some((run, report)) = report_rx.recv() => net.on_report(run, report),
            _ = tick.tick() => {
                net.check().await;
                // A slow probe mustn't be followed at once by the next, or an `Offline` is replaced before anyone sees it.
                tick.reset();
            }
        }
    }
    net.stop_sync();
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

/// One connection's worth of the status stream: the snapshot, then events until it ends. `stopped` says whether the
/// app was last told the factory is stopped, and is cleared by anything heard. Returns whether the factory said it
/// doesn't know the request.
async fn follow_status(
    endpoint: &Endpoint,
    id: EndpointId,
    events: &mpsc::Sender<NetEvent>,
    stopped: &mut bool,
) -> Result<bool, trailhead_factory::Error> {
    let mut stream = StatusStream::open(dial_ui(endpoint, id).await?).await?;
    while let Some(frame) = stream.next().await? {
        let update = match frame {
            StatusFrame::Snapshot { repos } => FactoryUpdate::Snapshot(repos),
            StatusFrame::Event { repo, event } => FactoryUpdate::Event { repo, event },
            StatusFrame::Unsupported(unsupported) => {
                *stopped = false;
                let _ = events.send(NetEvent::Factory(FactoryUpdate::TooOld(too_old_message(&unsupported))));
                return Ok(true);
            }
        };
        *stopped = false;
        if events.send(NetEvent::Factory(update)).is_err() {
            return Ok(false);
        }
    }
    Ok(false)
}

/// What the map says of a factory that didn't know our request: what it was asked, what it serves, and its build.
fn too_old_message(u: &trailhead_daemon::ui::Unsupported) -> String {
    format!(
        "the factory is older than this Lookout: reinstall trailhead-factory and restart it (it was asked {}, and \
         serves {}; version {}, build {})",
        u.got,
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
                Ok(api) => self.target = Some(Target { id, api }),
                Err(e) => {
                    self.drop_target();
                    return self.set(Link::Offline, Some(Reason::Unreachable(e.to_string())));
                }
            }
            self.via = Via::Tunnel;
            self.start_status(id);
            self.token = None;
            self.state = None;
            self.stop_sync();
            self.resume.clear();
        }
        if std::fs::create_dir_all(&self.config.config_dir).is_ok()
            && let Err(e) = std::fs::write(self.config.target_file(), format!("{id}\n"))
        {
            tracing::warn!(error = %e, "can't remember the control plane for next time");
        }
        self.check().await;
    }

    /// Forgets the control plane a failed Connect was meant to replace, so the probe doesn't go back to it and bury
    /// the error.
    fn drop_target(&mut self) {
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
        let mut live =
            matches!(self.state, Some((Link::Live, _) | (Link::Syncing, Some(Reason::Loading | Reason::Reconnecting))))
                && self.pasted.is_none();
        if !live {
            self.set(Link::Syncing, Some(Reason::Dialing));
        }

        // The control plane forwards to the server, which answers /v1/health to anyone.
        let (health, via) = match self.http.get(format!("{tunnel}/v1/health")).send().await {
            Ok(r) if r.status().is_success() => (r.json::<Health>().await.unwrap_or_default(), Via::Tunnel),
            Ok(r) => {
                return self
                    .set(Link::Offline, Some(Reason::Unreachable(format!("the server answered {}", r.status()))));
            }
            Err(_) => {
                // Already on the server's own URL: the control plane stays down, and asking why again is slow.
                let reason = if self.via == Via::Direct {
                    Reason::Unreachable("the control plane isn't answering".into())
                } else {
                    self.diagnose(id).await
                };
                // A control plane that refuses this Lookout is up, so that is told rather than hidden; one that is
                // gone leaves the board working from the server the CLI uses.
                match (&reason, self.direct_health().await) {
                    (Reason::Unreachable(_), Some(health)) => (health, Via::Direct),
                    _ => return self.set(Link::Offline, Some(reason)),
                }
            }
        };
        if via != self.via {
            // Moving between the tunnel and the server: sign in and sync again on the new address.
            self.via = via;
            live = false;
        }
        let base = match via {
            Via::Tunnel => tunnel,
            Via::Direct => self.config.server_url.clone().unwrap_or_default(),
        };
        if live {
            return;
        }

        let store = self.config.creds(health.instance.as_deref());
        let saved = store.as_ref().and_then(|s| s.read(Cred::User)).map(|(t, _)| t);
        let pasted = self.pasted.take();
        let Some(token) = pasted.clone().or_else(|| self.token.clone()).or(saved) else {
            // A refusal stays on show until a new login, rather than decaying into "needs login".
            if matches!(&self.state, Some((_, Some(Reason::TokenRejected | Reason::AgentToken)))) {
                return;
            }
            return self.set(Link::Offline, Some(Reason::NeedsLogin));
        };
        self.set(Link::Syncing, Some(Reason::SigningIn));
        let me = self.http.get(format!("{base}/v1/me")).bearer_auth(&token).send().await;
        let me = match me {
            Ok(r) if r.status().is_success() => r.json::<serde_json::Value>().await.unwrap_or_default(),
            Ok(r) if r.status() == reqwest::StatusCode::UNAUTHORIZED => {
                self.token = None;
                self.resume.clear();
                return self.set(Link::Offline, Some(Reason::TokenRejected));
            }
            Ok(r) => {
                return self
                    .set(Link::Offline, Some(Reason::Unreachable(format!("the server answered {}", r.status()))));
            }
            Err(e) => return self.set(Link::Offline, Some(Reason::Unreachable(e.to_string()))),
        };
        // The user token only: an agent's would let Lookout act as one.
        if me["agent"]["name"].as_str().is_some() {
            self.token = None;
            self.resume.clear();
            return self.set(Link::Offline, Some(Reason::AgentToken));
        }
        if pasted.is_some()
            && let Some(Err(e)) = store.map(|s| s.save(Cred::User, &token))
        {
            tracing::warn!(error = %e, "can't save the token; it will be asked for again next start");
        }
        if pasted.is_some() {
            self.resume.clear();
        }
        let user = me["user"]["name"].as_str().unwrap_or("you").to_string();
        self.token = Some(token.clone());
        let access = ApiAccess { base: base.clone(), token: token.clone(), user };
        let _ = self.events.send(NetEvent::Access(access));
        self.set(Link::Syncing, Some(Reason::Loading));
        self.start_sync(base, token);
    }

    /// Where the Trailhead API is reached right now.
    fn api_base(&self) -> Option<String> {
        match self.via {
            Via::Tunnel => self.target.as_ref().map(|t| format!("http://{}", t.api)),
            Via::Direct => self.config.server_url.clone(),
        }
    }

    /// The server's health at the URL the CLI uses, if it answers there.
    async fn direct_health(&self) -> Option<Health> {
        let url = self.config.server_url.as_ref()?;
        let r = self.http.get(format!("{url}/v1/health")).send().await.ok()?.error_for_status().ok()?;
        Some(r.json::<Health>().await.unwrap_or_default())
    }

    /// Follows the control plane's status stream on its own connection, and says when it can't be reached.
    fn start_status(&mut self, id: EndpointId) {
        self.stop_status();
        let (endpoint, events) = (self.endpoint.clone(), self.events.clone());
        self.status = Some(tokio::spawn(async move {
            let mut stopped = false;
            loop {
                let followed = follow_status(&endpoint, id, &events, &mut stopped).await;
                let old = matches!(followed, Ok(true));
                if let Err(e) = followed {
                    tracing::debug!(error = %e, "the factory status stream ended");
                }
                if old {
                    tokio::time::sleep(STATUS_RETRY_OLD).await;
                    continue;
                }
                if !stopped {
                    stopped = true;
                    if events.send(NetEvent::Factory(FactoryUpdate::Stopped)).is_err() {
                        return;
                    }
                }
                tokio::time::sleep(STATUS_RETRY).await;
            }
        }));
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
            return reply(Err("not connected".into()));
        };
        let (http, token) = (self.http.clone(), token.clone());
        tokio::spawn(async move { reply(commands::execute(&http, &base, &token, &request).await) });
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
