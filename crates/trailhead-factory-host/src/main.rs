//! The factory's control plane, run next to a local Trailhead server (`docs/factory/state.md`):
//!
//! ```sh
//! trailhead-factory login sprites   # also `claude` and `github`; or set SPRITES_TOKEN, CLAUDE_CODE_OAUTH_TOKEN, GITHUB_TOKEN
//! trailhead-factory run             # or `trailhead factory run`
//! ```
//!
//! The factory's settings are the `[factory]` table of each repo's `.trailhead.toml`: every `--repo <dir>`, else the `repos` in
//! `[factory]` of the user config, else the nearest one at or above the working directory,
//! or `--config <path>`. Repos share one process, so their `[factory.compute]` and `[factory.daemon]` must match.
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
use tracing::Instrument;
use trailhead_codehost_github::GitHub;
use trailhead_compute_sprites::SpritesCompute;
use trailhead_control::{CoreTracker, Factory, Host, daemon_binary};
use trailhead_core::{Ctx, Store};
use trailhead_daemon::net::{API_ALPN, ApiTunnel, Launch, RemoteDaemon};
use trailhead_factory::{Compute, ComputeConfig, FactoryConfig, Target};
use trailhead_provider_anthropic::Anthropic;
use trailhead_server::creds::{Cred, CredStore};

mod find;
mod login;
mod ops;
mod repos;
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

#[derive(clap::Args, Clone)]
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

/// What `check` and `run` take: the repos to work on, and the shared options.
#[derive(clap::Args, Clone)]
struct RunOpts {
    /// A repo directory whose `.trailhead.toml` has a `[factory]` table; repeat for several. Default: the `repos` in
    /// `[factory]` of the user config, else the repo at or above the working directory.
    #[arg(long, conflicts_with = "config")]
    repo: Vec<PathBuf>,
    #[command(flatten)]
    opts: Opts,
}

#[derive(Subcommand)]
enum Cmd {
    /// Set everything up and report, without running anything.
    Check(RunOpts),
    /// Run the control plane for every repo until Ctrl-C.
    Run(RunOpts),
    /// Show the sandbox pool, what is running and how queued tasks have gone. Read-only; runs beside `run`.
    Status(Opts),
    /// Print the daemon's service log from a task's sandbox.
    Logs(ops::LogsOpts),
    /// Save a secret in the OS keyring (read from stdin), so `run` needs no environment variables.
    #[command(subcommand)]
    Login(login::Login),
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

/// `opts` with the secrets the environment didn't set filled in from what `trailhead factory login` saved.
fn resolve_opts(opts: &Opts, paths: &trailhead_server::Paths) -> Opts {
    let creds = paths.creds();
    Opts {
        sprites_token: login::resolve(opts.sprites_token.clone(), &creds, login::SPRITES),
        github_token: login::resolve(opts.github_token.clone(), &creds, login::GITHUB),
        claude_oauth_token: login::resolve(opts.claude_oauth_token.clone(), &creds, login::CLAUDE),
        ..opts.clone()
    }
}

/// The factory file's path, the git remote beside it, and the `[factory]` settings read from it.
fn load_config(opts: &Opts) -> Result<(PathBuf, Option<trailhead_factory::Repo>, FactoryConfig), String> {
    let config_path = match &opts.config {
        Some(path) => path.clone(),
        None => find::config(&std::env::current_dir().map_err(|e| e.to_string())?)?,
    };
    let src = std::fs::read_to_string(&config_path).map_err(|e| format!("{}: {e}", config_path.display()))?;
    let repo_dir = config_path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_path_buf();
    let remote = find::remote(&repo_dir);
    let config =
        FactoryConfig::from_repo_file(&src, remote.clone()).map_err(|e| format!("{}: {e}", config_path.display()))?;
    Ok((config_path, remote, config))
}

/// One repo's factory, ready to run.
struct Entry {
    factory: Arc<Factory>,
    /// `owner/name`, which the repo's log lines carry as `repo=`.
    slug: String,
}

/// Everything `check` and `run` share: one store, agent, key set, iroh endpoint with its API tunnel, compute and
/// host, and a factory for each repo.
struct Setup {
    repos: Vec<Entry>,
    tunnel: Router,
    _lock: File,
    store: Arc<Store>,
    ctx: Ctx,
}

async fn setup(run: &RunOpts) -> Result<Setup, String> {
    let paths = trailhead_server::Paths::from_env().ok_or("can't find Trailhead's data and config directories")?;
    let opts = &resolve_opts(&run.opts, &paths);
    let user_config = paths.config_dir.join("config.toml");
    let host_config = match std::fs::read_to_string(&user_config) {
        Ok(src) => trailhead_factory::HostConfig::from_user_config(&src)
            .map_err(|e| format!("{}: {e}", user_config.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Default::default(),
        Err(e) => return Err(format!("{}: {e}", user_config.display())),
    };
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let dirs = repos::dirs(&run.repo, opts.config.as_deref(), &host_config, home.as_deref(), &cwd)?;
    let mut specs = repos::load_all(&dirs, opts.config.as_deref())?;

    let (pause_at, warnings) =
        host_config.effective_pause_at(specs.iter().map(|s| (s.config.repo.project.as_str(), &s.config.usage)));
    warnings.iter().for_each(|w| tracing::warn!("{w}"));
    for spec in &mut specs {
        spec.config.usage.pause_at = pause_at.clone();
        if opts.claude_oauth_token.is_some() {
            tracing::warn!(repo = %spec.slug(), "subscription mode: sandboxes hold a Claude OAuth token and can reach api.anthropic.com");
            spec.config.egress.always.push(ANTHROPIC_API_HOST.into());
        }
    }

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
    let slugs: Vec<String> = specs.iter().map(|s| s.slug()).collect();
    // Before the store opens or the endpoint binds, so a refused start changes nothing.
    let lock = lock_control_plane(&paths.data_dir, &slugs.join(", "))?;
    let db = paths.db();
    if let Some(dir) = db.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let store = Arc::new(Store::open_local(&db).await.map_err(|e| e.to_string())?);
    let ctx = factory_ctx(&store, opts.create_agent).await?;
    let tracker = Arc::new(CoreTracker::new(store.clone(), ctx.clone(), AGENT));
    let (key, master) = load_or_create_keys(&paths.creds())?;

    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(key)
        .bind()
        .await
        .map_err(|e| format!("binding the iroh endpoint: {e}"))?;
    // `repos::validate` made the compute and daemon settings the same for every repo.
    let shared = &specs[0].config;
    let binary = Arc::new(binary);
    let (compute, target) = compute(&shared.compute, opts, &paths, binary.clone())?;
    let launch = Launch {
        claude_oauth_token: opts.claude_oauth_token.clone(),
        log: shared.daemon.log.clone(),
        ..Launch::new(binary, &target)
    };
    let daemon = Arc::new(RemoteDaemon::new(endpoint.clone(), master, compute.clone(), launch));
    let tunnel =
        Router::builder(endpoint.clone()).accept(API_ALPN, ApiTunnel::new(opts.server, daemon.peers())).spawn();
    tracing::info!(endpoint = %endpoint.id(), repos = %slugs.join(", "), "factory ready");

    let host = Host::new(host_config.max_sandboxes);
    let mut repos = Vec::new();
    for spec in specs {
        let slug = spec.slug();
        let repos::RepoSpec { config_path, remote, config, .. } = spec;
        let model_endpoint = target.model_endpoint(&opts.model_endpoint).to_string();
        let mut factory = Factory::new(
            config,
            compute.clone(),
            tracker.clone(),
            Arc::new(Anthropic::new(model_endpoint)),
            daemon.clone(),
            host.clone(),
        )
        .with_server_check(server_check(opts.server))
        .with_merge_mode_source(Box::new(move || {
            let src = std::fs::read_to_string(&config_path).map_err(|e| format!("{}: {e}", config_path.display()))?;
            FactoryConfig::from_repo_file(&src, remote.clone())
                .map(|c| c.merge.mode)
                .map_err(|e| format!("{}: {e}", config_path.display()))
        }));
        match &opts.github_token {
            Some(token) => {
                factory = factory
                    .with_codehost(Arc::new(GitHub::new(trailhead_codehost_github::API_URL, Some(token.clone()))))
            }
            None => tracing::warn!(
                repo = %slug,
                "no GitHub token (GITHUB_TOKEN or `trailhead factory login github`): merged and closed PRs are not noticed; finish merged tasks by hand"
            ),
        }
        repos.push(Entry { factory: Arc::new(factory), slug });
    }
    Ok(Setup { repos, tunnel, _lock: lock, store, ctx })
}

/// Resolves once the watch channel it came from is set.
async fn fired(mut rx: tokio::sync::watch::Receiver<bool>) {
    let _ = rx.wait_for(|set| *set).await;
}

/// One repo's run loop, with the wake-up watcher for its project. Whether it stopped on an error (already logged).
async fn run_repo(
    entry: Entry,
    store: Arc<Store>,
    ctx: Ctx,
    poll: Duration,
    drain: tokio::sync::watch::Receiver<bool>,
    cancel: tokio::sync::watch::Receiver<bool>,
) -> bool {
    let wake = Arc::new(Notify::new());
    let project = entry.factory.config.repo.project.clone();
    let watcher = tokio::spawn(
        wake::wake_on_changes(store, ctx, project, wake.clone(), wake::INTERVAL).instrument(tracing::Span::current()),
    );
    let ran = entry.factory.clone().run_draining(wake, poll, fired(drain), fired(cancel)).await;
    watcher.abort();
    match ran {
        Ok(()) => false,
        Err(e) => {
            tracing::error!(error = %e, "this repo's loop stopped; the other repos carry on");
            true
        }
    }
}

/// Whether something accepts connections at the server's address, which the daemons' API tunnel forwards to.
fn server_check(server: SocketAddr) -> trailhead_control::ServerCheck {
    Box::new(move || {
        Box::pin(async move {
            match tokio::time::timeout(Duration::from_secs(3), tokio::net::TcpStream::connect(server)).await {
                Ok(Ok(_)) => Ok(()),
                Ok(Err(e)) => Err(format!("can't connect to {server}: {e}")),
                Err(_) => Err(format!("connecting to {server} timed out")),
            }
        })
    })
}

/// The sandboxes' `Compute`, and where the daemon lives in them.
#[cfg_attr(not(feature = "docker"), allow(unused_variables))]
fn compute(
    config: &ComputeConfig,
    opts: &Opts,
    paths: &trailhead_server::Paths,
    binary: Arc<Vec<u8>>,
) -> Result<(Arc<dyn Compute>, Target), String> {
    match config {
        ComputeConfig::Sprites => {
            let token = opts
                .sprites_token
                .clone()
                .ok_or("Sprites compute needs SPRITES_TOKEN or `trailhead factory login sprites`")?;
            Ok((Arc::new(SpritesCompute::new(token).map_err(|e| e.to_string())?), trailhead_compute_sprites::target()))
        }
        #[cfg(feature = "docker")]
        ComputeConfig::Docker { image, network, runtime } => {
            use trailhead_compute_docker::{DockerCompute, DockerConfig};
            let mut docker = DockerConfig::new(paths.data_dir.join("factory").join("docker"), binary);
            if let Some(image) = image {
                docker.image = image.clone();
            }
            if let Some(network) = network {
                docker.network = network.clone();
            }
            docker.runtime = runtime.clone();
            docker.connections = gateway_connections(login::github_writer_token(&paths.creds()));
            if docker.connections.is_empty() {
                tracing::warn!(
                    "no GitHub writer token (GITHUB_WRITER_TOKEN or `trailhead factory login github-writer`): Docker sandboxes can't publish"
                );
            }
            if opts.claude_oauth_token.is_none() {
                tracing::warn!(
                    "no Claude subscription token: Docker sandboxes have no model access until the gateway holds a credential for it"
                );
            }
            let compute = DockerCompute::new(docker).map_err(|e| e.to_string())?;
            Ok((Arc::new(compute), trailhead_compute_docker::target()))
        }
        #[cfg(not(feature = "docker"))]
        ComputeConfig::Docker { .. } => {
            Err("Docker compute needs trailhead-factory built with `--features docker`".into())
        }
    }
}

/// What the Docker gateway may forward to. GitHub only: subscription mode reaches the model API with the sandbox's
/// own token, through the gateway's proxy (TRAIL-487).
#[cfg(feature = "docker")]
fn gateway_connections(github_writer: Option<String>) -> Vec<trailhead_factory::GatewayConnection> {
    use trailhead_factory::{AuthScheme, GATEWAY_GITHUB_ID, GatewayConnection, Secret};
    github_writer
        .map(|token| GatewayConnection {
            provider: "github".into(),
            id: GATEWAY_GITHUB_ID.into(),
            upstream: trailhead_codehost_github::API_URL.into(),
            auth: AuthScheme::Bearer,
            credential: Secret::new(token),
        })
        .into_iter()
        .collect()
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
        Cmd::Check(run) => {
            let setup = setup(&run).await?;
            let mut failed = 0;
            for entry in &setup.repos {
                let factory = &entry.factory;
                let project = &factory.config.repo.project;
                let queue = factory.config.repo.queue.clone().map(trailhead_factory::TaskKey);
                match factory.tracker.ready(project, queue.as_ref(), 10).await {
                    Ok(ready) => {
                        let scope = queue.as_ref().map_or(project.clone(), |q| format!("{project}, under {}", q.0));
                        println!(
                            "{}: tracker: acting as {AGENT}; {} ready task(s) in {scope}",
                            entry.slug,
                            ready.len()
                        );
                        for t in &ready {
                            println!("  {} {}", t.key.0, t.title);
                        }
                    }
                    Err(e) => {
                        failed += 1;
                        println!("{}: tracker: acting as {AGENT}; {e}", entry.slug);
                    }
                }
            }
            let first = &setup.repos[0].factory;
            match first.compute.list(&Default::default()).await {
                Ok(live) => println!(
                    "compute: {} reachable; {} sandbox(es) visible",
                    compute_name(&first.config.compute),
                    live.len()
                ),
                Err(e) => {
                    failed += 1;
                    println!("compute: {e}");
                }
            }
            println!("endpoint: {}", setup.tunnel.endpoint().id());
            println!("tunnel: forwarding to {}", run.opts.server);
            setup.tunnel.shutdown().await.map_err(|e| e.to_string())?;
            check_result(failed)
        }
        Cmd::Status(opts) => ops::status(&opts).await,
        Cmd::Logs(logs) => ops::logs(&logs).await,
        Cmd::Login(cmd) => {
            let paths =
                trailhead_server::Paths::from_env().ok_or("can't find Trailhead's data and config directories")?;
            login::run(cmd, &paths.creds()).await
        }
        Cmd::Run(run) => {
            let Setup { repos, tunnel, _lock, store, ctx } = setup(&run).await?;
            // The first Ctrl-C drains; the second cancels what is left.
            let (drain_tx, drain) = tokio::sync::watch::channel(false);
            let (cancel_tx, cancel) = tokio::sync::watch::channel(false);
            tokio::spawn(async move {
                let _ = tokio::signal::ctrl_c().await;
                let _ = drain_tx.send(true);
                let _ = tokio::signal::ctrl_c().await;
                tracing::info!("stopping; runs in flight are requeued on the next start");
                let _ = cancel_tx.send(true);
            });
            let poll = Duration::from_secs(run.opts.poll_secs);
            let mut loops = tokio::task::JoinSet::new();
            for entry in repos {
                let span = tracing::info_span!("repo", repo = %entry.slug);
                let (store, ctx, drain, cancel) = (store.clone(), ctx.clone(), drain.clone(), cancel.clone());
                loops.spawn(run_repo(entry, store, ctx, poll, drain, cancel).instrument(span));
            }
            let mut failed = 0;
            while let Some(joined) = loops.join_next().await {
                match joined {
                    Ok(false) => {}
                    Ok(true) => failed += 1,
                    Err(e) => {
                        failed += 1;
                        tracing::error!(error = %e, "a repo's loop panicked; the other repos carry on");
                    }
                }
            }
            tunnel.shutdown().await.map_err(|e| e.to_string())?;
            match failed {
                0 => Ok(()),
                n => Err(format!("{n} repo loop(s) stopped on an error")),
            }
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
    use super::{check_result, lock_control_plane, server_check};

    #[tokio::test]
    async fn server_check_sees_whether_something_listens() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        assert_eq!(server_check(addr)().await, Ok(()));
        drop(listener);
        assert!(server_check(addr)().await.unwrap_err().contains("can't connect"));
    }

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
