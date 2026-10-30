//! The factory's control plane, run next to a local Trailhead server (`docs/factory/state.md`):
//!
//! ```sh
//! SPRITES_TOKEN=... trailhead-factory run   # or `trailhead factory run`
//! ```
//!
//! The factory's settings are the `[factory]` table of the nearest `.trailhead.toml` at or above the working directory,
//! or of `--config <path>`.
//!
//! The daemon uploaded to sandboxes is `--daemon <path>`, else a `trailhead-daemon` next to this binary or on PATH,
//! else the release asset matching this binary's version.
//!
//! Sandboxes run on Sprites unless the factory file sets `[factory.compute] kind = "docker"`, which needs this binary built
//! with the `docker` feature.
//!
//! It opens the server's database as the `factory` agent, keeps its iroh identity and daemon master key in the
//! credential store (so a restart can still reach the sandboxes it started), serves the API tunnel to the server
//! and runs the factory loop. `check` does everything up to the loop and prints what it found.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use iroh::endpoint::presets;
use iroh::protocol::Router;
use iroh::{Endpoint, SecretKey};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use trailhead_codehost_github::GitHub;
use trailhead_compute_sprites::SpritesCompute;
use trailhead_control::{CoreTracker, Factory, daemon_binary};
use trailhead_core::{Ctx, Store};
use trailhead_daemon::net::{API_ALPN, ApiTunnel, Launch, RemoteDaemon};
use trailhead_factory::{Compute, ComputeConfig, FactoryConfig};
use trailhead_provider_anthropic::Anthropic;
use trailhead_server::creds::{Cred, CredStore};

mod find;
mod wake;

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
    /// The repo file with the `[factory]` table (`docs/factory/sandbox.md`, `agents.md`, `review.md`). Default: the
    /// nearest `.trailhead.toml` at or above the working directory.
    #[arg(long)]
    config: Option<PathBuf>,
    /// A daemon binary to start in each sandbox, built for Linux x86_64. Default: a `trailhead-daemon` next to this
    /// binary or on PATH, else the release asset for this version.
    #[arg(long)]
    daemon: Option<PathBuf>,
    /// The local Trailhead server the API tunnel forwards to.
    #[arg(long, default_value = trailhead_server::DEFAULT_BIND)]
    server: SocketAddr,
    /// Where sandboxes send model calls: a connector's gateway URL, or the API.
    #[arg(long, default_value = trailhead_provider_anthropic::API_URL)]
    model_endpoint: String,
    /// Needed when sandboxes run on Sprites.
    #[arg(long, env = "SPRITES_TOKEN", hide_env_values = true)]
    sprites_token: Option<String>,
    /// Lets the control plane see when a task's PR is merged or closed, to finish or park the task. Read access to
    /// the repo's pull requests is enough. Without it, merges are not noticed and `trailhead done` stays manual.
    #[arg(long, env = "GITHUB_TOKEN", hide_env_values = true)]
    github_token: Option<String>,
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

/// Takes the machine's control-plane lock, held until the returned file drops (or the process dies). Every control
/// plane on a machine shares one iroh identity (`load_or_create_keys`), so two at once, for any repos, flap each
/// other's daemon connections and reconcile each other's busy sandboxes as interrupted.
fn lock_control_plane(data_dir: &Path, repo: &str) -> Result<File, String> {
    let dir = data_dir.join("factory");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join("control.lock");
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            let mut holder = String::new();
            let _ = file.read_to_string(&mut holder);
            let who = match holder.trim().split_once(' ') {
                Some((pid, repo)) => format!("pid {pid}, running {repo}"),
                None if !holder.trim().is_empty() => format!("pid {}", holder.trim()),
                None => "another process".to_string(),
            };
            return Err(format!(
                "a control plane is already running on this machine ({who}, lock {}); stop it first",
                path.display()
            ));
        }
        Err(TryLockError::Error(e)) => return Err(format!("{}: {e}", path.display())),
    }
    file.set_len(0).and_then(|()| file.rewind()).map_err(|e| format!("{}: {e}", path.display()))?;
    write!(file, "{} {repo}", std::process::id()).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(file)
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

/// What the run loop's wake-up watcher reads the project's changes with.
struct Wake {
    store: Arc<Store>,
    ctx: Ctx,
    project: String,
}

async fn setup(opts: &Opts) -> Result<(Arc<Factory>, Router, File, Wake), String> {
    let config_path = match &opts.config {
        Some(path) => path.clone(),
        None => find::config(&std::env::current_dir().map_err(|e| e.to_string())?)?,
    };
    let config = std::fs::read_to_string(&config_path).map_err(|e| format!("{}: {e}", config_path.display()))?;
    let repo_dir = config_path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_path_buf();
    let mut config = FactoryConfig::from_repo_file(&config, find::remote(&repo_dir))
        .map_err(|e| format!("{}: {e}", config_path.display()))?;
    if opts.claude_oauth_token.is_some() {
        tracing::warn!("subscription mode: sandboxes hold a Claude OAuth token and can reach api.anthropic.com");
        config.egress.always.push(ANTHROPIC_API_HOST.into());
    }

    let paths = trailhead_server::Paths::from_env().ok_or("can't find Trailhead's data and config directories")?;
    let release = daemon_binary::Release {
        base_url: daemon_binary::GITHUB.into(),
        repo: env!("CARGO_PKG_REPOSITORY").trim_start_matches("https://github.com/").into(),
        version: env!("CARGO_PKG_VERSION").into(),
    };
    let beside = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf));
    let path_dirs: Vec<PathBuf> =
        std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    let local = daemon_binary::find_local(beside.as_deref(), &path_dirs);
    let binary = daemon_binary::resolve(
        opts.daemon.as_deref(),
        local.as_deref(),
        &release,
        &daemon_binary::cache_dir(&paths.data_dir),
    )
    .await?;
    // Before the store opens or the endpoint binds, so a refused start changes nothing.
    let lock = lock_control_plane(&paths.data_dir, &config.repo.repo().slug())?;
    let db = paths.db();
    if let Some(dir) = db.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let store = Arc::new(Store::open_local(&db).await.map_err(|e| e.to_string())?);
    let ctx = factory_ctx(&store, opts.create_agent).await?;
    let tracker = CoreTracker::new(store.clone(), ctx.clone(), AGENT);
    let (key, master) = load_or_create_keys(&paths.creds())?;

    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(key)
        .bind()
        .await
        .map_err(|e| format!("binding the iroh endpoint: {e}"))?;
    let (compute, launch) = compute(&config.compute, opts, &paths, Launch::new(binary))?;
    let launch =
        Launch { claude_oauth_token: opts.claude_oauth_token.clone(), log: config.daemon.log.clone(), ..launch };
    let daemon = Arc::new(RemoteDaemon::new(endpoint.clone(), master, compute.clone(), launch));
    let tunnel =
        Router::builder(endpoint.clone()).accept(API_ALPN, ApiTunnel::new(opts.server, daemon.peers())).spawn();
    tracing::info!(endpoint = %endpoint.id(), project = %config.repo.project, repo = %config.repo.repo().slug(), "factory ready");

    let mut factory =
        Factory::new(config, compute, Arc::new(tracker), Arc::new(Anthropic::new(opts.model_endpoint.clone())), daemon);
    match &opts.github_token {
        Some(token) => {
            factory =
                factory.with_codehost(Arc::new(GitHub::new(trailhead_codehost_github::API_URL, Some(token.clone()))))
        }
        None => tracing::warn!("no GITHUB_TOKEN: merged and closed PRs are not noticed; finish merged tasks by hand"),
    }
    let wake = Wake { store, ctx, project: factory.config.repo.project.clone() };
    Ok((Arc::new(factory), tunnel, lock, wake))
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
            let (factory, tunnel, _lock, _) = setup(&opts).await?;
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
            let (factory, tunnel, _lock, source) = setup(&opts).await?;
            let wake = Arc::new(Notify::new());
            let watcher = tokio::spawn(wake::wake_on_changes(
                source.store,
                source.ctx,
                source.project,
                wake.clone(),
                wake::INTERVAL,
            ));
            // The first Ctrl-C drains; the second cancels what is left.
            let (drain_tx, drain) = tokio::sync::oneshot::channel::<()>();
            let (cancel_tx, cancel) = tokio::sync::oneshot::channel::<()>();
            tokio::spawn(async move {
                let _ = tokio::signal::ctrl_c().await;
                let _ = drain_tx.send(());
                let _ = tokio::signal::ctrl_c().await;
                tracing::info!("stopping; runs in flight are requeued on the next start");
                let _ = cancel_tx.send(());
            });
            let ran = factory
                .run_draining(
                    wake,
                    Duration::from_secs(opts.poll_secs),
                    async {
                        let _ = drain.await;
                    },
                    async {
                        let _ = cancel.await;
                    },
                )
                .await;
            watcher.abort();
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
    use super::{check_result, lock_control_plane};

    #[test]
    fn second_control_plane_is_refused_naming_the_holder() {
        let dir = tempfile::tempdir().unwrap();
        let held = lock_control_plane(dir.path(), "acme/app").unwrap();
        let err = lock_control_plane(dir.path(), "acme/other").unwrap_err();
        assert!(err.contains(&format!("pid {}, running acme/app", std::process::id())), "{err}");
        drop(held);
        assert!(lock_control_plane(dir.path(), "acme/other").is_ok());
    }

    #[test]
    fn check_result_counts_failures() {
        assert_eq!(check_result(0), Ok(()));
        assert_eq!(check_result(1), Err("1 check failed".into()));
        assert_eq!(check_result(2), Err("2 checks failed".into()));
    }
}
