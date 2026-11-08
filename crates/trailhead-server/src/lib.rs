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
}

impl AppState {
    pub fn new(store: impl Into<Arc<Store>>) -> Self {
        AppState {
            store: store.into(),
            info: ServerInfo::current(None),
            shutdown: tokio::sync::watch::Sender::new(false),
            limits: Default::default(),
            webhook_targets: webhooks::Targets::PublicOnly,
        }
    }
}

/// How long in-flight requests get to finish after a shutdown signal.
const DRAIN: Duration = Duration::from_secs(10);

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
    if let Some(pid_file) = &cfg.pid_file {
        if let Some(dir) = pid_file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(pid_file, format!("{}\n", std::process::id()))?;
    }
    tracing::info!(addr = %cfg.bind, db = %cfg.db.display(), "trailhead listening");
    let stopping = state.shutdown.clone();
    let mut stopped = state.shutdown.subscribe();
    let server = axum::serve(listener, router(state)).with_graceful_shutdown(async move {
        shutdown_signal().await;
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

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => tracing::info!("received SIGINT; shutting down"),
        _ = term.recv() => tracing::info!("received SIGTERM; shutting down"),
    }
}
