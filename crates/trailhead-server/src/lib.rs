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
    /// Reported by `/v1/health`; see [`ServerInfo::instance`].
    pub instance: Option<String>,
    /// Where the factory's control plane finds the user config and keeps its state. With the `factory` feature and
    /// `[factory] repos` listed there, the server runs the control plane; `None` never does.
    pub factory: Option<Paths>,
}

/// Opens the local database, makes sure the workspace, user and a CLI token exist,
/// and returns the store with the user's context.
pub async fn open_local(cfg: &LocalConfig) -> Result<(Store, Ctx), Box<dyn std::error::Error>> {
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
    Ok((store, ctx))
}

/// Runs the local server until Ctrl-C or SIGTERM, finishing in-flight requests first.
pub async fn serve_local(cfg: LocalConfig) -> Result<(), Box<dyn std::error::Error>> {
    let (store, ctx) = open_local(&cfg).await?;
    let state = Arc::new(AppState {
        info: ServerInfo { instance: cfg.instance.clone(), ..ServerInfo::current(Some(cfg.db.clone())) },
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
            let started = trailhead_factory_server::start(paths, state.store.clone(), cfg.bind).map_err(|e| {
                format!(
                    "the factory's configuration is broken, so the server won't start: {e}\nFix it, or run `trailhead factory disable` to run without the factory (it edits {})",
                    user_config.display()
                )
            })?;
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
    #[cfg(feature = "factory")]
    let (draining, signal_state) = (factory.clone(), state.clone());
    let app = router(state.clone());
    #[cfg(feature = "factory")]
    let app = match factory_api {
        Some(api) => with_factory_routes(app, state.clone(), api),
        None => app,
    };
    let server = axum::serve(listener, app).with_graceful_shutdown(async move {
        signals.next().await;
        // The API keeps serving while the control plane drains, since its runs reach it through the tunnel.
        #[cfg(feature = "factory")]
        if let Some(factory) = draining {
            signal_state.draining.store(true, std::sync::atomic::Ordering::SeqCst);
            match factory.runs_in_flight() {
                0 => tracing::info!("stopping the factory; no runs in flight"),
                runs => {
                    tracing::info!(runs, "draining the factory: no new runs; a second signal cancels those in flight")
                }
            }
            factory.drain();
            tokio::select! {
                _ = factory.finished() => tracing::info!("the factory drained"),
                _ = signals.next() => {
                    tracing::info!("cancelling the factory's runs; they're requeued on the next start");
                    factory.stop(FACTORY_STOP).await;
                }
            }
        }
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
    if let Some(pid_file) = &cfg.pid_file
        && std::fs::read_to_string(pid_file).is_ok_and(|p| p.trim() == std::process::id().to_string())
    {
        std::fs::remove_file(pid_file).ok();
    }
    tracing::info!("trailhead stopped");
    Ok(served?)
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
}

impl Signals {
    fn new() -> std::io::Result<Signals> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Signals { int: signal(SignalKind::interrupt())?, term: signal(SignalKind::terminate())? })
    }

    async fn next(&mut self) {
        tokio::select! {
            _ = self.int.recv() => tracing::info!("received SIGINT; shutting down"),
            _ = self.term.recv() => tracing::info!("received SIGTERM; shutting down"),
        }
    }
}
