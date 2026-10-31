//! The daemon binary the control plane starts in each sandbox:
//! `trailhead-daemon --secrets <file> --workdir <dir>`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use clap::{CommandFactory, Parser};
use iroh::Endpoint;
use iroh::endpoint::presets;
use iroh::protocol::Router;
use trailhead_codehost_github::{GitHub, Identity};
use trailhead_daemon::awake::SpriteHolds;
use trailhead_daemon::net::{
    ALPN, DaemonProtocol, build_id, open_api_tunnel, parse_endpoint_id, parse_secret_key, watch_network,
};
use trailhead_daemon::{Connect, GitWorkspace, HEARTBEAT_EVERY, RENEW_EVERY, RestTracker, Runner};
use trailhead_factory::{
    BranchUpdate, ChangedFile, Ci, CodeHost, Commit, DaemonSecrets, Error, MergeOutcome, NewPr, PrRef, PrState, Repo,
    Result, Tracker,
};
use trailhead_harness_claude_code::ClaudeCode;

#[derive(Parser)]
#[command(args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    daemon: Option<Args>,
}

#[derive(clap::Subcommand)]
enum Command {
    /// The Docker sandboxes' gateway: an allowlisting HTTP proxy and its control socket.
    Gateway {
        /// Where the proxy listens.
        #[arg(long, default_value = "0.0.0.0:3128")]
        listen: std::net::SocketAddr,
        /// The control socket's path.
        #[arg(long, default_value = "/run/trailhead-gateway.sock")]
        socket: PathBuf,
        #[arg(long, default_value = "info")]
        log: String,
    },
    /// Sends the gateway one control message (JSON on stdin) and prints its response.
    GatewayCtl {
        #[arg(long, default_value = "/run/trailhead-gateway.sock")]
        socket: PathBuf,
    },
}

#[derive(clap::Args)]
struct Args {
    /// A JSON `DaemonSecrets` file (0600). It stays, because the Sprite restarts the daemon after a cold wake.
    #[arg(long)]
    secrets: PathBuf,
    /// Where the repo is checked out.
    #[arg(long)]
    workdir: PathBuf,
    /// The Claude Code CLI.
    #[arg(long, default_value = "claude")]
    claude: PathBuf,
    /// Who published commits are attributed to.
    #[arg(long, default_value = "Trailhead Factory")]
    author_name: String,
    #[arg(long, default_value = "factory@trailhead.invalid")]
    author_email: String,
    /// A `tracing` filter; `RUST_LOG` wins if set.
    #[arg(long, default_value = "info")]
    log: String,
    /// The compute's socket for holding the sandbox awake, used when it exists (the control plane passes it for
    /// computes that pause idle sandboxes).
    #[arg(long)]
    hold_socket: Option<PathBuf>,
}

/// Stands in for a code host when the sandbox has no GitHub connector, so runs fail with the reason.
struct NoCodeHost(String);

#[async_trait]
impl CodeHost for NoCodeHost {
    async fn publish(&self, _: &Repo, _: &str, _: &str, _: &[Commit]) -> Result<()> {
        Err(Error::Forbidden(self.0.clone()))
    }
    async fn open_pr(&self, _: &Repo, _: NewPr) -> Result<PrRef> {
        Err(Error::Forbidden(self.0.clone()))
    }
    async fn diff(&self, _: &PrRef) -> Result<String> {
        Err(Error::Forbidden(self.0.clone()))
    }
    async fn comment(&self, _: &PrRef, _: &str) -> Result<()> {
        Err(Error::Forbidden(self.0.clone()))
    }
    async fn merge(&self, _: &PrRef, _: &str) -> Result<MergeOutcome> {
        Err(Error::Forbidden(self.0.clone()))
    }
    async fn behind(&self, _: &PrRef) -> Result<bool> {
        Err(Error::Forbidden(self.0.clone()))
    }

    async fn update_branch(&self, _: &PrRef) -> Result<BranchUpdate> {
        Err(Error::Forbidden(self.0.clone()))
    }

    async fn carries_verdict(&self, _: &PrRef, _: &str) -> Result<bool> {
        Err(Error::Forbidden(self.0.clone()))
    }

    async fn pr_state(&self, _: &PrRef) -> Result<PrState> {
        Err(Error::Forbidden(self.0.clone()))
    }

    async fn title(&self, _: &PrRef) -> Result<String> {
        Err(Error::Forbidden(self.0.clone()))
    }

    async fn files(&self, _: &PrRef) -> Result<Vec<ChangedFile>> {
        Err(Error::Forbidden(self.0.clone()))
    }

    async fn head(&self, _: &PrRef) -> Result<String> {
        Err(Error::Forbidden(self.0.clone()))
    }

    async fn ci(&self, _: &PrRef) -> Result<Ci> {
        Err(Error::Forbidden(self.0.clone()))
    }
}

/// GitHub through the Sprite's GitHub connector, found in the gateway's list.
async fn codehost(author: Identity, gateway_list: &str) -> Arc<dyn CodeHost> {
    let found = async {
        let list: serde_json::Value = reqwest::get(gateway_list).await?.error_for_status()?.json().await?;
        let conns = list["connections"].as_array().cloned().unwrap_or_default();
        Ok::<_, reqwest::Error>(
            conns
                .into_iter()
                .find(|c| c["provider"] == "github")
                .and_then(|c| c["gateway_base_url"].as_str().map(str::to_string)),
        )
    };
    match found.await {
        Ok(Some(url)) => {
            tracing::info!(%url, "using the GitHub connector");
            Arc::new(GitHub::gateway(url).with_author(author))
        }
        Ok(None) => Arc::new(NoCodeHost("no GitHub connector is granted to this sandbox".into())),
        Err(e) => Arc::new(NoCodeHost(format!("couldn't list this sandbox's connectors: {e}"))),
    }
}

fn read_secrets(path: &PathBuf) -> std::result::Result<DaemonSecrets, String> {
    let raw = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&raw).map_err(|e| format!("{}: {e}", path.display()))
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let args = match (cli.command, cli.daemon) {
        (Some(command), _) => return gateway_command(command).await,
        (None, Some(args)) => args,
        (None, None) => Cli::command()
            .error(clap::error::ErrorKind::MissingRequiredArgument, "--secrets and --workdir are required")
            .exit(),
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| args.log.as_str().into());
    tracing_subscriber::fmt().with_env_filter(filter).init();
    match serve(args).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("trailhead-daemon: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn gateway_command(command: Command) -> std::process::ExitCode {
    use std::io::Read;
    use trailhead_daemon::gateway;
    use trailhead_factory::{GatewayRequest, GatewayResponse};
    let result: std::result::Result<(), String> = async {
        match command {
            Command::Gateway { listen, socket, log } => {
                let filter =
                    tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| log.as_str().into());
                tracing_subscriber::fmt().with_env_filter(filter).init();
                gateway::run(listen, &socket).await.map_err(|e| e.to_string())
            }
            Command::GatewayCtl { socket } => {
                let mut input = String::new();
                std::io::stdin().read_to_string(&mut input).map_err(|e| e.to_string())?;
                let req: GatewayRequest = serde_json::from_str(&input).map_err(|e| format!("bad message: {e}"))?;
                let resp = gateway::ctl(&socket, &req).await.map_err(|e| format!("{}: {e}", socket.display()))?;
                println!("{}", serde_json::to_string(&resp).map_err(|e| e.to_string())?);
                match resp {
                    GatewayResponse::Error { message } => Err(message),
                    _ => Ok(()),
                }
            }
        }
    }
    .await;
    match result {
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
        .proxy_from_env()
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
        harness: Arc::new(match secrets.claude_oauth_token.clone() {
            Some(token) => ClaudeCode::new(args.claude).with_oauth_token(token),
            None => ClaudeCode::new(args.claude),
        }),
        codehost: codehost(Identity { name: args.author_name, email: args.author_email }, &secrets.gateway_list).await,
        workspace: Arc::new(GitWorkspace::new(args.workdir)),
        connect,
        renew_every: RENEW_EVERY,
        heartbeat_every: HEARTBEAT_EVERY,
    };
    tracing::info!(id = %endpoint.id(), %control_plane, "trailhead-daemon listening");
    watch_network(endpoint.clone());
    let build = match std::env::current_exe().and_then(std::fs::read) {
        Ok(binary) => build_id(&binary),
        Err(e) => {
            tracing::warn!(error = %e, "couldn't read the daemon's own executable; the control plane will restart it");
            "unknown".into()
        }
    };
    tracing::info!(version = env!("CARGO_PKG_VERSION"), %build, "trailhead-daemon build");
    let mut protocol = DaemonProtocol::new(Arc::new(runner), control_plane).on(endpoint.clone()).build(build);
    if let Some(socket) = args.hold_socket.filter(|s| s.exists()) {
        protocol = protocol.holding(SpriteHolds::new(socket));
    }
    let router = Router::builder(endpoint).accept(ALPN, protocol).spawn();
    tokio::signal::ctrl_c().await.map_err(|e| e.to_string())?;
    router.shutdown().await.map_err(|e| e.to_string())
}
