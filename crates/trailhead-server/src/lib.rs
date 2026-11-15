//! Trailhead's HTTP server. REST lives under `/v1`; every handler is a thin
//! adapter over [`trailhead_core::Store`].

mod auth;
mod error;
mod idempotency;
mod llms;
mod mcp;
mod openapi;
mod params;
mod ratelimit;
mod routes;
mod stream;
pub mod webhooks;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use tower_http::trace::TraceLayer;
use trailhead_client::creds;
use trailhead_core::{Ctx, DEFAULT_REMINDER_WINDOW, Principal, Store};

pub use auth::BATCH_HEADER;
pub use idempotency::{IDEMPOTENCY_KEY, REPLAYED_HEADER};

pub use trailhead_client::paths::{DEFAULT_BIND, Paths};

pub struct AppState {
    /// Shared, so the factory's control plane can run on the same store in the same process.
    pub store: Arc<Store>,
    pub info: ServerInfo,
    /// Flips to true when the server starts shutting down, so long-lived
    /// streams end instead of holding graceful shutdown open forever.
    pub shutdown: tokio::sync::watch::Sender<bool>,
    pub limits: ratelimit::RateLimiter,
    /// Public-only unless this is the local server, where receivers on the same
    /// machine or network are the point.
    pub webhook_targets: webhooks::Targets,
    /// The factory's runs in flight, when this server runs its control plane; `/v1/health` reports it.
    pub runs_in_flight: std::sync::OnceLock<Box<dyn Fn() -> usize + Send + Sync>>,
    /// Set once a stop signal has started draining the control plane; `/v1/health` reports it.
    pub draining: std::sync::atomic::AtomicBool,
    /// Set once SIGUSR1 asked for a restart onto a new binary; `/v1/health` reports it.
    pub upgrade_pending: std::sync::atomic::AtomicBool,
}

impl AppState {
    pub fn new(store: impl Into<Arc<Store>>) -> Self {
        AppState {
            store: store.into(),
            info: ServerInfo::current(None),
            shutdown: tokio::sync::watch::Sender::new(false),
            limits: Default::default(),
            webhook_targets: webhooks::Targets::PublicOnly,
            runs_in_flight: Default::default(),
            draining: Default::default(),
            upgrade_pending: Default::default(),
        }
    }
}

/// How long in-flight requests get to finish after a shutdown signal.
const DRAIN: Duration = Duration::from_secs(10);

/// How long cancelled factory runs get to record what they produced and tear down.
#[cfg(feature = "factory")]
const FACTORY_STOP: Duration = Duration::from_secs(30);

/// What `/v1/health` reports, so the CLI can tell which binary a server runs
/// and restart it after an upgrade. Everyone sees the version, instance and start
/// time; the pid and paths only go to callers with a token.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ServerInfo {
    pub version: String,
    /// The id in the data directory's `instance` file, so a CLI can tell its own
    /// machine's server from one reached through a tunnel on the same port.
    pub instance: Option<String>,
    pub pid: u32,
    /// The binary this server was started from.
    pub exe: Option<PathBuf>,
    /// That binary's modification time when the server started, in Unix milliseconds.
    pub exe_modified_ms: Option<u64>,
    pub db: Option<PathBuf>,
    pub started_at: jiff::Timestamp,
    /// Started at a terminal (`trailhead serve`, `trailhead factory run`) rather than by the CLI in the background,
    /// so the CLI doesn't replace it with a background one after a reinstall.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub foreground: bool,
}

impl ServerInfo {
    pub fn current(db: Option<PathBuf>) -> Self {
        let exe = std::env::current_exe().ok();
        ServerInfo {
            version: env!("CARGO_PKG_VERSION").into(),
            instance: None,
            pid: std::process::id(),
            exe_modified_ms: exe.as_deref().and_then(modified_ms),
            exe,
            db,
            started_at: jiff::Timestamp::now(),
            foreground: false,
        }
    }
}

pub fn modified_ms(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as u64)
}

/// `app` with the factory's status routes under `/v1/factory`. Their event stream carries every run's tool calls,
/// which can name commands and paths, so only the user's token reads them, not an agent's (a sandbox's included).
#[cfg(feature = "factory")]
pub fn with_factory_routes(app: Router, state: Arc<AppState>, api: Router) -> Router {
    app.nest("/v1/factory", api.layer(axum::middleware::from_fn_with_state(state, auth::human_only)))
}

pub fn router(state: Arc<AppState>) -> Router {
    let mcp = Router::new()
        .nest_service("/mcp", mcp::service(state.clone()))
        .layer(axum::middleware::from_fn_with_state(state.clone(), mcp::auth));
    Router::new()
        .nest("/v1", routes::v1())
        .route("/llms.txt", axum::routing::get(llms::serve))
        .merge(mcp)
        .layer(axum::middleware::from_fn(auth::batch))
        // Outside `batch`, so a replay carries the original request's batch id.
        .layer(axum::middleware::from_fn_with_state(state.clone(), idempotency::layer))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

pub struct LocalConfig {
    pub bind: SocketAddr,
    pub db: PathBuf,
    /// Where the local user's CLI token is kept.
    pub creds: creds::CredStore,
    pub user_name: String,
    /// Holds the running server's pid while it's up.
    pub pid_file: Option<PathBuf>,
    /// Held while the server runs, so a second server can't serve the same data directory on another port
    /// ([`trailhead_client::paths::lock`]).
    pub lock_file: Option<PathBuf>,
    /// Reported by `/v1/health`; see [`ServerInfo::foreground`].
    pub foreground: bool,
    /// Reported by `/v1/health`; see [`ServerInfo::instance`].
    pub instance: Option<String>,
    /// Where the factory's control plane finds the user config and keeps its state. With the `factory` feature and
    /// `[factory] repos` listed there, the server runs the control plane; `None` never does.
    pub factory: Option<Paths>,
}

/// Opens the local database, makes sure the workspace, user and a CLI token exist,
/// and returns the store with the user's context.
pub async fn open_local(cfg: &LocalConfig) -> Result<(Store, Ctx), Box<dyn std::error::Error + Send + Sync>> {
    if let Some(dir) = cfg.db.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let store = Store::open_local(&cfg.db).await?;
    let (ws, user) = store.bootstrap_local(&cfg.user_name).await?;
    let ctx = Ctx::user(ws.id, user.id);
    let existing = cfg.creds.read(creds::Cred::User);
    let valid = match &existing {
        Some((secret, _)) => store.authenticate(secret).await.is_ok_and(|c| c.user_id == user.id && c.is_human()),
        None => false,
    };
    match existing {
        // Still a plaintext file from before keyring support: move it in. If the
        // keyring isn't reachable the file just gets rewritten in place.
        Some((secret, creds::Saved::File(_))) if valid && cfg.creds.keyring => {
            cfg.creds.save(creds::Cred::User, &secret)?;
        }
        _ if valid => {}
        _ => {
            let (_, secret) = store.create_token(&ctx, Principal::User(user.id), "local cli").await?;
            cfg.creds.save(creds::Cred::User, &secret)?;
        }
    }
    match store.parent_cycles(&ctx).await {
        Ok(cycles) => {
            for cycle in cycles {
                tracing::warn!(
                    "parent cycle in the database: {}; unparent one of them (`trailhead update <key> --parent none`)",
                    cycle.join(" -> ")
                );
            }
        }
        Err(e) => tracing::warn!("couldn't check for parent cycles: {e}"),
    }
    Ok((store, ctx))
}

/// The factory's configuration is broken, so the server won't start. Keeps the factory's typed error as its source;
/// `Display` leaves it out, so a chain printer (`{:#}`) shows it once.
#[cfg(feature = "factory")]
#[derive(Debug)]
struct FactoryConfigBroken {
    source: trailhead_factory_server::Error,
    user_config: std::path::PathBuf,
}

#[cfg(feature = "factory")]
impl std::fmt::Display for FactoryConfigBroken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the factory's configuration is broken, so the server won't start (fix it, or run `trailhead factory disable` to run without the factory; it edits {}); the cause is",
            self.user_config.display()
        )
    }
}

#[cfg(feature = "factory")]
impl std::error::Error for FactoryConfigBroken {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Runs the local server until Ctrl-C or SIGTERM, finishing in-flight requests first, or until SIGUSR1 asks it to
/// restart onto a new binary.
pub async fn serve_local(cfg: LocalConfig) -> Result<Exit, Box<dyn std::error::Error + Send + Sync>> {
    let lock = match &cfg.lock_file {
        Some(path) => Some(trailhead_client::paths::lock(path, &format!("a server at {}", cfg.bind))?),
        None => None,
    };
    let (store, ctx) = open_local(&cfg).await?;
    let state = Arc::new(AppState {
        info: ServerInfo {
            instance: cfg.instance.clone(),
            foreground: cfg.foreground,
            ..ServerInfo::current(Some(cfg.db.clone()))
        },
        webhook_targets: webhooks::Targets::Any,
        ..AppState::new(store)
    });
    let reaper = state.clone();
    let reminder_ctx = ctx.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(15));
        loop {
            tick.tick().await;
            if let Err(e) = reaper.store.reap_expired_claims(&ctx.next_batch()).await {
                tracing::warn!(error = %e, "reaping expired claims failed");
            }
        }
    });
    spawn_archiver(state.clone());
    spawn_heap_trim();
    webhooks::spawn(state.clone());
    let reminders = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            if let Err(e) =
                reminders.store.emit_due_reminders(&reminder_ctx.next_batch(), DEFAULT_REMINDER_WINDOW).await
            {
                tracing::warn!(error = %e, "emitting due reminders failed");
            }
        }
    });
    let listener = tokio::net::TcpListener::bind(cfg.bind).await?;
    // After the listener, so the API tunnel has a server to forward to; before the pid file, so a configuration error
    // stops the server before it says it's up.
    #[cfg(feature = "factory")]
    let started: Option<trailhead_factory_server::InServer> = match cfg.factory.clone() {
        Some(paths) => {
            let user_config = paths.config_dir.join("config.toml");
            let started = trailhead_factory_server::start(paths, state.store.clone(), cfg.bind)
                .map_err(|source| FactoryConfigBroken { source, user_config })?;
            Some(started)
        }
        None => None,
    };
    #[cfg(feature = "factory")]
    let (factory, factory_api) = match started {
        Some(started) => (started.running.map(Arc::new), Some(trailhead_factory_server::api(&started.feed))),
        None => (None, None),
    };
    #[cfg(feature = "factory")]
    if let Some(running) = factory.clone() {
        let _ = state.runs_in_flight.set(Box::new(move || running.runs_in_flight()));
    }
    #[cfg(not(feature = "factory"))]
    let _ = &cfg.factory;
    if let Some(pid_file) = &cfg.pid_file {
        if let Some(dir) = pid_file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(pid_file, format!("{}\n", std::process::id()))?;
    }
    tracing::info!(addr = %cfg.bind, db = %cfg.db.display(), "trailhead listening");
    let stopping = state.shutdown.clone();
    let mut stopped = state.shutdown.subscribe();
    let mut signals = Signals::new()?;
    let signal_state = state.clone();
    #[cfg(feature = "factory")]
    let draining = factory.clone();
    let restart = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let restarting = restart.clone();
    let app = router(state.clone());
    #[cfg(feature = "factory")]
    let app = match factory_api {
        Some(api) => with_factory_routes(app, state.clone(), api),
        None => app,
    };
    let server = axum::serve(listener, app).with_graceful_shutdown(async move {
        use std::sync::atomic::Ordering::SeqCst;
        #[cfg_attr(not(feature = "factory"), allow(unused_mut))]
        let mut upgrade = signals.first().await;
        if upgrade {
            tracing::info!("a newer trailhead is installed; restarting onto it once the factory's runs finish");
            signal_state.upgrade_pending.store(true, SeqCst);
        }
        // The API keeps serving while the control plane drains, since its runs reach it through the tunnel.
        #[cfg(feature = "factory")]
        if let Some(factory) = draining {
            signal_state.draining.store(true, SeqCst);
            match factory.runs_in_flight() {
                0 => tracing::info!("stopping the factory; no runs in flight"),
                runs => {
                    tracing::info!(runs, "draining the factory: no new runs; a second signal cancels those in flight")
                }
            }
            factory.drain();
            loop {
                tokio::select! {
                    _ = factory.finished() => tracing::info!("the factory drained"),
                    _ = signals.next() => {
                        // A stop asked for during an upgrade's drain is a stop: it still waits for the runs, and a
                        // further signal cancels them.
                        if std::mem::take(&mut upgrade) {
                            signal_state.upgrade_pending.store(false, SeqCst);
                            tracing::info!("stopping instead of restarting once the factory's runs finish; a second signal cancels them");
                            continue;
                        }
                        tracing::info!("cancelling the factory's runs; they're requeued on the next start");
                        factory.stop(FACTORY_STOP).await;
                    }
                }
                break;
            }
        }
        restarting.store(upgrade, SeqCst);
        stopping.send_replace(true);
    });
    let served = tokio::select! {
        r = server => r,
        _ = async {
            stopped.wait_for(|s| *s).await.ok();
            tokio::time::sleep(DRAIN).await;
        } => {
            tracing::warn!("requests still running after {}s; stopping anyway", DRAIN.as_secs());
            Ok(())
        }
    };
    #[cfg(feature = "factory")]
    if let Some(factory) = factory {
        factory.stop(FACTORY_STOP).await;
    }
    let restart = restart.load(std::sync::atomic::Ordering::SeqCst);
    // Before the pid file goes, which the CLI waits for: a server it starts next then finds the lock free.
    drop(lock);
    // A restart execs the new binary in this process, which keeps the pid, so the file stays.
    if !restart
        && let Some(pid_file) = &cfg.pid_file
        && std::fs::read_to_string(pid_file).is_ok_and(|p| p.trim() == std::process::id().to_string())
    {
        std::fs::remove_file(pid_file).ok();
    }
    served?;
    if restart {
        tracing::info!("trailhead stopped; restarting onto the new binary");
        return Ok(Exit::Restart);
    }
    tracing::info!("trailhead stopped");
    Ok(Exit::Stopped)
}

/// Why [`serve_local`] returned.
#[derive(Debug, PartialEq, Eq)]
pub enum Exit {
    Stopped,
    /// SIGUSR1 asked it to restart onto a newly installed binary, which the caller execs. With the factory, that
    /// waited for its runs to finish.
    Restart,
}

/// Every minute, hands the heap's free memory back to the system. glibc keeps what a burst of work freed, so the
/// server's size only ever rises to its busiest moment: after 240 large reads, 16 at a time, a server held 321 MB
/// with 34 MB in use. A trim took that to 138 MB; the rest is free memory in pages that still hold something
/// (TRAIL-287). Other allocators and platforms are left to themselves.
fn spawn_heap_trim() {
    tokio::spawn(async {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            // Off the runtime's threads: a trim walks every arena under its lock.
            let _ = tokio::task::spawn_blocking(trim_heap).await;
        }
    });
}

/// Whether the allocator gave anything back.
fn trim_heap() -> bool {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: `malloc_trim` takes glibc's own locks and releases only pages no allocation uses.
        unsafe { libc::malloc_trim(0) == 1 }
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    {
        false
    }
}

/// Hourly, moves events older than the undo window to `events_archive`.
fn spawn_archiver(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60 * 60));
        loop {
            tick.tick().await;
            let mut total = 0;
            loop {
                match state.store.archive_events().await {
                    Ok(0) => break,
                    Ok(n) => total += n,
                    Err(e) => {
                        tracing::warn!(error = %e, "archiving events failed");
                        break;
                    }
                }
            }
            if total > 0 {
                tracing::info!(events = total, "archived old events");
            }
        }
    });
}

/// SIGINT and SIGTERM, each delivery once. The first starts a shutdown; with the factory, a second cancels its runs.
struct Signals {
    int: tokio::signal::unix::Signal,
    term: tokio::signal::unix::Signal,
    usr1: tokio::signal::unix::Signal,
}

impl Signals {
    fn new() -> std::io::Result<Signals> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Signals {
            int: signal(SignalKind::interrupt())?,
            term: signal(SignalKind::terminate())?,
            usr1: signal(SignalKind::user_defined1())?,
        })
    }

    /// The first of a stop signal or SIGUSR1, which the CLI sends when a newer binary is installed but factory runs
    /// are in flight (not SIGHUP, which a foreground `trailhead serve` gets when its terminal closes). True for SIGUSR1.
    async fn first(&mut self) -> bool {
        tokio::select! {
            _ = self.int.recv() => tracing::info!("received SIGINT; shutting down"),
            _ = self.term.recv() => tracing::info!("received SIGTERM; shutting down"),
            _ = self.usr1.recv() => {
                tracing::info!("received SIGUSR1; restarting onto the new binary");
                return true;
            }
        }
        false
    }

    #[cfg_attr(not(feature = "factory"), allow(dead_code))]
    async fn next(&mut self) {
        tokio::select! {
            _ = self.int.recv() => tracing::info!("received SIGINT; shutting down"),
            _ = self.term.recv() => tracing::info!("received SIGTERM; shutting down"),
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "factory")]
    #[test]
    fn a_broken_factory_config_names_its_cause_once_in_the_chain() {
        let cause = trailhead_factory_server::Error::NoAgent { agent: "factory" };
        let cause_text = cause.to_string();
        let err: Box<dyn std::error::Error + Send + Sync> =
            Box::new(super::FactoryConfigBroken { source: cause, user_config: "/cfg/config.toml".into() });
        let mut chain = vec![err.to_string()];
        let mut next = err.source();
        while let Some(e) = next {
            chain.push(e.to_string());
            next = e.source();
        }
        assert_eq!(chain.len(), 2, "the typed cause is the source: {chain:?}");
        assert_eq!(chain[1], cause_text);
        assert!(!chain[0].contains(&cause_text), "Display leaves the cause to the chain: {chain:?}");
        assert!(chain[0].contains("/cfg/config.toml"));
    }

    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn a_trim_gives_back_what_a_burst_of_small_allocations_freed() {
        // Many small blocks, freed all at once, as a large response's rows are: glibc keeps them until it is asked.
        let burst: Vec<Vec<u8>> = (0..200_000).map(|i| vec![i as u8; 256]).collect();
        drop(burst);
        assert!(super::trim_heap(), "about 50 MB was free to give back");
    }
}
