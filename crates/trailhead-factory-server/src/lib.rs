//! The factory's control plane, as a library (`docs/factory/server-control-plane.md`): the Trailhead server runs it
//! behind its `factory` feature ([`start`]), and `trailhead factory <cmd>` ([`Command`]) checks and inspects it.
//!
//! The factory's settings are the `[factory]` table of each repo's `.trailhead.toml`. The server runs the `repos` in
//! `[factory]` of the user config; `check` takes every `--repo <dir>`, else those, else the nearest one at or above
//! the working directory, or `--config <path>`. Repos share one process, so their `[factory.compute]` and
//! `[factory.daemon]` must match.
//!
//! The daemon uploaded to sandboxes is `--daemon <path>` (`TRAILHEAD_DAEMON`), else a `trailhead-daemon` next to
//! this binary or on PATH, else the release asset matching this binary's version.
//!
//! Sandboxes run on Sprites unless the factory file sets `[factory.compute] kind = "docker"`, which needs the `docker`
//! feature.
//!
//! It opens the server's database as the `factory` agent, keeps its iroh identity and daemon master key in the
//! credential store (so a restart can still reach the sandboxes it started), serves the API tunnel to the server
//! and runs the factory loop. [`check`] does everything up to the loop and prints what it found.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::presets;
use iroh::protocol::Router;
use iroh::{Endpoint, SecretKey};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::Instrument;
use trailhead_client::creds::{Cred, CredStore};
use trailhead_client::paths::{DEFAULT_BIND, Paths};
use trailhead_codehost_github::GitHub;
use trailhead_control::{CoreTracker, Factory, Host, Placed, daemon_binary};
use trailhead_core::{Ctx, Store};
use trailhead_factory::{
    Compute, ComputeConfig, Daemon, FactoryConfig, FactoryState, HarnessSecrets, HostConfig, Target,
};
use trailhead_plugins::{CLAUDE_CODE, CLAUDE_CODE_OAUTH_TOKEN, harnesses, providers};
use trailhead_wire::api::ApiTunnel;
use trailhead_wire::daemon::{Launch, RemoteDaemon};
use trailhead_wire::ui::{UI_ALPN, UiAllowlist, UiProtocol};
use trailhead_wire::{API_ALPN, build_id};

mod error;
mod find;
mod fnox;
mod logfile;
pub mod login;
pub mod ops;
mod repos;
mod surface;

pub use error::{Error, Result};
pub use surface::Feed;
mod wake;

/// The agent the control plane acts as; sandboxes get its sub-agents.
const AGENT: &str = "factory";
/// Where the control plane's iroh key and daemon master key are kept.
const KEYS: Cred<'static> = Cred::Secret("factory-keys");

#[derive(clap::Args, Clone)]
pub struct Opts {
    /// The repo file with the `[factory]` table (`docs/factory/sandbox.md`, `agents.md`, `review.md`). Default: the
    /// nearest `.trailhead.toml` at or above the working directory.
    #[arg(long)]
    config: Option<PathBuf>,
    /// A daemon binary to start in each sandbox, built for Linux x86_64. Default: a `trailhead-daemon` next to this
    /// binary or on PATH, else the release asset for this version.
    #[arg(long, env = "TRAILHEAD_DAEMON")]
    daemon: Option<PathBuf>,
    /// The local Trailhead server the API tunnel forwards to.
    #[arg(long, default_value = DEFAULT_BIND)]
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
    /// How often the run loops look for ready work and reconcile, besides waking on each commit.
    #[arg(long, env = "TRAILHEAD_FACTORY_POLL_SECS", default_value_t = 30)]
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
pub struct RunOpts {
    /// A repo directory whose `.trailhead.toml` has a `[factory]` table; repeat for several. Default: the `repos` in
    /// `[factory]` of the user config, else the repo at or above the working directory.
    #[arg(long, conflicts_with = "config")]
    repo: Vec<PathBuf>,
    #[command(flatten)]
    opts: Opts,
}

#[derive(Serialize, Deserialize)]
struct Keys {
    endpoint: String,
    master: String,
}

/// The control plane's endpoint id, if it has run here and saved its keys. Read-only, for `status`.
fn saved_endpoint_id(creds: &CredStore) -> Option<iroh::EndpointId> {
    let (saved, _) = creds.read(KEYS)?;
    let keys: Keys = serde_json::from_str(&saved).ok()?;
    Some(trailhead_wire::parse_secret_key(&keys.endpoint).ok()?.public())
}

fn load_or_create_keys(creds: &CredStore) -> Result<(SecretKey, [u8; 32])> {
    if let Some((saved, _)) = creds.read(KEYS) {
        let keys: Keys = serde_json::from_str(&saved).map_err(Error::BadKeys)?;
        let endpoint = trailhead_wire::parse_secret_key(&keys.endpoint).map_err(Error::backend)?;
        let master = hex::decode(&keys.master).ok().and_then(|b| b.try_into().ok()).ok_or(Error::BadMasterKey)?;
        return Ok((endpoint, master));
    }
    let (endpoint, master) = (SecretKey::generate(), SecretKey::generate().to_bytes());
    let keys = Keys { endpoint: hex::encode(endpoint.to_bytes()), master: hex::encode(master) };
    creds.save(KEYS, &serde_json::to_string(&keys).expect("keys serialize")).map_err(Error::backend)?;
    tracing::info!("created the factory's keys; losing them strands its running sandboxes");
    Ok((endpoint, master))
}

fn ui_allowlist(paths: &Paths) -> UiAllowlist {
    UiAllowlist::new(paths.data_dir.join("factory").join("ui-allowed"))
}

fn allowlist() -> Result<UiAllowlist> {
    let paths = Paths::from_env().ok_or(Error::NoDirs)?;
    Ok(ui_allowlist(&paths))
}

/// `allow` (`add`) or `disallow` a UI endpoint id.
pub fn allow(endpoint_id: &str, add: bool) -> Result<()> {
    let id = trailhead_wire::parse_endpoint_id(endpoint_id).map_err(Error::backend)?;
    let list = allowlist()?;
    let changed = if add { list.add(id) } else { list.remove(&id) }.map_err(Error::backend)?;
    match (add, changed) {
        (true, true) => println!("allowed {id}"),
        (true, false) => println!("{id} was already allowed"),
        (false, true) => println!("no longer allowed: {id}"),
        (false, false) => println!("{id} wasn't allowed"),
    }
    Ok(())
}

/// Prints the UI endpoint ids allowed to connect.
pub fn allowed() -> Result<()> {
    let ids = allowlist()?.list().map_err(Error::backend)?;
    if ids.is_empty() {
        println!("no UI endpoints are allowed; add one with `trailhead factory allow <endpoint-id>`");
    }
    ids.iter().for_each(|id| println!("{id}"));
    Ok(())
}

/// The `factory` agent's context, looked up through the local user.
async fn factory_ctx(store: &Store, create: bool) -> Result<Ctx> {
    let user = std::env::var("USER").unwrap_or_else(|_| "me".into());
    let (ws, me) = store.bootstrap_local(&user).await.map_err(Error::backend)?;
    let human = Ctx::user(ws.id, me.id);
    let agent = match store.list_agents(&human).await.map_err(Error::backend)?.into_iter().find(|a| a.name == AGENT) {
        Some(a) => a,
        None if create => store.create_agent(&human, AGENT).await.map_err(Error::backend)?,
        None => return Err(Error::NoAgent { agent: AGENT }),
    };
    store.agent_ctx(ws.id, agent.id).await.map_err(Error::backend)
}

/// `opts` with the secrets the environment didn't set filled in from fnox (run in `dir`), then from what `trailhead
/// factory login` saved, and the fnox they were looked up in. It runs nothing until a name is missing from the
/// environment, and reads its config once.
fn resolve_opts(opts: &Opts, paths: &Paths, dir: &Path) -> Result<(Opts, fnox::Fnox)> {
    let creds = paths.creds();
    let fnox = fnox::Fnox::new("fnox", dir, fnox::global_config().as_deref());
    let resolved = Opts {
        sprites_token: login::resolve("SPRITES_TOKEN", opts.sprites_token.clone(), &fnox, &creds, login::SPRITES)?,
        github_token: login::resolve("GITHUB_TOKEN", opts.github_token.clone(), &fnox, &creds, login::GITHUB)?,
        claude_oauth_token: login::resolve(
            "CLAUDE_CODE_OAUTH_TOKEN",
            opts.claude_oauth_token.clone(),
            &fnox,
            &creds,
            login::CLAUDE,
        )?,
        ..opts.clone()
    };
    Ok((resolved, fnox))
}

/// The directory of the repo whose config is at `config_path`.
fn config_dir(config_path: &Path) -> PathBuf {
    config_path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_path_buf()
}

/// The factory file's path, the git remote beside it, and the `[factory]` settings read from it.
fn load_config(opts: &Opts) -> Result<(PathBuf, Option<trailhead_factory::Repo>, FactoryConfig)> {
    let config_path = match &opts.config {
        Some(path) => path.clone(),
        None => find::config(&std::env::current_dir().map_err(Error::backend)?)?,
    };
    let src = std::fs::read_to_string(&config_path).map_err(|e| Error::at(&config_path, e))?;
    let repo_dir = config_dir(&config_path);
    let remote = find::remote(&repo_dir);
    let config = FactoryConfig::from_repo_file(&src, remote.clone()).map_err(|e| Error::at(&config_path, e))?;
    Ok((config_path, remote, config))
}

/// Whether `now` differs from the `started` config (whose `[factory.merge]` is blanked, as it is re-read live) in a way that
/// needs a restart and wasn't already `warned` about, so an edit is reported once, not on every merge decision.
fn changed_without_restart(started: &FactoryConfig, now: FactoryConfig, warned: &mut Option<FactoryConfig>) -> bool {
    let now = FactoryConfig { merge: Default::default(), ..now };
    if now == *started {
        *warned = None;
        return false;
    }
    if warned.as_ref() == Some(&now) {
        return false;
    }
    *warned = Some(now);
    true
}

/// The baseline [`changed_without_restart`] compares against: `config` as read from the file, with `[factory.merge]` blanked
/// as it is re-read live. Take it before `wire` adds anything at runtime, which a re-read of the file never has.
fn started_baseline(config: &FactoryConfig) -> FactoryConfig {
    FactoryConfig { merge: Default::default(), ..config.clone() }
}

/// One repo's factory, ready to run.
struct Entry {
    factory: Arc<Factory>,
    /// `owner/name`, which the repo's log lines carry as `repo=`.
    slug: String,
}

/// Everything the server's control plane runs on: one store, agent, key set, iroh endpoint with its API tunnel, compute and
/// host, and a factory for each repo.
struct Setup {
    repos: Vec<Entry>,
    tunnel: Router,
    store: Arc<Store>,
    ctx: Ctx,
}

/// The user config's `[factory]` table; a missing file is the default.
fn read_host_config(paths: &Paths) -> Result<HostConfig> {
    let user_config = paths.config_dir.join("config.toml");
    match std::fs::read_to_string(&user_config) {
        Ok(src) => HostConfig::from_user_config(&src).map_err(|e| Error::at(&user_config, e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
        Err(e) => Err(Error::at(&user_config, e)),
    }
}

/// What the configuration says to run, read and checked without reaching anything outside this machine's files.
/// An error here is a configuration error: retrying won't fix it.
#[derive(Clone)]
struct Plan {
    opts: Opts,
    host_config: HostConfig,
    specs: Vec<repos::RepoSpec>,
    /// The worker profile's harness, the same for every repo (`repos::validate`).
    adapter: String,
}

fn plan(run: &RunOpts, host_config: HostConfig) -> Result<Plan> {
    let cwd = std::env::current_dir().map_err(Error::backend)?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let dirs = repos::dirs(&run.repo, run.opts.config.as_deref(), &host_config, home.as_deref(), &cwd)?;
    let specs = repos::load_all(&dirs, run.opts.config.as_deref())?;
    let (harnesses, providers) = (harnesses(), providers());
    for spec in &specs {
        spec.config
            .check_plugins(&harnesses.names(), &providers.names())
            .map_err(|e| Error::at(&spec.config_path, e))?;
        #[cfg(not(feature = "docker"))]
        if spec.config.target_specs().iter().any(|t| matches!(t.compute, ComputeConfig::Docker { .. })) {
            return Err(Error::DockerNotBuiltFor { path: spec.config_path.clone() });
        }
    }
    let (adapter, _) = specs[0].config.plugins().ok_or(Error::NoWorkerProfile)?;
    let adapter = adapter.to_string();
    Ok(Plan { opts: run.opts.clone(), host_config, specs, adapter })
}

/// Reaches out from a [`Plan`]: secrets, the daemon binary, the store, the iroh endpoint and the
/// computes. Fnox runs in `fnox_dir`, the first repo's directory. `store` is the server's own: the only process that opens the database.
async fn wire(plan: Plan, paths: &Paths, fnox_dir: &Path, store: Arc<Store>, feed: surface::Feed) -> Result<Setup> {
    let Plan { opts, host_config, mut specs, adapter } = plan;
    let (opts, fnox) = resolve_opts(&opts, paths, fnox_dir)?;
    let opts = &opts;
    let (harnesses, providers) = (harnesses(), providers());
    let mut harness_secrets = HarnessSecrets::new();
    if let Some(token) = &opts.claude_oauth_token {
        if adapter == CLAUDE_CODE {
            harness_secrets.insert(CLAUDE_CODE_OAUTH_TOKEN.into(), trailhead_factory::Secret::new(token.clone()));
        } else {
            tracing::warn!(harness = %adapter, "a Claude subscription token is set but this harness can't use it; ignoring it");
        }
    }
    let baselines: Vec<_> = specs.iter().map(|spec| started_baseline(&spec.config)).collect();
    for spec in &mut specs {
        if !harness_secrets.is_empty() {
            tracing::warn!(repo = %spec.slug(), harness = %adapter, "subscription mode: sandboxes hold the harness's credential and can reach its API");
            spec.config
                .egress
                .always
                .extend((harnesses.get(&adapter).map_err(Error::backend)?.egress)(&harness_secrets));
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
    .await
    .map_err(Error::backend)?;
    let slugs: Vec<String> = specs.iter().map(|s| s.slug()).collect();
    let ctx = factory_ctx(&store, opts.create_agent).await?;
    let tracker = Arc::new(CoreTracker::new(store.clone(), ctx.clone(), AGENT));
    let (key, master) = load_or_create_keys(&paths.creds())?;

    // `repos::validate` made the compute and daemon settings the same for every repo. Built before the endpoint
    // binds, so a failure here leaves no endpoint to close.
    let shared = &specs[0].config;
    let binary = Arc::new(binary);
    let built = computes(shared, opts, &fnox, paths, binary.clone(), false)?;
    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(key)
        .bind()
        .await
        .map_err(|e| Error::context("binding the iroh endpoint", e))?;
    let peers = trailhead_wire::Peers::default();
    let mut targets: Vec<Wired> = Vec::new();
    for (compute, target) in built {
        let launch = Launch {
            harness_secrets: harness_secrets.clone(),
            log: shared.daemon.log.clone(),
            ..Launch::new(binary.clone(), &target, adapter.clone())
        };
        let daemon = RemoteDaemon::new(endpoint.clone(), master, compute.clone(), launch).with_peers(peers.clone());
        targets.push((target, compute, Arc::new(daemon)));
    }
    let build =
        std::env::current_exe().and_then(std::fs::read).map(|b| build_id(&b)).unwrap_or_else(|_| "unknown".into());
    let ui = UiProtocol::new(opts.server, ui_allowlist(paths)).with_status(Arc::new(feed.clone()));
    let ui = match LOGS.get() {
        Some(buffer) => ui.with_logs(Arc::new(LogsFeed(buffer.clone()))),
        None => ui,
    };
    let ui = ui.build(build);
    let tunnel = Router::builder(endpoint.clone())
        .accept(API_ALPN, ApiTunnel::new(opts.server, peers))
        .accept(UI_ALPN, ui)
        .spawn();
    tracing::info!(endpoint = %endpoint.id(), repos = %slugs.join(", "), "factory ready");
    tracing::info!(
        "a UI connects to endpoint id {}; allow it with `trailhead factory allow <its endpoint id>`",
        endpoint.id()
    );

    let host = Host::with_usage_file(&host_config, paths.data_dir.join("factory").join("usage.json"));
    let mut repos = Vec::new();
    for (spec, started) in specs.into_iter().zip(baselines) {
        let slug = spec.slug();
        let repos::RepoSpec { config_path, remote, config, .. } = spec;
        let (_, provider_name) = config.plugins().ok_or(Error::NoWorkerProfile)?;
        let provider = *providers.get(provider_name).map_err(Error::backend)?;
        let placed = targets
            .iter()
            .map(|(target, compute, daemon)| Placed {
                target: target.clone(),
                compute: compute.clone(),
                daemon: daemon.clone(),
                provider: provider(target.model_endpoint(&opts.model_endpoint).to_string()),
            })
            .collect();
        let warned = std::sync::Mutex::new(None::<FactoryConfig>);
        let mut factory = Factory::with_targets(config, placed, tracker.clone(), host.clone())
            .with_spend_file(paths.data_dir.join("factory").join(format!("spend-{}.json", slug.replace('/', "-"))));
        factory = factory.with_merge_mode_source(Box::new(move || {
            // The control plane's `MergeModeSource` reports a `String`.
            let read = || -> Result<_> {
                let src = std::fs::read_to_string(&config_path).map_err(|e| Error::at(&config_path, e))?;
                let config =
                    FactoryConfig::from_repo_file(&src, remote.clone()).map_err(|e| Error::at(&config_path, e))?;
                let mode = config.merge.mode;
                if changed_without_restart(&started, config, &mut warned.lock().unwrap()) {
                    tracing::warn!(
                        path = %config_path.display(),
                        "the factory settings changed in the file, but only [factory.merge] mode is re-read live; restart the server to apply them"
                    );
                }
                Ok(mode)
            };
            read().map_err(|e| e.to_string())
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
    feed.set_boards(repos.iter().map(|e| (e.slug.clone(), e.factory.status().clone())).collect());
    feed.set_state(FactoryState::Running);
    Ok(Setup { repos, tunnel, store, ctx })
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
    feed: surface::Feed,
) -> bool {
    let wake = Arc::new(Notify::new());
    let project = entry.factory.config.repo.project.clone();
    let watcher =
        tokio::spawn(wake::wake_on_changes(store, ctx, project, wake.clone()).instrument(tracing::Span::current()));
    let ran = entry.factory.clone().run_draining(wake, poll, fired(drain), fired(cancel)).await;
    watcher.abort();
    match ran {
        Ok(()) => false,
        Err(e) => {
            tracing::error!(error = %e, "this repo's loop stopped; the other repos carry on");
            entry.factory.status().control(trailhead_factory::ControlState::Stopped);
            feed.set_stopped(&entry.slug, e.to_string());
            true
        }
    }
}

/// A target's `Compute` and launch facts.
pub(crate) type Built = (Arc<dyn Compute>, Target);

/// A target with its `Compute` and the daemon launched in its sandboxes.
type Wired = (Target, Arc<dyn Compute>, Arc<dyn Daemon>);

/// A factory file's targets, each with its `Compute` and launch facts, in placement order. `read_only` is as for
/// [`compute`].
fn computes(
    config: &FactoryConfig,
    opts: &Opts,
    fnox: &fnox::Fnox,
    paths: &Paths,
    binary: Arc<Vec<u8>>,
    read_only: bool,
) -> Result<Vec<Built>> {
    config
        .target_specs()
        .into_iter()
        .map(|spec| {
            let (compute, target) = compute(&spec.compute, opts, fnox, paths, binary.clone(), read_only)
                .map_err(|e| Error::context(format!("target {}", spec.name), e))?;
            Ok((compute, target.listed(&spec)))
        })
        .collect()
}

/// The sandboxes' `Compute`, and where the daemon lives in them.
#[cfg_attr(not(feature = "docker"), allow(unused_variables))]
/// `read_only` is for `status` and `logs`, which list sandboxes beside a running factory: a Docker compute made for
/// it never starts, replaces or syncs the gateway.
fn compute(
    config: &ComputeConfig,
    opts: &Opts,
    fnox: &fnox::Fnox,
    paths: &Paths,
    binary: Arc<Vec<u8>>,
    read_only: bool,
) -> Result<(Arc<dyn Compute>, Target)> {
    match config {
        ComputeConfig::Sprites => {
            let token = opts.sprites_token.clone().ok_or(Error::NoSpritesToken)?;
            trailhead_compute_sprites::build(token).map_err(Error::backend)
        }
        #[cfg(feature = "docker")]
        ComputeConfig::Docker { .. } => {
            use trailhead_compute_docker::DockerConfig;
            let mut docker = DockerConfig::new(paths.data_dir.join("factory").join("docker"), binary);
            docker.connections = gateway_connections(login::github_writer_token(fnox, &paths.creds())?);
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
            trailhead_compute_docker::build(config, docker, read_only).map_err(Error::backend)
        }
        #[cfg(not(feature = "docker"))]
        ComputeConfig::Docker { .. } => Err(Error::DockerNotBuilt),
    }
}

/// What the Docker gateway may forward to: GitHub's REST API for publishing, and `github.com` itself for cloning
/// and fetching private repos, both with the writer token. Subscription mode reaches the model API with the
/// sandbox's own token, through the gateway's proxy (TRAIL-487).
#[cfg(feature = "docker")]
fn gateway_connections(github_writer: Option<String>) -> Vec<trailhead_factory::GatewayConnection> {
    use trailhead_factory::{AuthScheme, GATEWAY_GIT_ID, GATEWAY_GITHUB_ID, GatewayConnection, Secret};
    let Some(token) = github_writer else {
        return Vec::new();
    };
    vec![
        GatewayConnection {
            provider: "github".into(),
            id: GATEWAY_GITHUB_ID.into(),
            upstream: trailhead_codehost_github::API_URL.into(),
            auth: AuthScheme::Bearer,
            credential: Secret::new(token.clone()),
        },
        GatewayConnection {
            provider: "git".into(),
            id: GATEWAY_GIT_ID.into(),
            upstream: "https://github.com".into(),
            auth: AuthScheme::GitBasic,
            credential: Secret::new(token),
        },
    ]
}

fn compute_name(config: &ComputeConfig) -> &'static str {
    match config {
        ComputeConfig::Sprites => "Sprites",
        ComputeConfig::Docker { .. } => "Docker",
    }
}

fn check_result(failed: usize) -> Result<()> {
    match failed {
        0 => Ok(()),
        n => Err(Error::ChecksFailed(n)),
    }
}

/// Reports what the setup for `run`'s repos can see, without running anything. It asks the server's API for tasks
/// (only the server opens the database) and lists the computes itself; it binds no iroh endpoint, so it runs beside
/// the server.
pub async fn check(run: &RunOpts) -> Result<()> {
    let paths = Paths::from_env().ok_or(Error::NoDirs)?;
    let plan = plan(run, read_host_config(&paths)?)?;
    let (opts, fnox) = resolve_opts(&plan.opts, &paths, &plan.specs[0].dir)?;
    let tracker = ops::api_tracker(&opts, &paths)?;
    let mut failed = 0;
    for spec in &plan.specs {
        let slug = spec.slug();
        let project = &spec.config.repo.project;
        let queue: Vec<_> = spec.config.repo.queue.iter().cloned().map(trailhead_factory::TaskKey).collect();
        match trailhead_factory::Tracker::ready(&tracker, project, &queue, 10).await {
            Ok(ready) => {
                let scope = if queue.is_empty() {
                    project.clone()
                } else {
                    let keys: Vec<_> = queue.iter().map(|q| q.0.as_str()).collect();
                    format!("{project}, under {}", keys.join(", "))
                };
                println!("{slug}: tracker: the server's API; {} ready task(s) in {scope}", ready.len());
                for t in &ready {
                    println!("  {} {}", t.key.0, t.title);
                }
            }
            Err(e) => {
                failed += 1;
                println!("{slug}: tracker: {}", ops::reach(e));
            }
        }
    }
    let first = &plan.specs[0].config;
    let specs = first.target_specs();
    match computes(first, &opts, &fnox, &paths, Default::default(), true) {
        Ok(built) => {
            for ((compute, target), spec) in built.iter().zip(specs.iter().map(|s| compute_name(&s.compute))) {
                match compute.list(&Default::default()).await {
                    Ok(live) => println!(
                        "compute: {} ({spec}, priority {}) reachable; {} sandbox(es) visible",
                        target.name,
                        target.priority,
                        live.len()
                    ),
                    Err(e) => {
                        failed += 1;
                        println!("compute: {} ({spec}): {e}", target.name);
                    }
                }
            }
        }
        Err(e) => {
            failed += 1;
            println!("compute: {e}");
        }
    }
    match saved_endpoint_id(&paths.creds()) {
        Some(id) => println!("endpoint: {id}"),
        None => println!("endpoint: none yet (the server's control plane makes it on its first run)"),
    }
    println!("tunnel: the server's control plane forwards to {}", opts.server);
    check_result(failed)
}

/// Logs a repo loop that panicked and marks it stopped on its status board and in the feed, since the loop never got
/// to say so itself.
fn loop_panicked(
    e: &tokio::task::JoinError,
    feed: &surface::Feed,
    boards: &std::collections::HashMap<tokio::task::Id, (String, Arc<trailhead_control::StatusBoard>)>,
) {
    let repo = boards.get(&e.id()).map(|(slug, board)| {
        board.control(trailhead_factory::ControlState::Stopped);
        feed.set_stopped(slug, e.to_string());
        slug.as_str()
    });
    tracing::error!(error = %e, ?repo, "a repo's loop panicked; the other repos carry on");
}

/// Runs every repo's loop until `drain` lets the runs finish or `cancel` stops them.
async fn run_loops(
    setup: Setup,
    poll: Duration,
    drain: tokio::sync::watch::Receiver<bool>,
    cancel: tokio::sync::watch::Receiver<bool>,
    feed: surface::Feed,
) -> Result<()> {
    let Setup { repos, tunnel, store, ctx } = setup;
    let mut loops = tokio::task::JoinSet::new();
    let mut boards = std::collections::HashMap::new();
    for entry in repos {
        let span = tracing::info_span!("repo", repo = %entry.slug);
        let (store, ctx, drain, cancel) = (store.clone(), ctx.clone(), drain.clone(), cancel.clone());
        let board = entry.factory.status().clone();
        let slug = entry.slug.clone();
        let id = loops.spawn(run_repo(entry, store, ctx, poll, drain, cancel, feed.clone()).instrument(span)).id();
        boards.insert(id, (slug, board));
    }
    let mut failed = 0;
    while let Some(joined) = loops.join_next().await {
        match joined {
            Ok(false) => {}
            Ok(true) => failed += 1,
            Err(e) => {
                failed += 1;
                loop_panicked(&e, &feed, &boards);
            }
        }
    }
    tunnel.shutdown().await.map_err(Error::backend)?;
    match failed {
        0 => Ok(()),
        n => Err(Error::LoopsFailed(n)),
    }
}

/// The control plane running inside the Trailhead server, from [`start`].
pub struct Running {
    drain: tokio::sync::watch::Sender<bool>,
    cancel: tokio::sync::watch::Sender<bool>,
    /// Set when the control plane has stopped, drained or not.
    done: tokio::sync::watch::Receiver<bool>,
    feed: surface::Feed,
}

impl Running {
    /// The runs and reviews in flight across every repo.
    pub fn runs_in_flight(&self) -> usize {
        self.feed.boards().iter().map(|(_, b)| b.snapshot().runs.len()).sum()
    }

    /// Starts no new runs or reviews; the ones in flight finish, then the control plane stops. A control plane
    /// still retrying its start gives up.
    pub fn drain(&self) {
        self.drain.send_replace(true);
    }

    /// Cancels the runs in flight, which are requeued on the next start.
    pub fn cancel(&self) {
        self.drain.send_replace(true);
        self.cancel.send_replace(true);
    }

    /// Resolves once the control plane has stopped.
    pub async fn finished(&self) {
        let _ = self.done.clone().wait_for(|done| *done).await;
    }

    /// [`Running::cancel`], then waits up to `within` for it to stop.
    pub async fn stop(&self, within: Duration) {
        self.cancel();
        if tokio::time::timeout(within, self.finished()).await.is_err() {
            tracing::warn!("the control plane didn't stop within {}s; leaving it to reconcile", within.as_secs());
        }
    }
}

/// How long the server waits before trying the control plane again after an outside failure, at first and at most.
const RETRY_FIRST: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(300);

/// Starts the control plane inside the Trailhead server on `store`, with the API tunnel forwarding to `server`
/// (`docs/factory/server-control-plane.md`). Only the user config's `[factory] repos` turns it on: with none listed,
/// or with `disabled` set, this returns `None` and runs nothing. A configuration error (a repo file that won't parse, repos that collide, an
/// unknown harness or provider, a missing repo) is returned, and the server refuses to start. Outside failures
/// (Sprites or GitHub unreachable, a locked keyring, a daemon download)
/// are logged and retried with backoff while the server keeps serving.
pub fn start(paths: Paths, store: Arc<Store>, server: SocketAddr) -> Result<InServer> {
    let host_config = read_host_config(&paths)?;
    let idle = |state| Ok(InServer { feed: surface::Feed::new(state), running: None });
    if host_config.disabled {
        tracing::info!("control: disabled; `trailhead factory enable` turns it back on");
        return idle(FactoryState::Disabled);
    }
    if host_config.repos.is_empty() {
        return idle(FactoryState::Off);
    }
    #[derive(clap::Parser)]
    struct Defaults {
        #[command(flatten)]
        run: RunOpts,
    }
    let mut run = <Defaults as clap::Parser>::try_parse_from(["serve"]).map_err(Error::backend)?.run;
    run.opts.server = server;
    // Listing repos is the person's consent; the agent the factory acts as is the server's to make.
    run.opts.create_agent = true;
    let plan = plan(&run, host_config)?;
    // Fnox finds a repo's `fnox.toml` from its directory; the server's working directory is an accident.
    let fnox_dir = plan.specs[0].dir.clone();
    let poll = Duration::from_secs(run.opts.poll_secs);
    let (drain_tx, drain) = tokio::sync::watch::channel(false);
    let (cancel_tx, cancel) = tokio::sync::watch::channel(false);
    let (done_tx, done) = tokio::sync::watch::channel(false);
    let feed = surface::Feed::new(FactoryState::Starting { reason: None });
    let shared = feed.clone();
    let control = async move {
        let mut wait = RETRY_FIRST;
        let setup = loop {
            match wire(plan.clone(), &paths, &fnox_dir, store.clone(), shared.clone()).await {
                Ok(setup) => break setup,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        retry_secs = wait.as_secs(),
                        "control: starting; the server keeps serving and tries the factory again"
                    );
                    shared.set_state(FactoryState::Starting { reason: Some(e.to_string()) });
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = fired(drain.clone()) => {
                    shared.set_state(FactoryState::Stopped);
                    return;
                }
            }
            wait = (wait * 2).min(RETRY_MAX);
        };
        if let Err(e) = run_loops(setup, poll, drain, cancel, shared.clone()).await {
            tracing::error!(error = %e, "the control plane stopped; the server keeps serving");
        }
        shared.set_state(FactoryState::Stopped);
    };
    tokio::spawn(async move {
        control.await;
        done_tx.send_replace(true);
    });
    let running = Running { drain: drain_tx, cancel: cancel_tx, done, feed: feed.clone() };
    Ok(InServer { feed, running: Some(running) })
}

/// What [`start`] gives the server: the status feed its `/v1/factory` routes serve, whatever the control plane is
/// doing, and the control plane when the user config turns it on.
pub struct InServer {
    pub feed: Feed,
    pub running: Option<Running>,
}

/// The server's `/v1/factory/status` and `/v1/factory/events`, for it to mount behind its own bearer token.
pub fn api(feed: &Feed) -> axum::Router {
    surface::routes(feed.clone())
}

/// Whether the user config lists any repos for the factory, for a caller deciding whether to open `factory.log`.
/// Unreadable config counts as yes, so [`start`]'s error is logged where the factory's lines go.
pub fn configured(paths: &Paths) -> bool {
    read_host_config(paths).map_or(true, |c| !c.disabled && !c.repos.is_empty())
}

/// The server's logging when it may run the control plane: the factory's events (its crates, iroh and anything in a
/// `repo` span) go to `factory.log` (when `configured`) and the buffer Lookout reads, everything else to stdout,
/// which the CLI points at `server.log`. `factory_on_stdout` sends the factory's events to stdout as well, for
/// `trailhead factory run` at a terminal.
pub fn init_server_logging(paths: &Paths, default: &str, factory_on_stdout: bool) {
    use tracing_subscriber::fmt::writer::BoxMakeWriter;
    use tracing_subscriber::util::SubscriberInitExt;

    let buffer = LOGS.get_or_init(trailhead_control::logs::LogBuffer::default);
    let file = if configured(paths) { open_log_file() } else { None };
    let file = file.map(|w| BoxMakeWriter::new(std::sync::Mutex::new(w)));
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stdout());
    let stdout = BoxMakeWriter::new(std::io::stdout);
    server_subscriber(trailhead_log::env_filter_or(default), buffer, stdout, ansi, file, factory_on_stdout).init();
}

/// [`init_server_logging`]'s subscriber, with its writers given. Spans pass every layer, so each one sees whether an
/// event is inside a `repo` span; only events are routed.
fn server_subscriber(
    filter: tracing_subscriber::EnvFilter,
    buffer: &trailhead_control::logs::LogBuffer,
    stdout: tracing_subscriber::fmt::writer::BoxMakeWriter,
    ansi: bool,
    file: Option<tracing_subscriber::fmt::writer::BoxMakeWriter>,
    factory_on_stdout: bool,
) -> impl tracing::Subscriber + Send + Sync {
    use tracing_subscriber::filter::{FilterExt, dynamic_filter_fn};
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::{Layer, registry::LookupSpan};

    fn is_factory<S: tracing::Subscriber + for<'a> LookupSpan<'a>>(
        meta: &tracing::Metadata<'_>,
        cx: &tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
        const TARGETS: [&str; 7] = [
            "trailhead_factory",
            "trailhead_control",
            "trailhead_wire",
            "trailhead_compute_",
            "trailhead_codehost_",
            "trailhead_provider_",
            "iroh",
        ];
        TARGETS.iter().any(|t| meta.target().starts_with(t))
            || cx.lookup_current().is_some_and(|span| span.scope().any(|s| s.name() == "repo"))
    }
    fn factory<S: tracing::Subscriber + for<'a> LookupSpan<'a>>(
        meta: &tracing::Metadata<'_>,
        cx: &tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
        !meta.is_event() || is_factory(meta, cx)
    }

    let stdout =
        tracing_subscriber::fmt::layer().with_timer(trailhead_log::local_timer()).with_ansi(ansi).with_writer(stdout);
    let file = file.map(|w| {
        tracing_subscriber::fmt::layer().with_timer(trailhead_log::local_timer()).with_ansi(false).with_writer(w)
    });
    tracing_subscriber::registry()
        .with(buffer.layer().with_filter(filter.clone().and(dynamic_filter_fn(factory))))
        .with(file.with_filter(filter.clone().and(dynamic_filter_fn(factory))))
        .with(
            stdout.with_filter(filter.and(dynamic_filter_fn(move |meta, cx| {
                !meta.is_event() || factory_on_stdout || !is_factory(meta, cx)
            }))),
        )
}

/// The factory's commands, as `trailhead factory <cmd>` takes them.
#[derive(clap::Subcommand)]
pub enum Command {
    /// Report what the factory can see (queued tasks via the server, the compute, its endpoint), without running anything.
    Check(RunOpts),
    /// Show the sandbox pool, what is running and how queued tasks have gone (the latest finished ones; `--all` for every one). Read-only; asks the server.
    Status(ops::StatusOpts),
    /// Print the daemon's service log from a task's sandbox.
    Logs(ops::LogsOpts),
    /// Let a UI (Lookout) connect: add its endpoint id to the allowlist. Takes effect on a running control plane at once.
    Allow { endpoint_id: String },
    /// Stop allowing a UI endpoint.
    Disallow { endpoint_id: String },
    /// List the UI endpoint ids allowed to connect.
    Allowed,
    /// Save a secret in the OS keyring (read from stdin), so the server's control plane needs no environment variables.
    #[command(subcommand)]
    Login(login::Login),
}

/// Sets up logging to the terminal and runs `cmd`.
pub async fn command(cmd: Command) -> Result<()> {
    trailhead_log::init_stdout(trailhead_log::env_filter_or("info"));
    match cmd {
        Command::Check(opts) => check(&opts).await,
        Command::Status(opts) => ops::status(&opts).await,
        Command::Logs(logs) => ops::logs(&logs).await,
        Command::Allow { endpoint_id } => allow(&endpoint_id, true),
        Command::Disallow { endpoint_id } => allow(&endpoint_id, false),
        Command::Allowed => allowed(),
        Command::Login(cmd) => login(cmd).await,
    }
}

/// Saves a secret for the control plane in the OS keyring (`trailhead factory login ...`).
pub async fn login(cmd: login::Login) -> Result<()> {
    let paths = Paths::from_env().ok_or(Error::NoDirs)?;
    login::run(cmd, &paths.creds()).await
}

/// The control plane's log file, if it can be opened.
fn open_log_file() -> Option<logfile::Rotating> {
    let paths = Paths::from_env()?;
    let path = logfile::path(&paths.data_dir);
    match logfile::Rotating::open(&path) {
        Ok(file) => Some(file),
        Err(e) => {
            eprintln!(
                "trailhead: can't open the factory's log file {}: {e}; logging to the terminal only",
                path.display()
            );
            None
        }
    }
}

/// The control plane's recent log, for Lookout's Logs page; filled by the layer `init_server_logging` adds.
static LOGS: std::sync::OnceLock<trailhead_control::logs::LogBuffer> = std::sync::OnceLock::new();

/// [`LOGS`] as the feed `trailhead/ui/0` serves.
struct LogsFeed(trailhead_control::logs::LogBuffer);

impl trailhead_wire::ui::LogFeed for LogsFeed {
    fn subscribe(
        &self,
    ) -> (Vec<trailhead_factory::logs::LogLine>, tokio::sync::broadcast::Receiver<trailhead_factory::logs::LogLine>)
    {
        self.0.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::check_result;

    #[test]
    fn a_changed_setting_other_than_merge_mode_warns_once() {
        use trailhead_factory::FactoryConfig;
        let src = "[repo]\nowner = \"o\"\nname = \"n\"\n[profiles.worker]\nrole = \"worker\"\nadapter = \"claude-code\"\nprovider = \"anthropic\"\nmodel = \"sonnet\"\nbudget = { usd = 5, minutes = 60, steps = 40 }\n[profiles.reviewer]\nrole = \"reviewer\"\nadapter = \"claude-code\"\nprovider = \"anthropic\"\nmodel = \"opus\"\nbudget = { usd = 2, minutes = 20, steps = 20 }\n";
        let parse = |extra: &str| FactoryConfig::parse_table(&format!("{extra}\n{src}"), "TRAIL", None).unwrap();
        let base = parse("");
        let started = super::started_baseline(&base);
        let mut warned = None;
        // A mode change is applied live, so it isn't a reason to restart.
        assert!(!super::changed_without_restart(&started, parse("[merge]\nmode = \"on\""), &mut warned));
        let edited = parse("[checks]\nfmt = \"cargo fmt\"");
        assert!(super::changed_without_restart(&started, edited.clone(), &mut warned));
        assert!(!super::changed_without_restart(&started, edited, &mut warned));
        assert!(!super::changed_without_restart(&started, base, &mut warned));
    }

    #[test]
    fn egress_added_at_runtime_is_not_a_change_to_the_file() {
        use trailhead_factory::FactoryConfig;
        let src = "[repo]\nowner = \"o\"\nname = \"n\"\n[profiles.worker]\nrole = \"worker\"\nadapter = \"claude-code\"\nprovider = \"anthropic\"\nmodel = \"sonnet\"\nbudget = { usd = 5, minutes = 60, steps = 40 }\n[profiles.reviewer]\nrole = \"reviewer\"\nadapter = \"claude-code\"\nprovider = \"anthropic\"\nmodel = \"opus\"\nbudget = { usd = 2, minutes = 20, steps = 20 }\n";
        let mut config = FactoryConfig::parse_table(src, "TRAIL", None).unwrap();
        let started = super::started_baseline(&config);
        // What `wire` does in subscription mode, after the baseline is taken.
        config.egress.always.push("api.anthropic.com".into());
        let reread = FactoryConfig::parse_table(src, "TRAIL", None).unwrap();
        assert!(!super::changed_without_restart(&started, reread, &mut None));
        assert_ne!(started.egress, config.egress);
    }

    #[tokio::test]
    async fn a_panicked_loop_is_marked_stopped_on_its_board() {
        use trailhead_factory::ControlState;
        let (up, other) = (trailhead_control::StatusBoard::new(), trailhead_control::StatusBoard::new());
        up.control(ControlState::Up);
        other.control(ControlState::Up);
        let mut loops = tokio::task::JoinSet::new();
        let mut boards = std::collections::HashMap::new();
        let id = loops.spawn(async { panic!("boom") }).id();
        boards.insert(id, ("o/n".to_string(), up.clone()));
        let id = loops.spawn(std::future::pending::<()>()).id();
        boards.insert(id, ("o/m".to_string(), other.clone()));
        let e = loops.join_next().await.unwrap().unwrap_err();
        let feed = super::surface::Feed::new(trailhead_factory::FactoryState::Running);
        feed.set_boards(vec![("o/n".to_string(), up.clone()), ("o/m".to_string(), other.clone())]);
        super::loop_panicked(&e, &feed, &boards);
        let reply = feed.reply();
        assert!(reply.repos[0].stopped.as_ref().is_some_and(|m| m.contains("panic")));
        assert_eq!(reply.repos[1].stopped, None);
        assert_eq!(up.snapshot().control, ControlState::Stopped);
        assert_eq!(other.snapshot().control, ControlState::Up);
    }

    /// Paths under `dir`, and a store in it.
    async fn instance(
        dir: &std::path::Path,
        user_config: &str,
    ) -> (trailhead_client::paths::Paths, std::sync::Arc<trailhead_core::Store>) {
        let paths = trailhead_client::paths::Paths { data_dir: dir.join("data"), config_dir: dir.join("config") };
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(paths.config_dir.join("config.toml"), user_config).unwrap();
        let store = trailhead_core::Store::open_local(&dir.join("t.db")).await.unwrap();
        (paths, std::sync::Arc::new(store))
    }

    #[tokio::test]
    async fn no_repos_listed_runs_no_factory() {
        let dir = tempfile::tempdir().unwrap();
        let (paths, store) = instance(dir.path(), "url = \"http://127.0.0.1:7241\"\n").await;
        let started = super::start(paths, store, "127.0.0.1:7241".parse().unwrap()).unwrap();
        assert!(started.running.is_none());
        assert_eq!(started.feed.state(), trailhead_factory::FactoryState::Off);
    }

    #[tokio::test]
    async fn disabled_runs_nothing_even_with_a_broken_repo() {
        let dir = tempfile::tempdir().unwrap();
        let (paths, store) = instance(dir.path(), "[factory]\nrepos = [\"/no/such/repo\"]\ndisabled = true\n").await;
        assert!(!super::configured(&paths));
        let started = super::start(paths, store, "127.0.0.1:7241".parse().unwrap()).unwrap();
        assert!(started.running.is_none());
        assert_eq!(started.feed.state(), trailhead_factory::FactoryState::Disabled);
    }

    #[tokio::test]
    async fn a_configuration_error_is_returned_not_retried() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("no-such-repo");
        let config = format!("[factory]\nrepos = [{:?}]\n", missing.display().to_string());
        let (paths, store) = instance(dir.path(), &config).await;
        let err = super::start(paths, store, "127.0.0.1:7241".parse().unwrap()).err().unwrap().to_string();
        assert!(err.contains("no-such-repo"), "{err}");

        let (paths, store) = instance(dir.path(), "[factory]\nrepos = 3\n").await;
        let err = super::start(paths, store, "127.0.0.1:7241".parse().unwrap()).err().unwrap().to_string();
        assert!(err.contains("config.toml"), "{err}");
    }

    #[cfg(not(feature = "docker"))]
    #[tokio::test]
    async fn docker_compute_without_the_feature_is_a_configuration_error() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let profile = |role: &str| {
            format!(
                "[factory.profiles.{role}]\nrole = \"{role}\"\nadapter = \"claude-code\"\nprovider = \"anthropic\"\n\
                 model = \"sonnet\"\nbudget = {{ usd = 1, minutes = 1, steps = 1 }}\n"
            )
        };
        let file = format!(
            "project = \"P\"\n[factory.repo]\nowner = \"o\"\nname = \"r\"\n[factory.compute]\nkind = \"docker\"\n\
             [factory.checks]\nfmt = \"f\"\nlint = \"l\"\ntest = \"t\"\n{}{}",
            profile("worker"),
            profile("reviewer")
        );
        std::fs::write(repo.join(".trailhead.toml"), file).unwrap();
        let config = format!("[factory]\nrepos = [{:?}]\n", repo.display().to_string());
        let (paths, store) = instance(dir.path(), &config).await;
        let err = super::start(paths, store, "127.0.0.1:7241".parse().unwrap()).err().unwrap().to_string();
        assert!(err.contains("`docker` feature"), "{err}");
    }

    #[test]
    fn a_servers_event_inside_a_repo_span_goes_to_the_factory_log_only() {
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::fmt::writer::BoxMakeWriter;

        #[derive(Clone, Default)]
        struct Capture(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Capture {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl Capture {
            fn text(&self) -> String {
                String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
            }
        }

        let (server, factory) = (Capture::default(), Capture::default());
        let (s, f) = (server.clone(), factory.clone());
        let subscriber = super::server_subscriber(
            "info".into(),
            &trailhead_control::logs::LogBuffer::default(),
            BoxMakeWriter::new(move || s.clone()),
            false,
            Some(BoxMakeWriter::new(move || f.clone())),
            false,
        );
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "trailhead_core", "outside any repo");
            let span = tracing::info_span!(target: "trailhead_factory_server", "repo", repo = "a/b");
            let _in = span.enter();
            tracing::info!(target: "trailhead_core", "inside a repo");
            tracing::info!(target: "trailhead_control", "the factory's own");
        });
        let (server, factory) = (server.text(), factory.text());
        assert!(server.contains("outside any repo"), "{server}");
        assert!(!server.contains("inside a repo") && !server.contains("the factory's own"), "{server}");
        assert!(factory.contains("inside a repo") && factory.contains("the factory's own"), "{factory}");
        assert!(!factory.contains("outside any repo"), "{factory}");
    }

    #[test]
    fn check_result_counts_failures() {
        check_result(0).unwrap();
        assert_eq!(check_result(1).unwrap_err().to_string(), "1 check failed");
        assert_eq!(check_result(2).unwrap_err().to_string(), "2 checks failed");
        assert!(matches!(check_result(3), Err(crate::Error::ChecksFailed(3))));
    }
}
