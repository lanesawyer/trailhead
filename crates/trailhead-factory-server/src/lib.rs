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
//!
//! This file is the start itself: the options, wiring a repo's factory to its sandboxes, the loops and their
//! supervisor, and the `trailhead factory` commands. Beside it: `keys` (the endpoint's identity and the UI's
//! allowlist), `computes` (the backends the targets name), `changed` (which settings wait for a restart), `check`
//! and `logging`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use iroh::Endpoint;
use iroh::endpoint::presets;
use iroh::protocol::Router;
use tokio::sync::Notify;
use tracing::Instrument;
use trailhead_client::paths::{DEFAULT_BIND, Paths};
use trailhead_codehost_github::GitHub;
use trailhead_control::{CoreTracker, Factory, Host, Placed, daemon_binary};
use trailhead_core::{Ctx, Store};
pub use trailhead_factory::HostConfig;
use trailhead_factory::{FactoryConfig, FactoryState, HarnessSecrets};
use trailhead_plugins::{CLAUDE_CODE, CLAUDE_CODE_OAUTH_TOKEN, harnesses, providers};
use trailhead_wire::api::ApiTunnel;
use trailhead_wire::daemon::{Launch, RemoteDaemon};
use trailhead_wire::ui::{UI_ALPN, UiProtocol};
use trailhead_wire::{API_ALPN, build_id};

mod changed;
mod check;
mod computes;
mod error;
mod find;
mod fnox;
mod keys;
mod logfile;
mod logging;
pub mod login;
pub mod ops;
mod repos;
mod surface;

pub use check::check;
pub use error::{Error, Result};
pub use keys::{allow, allowed};
pub use logging::init_server_logging;

use changed::{changed_sections, changed_without_restart, started_baseline};
use computes::{Wired, computes};
use keys::{load_or_create_keys, saved_endpoint_id, ui_allowlist};
use logging::{LOGS, LogsFeed};
pub use surface::Feed;
mod wake;

/// The agent the control plane acts as; sandboxes get its sub-agents.
const AGENT: &str = trailhead_core::FACTORY_AGENT;
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

/// The factory file's path, the git remote beside it, and the `[factory]` settings read from it.
fn load_config(opts: &Opts) -> Result<(PathBuf, Option<trailhead_factory::Repo>, FactoryConfig)> {
    find::load_config(opts.config.as_deref())
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
    store: Arc<Store>,
    ctx: Ctx,
}

/// The server's way in over iroh: the endpoint, with the API tunnel sandboxes reach Trailhead through and the UI
/// protocol the Trailhead app dials. It is opened before anything the factory needs (secrets, the daemon binary,
/// the compute), so the app shows tasks and projects, and what the factory is waiting for, while the factory can't
/// start.
struct Door {
    endpoint: Endpoint,
    /// The key the daemons' tokens are made from.
    master: [u8; 32],
    peers: trailhead_wire::Peers,
    tunnel: Router,
}

async fn open_door(server: SocketAddr, paths: &Paths, feed: &surface::Feed) -> Result<Door> {
    let (key, master) = load_or_create_keys(&paths.creds())?;
    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(key)
        .bind()
        .await
        .map_err(|e| Error::context("binding the iroh endpoint", e))?;
    let peers = trailhead_wire::Peers::default();
    let build =
        std::env::current_exe().and_then(std::fs::read).map(|b| build_id(&b)).unwrap_or_else(|_| "unknown".into());
    let ui = UiProtocol::new(server, ui_allowlist(paths)).with_status(Arc::new(feed.clone()));
    let ui = match LOGS.get() {
        Some(buffer) => ui.with_logs(Arc::new(LogsFeed(buffer.clone()))),
        None => ui,
    };
    let tunnel = Router::builder(endpoint.clone())
        .accept(API_ALPN, ApiTunnel::new(server, peers.clone()))
        .accept(UI_ALPN, ui.build(build))
        .spawn();
    tracing::info!(
        "a UI connects to endpoint id {}; allow it with `trailhead factory allow <its endpoint id>`",
        endpoint.id()
    );
    Ok(Door { endpoint, master, peers, tunnel })
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
        if spec
            .config
            .target_specs()
            .iter()
            .any(|t| matches!(t.compute, trailhead_factory::ComputeConfig::Docker { .. }))
        {
            return Err(Error::DockerNotBuiltFor { path: spec.config_path.clone() });
        }
    }
    let (adapter, _) = specs[0].config.plugins().ok_or(Error::NoWorkerProfile)?;
    let adapter = adapter.to_string();
    Ok(Plan { opts: run.opts.clone(), host_config, specs, adapter })
}

/// Reaches out from a [`Plan`]: secrets, the daemon binary, the store, the iroh endpoint and the
/// computes. Fnox runs in `fnox_dir`, the first repo's directory. `store` is the server's own: the only process that opens the database.
async fn wire(
    plan: Plan,
    paths: &Paths,
    fnox_dir: &Path,
    store: Arc<Store>,
    feed: surface::Feed,
    door: &Door,
) -> Result<Setup> {
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

    // `repos::validate` made the compute and daemon settings the same for every repo.
    let shared = &specs[0].config;
    let binary = Arc::new(binary);
    let built = computes(shared, opts, &fnox, paths, binary.clone(), false, !harness_secrets.is_empty())?;
    let (endpoint, master, peers) = (&door.endpoint, door.master, &door.peers);
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
    tracing::info!(endpoint = %endpoint.id(), repos = %slugs.join(", "), "factory ready");

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
        factory = factory.with_live_settings(Box::new(move || {
            // The control plane's `LiveSettingsSource` reports a `String`.
            let read = || -> Result<_> {
                let src = std::fs::read_to_string(&config_path).map_err(|e| Error::at(&config_path, e))?;
                let config =
                    FactoryConfig::from_repo_file(&src, remote.clone()).map_err(|e| Error::at(&config_path, e))?;
                let merge_mode = config.merge.mode;
                let restart_for = changed_sections(&started, &config).into_iter().map(String::from).collect();
                let changed = changed_without_restart(&started, config, &mut warned.lock().unwrap());
                if !changed.is_empty() {
                    tracing::warn!(
                        path = %config_path.display(),
                        changed = %changed.join(", "),
                        "the factory settings changed in the file, but only [factory.merge] mode is re-read live; restart the server to apply them"
                    );
                }
                Ok(trailhead_control::LiveSettings { merge_mode, restart_for })
            };
            read().map_err(|e| e.to_string())
        }));
        match &opts.github_token {
            Some(token) => {
                factory = factory
                    .with_codehost(Arc::new(GitHub::new(trailhead_codehost_github::API_URL, Some(token.clone()))));
            }
            None => tracing::warn!(
                repo = %slug,
                "no GitHub token (GITHUB_TOKEN or `trailhead factory login github`): merged and closed PRs are not noticed; finish merged tasks by hand"
            ),
        }
        repos.push(Entry { factory: Arc::new(factory), slug });
    }
    feed.set_boards(repos.iter().map(|e| (e.slug.clone(), e.factory.status().clone())).collect());
    feed.set_factories(repos.iter().map(|e| (e.slug.clone(), e.factory.clone())).collect());
    feed.set_state(FactoryState::Running);
    Ok(Setup { repos, store, ctx })
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
    let slug = entry.factory.config.repo.repo().slug();
    let watched = wake::wake_on_changes(store, ctx, project, slug, wake.clone());
    let watcher = tokio::spawn(watched.instrument(tracing::Span::current()));
    let ran = entry.factory.clone().run_draining(wake, poll, fired(drain), fired(cancel)).await;
    watcher.abort();
    match ran {
        Ok(()) => false,
        Err(e) => {
            tracing::error!(error = %e, "this repo's loop stopped; the other repos carry on");
            loop_stopped(&entry.slug, entry.factory.status(), &feed, &e.to_string());
            true
        }
    }
}

/// Marks a repo whose loop returned an error stopped, on its status board and in the feed.
fn loop_stopped(slug: &str, board: &trailhead_control::StatusBoard, feed: &surface::Feed, error: &str) {
    board.control(trailhead_factory::ControlState::Stopped);
    feed.set_stopped(slug, error.to_string());
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
        board.error(format!("loop panicked: {e}"));
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
    let Setup { repos, store, ctx } = setup;
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

/// What [`start`] checks of a user config before it starts anything, for a config that may not be written yet: `src`
/// is its text and `user_config` where it is, which an error names. It reads the repos' files and nothing outside
/// this machine, and starts nothing. An error is one [`start`] would refuse the server with, in the same words.
pub fn check_user_config(user_config: &Path, src: &str) -> Result<HostConfig> {
    let host_config = HostConfig::from_user_config(src).map_err(|e| Error::at(user_config, e))?;
    if !host_config.disabled && !host_config.repos.is_empty() {
        plan(&default_run()?, host_config.clone())?;
    }
    Ok(host_config)
}

/// The run options the server's control plane takes: every flag at its default.
fn default_run() -> Result<RunOpts> {
    #[derive(clap::Parser)]
    struct Defaults {
        #[command(flatten)]
        run: RunOpts,
    }
    Ok(<Defaults as clap::Parser>::try_parse_from(["serve"]).map_err(Error::backend)?.run)
}

/// How long the server waits before trying the control plane again after an outside failure, at first and at most.
const RETRY_FIRST: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(300);

/// Starts the control plane inside the Trailhead server on `store`, with the API tunnel forwarding to `server`
/// (`docs/factory/server-control-plane.md`). Only the user config's `[factory] repos` turns it on: with none listed,
/// or with `disabled` set, this returns `None` and runs nothing. Only a user config that can't be read is returned, and the
/// server refuses to start on it. Everything else is logged, shown as the reason the control plane is `starting`,
/// and retried with backoff while the server keeps serving: a repo's file that won't parse (a key from a newer
/// Trailhead than the one installed, say), repos that collide, an unknown harness or provider, a missing repo
/// (TRAIL-1634: a file in a repo must never be what locks a person out of their tasks), and the outside failures
/// (Sprites or GitHub unreachable, a locked keyring, a daemon download). A repo file put right is picked up by the
/// next try, without a restart.
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
    let mut run = default_run()?;
    run.opts.server = server;
    // Listing repos is the person's consent; the agent the factory acts as is the server's to make.
    run.opts.create_agent = true;
    let poll = Duration::from_secs(run.opts.poll_secs);
    let (drain_tx, drain) = tokio::sync::watch::channel(false);
    let (cancel_tx, cancel) = tokio::sync::watch::channel(false);
    let (done_tx, done) = tokio::sync::watch::channel(false);
    let feed = surface::Feed::new(FactoryState::Starting { reason: None });
    let shared = feed.clone();
    let control = async move {
        // First, so a repo file this build can't read is what `factory status` says, not a missing secret.
        let planned = until_ready(&shared, &drain, || async { plan(&run, host_config.clone()) }).await;
        let Some(plan) = planned else { return };
        // Fnox finds a repo's `fnox.toml` from its directory; the server's working directory is an accident.
        let fnox_dir = plan.specs[0].dir.clone();
        let Some(door) = until_ready(&shared, &drain, || open_door(server, &paths, &shared)).await else { return };
        let wired = until_ready(&shared, &drain, || {
            wire(plan.clone(), &paths, &fnox_dir, store.clone(), shared.clone(), &door)
        })
        .await;
        if let Some(setup) = wired
            && let Err(e) = run_loops(setup, poll, drain, cancel, shared.clone()).await
        {
            tracing::error!(error = %e, "the control plane stopped; the server keeps serving");
        }
        if let Err(e) = door.tunnel.shutdown().await {
            tracing::warn!(error = %e, "closing the iroh endpoint");
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

/// Tries `step` until it works, waiting longer after each failure and saying why on the status feed. `None` once the
/// server is draining, with the feed marked stopped.
async fn until_ready<T, F>(
    feed: &surface::Feed,
    drain: &tokio::sync::watch::Receiver<bool>,
    mut step: impl FnMut() -> F,
) -> Option<T>
where
    F: Future<Output = Result<T>>,
{
    let mut wait = RETRY_FIRST;
    loop {
        match step().await {
            Ok(ready) => return Some(ready),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    retry_secs = wait.as_secs(),
                    "control: starting; the server keeps serving and tries the factory again"
                );
                feed.set_state(FactoryState::Starting { reason: Some(e.to_string()) });
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = fired(drain.clone()) => {
                feed.set_state(FactoryState::Stopped);
                return None;
            }
        }
        wait = (wait * 2).min(RETRY_MAX);
    }
}

/// What [`start`] gives the server: the status feed its `/v1/factory` routes serve, whatever the control plane is
/// doing, and the control plane when the user config turns it on.
pub struct InServer {
    pub feed: Feed,
    pub running: Option<Running>,
}

/// The server's `/v1/factory/status`, `/v1/factory/events` and `/v1/factory/refresh`, for it to mount behind its own bearer token.
pub fn api(feed: &Feed) -> axum::Router {
    surface::routes(feed.clone())
}

/// Whether the user config lists any repos for the factory, for a caller deciding whether to open `factory.log`.
/// Unreadable config counts as yes, so [`start`]'s error is logged where the factory's lines go.
pub fn configured(paths: &Paths) -> bool {
    read_host_config(paths).map_or(true, |c| !c.disabled && !c.repos.is_empty())
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
    /// Empty the worker pool so its sandboxes are made again from the image: idle ones are destroyed now with their warm checkpoints, busy ones when their run ends. For a rebuilt sandbox image, or to free the disk warm checkpoints have grown into. The next task on each sandbox pays a cold build.
    Refresh(ops::RefreshOpts),
    /// Let a UI (Lookout) connect: add its endpoint id to the allowlist. Takes effect on a running control plane at once.
    Allow { endpoint_id: String },
    /// Stop allowing a UI endpoint.
    Disallow { endpoint_id: String },
    /// List the UI endpoint ids allowed to connect.
    Allowed,
    /// Save a secret in the OS keyring (read from stdin), so the server's control plane needs no environment variables.
    #[command(subcommand)]
    Login(login::Login),
    /// Remove a secret saved by `login` (from the keyring and the file). The environment and fnox are untouched.
    Logout {
        #[arg(value_enum)]
        name: login::Secret,
    },
}

/// Sets up logging to the terminal and runs `cmd`.
pub async fn command(cmd: Command) -> Result<()> {
    trailhead_log::init_stdout(trailhead_log::env_filter_or("info"));
    match cmd {
        Command::Check(opts) => check(&opts).await,
        Command::Status(opts) => ops::status(&opts).await,
        Command::Logs(logs) => ops::logs(&logs).await,
        Command::Refresh(opts) => ops::refresh(&opts).await,
        Command::Allow { endpoint_id } => allow(&endpoint_id, true),
        Command::Disallow { endpoint_id } => allow(&endpoint_id, false),
        Command::Allowed => allowed(),
        Command::Login(cmd) => login(cmd).await,
        Command::Logout { name } => {
            let paths = Paths::from_env().ok_or(Error::NoDirs)?;
            println!("{}", login::logout(&paths.creds(), name)?);
            Ok(())
        }
    }
}

/// Saves a secret for the control plane in the OS keyring (`trailhead factory login ...`).
pub async fn login(cmd: login::Login) -> Result<()> {
    let paths = Paths::from_env().ok_or(Error::NoDirs)?;
    login::run(cmd, &paths.creds()).await
}

#[cfg(test)]
mod tests;
