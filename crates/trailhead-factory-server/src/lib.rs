//! The factory's control plane, as a library (`docs/factory/server-control-plane.md`): the setup the
//! `trailhead-factory` binary runs today and the Trailhead server will run behind its `factory` feature.
//!
//! The factory's settings are the `[factory]` table of each repo's `.trailhead.toml`: every `--repo <dir>`, else the
//! `repos` in `[factory]` of the user config, else the nearest one at or above the working directory, or `--config
//! <path>`. Repos share one process, so their `[factory.compute]` and `[factory.daemon]` must match.
//!
//! The daemon uploaded to sandboxes is `--daemon <path>`, else a `trailhead-daemon` next to this binary or on PATH,
//! else the release asset matching this binary's version.
//!
//! Sandboxes run on Sprites unless the factory file sets `[factory.compute] kind = "docker"`, which needs the `docker`
//! feature.
//!
//! It opens the server's database as the `factory` agent, keeps its iroh identity and daemon master key in the
//! credential store (so a restart can still reach the sandboxes it started), serves the API tunnel to the server
//! and runs the factory loop. [`check`] does everything up to the loop and prints what it found.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, Write};
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
use trailhead_compute_sprites::SpritesCompute;
use trailhead_control::{CoreTracker, Factory, Host, Placed, daemon_binary};
use trailhead_core::{Ctx, Store};
use trailhead_factory::{Compute, ComputeConfig, Daemon, FactoryConfig, HarnessSecrets, HostConfig, Target};
use trailhead_plugins::{CLAUDE_CODE, CLAUDE_CODE_OAUTH_TOKEN, harnesses, providers};
use trailhead_wire::api::ApiTunnel;
use trailhead_wire::daemon::{Launch, RemoteDaemon};
use trailhead_wire::ui::{UI_ALPN, UiAllowlist, UiProtocol};
use trailhead_wire::{API_ALPN, build_id};

mod find;
mod fnox;
mod logfile;
pub mod login;
pub mod ops;
mod repos;
mod surface;
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
    #[arg(long)]
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

fn load_or_create_keys(creds: &CredStore) -> Result<(SecretKey, [u8; 32]), String> {
    if let Some((saved, _)) = creds.read(KEYS) {
        let keys: Keys = serde_json::from_str(&saved).map_err(|e| format!("bad saved factory keys: {e}"))?;
        let endpoint = trailhead_wire::parse_secret_key(&keys.endpoint).map_err(|e| e.to_string())?;
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

/// The UI endpoints allowed on `trailhead/ui/0`, kept beside the control plane's other state.
fn ui_allowlist(paths: &Paths) -> UiAllowlist {
    UiAllowlist::new(paths.data_dir.join("factory").join("ui-allowed"))
}

fn allowlist() -> Result<UiAllowlist, String> {
    let paths = Paths::from_env().ok_or("can't find Trailhead's data and config directories")?;
    Ok(ui_allowlist(&paths))
}

/// `allow` (`add`) or `disallow` a UI endpoint id.
pub fn allow(endpoint_id: &str, add: bool) -> Result<(), String> {
    let id = trailhead_wire::parse_endpoint_id(endpoint_id).map_err(|e| e.to_string())?;
    let list = allowlist()?;
    let changed = if add { list.add(id) } else { list.remove(&id) }.map_err(|e| e.to_string())?;
    match (add, changed) {
        (true, true) => println!("allowed {id}"),
        (true, false) => println!("{id} was already allowed"),
        (false, true) => println!("no longer allowed: {id}"),
        (false, false) => println!("{id} wasn't allowed"),
    }
    Ok(())
}

/// Prints the UI endpoint ids allowed to connect.
pub fn allowed() -> Result<(), String> {
    let ids = allowlist()?.list().map_err(|e| e.to_string())?;
    if ids.is_empty() {
        println!("no UI endpoints are allowed; add one with `trailhead factory allow <endpoint-id>`");
    }
    ids.iter().for_each(|id| println!("{id}"));
    Ok(())
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

/// `opts` with the secrets the environment didn't set filled in from fnox (run in `dir`), then from what `trailhead
/// factory login` saved, and the fnox they were looked up in. It runs nothing until a name is missing from the
/// environment, and reads its config once.
fn resolve_opts(opts: &Opts, paths: &Paths, dir: &Path) -> Result<(Opts, fnox::Fnox), String> {
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

/// The user config's `[factory]` table; a missing file is the default.
fn read_host_config(paths: &Paths) -> Result<HostConfig, String> {
    let user_config = paths.config_dir.join("config.toml");
    match std::fs::read_to_string(&user_config) {
        Ok(src) => HostConfig::from_user_config(&src).map_err(|e| format!("{}: {e}", user_config.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
        Err(e) => Err(format!("{}: {e}", user_config.display())),
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

fn plan(run: &RunOpts, host_config: HostConfig) -> Result<Plan, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let dirs = repos::dirs(&run.repo, run.opts.config.as_deref(), &host_config, home.as_deref(), &cwd)?;
    let mut specs = repos::load_all(&dirs, run.opts.config.as_deref())?;
    let (harnesses, providers) = (harnesses(), providers());
    for spec in &specs {
        spec.config
            .check_plugins(&harnesses.names(), &providers.names())
            .map_err(|e| format!("{}: {e}", spec.config_path.display()))?;
        #[cfg(not(feature = "docker"))]
        if spec.config.target_specs().iter().any(|t| matches!(t.compute, ComputeConfig::Docker { .. })) {
            return Err(format!(
                "{}: Docker compute needs a build with the `docker` feature (`cargo install --features docker`)",
                spec.config_path.display()
            ));
        }
    }
    let (adapter, _) = specs[0].config.plugins().ok_or("the worker profile is missing")?;
    let adapter = adapter.to_string();
    let (pause_at, warnings) =
        host_config.effective_pause_at(specs.iter().map(|s| (s.config.repo.project.as_str(), &s.config.usage)));
    warnings.iter().for_each(|w| tracing::warn!("{w}"));
    for spec in &mut specs {
        spec.config.usage.pause_at = pause_at.clone();
    }
    Ok(Plan { opts: run.opts.clone(), host_config, specs, adapter })
}

async fn setup(run: &RunOpts) -> Result<Setup, String> {
    let paths = Paths::from_env().ok_or("can't find Trailhead's data and config directories")?;
    let plan = plan(run, read_host_config(&paths)?)?;
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    wire(plan, &paths, &cwd, None).await
}

/// Reaches out from a [`Plan`]: secrets, the daemon binary, the lock, the store, the iroh endpoint and the
/// computes. Fnox runs in `fnox_dir`. `store` is the server's own when it runs the control plane, which then needs no
/// check that the server is up; without one this opens the database itself.
async fn wire(plan: Plan, paths: &Paths, fnox_dir: &Path, store: Option<Arc<Store>>) -> Result<Setup, String> {
    let Plan { opts, host_config, mut specs, adapter } = plan;
    let (opts, fnox) = resolve_opts(&opts, paths, fnox_dir)?;
    let opts = &opts;
    let (harnesses, providers) = (harnesses(), providers());
    let mut harness_secrets = HarnessSecrets::new();
    if let (Some(token), CLAUDE_CODE) = (&opts.claude_oauth_token, adapter.as_str()) {
        harness_secrets.insert(CLAUDE_CODE_OAUTH_TOKEN.into(), token.clone());
    }
    for spec in &mut specs {
        if !harness_secrets.is_empty() {
            tracing::warn!(repo = %spec.slug(), harness = %adapter, "subscription mode: sandboxes hold the harness's credential and can reach its API");
            spec.config
                .egress
                .always
                .extend((harnesses.get(&adapter).map_err(|e| e.to_string())?.egress)(&harness_secrets));
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
    let in_server = store.is_some();
    let store = match store {
        Some(store) => store,
        None => {
            let db = paths.db();
            if let Some(dir) = db.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            Arc::new(Store::open_local(&db).await.map_err(|e| e.to_string())?)
        }
    };
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
        .map_err(|e| format!("binding the iroh endpoint: {e}"))?;
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
    let feed = surface::BoardFeed::default();
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

    let host = Host::new(host_config.max_sandboxes);
    let mut repos = Vec::new();
    for spec in specs {
        let slug = spec.slug();
        let repos::RepoSpec { config_path, remote, config, .. } = spec;
        let (_, provider_name) = config.plugins().ok_or("the worker profile is missing")?;
        let provider = *providers.get(provider_name).map_err(|e| e.to_string())?;
        let placed = targets
            .iter()
            .map(|(target, compute, daemon)| Placed {
                target: target.clone(),
                compute: compute.clone(),
                daemon: daemon.clone(),
                provider: provider(target.model_endpoint(&opts.model_endpoint).to_string()),
            })
            .collect();
        let mut factory = Factory::with_targets(config, placed, tracker.clone(), host.clone())
            .with_spend_file(paths.data_dir.join("factory").join(format!("spend-{}.json", slug.replace('/', "-"))));
        if !in_server {
            factory = factory.with_server_check(server_check(opts.server));
        }
        factory = factory.with_merge_mode_source(Box::new(move || {
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
    feed.set(repos.iter().map(|e| (e.slug.clone(), e.factory.status().clone())).collect());
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

/// Whether a Trailhead server answers at the server's address, which the daemons' API tunnel forwards to: `GET /v1/health`
/// must come back as JSON with `ok: true`, so another process on the port, or a wedged server, doesn't pass.
async fn server_reachable(server: SocketAddr) -> Result<(), String> {
    let probe = async {
        let client = reqwest::Client::builder().no_proxy().build().map_err(|e| format!("can't probe {server}: {e}"))?;
        let resp = client
            .get(format!("http://{server}/v1/health"))
            .send()
            .await
            .map_err(|e| format!("can't connect to {server}: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!(
                "{server} answered /v1/health with {}; it isn't a healthy Trailhead server",
                resp.status()
            ));
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("{server} answered /v1/health with something that isn't Trailhead's: {e}"))?;
        if body.get("ok") == Some(&serde_json::Value::Bool(true)) {
            Ok(())
        } else {
            Err(format!("{server} answered /v1/health without `ok: true`; it isn't a healthy Trailhead server"))
        }
    };
    match tokio::time::timeout(Duration::from_secs(3), probe).await {
        Ok(r) => r,
        Err(_) => Err(format!("probing {server} timed out")),
    }
}

/// Checks the server is reachable. When it isn't, tries once per outage to start it the way the CLI does
/// (a CLI command, which starts it in its own scope from the installed binary, and refuses to when the address is a tunnel to another machine), so runs aren't held until a CLI command
/// happens to do it. A failed start is reported along with the reason the server is down.
fn server_check(server: SocketAddr) -> trailhead_control::ServerCheck {
    server_check_with(server, |server| Box::pin(start_server(server)))
}

fn server_check_with<F>(server: SocketAddr, start: F) -> trailhead_control::ServerCheck
where
    F: Fn(SocketAddr) -> futures_util::future::BoxFuture<'static, Result<(), String>> + Send + Sync + 'static,
{
    let start = Arc::new(start);
    let tried = Arc::new(std::sync::atomic::AtomicBool::new(false));
    Box::new(move || {
        let (start, tried) = (start.clone(), tried.clone());
        Box::pin(async move {
            use std::sync::atomic::Ordering::SeqCst;
            let Err(down) = server_reachable(server).await else {
                tried.store(false, SeqCst);
                return Ok(());
            };
            if tried.swap(true, SeqCst) {
                return Err(down);
            }
            tracing::warn!(why = %down, "the Trailhead server is down; starting it");
            if let Err(e) = start(server).await {
                return Err(format!("{down}; starting it failed: {e}"));
            }
            server_reachable(server).await
        })
    })
}

/// Runs `trailhead whoami` (any command connects, and so starts the server) against `server`: the `trailhead` beside this binary, else the one on PATH.
async fn start_server(server: SocketAddr) -> Result<(), String> {
    let beside = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|d| d.join("trailhead")));
    let program = beside.filter(|p| p.is_file()).unwrap_or_else(|| PathBuf::from("trailhead"));
    let mut cmd = tokio::process::Command::new(&program);
    cmd.args(["--url", &format!("http://{server}"), "whoami"]).stdin(std::process::Stdio::null()).kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(60), cmd.output())
        .await
        .map_err(|_| format!("`{} whoami` timed out", program.display()))?
        .map_err(|e| format!("can't run `{}`: {e}", program.display()))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    Err(format!("`{} whoami` failed ({}): {}", program.display(), out.status, stderr.trim()))
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
) -> Result<Vec<Built>, String> {
    config
        .target_specs()
        .into_iter()
        .map(|spec| {
            let (compute, target) = compute(&spec.compute, opts, fnox, paths, binary.clone(), read_only)
                .map_err(|e| format!("target {}: {e}", spec.name))?;
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
        ComputeConfig::Docker { image, network, runtime, memory_mib } => {
            use trailhead_compute_docker::{DockerCompute, DockerConfig};
            let mut docker = DockerConfig::new(paths.data_dir.join("factory").join("docker"), binary);
            if let Some(image) = image {
                docker.image = image.clone();
            }
            if let Some(network) = network {
                docker.network = network.clone();
            }
            docker.runtime = runtime.clone();
            docker.memory_bytes = memory_mib.map(|m| m * 1024 * 1024);
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
            let compute = if read_only { DockerCompute::read_only(docker) } else { DockerCompute::new(docker) }
                .map_err(|e| e.to_string())?;
            Ok((Arc::new(compute), trailhead_compute_docker::target()))
        }
        #[cfg(not(feature = "docker"))]
        ComputeConfig::Docker { .. } => Err("Docker compute needs a build with the `docker` feature".into()),
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

fn check_result(failed: usize) -> Result<(), String> {
    match failed {
        0 => Ok(()),
        1 => Err("1 check failed".into()),
        n => Err(format!("{n} checks failed")),
    }
}

/// Sets everything up for `run`'s repos and reports what it found, without running anything.
pub async fn check(run: &RunOpts) -> Result<(), String> {
    let setup = setup(run).await?;
    let mut failed = 0;
    for entry in &setup.repos {
        let factory = &entry.factory;
        let project = &factory.config.repo.project;
        let queue: Vec<_> = factory.config.repo.queue.iter().cloned().map(trailhead_factory::TaskKey).collect();
        match factory.tracker.ready(project, &queue, 10).await {
            Ok(ready) => {
                let scope = if queue.is_empty() {
                    project.clone()
                } else {
                    let keys: Vec<_> = queue.iter().map(|q| q.0.as_str()).collect();
                    format!("{project}, under {}", keys.join(", "))
                };
                println!("{}: tracker: acting as {AGENT}; {} ready task(s) in {scope}", entry.slug, ready.len());
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
    let specs = first.config.target_specs();
    for (p, spec) in first.targets().iter().zip(specs.iter().map(|s| compute_name(&s.compute))) {
        match p.compute.list(&Default::default()).await {
            Ok(live) => println!(
                "compute: {} ({spec}, priority {}) reachable; {} sandbox(es) visible",
                p.target.name,
                p.target.priority,
                live.len()
            ),
            Err(e) => {
                failed += 1;
                println!("compute: {} ({spec}): {e}", p.target.name);
            }
        }
    }
    println!("endpoint: {}", setup.tunnel.endpoint().id());
    println!("tunnel: forwarding to {}", run.opts.server);
    setup.tunnel.shutdown().await.map_err(|e| e.to_string())?;
    check_result(failed)
}

/// Runs the control plane for every repo until Ctrl-C: the first drains, the second cancels what is left.
pub async fn run(run: &RunOpts) -> Result<(), String> {
    let setup = setup(run).await?;
    let paths = Paths::from_env().ok_or("can't find Trailhead's data and config directories")?;
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
    run_loops(setup, &paths, Duration::from_secs(run.opts.poll_secs), drain, cancel).await
}

/// Serves the status surface and runs every repo's loop until `drain` lets the runs finish or `cancel` stops them.
async fn run_loops(
    setup: Setup,
    paths: &Paths,
    poll: Duration,
    drain: tokio::sync::watch::Receiver<bool>,
    cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<(), String> {
    let Setup { repos, tunnel, _lock, store, ctx } = setup;
    // Beside the lock, so it exists and says the same thing: this machine has one control plane.
    let boards = repos.iter().map(|e| (e.slug.clone(), e.factory.status().clone())).collect();
    let serving = surface::serve(&paths.data_dir.join("factory"), boards).await?;
    tracing::info!(addr = %serving.addr, "serving factory status for `trailhead-factory status` and `logs --follow`");
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
    drop(serving);
    tunnel.shutdown().await.map_err(|e| e.to_string())?;
    match failed {
        0 => Ok(()),
        n => Err(format!("{n} repo loop(s) stopped on an error")),
    }
}

/// The control plane running inside the Trailhead server, from [`start`].
pub struct Running {
    drain: tokio::sync::watch::Sender<bool>,
    cancel: tokio::sync::watch::Sender<bool>,
    /// Set when the control plane has stopped, drained or not.
    done: tokio::sync::watch::Receiver<bool>,
    /// Each repo's board, once the control plane is up.
    boards: Arc<std::sync::Mutex<Vec<Arc<trailhead_control::StatusBoard>>>>,
}

impl Running {
    /// The runs and reviews in flight across every repo.
    pub fn runs_in_flight(&self) -> usize {
        self.boards.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|b| b.snapshot().runs.len()).sum()
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
/// (Sprites or GitHub unreachable, a locked keyring, a daemon download, another control plane holding the lock)
/// are logged and retried with backoff while the server keeps serving.
pub fn start(paths: Paths, store: Arc<Store>, server: SocketAddr) -> Result<Option<Running>, String> {
    let host_config = read_host_config(&paths)?;
    if host_config.disabled {
        tracing::info!("control: disabled; `trailhead factory enable` turns it back on");
        return Ok(None);
    }
    if host_config.repos.is_empty() {
        return Ok(None);
    }
    #[derive(clap::Parser)]
    struct Defaults {
        #[command(flatten)]
        run: RunOpts,
    }
    let mut run = <Defaults as clap::Parser>::try_parse_from(["serve"]).map_err(|e| e.to_string())?.run;
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
    let boards = Arc::new(std::sync::Mutex::new(Vec::new()));
    let filled = boards.clone();
    let control = async move {
        let mut wait = RETRY_FIRST;
        let setup = loop {
            match wire(plan.clone(), &paths, &fnox_dir, Some(store.clone())).await {
                Ok(setup) => break setup,
                Err(e) => tracing::warn!(
                    error = %e,
                    retry_secs = wait.as_secs(),
                    "control: starting; the server keeps serving and tries the factory again"
                ),
            }
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = fired(drain.clone()) => return,
            }
            wait = (wait * 2).min(RETRY_MAX);
        };
        *filled.lock().unwrap_or_else(|e| e.into_inner()) =
            setup.repos.iter().map(|e| e.factory.status().clone()).collect();
        if let Err(e) = run_loops(setup, &paths, poll, drain, cancel).await {
            tracing::error!(error = %e, "the control plane stopped; the server keeps serving");
        }
    };
    tokio::spawn(async move {
        control.await;
        done_tx.send_replace(true);
    });
    Ok(Some(Running { drain: drain_tx, cancel: cancel_tx, done, boards }))
}

/// Whether the user config lists any repos for the factory, for a caller deciding whether to open `factory.log`.
/// Unreadable config counts as yes, so [`start`]'s error is logged where the factory's lines go.
pub fn configured(paths: &Paths) -> bool {
    read_host_config(paths).map_or(true, |c| !c.disabled && !c.repos.is_empty())
}

/// The server's logging when it may run the control plane: the factory's events (its crates, iroh and anything in a
/// `repo` span) go to `factory.log` (when `configured`) and the buffer Lookout reads, everything else to stdout,
/// which the CLI points at `server.log`.
pub fn init_server_logging(paths: &Paths, default: &str) {
    use tracing_subscriber::fmt::writer::BoxMakeWriter;
    use tracing_subscriber::util::SubscriberInitExt;

    let buffer = LOGS.get_or_init(trailhead_control::logs::LogBuffer::default);
    let file = if configured(paths) { open_log_file() } else { None };
    let file = file.map(|w| BoxMakeWriter::new(std::sync::Mutex::new(w)));
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stdout());
    server_subscriber(trailhead_log::env_filter_or(default), buffer, BoxMakeWriter::new(std::io::stdout), ansi, file)
        .init();
}

/// [`init_server_logging`]'s subscriber, with its writers given. Spans pass every layer, so each one sees whether an
/// event is inside a `repo` span; only events are routed.
fn server_subscriber(
    filter: tracing_subscriber::EnvFilter,
    buffer: &trailhead_control::logs::LogBuffer,
    stdout: tracing_subscriber::fmt::writer::BoxMakeWriter,
    ansi: bool,
    file: Option<tracing_subscriber::fmt::writer::BoxMakeWriter>,
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
    fn server<S: tracing::Subscriber + for<'a> LookupSpan<'a>>(
        meta: &tracing::Metadata<'_>,
        cx: &tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
        !meta.is_event() || !is_factory(meta, cx)
    }

    let stdout =
        tracing_subscriber::fmt::layer().with_timer(trailhead_log::local_timer()).with_ansi(ansi).with_writer(stdout);
    let file = file.map(|w| {
        tracing_subscriber::fmt::layer().with_timer(trailhead_log::local_timer()).with_ansi(false).with_writer(w)
    });
    tracing_subscriber::registry()
        .with(buffer.layer().with_filter(filter.clone().and(dynamic_filter_fn(factory))))
        .with(file.with_filter(filter.clone().and(dynamic_filter_fn(factory))))
        .with(stdout.with_filter(filter.and(dynamic_filter_fn(server))))
}

/// Saves a secret for `run` in the OS keyring (`trailhead-factory login ...`).
pub async fn login(cmd: login::Login) -> Result<(), String> {
    let paths = Paths::from_env().ok_or("can't find Trailhead's data and config directories")?;
    login::run(cmd, &paths.creds()).await
}

/// `run`'s log file, if it can be opened; without one the log still goes to the terminal.
pub fn open_log_file() -> Option<logfile::Rotating> {
    let paths = Paths::from_env()?;
    let path = logfile::path(&paths.data_dir);
    match logfile::Rotating::open(&path) {
        Ok(file) => Some(file),
        Err(e) => {
            eprintln!(
                "trailhead-factory: can't open the log file {}: {e}; logging to the terminal only",
                path.display()
            );
            None
        }
    }
}

/// The control plane's recent log, for Lookout's Logs page; filled by the layer `init_logging` adds.
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

/// Logs to the terminal, to `file` when there is one, and to the in-memory buffer Lookout reads.
pub fn init_logging(file: Option<logfile::Rotating>) {
    let buffer = LOGS.get_or_init(trailhead_control::logs::LogBuffer::default);
    trailhead_log::init(trailhead_log::env_filter_or("info"), file.map(std::sync::Mutex::new), buffer.layer());
}

#[cfg(test)]
mod tests {
    use super::{check_result, lock_control_plane, server_check_with};
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

    /// Serves `app` on a free local port.
    async fn serve(app: axum::Router) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        addr
    }

    #[tokio::test]
    async fn server_check_sees_whether_trailhead_answers() {
        use axum::{Json, Router, http::StatusCode, routing::get};
        let healthy =
            serve(Router::new().route("/v1/health", get(|| async { Json(serde_json::json!({"ok": true})) }))).await;
        assert_eq!(server_check_with(healthy, |_| Box::pin(async { Ok(()) }))().await, Ok(()));
        // Something else on the port, a wedged server and a server that says it isn't ok all fail.
        let other = serve(Router::new()).await;
        let wedged =
            serve(Router::new().route("/v1/health", get(|| async { StatusCode::INTERNAL_SERVER_ERROR }))).await;
        let not_ok =
            serve(Router::new().route("/v1/health", get(|| async { Json(serde_json::json!({"ok": false})) }))).await;
        let not_json = serve(Router::new().route("/v1/health", get(|| async { "hello" }))).await;
        for addr in [other, wedged, not_ok, not_json] {
            let r = server_check_with(addr, |_| Box::pin(async { Err("no".into()) }))().await;
            assert!(r.as_ref().is_err_and(|e| e.contains("starting it failed")), "{addr}: {r:?}");
        }
        // Bound but not listening: it refuses connections, and holding the port keeps another test from listening on
        // it, as one could on a port this test had just let go of.
        let quiet = tokio::net::TcpSocket::new_v4().unwrap();
        quiet.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = quiet.local_addr().unwrap();
        let refused = server_check_with(addr, |_| Box::pin(async { Err("no".into()) }))().await;
        assert!(refused.as_ref().is_err_and(|e| e.contains("can't connect")), "{addr}: {refused:?}");
    }

    #[tokio::test]
    async fn a_down_server_is_started_once_per_outage() {
        let quiet = tokio::net::TcpSocket::new_v4().unwrap();
        quiet.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = quiet.local_addr().unwrap();
        let starts = std::sync::Arc::new(AtomicUsize::new(0));
        let counted = starts.clone();
        let check = server_check_with(addr, move |_| {
            counted.fetch_add(1, SeqCst);
            Box::pin(async { Err("boom".into()) })
        });
        let first = check().await.unwrap_err();
        assert!(first.contains("starting it failed: boom"), "{first}");
        let second = check().await.unwrap_err();
        assert!(!second.contains("boom"), "{second}");
        assert_eq!(starts.load(SeqCst), 1);
    }

    #[test]
    fn second_control_plane_is_refused_naming_the_holder() {
        let _spawning = crate::fnox::SPAWNING.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let held = lock_control_plane(dir.path(), "acme/app").unwrap();
        let err = lock_control_plane(dir.path(), "acme/other").unwrap_err();
        assert!(err.contains(&format!("pid {}, running acme/app", std::process::id())), "{err}");
        drop(held);
        assert!(lock_control_plane(dir.path(), "acme/other").is_ok());
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
        assert!(started.is_none());
    }

    #[tokio::test]
    async fn disabled_runs_nothing_even_with_a_broken_repo() {
        let dir = tempfile::tempdir().unwrap();
        let (paths, store) = instance(dir.path(), "[factory]\nrepos = [\"/no/such/repo\"]\ndisabled = true\n").await;
        assert!(!super::configured(&paths));
        assert!(super::start(paths, store, "127.0.0.1:7241".parse().unwrap()).unwrap().is_none());
    }

    #[tokio::test]
    async fn a_configuration_error_is_returned_not_retried() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("no-such-repo");
        let config = format!("[factory]\nrepos = [{:?}]\n", missing.display().to_string());
        let (paths, store) = instance(dir.path(), &config).await;
        let err = super::start(paths, store, "127.0.0.1:7241".parse().unwrap()).err().unwrap();
        assert!(err.contains("no-such-repo"), "{err}");

        let (paths, store) = instance(dir.path(), "[factory]\nrepos = 3\n").await;
        let err = super::start(paths, store, "127.0.0.1:7241".parse().unwrap()).err().unwrap();
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
        let err = super::start(paths, store, "127.0.0.1:7241".parse().unwrap()).err().unwrap();
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
        assert_eq!(check_result(0), Ok(()));
        assert_eq!(check_result(1), Err("1 check failed".into()));
        assert_eq!(check_result(2), Err("2 checks failed".into()));
    }
}
