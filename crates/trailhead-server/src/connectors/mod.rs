//! Connectors (`docs/app-platform.md`, "Connectors"): the server's registry of them, read from each
//! `connectors/<id>/connector.toml` of the checkout its builder last built; whether each is on and its settings, kept
//! in the settings table; its secrets, in the credential store; and its runs ([`runs`]).

mod runs;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use trailhead_client::creds::{Cred, CredStore};
use trailhead_core::connector::{
    ConnectorInfo, ConnectorPatch, ConnectorRun, FeedInfo, FeedRun, Manifest, SecretInfo, SettingInfo, secret_cred,
    secret_override_var,
};
use trailhead_core::{Ctx, Error, Result, Store};

pub(crate) use runs::start_run;
pub use runs::{SchedulerOptions, spawn_scheduler};

/// The variables of the server's own environment a connector's run is given; nothing else of it.
const PASSED_ENV: &[&str] =
    &["PATH", "TMPDIR", "LANG", "LC_ALL", "TZ", "SSL_CERT_FILE", "SSL_CERT_DIR", "RUST_BACKTRACE"];

/// How a run of a connector is set up on this machine.
#[derive(Debug, Clone)]
pub struct RunConfig {
    /// The server's address, which a run hands its feeds to.
    pub url: String,
    /// The server's data directory; each connector's state file is `connectors/<id>/state.json` in it.
    pub data_dir: PathBuf,
    /// Where `trailhead connector login` saves the secrets.
    pub creds: CredStore,
    /// The variables of the server's environment a run is given ([`PASSED_ENV`]), as they were when it started.
    pub env: Vec<(String, String)>,
    /// The server's own environment, read for a secret's override ([`secret_override_var`]).
    pub overrides: HashMap<String, String>,
}

impl RunConfig {
    /// From the server's own environment.
    pub fn new(url: String, data_dir: PathBuf, creds: CredStore) -> RunConfig {
        let all: HashMap<String, String> = std::env::vars().collect();
        let env = PASSED_ENV.iter().filter_map(|k| all.get(*k).map(|v| ((*k).to_string(), v.clone()))).collect();
        let overrides = all.into_iter().filter(|(k, _)| k.starts_with("TRAILHEAD_CONNECTOR_")).collect();
        RunConfig { url, data_dir, creds, env, overrides }
    }

    pub fn state_file(&self, id: &str) -> PathBuf {
        self.data_dir.join("connectors").join(id).join("state.json")
    }

    /// The secret's value: the server's environment's override, else the one saved by `trailhead connector login`.
    fn secret(&self, id: &str, name: &str) -> Option<String> {
        let from_env = self.overrides.get(&secret_override_var(id, name)).map(|v| v.trim().to_string());
        from_env
            .filter(|v| !v.is_empty())
            .or_else(|| self.creds.read(Cred::Secret(&secret_cred(id, name))).map(|(v, _)| v))
            .filter(|v| !v.is_empty())
    }
}

/// One connector the server knows: its manifest, or why it couldn't be read, and its program.
#[derive(Debug, Clone)]
struct Known {
    manifest: std::result::Result<Arc<Manifest>, String>,
    program: Option<PathBuf>,
}

/// A run in progress: when it started, and what the feeds it handed over did.
struct Running {
    started_at: Timestamp,
    feeds: BTreeMap<String, FeedRun>,
}

/// The connectors this server knows and runs.
pub struct Connectors {
    config: std::sync::OnceLock<RunConfig>,
    known: RwLock<BTreeMap<String, Known>>,
    running: Mutex<HashMap<String, Running>>,
    /// Each connector's token for its runs, kept in memory only and reminted when it nears its end.
    tokens: Mutex<HashMap<String, runs::RunToken>>,
    /// Set while a scheduler runs them, so next run times are shown.
    scheduled: std::sync::atomic::AtomicBool,
}

impl Default for Connectors {
    fn default() -> Self {
        Connectors::new(None)
    }
}

/// `settings` row `connector.<id>`: whether it is on, and its settings as set.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    settings: Map<String, Value>,
}

/// `settings` row `connector.<id>.runs`: the last run, and the failures in a row since the last success.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Runs {
    #[serde(default)]
    last: Option<ConnectorRun>,
    #[serde(default)]
    failures: u32,
    /// Whether the failures have raised their notification.
    #[serde(default)]
    notified: bool,
}

fn stored_name(id: &str) -> String {
    format!("connector.{id}")
}

fn runs_name(id: &str) -> String {
    format!("connector.{id}.runs")
}

async fn load<T: Default + for<'de> Deserialize<'de>>(store: &Store, ctx: &Ctx, name: &str) -> Result<T> {
    match store.setting(ctx, name).await? {
        Some(v) => Ok(serde_json::from_value(v).unwrap_or_default()),
        None => Ok(T::default()),
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Reads every `connectors/<id>/connector.toml` in the checkout at `dir`, in id order. One that doesn't read is that
/// connector's error; the others are read.
pub fn read(dir: &Path) -> Vec<(String, std::result::Result<Manifest, String>)> {
    let entries = match std::fs::read_dir(dir.join("connectors")) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => return vec![("connectors".into(), Err(format!("reading connectors/: {e}")))],
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().join("connector.toml").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort();
    ids.into_iter()
        .map(|id| {
            let file = format!("connectors/{id}/connector.toml");
            let read = std::fs::read_to_string(dir.join(&file))
                .map_err(|e| e.to_string())
                .and_then(|text| Manifest::parse(&text).map_err(|e| e.0))
                .and_then(|m| {
                    if m.id == id { Ok(m) } else { Err(format!("its id is `{}`, not its directory's `{id}`", m.id)) }
                })
                .map_err(|e| format!("{file}: {e}"));
            (id, read)
        })
        .collect()
}

impl Connectors {
    /// Knows no connector yet. Without a [`RunConfig`] it runs none: it can still take feeds and show what it knows.
    pub fn new(config: Option<RunConfig>) -> Connectors {
        Connectors {
            config: config.map(std::sync::OnceLock::from).unwrap_or_default(),
            known: RwLock::default(),
            running: Mutex::default(),
            tokens: Mutex::default(),
            scheduled: Default::default(),
        }
    }

    /// Runs connectors as `config` says, from now on. Only the first call counts.
    pub fn configure(&self, config: RunConfig) {
        let _ = self.config.set(config);
    }

    /// Replaces what it knows with `read`, each with its program from `program`. A connector no longer in the checkout
    /// is forgotten; its settings stay in the settings table for when it comes back.
    pub fn set(
        &self,
        read: Vec<(String, std::result::Result<Manifest, String>)>,
        program: impl Fn(&str) -> Option<PathBuf>,
    ) {
        let known = read
            .into_iter()
            .map(|(id, manifest)| {
                let program = manifest.as_ref().ok().and_then(|_| program(&id));
                (id, Known { manifest: manifest.map(Arc::new), program })
            })
            .collect();
        *self.known.write().unwrap_or_else(PoisonError::into_inner) = known;
    }

    /// Adds or replaces one connector, as [`Connectors::set`] would.
    pub fn insert(&self, manifest: Manifest, program: Option<PathBuf>) {
        let id = manifest.id.clone();
        let known = Known { manifest: Ok(Arc::new(manifest)), program };
        self.known.write().unwrap_or_else(PoisonError::into_inner).insert(id, known);
    }

    fn known(&self, id: &str) -> Result<Known> {
        let known = self.known.read().unwrap_or_else(PoisonError::into_inner);
        known.get(id).cloned().ok_or_else(|| Error::NotFound(format!("connector {id}")))
    }

    /// The connector's manifest, when it is known and reads.
    pub(crate) fn manifest(&self, id: &str) -> Result<Arc<Manifest>> {
        self.known(id)?.manifest.map_err(|e| Error::Invalid(format!("connector {id} can't be used: {e}")))
    }

    fn ids(&self) -> Vec<String> {
        self.known.read().unwrap_or_else(PoisonError::into_inner).keys().cloned().collect()
    }

    pub(crate) fn is_running(&self, id: &str) -> bool {
        lock(&self.running).contains_key(id)
    }

    /// Adds what one feed's handover did to the connector's run in progress, if one is.
    pub(crate) fn record_handover(&self, id: &str, handed: &trailhead_core::connector::HandedOver) {
        if let Some(run) = lock(&self.running).get_mut(id) {
            run.feeds.entry(handed.feed.clone()).or_default().add(handed);
        }
    }

    /// What it still needs before it can run, in plain words.
    fn missing(&self, id: &str, known: &Known, manifest: &Manifest, stored: &Stored) -> Vec<String> {
        let mut missing = Vec::new();
        for name in manifest.missing_settings(&stored.settings) {
            missing.push(format!("the setting `{name}` (`trailhead connector set {id} {name} <value>`)"));
        }
        for name in &manifest.secrets {
            if self.config.get().and_then(|c| c.secret(id, name)).is_none() {
                missing.push(format!(
                    "the secret `{name}` (`trailhead connector login {id} {name}` on the server's machine)"
                ));
            }
        }
        if self.config.get().is_none() {
            missing.push("a server that runs connectors: this one has no apps repo to build them from".into());
        } else if known.program.is_none() {
            missing.push("a build of its program: the apps repo hasn't been built with it yet".into());
        }
        missing
    }

    /// Every connector it knows, as `GET /v1/connectors` shows them.
    pub(crate) async fn list(&self, store: &Store, ctx: &Ctx) -> Result<Vec<ConnectorInfo>> {
        let fed = fed_collections(store, ctx).await?;
        let mut out = Vec::new();
        for id in self.ids() {
            out.push(self.info_with(store, ctx, &id, &fed).await?);
        }
        Ok(out)
    }

    pub(crate) async fn info(&self, store: &Store, ctx: &Ctx, id: &str) -> Result<ConnectorInfo> {
        let fed = fed_collections(store, ctx).await?;
        self.info_with(store, ctx, id, &fed).await
    }

    async fn info_with(
        &self,
        store: &Store,
        ctx: &Ctx,
        id: &str,
        fed: &BTreeMap<String, Vec<String>>,
    ) -> Result<ConnectorInfo> {
        let known = self.known(id)?;
        let stored: Stored = load(store, ctx, &stored_name(id)).await?;
        let runs: Runs = load(store, ctx, &runs_name(id)).await?;
        let running = self.is_running(id);
        let mut info = ConnectorInfo {
            id: id.to_string(),
            name: id.to_string(),
            enabled: stored.enabled,
            running,
            last_run: runs.last.clone(),
            ..ConnectorInfo::default()
        };
        let manifest = match &known.manifest {
            Ok(m) => m.clone(),
            Err(e) => {
                info.error = Some(e.clone());
                info.missing.push(format!("a connector.toml that reads: {e}"));
                return Ok(info);
            }
        };
        info.name.clone_from(&manifest.name);
        info.every_secs = manifest.every.as_secs();
        info.timeout_secs = manifest.timeout.as_secs();
        info.hosts.clone_from(&manifest.hosts);
        info.settings = manifest
            .settings
            .iter()
            .map(|(name, spec)| SettingInfo {
                name: name.clone(),
                kind: spec.kind,
                label: spec.label.clone(),
                help: spec.help.clone(),
                required: spec.required,
                value: stored.settings.get(name).cloned(),
            })
            .collect();
        let config = self.config.get();
        info.secrets = manifest
            .secrets
            .iter()
            .map(|name| SecretInfo { name: name.clone(), set: config.and_then(|c| c.secret(id, name)).is_some() })
            .collect();
        info.feeds = manifest
            .feeds
            .iter()
            .map(|(name, spec)| FeedInfo {
                name: name.clone(),
                fields: spec.fields.clone(),
                collections: fed.get(&format!("{id}.{name}")).cloned().unwrap_or_default(),
            })
            .collect();
        info.missing = self.missing(id, &known, &manifest, &stored);
        let scheduled = self.scheduled.load(std::sync::atomic::Ordering::SeqCst);
        if scheduled && stored.enabled && info.missing.is_empty() && !running {
            info.next_run = Some(next_run(runs.last.as_ref(), manifest.every));
        }
        Ok(info)
    }

    /// `PATCH /v1/connectors/{id}`, as a person: turning it on makes its agent, and settings are checked against its
    /// manifest before they are kept.
    pub(crate) async fn change(
        &self,
        store: &Store,
        ctx: &Ctx,
        id: &str,
        patch: ConnectorPatch,
    ) -> Result<ConnectorInfo> {
        if !ctx.is_human() {
            return Err(Error::Forbidden("only a person changes a connector".into()));
        }
        let manifest = self.manifest(id)?;
        let mut stored: Stored = load(store, ctx, &stored_name(id)).await?;
        if let Some(changes) = patch.settings {
            let mut merged = stored.settings.clone();
            for (name, value) in changes {
                if value.is_null() {
                    merged.remove(&name);
                } else {
                    merged.insert(name, value);
                }
            }
            stored.settings =
                manifest.check_settings(Value::Object(merged)).map_err(|e| Error::Invalid(format!("{id}: {e}")))?;
        }
        if let Some(enabled) = patch.enabled {
            if enabled {
                store.connector_agent(ctx, id).await?;
            } else {
                self.revoke_token(store, ctx, id).await;
            }
            stored.enabled = enabled;
        }
        store.set_setting(ctx, &stored_name(id), &serde_json::to_value(&stored)?).await?;
        self.info(store, ctx, id).await
    }

    /// The settings, the secrets and the program for a run, or what is missing for one.
    async fn ready(&self, store: &Store, ctx: &Ctx, id: &str) -> Result<runs::Ready> {
        let known = self.known(id)?;
        let manifest = self.manifest(id)?;
        let stored: Stored = load(store, ctx, &stored_name(id)).await?;
        if !stored.enabled {
            return Err(Error::Invalid(format!(
                "connector {id} is off: turn it on first (`trailhead connector enable {id}`)"
            )));
        }
        let missing = self.missing(id, &known, &manifest, &stored);
        let (Some(config), Some(program), true) = (self.config.get(), &known.program, missing.is_empty()) else {
            return Err(Error::Invalid(format!("connector {id} can't run yet; it needs {}", missing.join(", "))));
        };
        let secrets =
            manifest.secrets.iter().filter_map(|name| config.secret(id, name).map(|v| (name.clone(), v))).collect();
        Ok(runs::Ready { manifest, program: program.clone(), settings: stored.settings, secrets })
    }
}

/// When a connector that runs every `every` runs next, after `last`: now, if it never has.
fn next_run(last: Option<&ConnectorRun>, every: Duration) -> Timestamp {
    let now = Timestamp::now();
    match last {
        Some(run) => {
            let next = run.started_at + jiff::SignedDuration::try_from(every).unwrap_or(jiff::SignedDuration::MAX);
            next.max(now)
        }
        None => now,
    }
}

/// Each feed's app collections, as `<app>.<collection>`, by feed (`<connector>.<feed>`).
async fn fed_collections(store: &Store, ctx: &Ctx) -> Result<BTreeMap<String, Vec<String>>> {
    let every_app = Ctx { apps: None, ..ctx.clone() };
    let mut fed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (app, declaration) in store.list_apps(&every_app).await? {
        for (name, c) in declaration.collections {
            if let Some(from) = c.from {
                fed.entry(from).or_default().push(format!("{app}.{name}"));
            }
        }
    }
    Ok(fed)
}

/// Reads the connectors of the checkout the builder last built into `connectors`, with each one's program from that
/// build, and logs what it read. `None` while the builder holds no built commit; what was read before stays.
pub async fn register_built(connectors: &Connectors, builds: &trailhead_builder::Builds) -> Option<usize> {
    let read = builds.with_built_checkout(read).await?;
    for (id, manifest) in &read {
        match manifest {
            Ok(_) => tracing::info!(target: "connectors", id, "read the connector's manifest"),
            Err(e) => tracing::warn!(target: "connectors", id, error = %e, "the connector's manifest doesn't read"),
        }
    }
    let n = read.len();
    connectors.set(read, |id| builds.connector_program(id));
    Some(n)
}
