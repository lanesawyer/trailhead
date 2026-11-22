//! A connector's runs: each a process of its own, given a clean environment and a token that may only hand over its
//! feeds, stopped at its time limit or when the server stops; the scheduler that starts them every `every`; and what
//! the server keeps of each, with the notification when they keep failing.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use serde_json::{Map, Value};
use tokio::io::AsyncReadExt;
use trailhead_core::connector::{ConnectorRun, FAILING, Manifest, agent_name, env, secret_var};
use trailhead_core::{Ctx, Error, NewNotification, Principal, Result, SOURCE_SERVER, Store, TokenOptions};
use uuid::Uuid;

use super::{Connectors, Running, Runs, Stored, load, lock, next_run, runs_name, stored_name};
use crate::AppState;

/// The failures in a row that raise the notification.
const FAILURES_TO_NOTIFY: u32 = 3;
/// How much of what a run writes to stderr is kept, from its end.
const STDERR_KEPT: usize = 16 * 1024;
/// How many of stderr's last lines a failed run's error is.
const ERROR_LINES: usize = 20;
/// How long a run's token lasts at least; one reminted is good for this or twice the time limit, the longer.
const TOKEN_LIFE: SignedDuration = SignedDuration::from_hours(24);
/// What a run stopped by the server's shutdown says. It isn't counted as a failure.
const STOPPED: &str = "the server stopped during the run";

/// What a run is given.
pub(super) struct Ready {
    pub manifest: Arc<Manifest>,
    pub program: PathBuf,
    pub settings: Map<String, Value>,
    pub secrets: Vec<(String, String)>,
}

/// A connector's token for its runs, in memory only.
pub(super) struct RunToken {
    id: Uuid,
    secret: String,
    expires_at: Timestamp,
}

impl Connectors {
    /// The token the connector's run hands its feeds over with: the one this server last minted for it while it has
    /// long enough left, else a new one. Kept in memory only, and revoked when the connector is turned off or the
    /// server stops, so the tokens table gains a row a day per connector rather than one per run.
    async fn token(&self, store: &Store, ctx: &Ctx, id: &str, timeout: Duration) -> Result<String> {
        let margin = SignedDuration::try_from(timeout).unwrap_or(TOKEN_LIFE) + SignedDuration::from_mins(5);
        if let Some(t) = lock(&self.tokens).get(id).filter(|t| t.expires_at > Timestamp::now() + margin) {
            return Ok(t.secret.clone());
        }
        let agent = store.connector_agent(ctx, id).await?;
        let life = TOKEN_LIFE.max(margin * 2);
        let opts = TokenOptions { expires_in: Some(life), ..TokenOptions::default() };
        let name = format!("{} runs", agent.name);
        let (token, secret) = store.create_token_with(ctx, Principal::Agent(agent.id), &name, opts).await?;
        let expires_at = token.expires_at.unwrap_or_else(|| Timestamp::now() + life);
        let fresh = RunToken { id: token.id, secret: secret.clone(), expires_at };
        let old = lock(&self.tokens).insert(id.to_string(), fresh);
        if let Some(old) = old
            && let Err(e) = store.revoke_token(ctx, old.id).await
        {
            tracing::warn!(target: "connectors", id, error = %e, "couldn't revoke a connector's old token");
        }
        Ok(secret)
    }

    pub(super) async fn revoke_token(&self, store: &Store, ctx: &Ctx, id: &str) {
        let token = lock(&self.tokens).remove(id);
        if let Some(token) = token
            && let Err(e) = store.revoke_token(ctx, token.id).await
        {
            tracing::warn!(target: "connectors", id, error = %e, "couldn't revoke a connector's token");
        }
    }

    /// Revokes every connector's token, as the server stops.
    pub async fn revoke_tokens(&self, store: &Store, ctx: &Ctx) {
        let ids: Vec<String> = lock(&self.tokens).keys().cloned().collect();
        for id in ids {
            self.revoke_token(store, ctx, &id).await;
        }
    }
}

/// Starts a run of the connector now, as `ctx` (a person), and returns once it has started. Refused, saying why, when
/// it is off, can't run yet, or is running already.
pub(crate) async fn start_run(state: &Arc<AppState>, ctx: &Ctx, id: &str) -> Result<()> {
    if !ctx.is_human() || ctx.is_scoped() {
        return Err(Error::Forbidden("only a person runs a connector".into()));
    }
    let connectors = &state.connectors;
    let ready = connectors.ready(&state.store, ctx, id).await?;
    {
        let mut running = lock(&connectors.running);
        if running.contains_key(id) {
            return Err(Error::AlreadyExists(format!("a run of connector {id}: it is running now")));
        }
        running.insert(id.to_string(), Running { started_at: Timestamp::now(), feeds: BTreeMap::new() });
    }
    let (state, ctx, id) = (state.clone(), ctx.next_batch(), id.to_string());
    tokio::spawn(async move {
        let outcome = run_once(&state, &ctx, &id, ready).await;
        if let Err(e) = finish(&state, &ctx, &id, outcome).await {
            tracing::warn!(target: "connectors", id, error = %e, "couldn't record the connector's run");
        }
    });
    Ok(())
}

/// How a run's process ended.
enum Ended {
    Exited(std::io::Result<std::process::ExitStatus>),
    TimedOut,
    Stopped,
}

async fn run_once(state: &AppState, ctx: &Ctx, id: &str, ready: Ready) -> std::result::Result<(), String> {
    let connectors = &state.connectors;
    let config = connectors.config.get().ok_or("this server runs no connectors")?;
    let timeout = ready.manifest.timeout;
    let token = connectors
        .token(&state.store, ctx, id, timeout)
        .await
        .map_err(|e| format!("the server couldn't make the run's token: {e}"))?;
    let state_file = config.state_file(id);
    if let Some(dir) = state_file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    let mut cmd = tokio::process::Command::new(&ready.program);
    cmd.env_clear()
        .envs(config.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .env(env::URL, &config.url)
        .env(env::TOKEN, token)
        .env(env::SETTINGS, Value::Object(ready.settings).to_string())
        .env(env::STATE, &state_file);
    for (name, value) in &ready.secrets {
        cmd.env(secret_var(name), value);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        // Its own process group, so stopping it stops whatever it started too.
        .process_group(0)
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| format!("couldn't start {}: {e}", ready.program.display()))?;
    let pid = child.id();
    let stderr = child.stderr.take().expect("stderr is piped");
    let tail = tokio::spawn(read_tail(stderr));
    let mut stopping = state.shutdown.subscribe();
    let ended = tokio::select! {
        waited = tokio::time::timeout(timeout, child.wait()) => match waited {
            Ok(status) => Ended::Exited(status),
            Err(_) => Ended::TimedOut,
        },
        _ = stopping.wait_for(|s| *s) => Ended::Stopped,
    };
    if !matches!(ended, Ended::Exited(_)) {
        kill_group(pid);
        let _ = child.wait().await;
    }
    let tail = last_lines(&tail.await.unwrap_or_default());
    match ended {
        Ended::Exited(Ok(status)) if status.success() => Ok(()),
        Ended::Exited(Ok(status)) if tail.is_empty() => {
            Err(format!("it exited with {status} and wrote nothing to stderr"))
        }
        Ended::Exited(Ok(_)) => Err(tail),
        Ended::Exited(Err(e)) => Err(format!("waiting for it: {e}")),
        Ended::TimedOut => {
            let said = if tail.is_empty() { String::new() } else { format!("; it last said: {tail}") };
            Err(format!("it ran longer than its time limit of {}s and was stopped{said}", timeout.as_secs()))
        }
        Ended::Stopped => Err(STOPPED.into()),
    }
}

/// Everything read from `stderr`, keeping only its last [`STDERR_KEPT`] bytes.
async fn read_tail(mut stderr: tokio::process::ChildStderr) -> Vec<u8> {
    let mut kept = VecDeque::with_capacity(STDERR_KEPT);
    let mut buf = [0u8; 4096];
    while let Ok(n) = stderr.read(&mut buf).await {
        if n == 0 {
            break;
        }
        kept.extend(&buf[..n]);
        let over = kept.len().saturating_sub(STDERR_KEPT);
        kept.drain(..over);
    }
    kept.into()
}

/// The last [`ERROR_LINES`] lines that say something, trimmed.
fn last_lines(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(ERROR_LINES)..].join("\n")
}

/// Stops a run's process group.
fn kill_group(pid: Option<u32>) {
    if let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()) {
        // SAFETY: kill takes no pointers; a group that has already gone gets ESRCH, which is fine.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
}

/// Records the run's outcome in the settings table, and raises the notification after the failures in a row that call
/// for it, or resolves it after a success.
async fn finish(state: &AppState, ctx: &Ctx, id: &str, outcome: std::result::Result<(), String>) -> Result<()> {
    let running = lock(&state.connectors.running).remove(id);
    let (started_at, feeds) = running.map_or((Timestamp::now(), BTreeMap::new()), |r| (r.started_at, r.feeds));
    let took = Timestamp::now().duration_since(started_at);
    let run = ConnectorRun {
        started_at,
        duration_ms: u64::try_from(took.as_millis()).unwrap_or(0),
        ok: outcome.is_ok(),
        error: outcome.err(),
        feeds,
    };
    match &run.error {
        None => tracing::info!(target: "connectors", id, ms = run.duration_ms, "the connector ran"),
        Some(e) => tracing::warn!(target: "connectors", id, error = %e, "the connector's run failed"),
    }
    let store = &state.store;
    let mut runs: Runs = load(store, ctx, &runs_name(id)).await?;
    let subject = agent_name(id);
    match &run.error {
        None => {
            if runs.notified {
                store.resolve_notifications_about(ctx, SOURCE_SERVER, &subject).await?;
            }
            runs.failures = 0;
            runs.notified = false;
        }
        Some(e) if e == STOPPED => {}
        Some(e) => {
            runs.failures += 1;
            if runs.failures >= FAILURES_TO_NOTIFY && !runs.notified {
                let name = state.connectors.manifest(id).map_or_else(|_| id.to_string(), |m| m.name.clone());
                let notice = NewNotification {
                    source: SOURCE_SERVER.into(),
                    kind: FAILING.into(),
                    title: format!("The {name} connector keeps failing"),
                    body: format!("Its last {} runs failed. The last one said:\n{e}", runs.failures),
                    subject: Some(subject),
                    ..NewNotification::default()
                };
                store.raise_notification(ctx, notice).await?;
                runs.notified = true;
            }
        }
    }
    runs.last = Some(run);
    store.set_setting(ctx, &runs_name(id), &serde_json::to_value(&runs)?).await
}

/// How the scheduler looks for runs that are due.
#[derive(Debug, Clone, Copy)]
pub struct SchedulerOptions {
    /// How often it looks.
    pub tick: Duration,
}

impl Default for SchedulerOptions {
    fn default() -> Self {
        SchedulerOptions { tick: Duration::from_secs(5) }
    }
}

/// Runs each connector that is on and can run every `every`, as `ctx` (the local person), until the server stops. A
/// connector that has run before runs `every` after its last run started, so a restart doesn't run them all at once.
pub fn spawn_scheduler(state: Arc<AppState>, ctx: Ctx, options: SchedulerOptions) -> tokio::task::JoinHandle<()> {
    state.connectors.scheduled.store(true, std::sync::atomic::Ordering::SeqCst);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(options.tick);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut stopping = state.shutdown.subscribe();
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = stopping.wait_for(|s| *s) => break,
            }
            for id in state.connectors.ids() {
                match due(&state, &ctx, &id).await {
                    Ok(true) => {
                        if let Err(e) = start_run(&state, &ctx.next_batch(), &id).await {
                            tracing::debug!(target: "connectors", id, error = %e, "the connector is due but can't run");
                        }
                    }
                    Ok(false) => {}
                    Err(e) => {
                        tracing::debug!(target: "connectors", id, error = %e, "couldn't tell if the connector is due");
                    }
                }
            }
        }
    })
}

async fn due(state: &AppState, ctx: &Ctx, id: &str) -> Result<bool> {
    let connectors = &state.connectors;
    let Ok(manifest) = connectors.manifest(id) else { return Ok(false) };
    if connectors.is_running(id) {
        return Ok(false);
    }
    let stored: Stored = load(&state.store, ctx, &stored_name(id)).await?;
    if !stored.enabled {
        return Ok(false);
    }
    let runs: Runs = load(&state.store, ctx, &runs_name(id)).await?;
    Ok(next_run(runs.last.as_ref(), manifest.every) <= Timestamp::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_is_the_last_lines_that_say_something() {
        let text: String = (1..=30).map(|n| format!("line {n}\n\n")).collect();
        let tail = last_lines(text.as_bytes());
        assert!(tail.starts_with("line 11\nline 12"), "{tail}");
        assert!(tail.ends_with("line 30"));
        assert_eq!(last_lines(b"  \n"), "");
    }
}
