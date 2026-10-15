//! Trailhead's HTTP server. REST lives under `/v1`; every handler is a thin
//! adapter over [`trailhead_core::Store`].

mod auth;
mod error;
mod routes;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use tower_http::trace::TraceLayer;
use trailhead_core::{Ctx, Principal, Store};

pub use auth::BATCH_HEADER;

/// Port 7241 spells T-R-A-I on a phone keypad.
pub const DEFAULT_BIND: &str = "127.0.0.1:7241";

pub struct AppState {
    pub store: Store,
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .nest("/v1", routes::v1())
        .layer(axum::middleware::from_fn(auth::batch))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// OS-standard locations for Trailhead's data and config.
pub struct Paths {
    pub data_dir: PathBuf,
    pub config_dir: PathBuf,
}

impl Paths {
    /// `TRAILHEAD_DATA_DIR` and `TRAILHEAD_CONFIG_DIR` override the OS defaults,
    /// which is how tests and side-by-side instances stay isolated.
    pub fn from_env() -> Option<Paths> {
        let dirs = directories::ProjectDirs::from("", "", "trailhead");
        let pick = |var: &str, default: Option<&Path>| {
            std::env::var_os(var).map(PathBuf::from).or_else(|| default.map(Path::to_path_buf))
        };
        Some(Paths {
            data_dir: pick("TRAILHEAD_DATA_DIR", dirs.as_ref().map(|d| d.data_dir()))?,
            config_dir: pick("TRAILHEAD_CONFIG_DIR", dirs.as_ref().map(|d| d.config_dir()))?,
        })
    }

    pub fn db(&self) -> PathBuf {
        self.data_dir.join("trailhead.db")
    }

    pub fn credentials(&self) -> PathBuf {
        self.config_dir.join("credentials")
    }

    /// Saved agent tokens, used by `trailhead --as <agent>`.
    pub fn agent_token(&self, name: &str) -> PathBuf {
        self.config_dir.join("agents").join(name)
    }

    pub fn server_log(&self) -> PathBuf {
        self.data_dir.join("server.log")
    }
}

pub struct LocalConfig {
    pub bind: SocketAddr,
    pub db: PathBuf,
    /// Where the local user's CLI token is kept.
    pub credentials: PathBuf,
    pub user_name: String,
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
    let existing = std::fs::read_to_string(&cfg.credentials).ok();
    let valid = match existing.as_deref().map(str::trim) {
        Some(secret) => store.authenticate(secret).await.is_ok_and(|c| c.user_id == user.id && c.is_human()),
        None => false,
    };
    if !valid {
        let (_, secret) = store.create_token(&ctx, Principal::User(user.id), "local cli").await?;
        write_secret(&cfg.credentials, &secret)?;
    }
    Ok((store, ctx))
}

/// Writes a token readable only by the current user.
pub fn write_secret(path: &Path, secret: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    std::io::Write::write_all(&mut opts.open(path)?, format!("{secret}\n").as_bytes())
}

/// Runs the local server until Ctrl-C.
pub async fn serve_local(cfg: LocalConfig) -> Result<(), Box<dyn std::error::Error>> {
    let (store, ctx) = open_local(&cfg).await?;
    let state = Arc::new(AppState { store });
    let reaper = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(15));
        loop {
            tick.tick().await;
            if let Err(e) = reaper.store.reap_expired_claims(&ctx.next_batch()).await {
                tracing::warn!(error = %e, "reaping expired claims failed");
            }
        }
    });
    let listener = tokio::net::TcpListener::bind(cfg.bind).await?;
    tracing::info!(addr = %cfg.bind, db = %cfg.db.display(), "trailhead listening");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.ok();
        })
        .await?;
    Ok(())
}
