//! The daemon binary the control plane starts in each sandbox:
//! `trailhead-daemon --secrets <file> --workdir <dir>`.

use std::path::PathBuf;
use std::sync::Arc;

use clap::{CommandFactory, Parser};
use iroh::Endpoint;
use iroh::endpoint::presets;
use iroh::protocol::Router;
use trailhead_codehost_github::Identity;
use trailhead_daemon::awake::SpriteHolds;
use trailhead_daemon::net::{DaemonProtocol, watch_network};
use trailhead_daemon::{Connect, GitWorkspace, HEARTBEAT_EVERY, RENEW_EVERY, Runner};
use trailhead_factory::{DaemonSecrets, HarnessSetup, Tracker};
use trailhead_tracker_rest::RestTracker;
use trailhead_wire::api::open_api_tunnel;
use trailhead_wire::{ALPN, build_id, parse_endpoint_id, parse_secret_key};

use crate::connectors::{Gateway, GatewayCodeHost};

mod connectors;

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
    /// Trims a cargo `target` directory to a size, for a repo's `trim` check (`"$TRAILHEAD_DAEMON" trim-cargo`).
    TrimCargo {
        /// The size to trim to, in bytes.
        #[arg(long)]
        cap: u64,
        /// The directory to trim.
        #[arg(long, default_value = "target")]
        target: PathBuf,
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
    /// The chosen harness's executable (the Claude Code CLI by default).
    #[arg(long, alias = "claude", default_value = "claude")]
    harness_executable: PathBuf,
    /// Who published commits are attributed to.
    #[arg(long, default_value = "Trailhead Factory")]
    author_name: String,
    #[arg(long, default_value = "factory@trailhead.invalid")]
    author_email: String,
    /// A `tracing` filter. Wins over `RUST_LOG`; without it, `RUST_LOG` applies, then `info`. An invalid value is
    /// a usage error.
    #[arg(long, value_parser = parse_log_filter)]
    log: Option<String>,
    /// The compute's socket for holding the sandbox awake, used when it exists (the control plane passes it for
    /// computes that pause idle sandboxes).
    #[arg(long)]
    hold_socket: Option<PathBuf>,
}

/// The secrets, with the [`build_id`] of the file they came from.
fn read_secrets(path: &PathBuf) -> std::result::Result<(DaemonSecrets, String), String> {
    let raw = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    // Not serde's error: it can quote the file, which holds the secret key.
    let secrets = serde_json::from_slice(&raw).map_err(|_| format!("{}: malformed secrets file", path.display()))?;
    Ok((secrets, build_id(&raw)))
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let args = match (cli.command, cli.daemon) {
        (Some(Command::TrimCargo { cap, target }), _) => {
            return match trailhead_daemon::trim::trim_target(&target, cap) {
                Ok(t) => {
                    println!("trimmed {} from {} to {} bytes (cap {cap})", target.display(), t.before, t.after);
                    std::process::ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("trailhead-daemon: trim-cargo {}: {e}", target.display());
                    std::process::ExitCode::FAILURE
                }
            };
        }
        (Some(command), _) => return gateway_command(command).await,
        (None, Some(args)) => args,
        (None, None) => Cli::command()
            .error(clap::error::ErrorKind::MissingRequiredArgument, "--secrets and --workdir are required")
            .exit(),
    };
    let filter = log_filter(args.log.as_deref(), std::env::var("RUST_LOG").ok().as_deref());
    trailhead_log::init_stdout(filter);
    match serve(args).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("trailhead-daemon: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Checks a `--log` value as clap parses it, so a typo exits with a usage error instead of being logged at `info`.
fn parse_log_filter(spec: &str) -> Result<String, String> {
    tracing_subscriber::EnvFilter::try_new(spec).map(|_| spec.to_string()).map_err(|e| e.to_string())
}

/// The daemon's log filter: an explicit `--log` first, then `RUST_LOG`, then `info`. A `RUST_LOG` that
/// doesn't parse falls back to `info`, with a warning on stderr (logging isn't up yet).
fn log_filter(flag: Option<&str>, env: Option<&str>) -> tracing_subscriber::EnvFilter {
    let spec = flag.or(env).unwrap_or("info");
    tracing_subscriber::EnvFilter::try_new(spec).unwrap_or_else(|e| {
        eprintln!("trailhead-daemon: invalid log filter {spec:?} ({e}); using info");
        tracing_subscriber::EnvFilter::new("info")
    })
}

async fn gateway_command(command: Command) -> std::process::ExitCode {
    use std::io::Read;
    use trailhead_daemon::gateway;
    use trailhead_factory::{GatewayRequest, GatewayResponse};
    let result: std::result::Result<(), String> = async {
        match command {
            Command::Gateway { listen, socket, log } => {
                trailhead_log::init_stdout(trailhead_log::env_filter_or(&log));
                gateway::run(listen, &socket).await.map_err(|e| e.to_string())
            }
            Command::TrimCargo { .. } => unreachable!("handled in main"),
            Command::GatewayCtl { socket } => {
                let mut input = String::new();
                std::io::stdin().read_to_string(&mut input).map_err(|e| e.to_string())?;
                let req: GatewayRequest = gateway::parse_request(&input).map_err(str::to_string)?;
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
    let (secrets, secrets_id) = read_secrets(&args.secrets)?;
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
    let gateway = Gateway::new(secrets.gateway_list.clone());
    let harness =
        (trailhead_plugins::harnesses().get(&secrets.harness).map_err(|e| e.to_string())?.build)(&HarnessSetup {
            executable: args.harness_executable,
            secrets: secrets.harness_secrets.clone(),
        });
    let runner = Runner {
        harness,
        codehost: Arc::new(GatewayCodeHost::new(
            gateway.clone(),
            Identity { name: args.author_name, email: args.author_email },
        )),
        workspace: Arc::new(GitWorkspace::with_found_remote(args.workdir, gateway.remote())),
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
    let mut protocol =
        DaemonProtocol::new(Arc::new(runner), control_plane).on(endpoint.clone()).build(build).secrets(secrets_id);
    if let Some(socket) = args.hold_socket.filter(|s| s.exists()) {
        protocol = protocol.holding(SpriteHolds::new(socket));
    }
    let router = Router::builder(endpoint).accept(ALPN, protocol).spawn();
    tokio::signal::ctrl_c().await.map_err(|e| e.to_string())?;
    router.shutdown().await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::{log_filter, read_secrets};

    #[test]
    fn malformed_secrets_error_does_not_echo_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.json");
        std::fs::write(&path, r#"{"secret_key":"hunter2-key","control_plane":["hunter2-key"]}"#).unwrap();
        let err = read_secrets(&path).unwrap_err();
        assert!(!err.contains("hunter2"), "{err}");
    }

    #[test]
    fn invalid_log_flag_is_a_usage_error() {
        let parse = |log: &str| {
            <super::Cli as clap::Parser>::try_parse_from([
                "trailhead-daemon",
                "--secrets",
                "s",
                "--workdir",
                "w",
                "--log",
                log,
            ])
        };
        assert!(parse("debug").is_ok());
        assert_eq!(parse("info,foo=nope").err().unwrap().kind(), clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn explicit_log_beats_rust_log() {
        assert_eq!(log_filter(Some("debug"), Some("warn")).to_string(), "debug");
    }

    #[test]
    fn rust_log_then_info_when_no_flag() {
        assert_eq!(log_filter(None, Some("warn")).to_string(), "warn");
        assert_eq!(log_filter(None, None).to_string(), "info");
    }

    fn executable(flags: &[&str]) -> std::path::PathBuf {
        let argv = ["trailhead-daemon", "--secrets", "s.json", "--workdir", "w"];
        let cli = <super::Cli as clap::Parser>::try_parse_from(argv.iter().chain(flags)).unwrap();
        cli.daemon.unwrap().harness_executable
    }

    #[test]
    fn harness_executable_flag_keeps_the_claude_alias() {
        assert_eq!(executable(&[]), std::path::PathBuf::from("claude"));
        assert_eq!(executable(&["--harness-executable", "/bin/h"]), std::path::PathBuf::from("/bin/h"));
        assert_eq!(executable(&["--claude", "/bin/c"]), std::path::PathBuf::from("/bin/c"));
    }

    #[test]
    fn invalid_rust_log_falls_back_to_info() {
        assert_eq!(log_filter(None, Some("foo=notalevel")).to_string(), "info");
        assert_eq!(log_filter(Some("=="), Some("warn")).to_string(), "info");
    }
}
