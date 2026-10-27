//! The daemon binary the control plane starts in each sandbox:
//! `trailhead-daemon --secrets <file> --workdir <dir>`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use clap::Parser;
use iroh::Endpoint;
use iroh::endpoint::presets;
use iroh::protocol::Router;
use trailhead_daemon::awake::{SPRITE_API_SOCKET, SpriteHolds};
use trailhead_daemon::net::{ALPN, DaemonProtocol, open_api_tunnel, parse_endpoint_id, parse_secret_key};
use trailhead_daemon::{Connect, GitWorkspace, RENEW_EVERY, RestTracker, Runner};
use trailhead_factory::{CodeHost, Commit, DaemonSecrets, Error, MergeOutcome, NewPr, PrRef, Repo, Result, Tracker};
use trailhead_harness_claude_code::ClaudeCode;

#[derive(Parser)]
struct Args {
    /// A JSON `DaemonSecrets` file. It's deleted once read.
    #[arg(long)]
    secrets: PathBuf,
    /// Where the repo is checked out.
    #[arg(long)]
    workdir: PathBuf,
    /// The Claude Code CLI.
    #[arg(long, default_value = "claude")]
    claude: PathBuf,
}

/// Placeholder until the GitHub code host lands (M5); runs that reach it fail cleanly.
struct NoCodeHost;

fn not_yet(what: &str, milestone: &str) -> Error {
    Error::Other(format!("{what} isn't available in the daemon until {milestone}"))
}

#[async_trait]
impl CodeHost for NoCodeHost {
    async fn publish(&self, _: &Repo, _: &str, _: &[Commit]) -> Result<()> {
        Err(not_yet("publishing", "M5"))
    }
    async fn open_pr(&self, _: &Repo, _: NewPr) -> Result<PrRef> {
        Err(not_yet("opening PRs", "M5"))
    }
    async fn diff(&self, _: &PrRef) -> Result<String> {
        Err(not_yet("reading PRs", "M5"))
    }
    async fn comment(&self, _: &PrRef, _: &str) -> Result<()> {
        Err(not_yet("commenting", "M5"))
    }
    async fn merge(&self, _: &PrRef) -> Result<MergeOutcome> {
        Err(not_yet("merging", "M5"))
    }
}

fn read_secrets(path: &PathBuf) -> std::result::Result<DaemonSecrets, String> {
    let raw = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    std::fs::remove_file(path).map_err(|e| format!("deleting {}: {e}", path.display()))?;
    serde_json::from_slice(&raw).map_err(|e| format!("{}: {e}", path.display()))
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    tracing_subscriber::fmt().with_env_filter(filter).init();
    match serve(Args::parse()).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("trailhead-daemon: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn serve(args: Args) -> std::result::Result<(), String> {
    let secrets = read_secrets(&args.secrets)?;
    let key = parse_secret_key(&secrets.secret_key).map_err(|e| e.to_string())?;
    let control_plane = parse_endpoint_id(&secrets.control_plane).map_err(|e| e.to_string())?;

    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(key)
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await
        .map_err(|e| format!("binding the iroh endpoint: {e}"))?;
    let api = open_api_tunnel(endpoint.clone(), control_plane).await.map_err(|e| e.to_string())?;
    let connect: Connect = Arc::new(move |cred| {
        Box::pin(async move { Ok(Arc::new(RestTracker::new(format!("http://{api}"), cred)) as Arc<dyn Tracker>) })
    });
    let runner = Runner {
        harness: Arc::new(ClaudeCode::new(args.claude)),
        codehost: Arc::new(NoCodeHost),
        workspace: Arc::new(GitWorkspace::new(args.workdir)),
        connect,
        renew_every: RENEW_EVERY,
    };
    tracing::info!(id = %endpoint.id(), %control_plane, "trailhead-daemon listening");
    let mut protocol = DaemonProtocol::new(Arc::new(runner), control_plane);
    if std::path::Path::new(SPRITE_API_SOCKET).exists() {
        protocol = protocol.holding(SpriteHolds::new(SPRITE_API_SOCKET));
    }
    let router = Router::builder(endpoint).accept(ALPN, protocol).spawn();
    tokio::signal::ctrl_c().await.map_err(|e| e.to_string())?;
    router.shutdown().await.map_err(|e| e.to_string())
}
