//! The factory's control plane, run next to a local Trailhead server (`docs/factory/state.md`):
//!
//! ```sh
//! SPRITES_TOKEN=... trailhead-factory run --config factory.toml --daemon target/release/trailhead-daemon
//! ```
//!
//! Sandboxes run on Sprites unless the factory file sets `[compute] kind = "docker"`, which needs this binary built
//! with the `docker` feature.
//!
//! It opens the server's database as the `factory` agent, keeps its iroh identity and daemon master key in the
//! credential store (so a restart can still reach the sandboxes it started), serves the API tunnel to the server
//! and runs the factory loop. `check` does everything up to the loop and prints what it found.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use iroh::endpoint::presets;
use iroh::protocol::Router;
use iroh::{Endpoint, SecretKey};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use trailhead_compute_sprites::SpritesCompute;
use trailhead_control::{CoreTracker, Factory};
use trailhead_core::{Ctx, Store};
use trailhead_daemon::net::{API_ALPN, ApiTunnel, Launch, RemoteDaemon};
use trailhead_factory::{Compute, ComputeConfig, FactoryConfig};
use trailhead_provider_anthropic::Anthropic;
use trailhead_server::creds::{Cred, CredStore};

/// The agent the control plane acts as; sandboxes get its sub-agents.
const AGENT: &str = "factory";
/// Where the control plane's iroh key and daemon master key are kept.
const KEYS: Cred<'static> = Cred::Secret("factory-keys");
/// What subscription mode adds to agent egress.
const ANTHROPIC_API_HOST: &str = "api.anthropic.com";

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Args)]
struct Opts {
    /// The repo's factory file (`docs/factory/sandbox.md`, `agents.md`, `review.md`).
    #[arg(long)]
    config: PathBuf,
    /// The daemon binary started in each sandbox, built for Linux x86_64.
    #[arg(long, default_value = "target/release/trailhead-daemon")]
    daemon: PathBuf,
    /// The local Trailhead server the API tunnel forwards to.
    #[arg(long, default_value = trailhead_server::DEFAULT_BIND)]
    server: SocketAddr,
    /// Where sandboxes send model calls: a connector's gateway URL, or the API.
    #[arg(long, default_value = trailhead_provider_anthropic::API_URL)]
    model_endpoint: String,
    /// Needed when sandboxes run on Sprites.
    #[arg(long, env = "SPRITES_TOKEN", hide_env_values = true)]
    sprites_token: Option<String>,
    /// How often to look for ready work.
    #[arg(long, default_value_t = 30)]
    poll_secs: u64,
    /// Subscription mode: run Claude Code in sandboxes on this OAuth token (`claude setup-token`) instead of a model
    /// connector. Each sandbox holds the token, and egress opens to api.anthropic.com. For Lane's own use.
    #[arg(long, env = "CLAUDE_CODE_OAUTH_TOKEN", hide_env_values = true)]
    claude_oauth_token: Option<String>,
    /// Create the `factory` agent if it's missing, as the local user. Otherwise Lane creates it with
    /// `trailhead --as-me agent add factory`, since agent identities are the human's to hand out.
    #[arg(long)]
    create_agent: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Set everything up and report, without running anything.
    Check(Opts),
    /// Run the control plane until Ctrl-C.
    Run(Opts),
}

#[derive(Serialize, Deserialize)]
struct Keys {
    endpoint: String,
    master: String,
}

fn load_or_create_keys(creds: &CredStore) -> Result<(SecretKey, [u8; 32]), String> {
    if let Some((saved, _)) = creds.read(KEYS) {
        let keys: Keys = serde_json::from_str(&saved).map_err(|e| format!("bad saved factory keys: {e}"))?;
        let endpoint = trailhead_daemon::net::parse_secret_key(&keys.endpoint).map_err(|e| e.to_string())?;
        let master = hex::decode(&keys.master).ok().and_then(|b| b.try_into().ok()).ok_or("bad saved master key")?;
        return Ok((endpoint, master));
    }
    let (endpoint, master) = (SecretKey::generate(), SecretKey::generate().to_bytes());
    let keys = Keys { endpoint: hex::encode(endpoint.to_bytes()), master: hex::encode(master) };
    creds.save(KEYS, &serde_json::to_string(&keys).expect("keys serialize")).map_err(|e| e.to_string())?;
    tracing::info!("created the factory's keys; losing them strands its running sandboxes");
    Ok((endpoint, master))
}

/// The `factory` agent's context, looked up through the local user.
async fn factory_ctx(store: &Store, create: bool) -> Result<Ctx, String> {
    let user = std::env::var("USER").unwrap_or_else(|_| "me".into());
    let (ws, me) = store.bootstrap_local(&user).await.map_err(|e| e.to_string())?;
    let human = Ctx::user(ws.id, me.id);
    let agent = match store.list_agents(&human).await.map_err(|e| e.to_string())?.into_iter().find(|a| a.name == AGENT)
    {
        Some(a) => a,
        None if create => store.create_agent(&human, AGENT).await.map_err(|e| e.to_string())?,
        None => return Err(format!("no `{AGENT}` agent yet; create it with `trailhead --as-me agent add {AGENT}`")),
    };
    store.agent_ctx(ws.id, agent.id).await.map_err(|e| e.to_string())
}

async fn setup(opts: &Opts) -> Result<(Arc<Factory>, Router), String> {
    let config = std::fs::read_to_string(&opts.config).map_err(|e| format!("{}: {e}", opts.config.display()))?;
    let mut config = FactoryConfig::parse(&config).map_err(|e| e.to_string())?;
    if opts.claude_oauth_token.is_some() {
        tracing::warn!("subscription mode: sandboxes hold a Claude OAuth token and can reach api.anthropic.com");
        config.egress.always.push(ANTHROPIC_API_HOST.into());
    }
    let binary = std::fs::read(&opts.daemon).map_err(|e| format!("daemon binary {}: {e}", opts.daemon.display()))?;

    let paths = trailhead_server::Paths::from_env().ok_or("can't find Trailhead's data and config directories")?;
    let db = paths.db();
    if let Some(dir) = db.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let store = Arc::new(Store::open_local(&db).await.map_err(|e| e.to_string())?);
    let tracker = CoreTracker::new(store.clone(), factory_ctx(&store, opts.create_agent).await?, AGENT);
    let (key, master) = load_or_create_keys(&paths.creds())?;

    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(key)
        .bind()
        .await
        .map_err(|e| format!("binding the iroh endpoint: {e}"))?;
    let (compute, launch) = compute(&config.compute, opts, &paths, Launch::new(binary))?;
    let launch = Launch { claude_oauth_token: opts.claude_oauth_token.clone(), ..launch };
    let daemon = Arc::new(RemoteDaemon::new(endpoint.clone(), master, compute.clone(), launch));
    let tunnel =
        Router::builder(endpoint.clone()).accept(API_ALPN, ApiTunnel::new(opts.server, daemon.peers())).spawn();
    tracing::info!(endpoint = %endpoint.id(), project = %config.repo.project, repo = %config.repo.repo().slug(), "factory ready");

    let factory =
        Factory::new(config, compute, Arc::new(tracker), Arc::new(Anthropic::new(opts.model_endpoint.clone())), daemon);
    Ok((Arc::new(factory), tunnel))
}

/// The sandboxes' `Compute`, and where the daemon lives in them.
#[cfg_attr(not(feature = "docker"), allow(unused_variables))]
fn compute(
    config: &ComputeConfig,
    opts: &Opts,
    paths: &trailhead_server::Paths,
    launch: Launch,
) -> Result<(Arc<dyn Compute>, Launch), String> {
    match config {
        ComputeConfig::Sprites => {
            let token = opts.sprites_token.clone().ok_or("Sprites compute needs SPRITES_TOKEN")?;
            Ok((Arc::new(SpritesCompute::new(token).map_err(|e| e.to_string())?), launch))
        }
        #[cfg(feature = "docker")]
        ComputeConfig::Docker { image, network, runtime } => {
            use trailhead_compute_docker::{DockerCompute, DockerConfig, HOME};
            let mut docker = DockerConfig::new(paths.data_dir.join("factory").join("docker"));
            if let Some(image) = image {
                docker.image = image.clone();
            }
            if let Some(network) = network {
                docker.network = network.clone();
            }
            docker.runtime = runtime.clone();
            let compute = DockerCompute::new(docker).map_err(|e| e.to_string())?;
            let launch = Launch { dir: format!("{HOME}/.trailhead"), workdir: format!("{HOME}/work"), ..launch };
            Ok((Arc::new(compute), launch))
        }
        #[cfg(not(feature = "docker"))]
        ComputeConfig::Docker { .. } => {
            Err("Docker compute needs trailhead-factory built with `--features docker`".into())
        }
    }
}

fn compute_name(config: &ComputeConfig) -> &'static str {
    match config {
        ComputeConfig::Sprites => "Sprites",
        ComputeConfig::Docker { .. } => "Docker",
    }
}

fn check_result(failed: usize) -> Result<(), String> {
    match failed {
        0 => Ok(()),
        1 => Err("1 check failed".into()),
        n => Err(format!("{n} checks failed")),
    }
}

async fn main_inner(cli: Cli) -> Result<(), String> {
    match cli.cmd {
        Cmd::Check(opts) => {
            let (factory, tunnel) = setup(&opts).await?;
            let project = &factory.config.repo.project;
            let queue = factory.config.repo.queue.clone().map(trailhead_factory::TaskKey);
            let mut failed = 0;
            match factory.tracker.ready(project, queue.as_ref(), 10).await {
                Ok(ready) => {
                    let scope = queue.as_ref().map_or(project.clone(), |q| format!("{project}, under {}", q.0));
                    println!("tracker: acting as {AGENT}; {} ready task(s) in {scope}", ready.len());
                    for t in &ready {
                        println!("  {} {}", t.key.0, t.title);
                    }
                }
                Err(e) => {
                    failed += 1;
                    println!("tracker: acting as {AGENT}; {e}");
                }
            }
            match factory.compute.list(&Default::default()).await {
                Ok(live) => println!(
                    "compute: {} reachable; {} sandbox(es) visible",
                    compute_name(&factory.config.compute),
                    live.len()
                ),
                Err(e) => {
                    failed += 1;
                    println!("compute: {e}");
                }
            }
            println!("endpoint: {}", tunnel.endpoint().id());
            println!("tunnel: forwarding to {}", opts.server);
            tunnel.shutdown().await.map_err(|e| e.to_string())?;
            check_result(failed)
        }
        Cmd::Run(opts) => {
            let (factory, tunnel) = setup(&opts).await?;
            let shutdown = async {
                let _ = tokio::signal::ctrl_c().await;
                tracing::info!("stopping; runs in flight are requeued on the next start");
            };
            let ran = factory.run(Arc::new(Notify::new()), Duration::from_secs(opts.poll_secs), shutdown).await;
            tunnel.shutdown().await.map_err(|e| e.to_string())?;
            ran.map_err(|e| e.to_string())
        }
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    tracing_subscriber::fmt().with_env_filter(filter).init();
    match main_inner(Cli::parse()).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("trailhead-factory: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::check_result;

    #[test]
    fn check_result_counts_failures() {
        assert_eq!(check_result(0), Ok(()));
        assert_eq!(check_result(1), Err("1 check failed".into()));
        assert_eq!(check_result(2), Err("2 checks failed".into()));
    }
}
