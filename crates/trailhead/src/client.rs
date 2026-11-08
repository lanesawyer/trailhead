use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use reqwest::{Method, StatusCode, Url};
use serde_json::Value;
use trailhead_server::{Paths, modified_ms};

use trailhead_client::creds::{Cred, CredStore};
pub use trailhead_client::{ApiError, Health};

/// The agent to act as, and the `.trailhead.toml` that chose it, if one did.
pub struct Identity {
    pub name: String,
    pub from: Option<std::path::PathBuf>,
}

pub struct Client {
    http: reqwest::Client,
    base: Url,
    token: String,
    /// The server is on a loopback address: this machine's, or a tunnel to another.
    pub local: bool,
    /// The server is this machine's own, so the CLI may start it again if it goes away.
    pub ours: bool,
    /// Where this server's tokens are saved on this machine.
    pub creds: CredStore,
}

pub fn is_local(url: &Url) -> bool {
    matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
}

impl Client {
    /// Connects to the server, starting this machine's one in the background if needed.
    pub async fn connect(url: &str, token: Option<String>, agent: Option<&Identity>) -> Result<Client> {
        let (http, base) = http_and_base(url)?;
        let paths = Paths::from_env().context("no home directory")?;
        let (ours, creds) = match health(&http, &base, None).await {
            Some(info) => {
                let ours = is_local(&base) && is_ours(&paths, &info);
                if is_local(&base) {
                    remember_tunnel(&paths, &base, !ours)?;
                }
                (ours, creds_for(&paths, &info, ours)?)
            }
            None if is_local(&base) && is_tunnel(&paths, &base) => bail!(
                "can't reach the Trailhead server at {url}, last reached through a tunnel from another machine; \
                 check the tunnel, or run `trailhead server start` to run a server on this machine instead"
            ),
            None if is_local(&base) => {
                start_local_server(&http, &base).await?;
                (true, paths.creds())
            }
            None => bail!("can't reach the Trailhead server at {url}"),
        };
        let token = saved_token(&creds, token.as_deref(), agent, (!ours).then_some(url))?;
        if ours
            && let Some(info) = health(&http, &base, Some(&token)).await
            && is_stale(&info)
            && !would_start_unscoped_under_systemd()
        {
            match info.runs_in_flight.unwrap_or(0) {
                // A restart now would cancel the factory's runs: the server drains them and restarts itself, and keeps
                // working meanwhile. A server that doesn't report `upgrade_pending` predates that and would die of
                // the SIGUSR1, so it is only told about; one already draining to stop is left to.
                runs @ 1.. => match (info.upgrade_pending, info.draining, info.pid) {
                    (Some(false), false, Some(pid)) => {
                        signal(pid, "-USR1")?;
                        eprintln!(
                            "note: a newer trailhead is installed; the server restarts onto it once its {runs} factory run(s) finish (`trailhead server restart --now` cancels them)"
                        );
                    }
                    (Some(true), ..) => eprintln!(
                        "note: a newer trailhead is installed; the server restarts onto it once its {runs} factory run(s) finish (`trailhead server restart --now` cancels them)"
                    ),
                    (_, true, _) => {}
                    _ => eprintln!(
                        "note: a newer trailhead is installed; {runs} factory run(s) are in flight, so the server keeps running. Restart it with `trailhead server restart` when they finish"
                    ),
                },
                // Someone started it at a terminal and may be watching it: replacing it with a background server would
                // take it away from them.
                0 if info.foreground => eprintln!(
                    "note: a newer trailhead is installed; restart the server running in the foreground (pid {}) to use it",
                    info.pid.map_or("?".into(), |p| p.to_string())
                ),
                0 => {
                    eprintln!("note: restarting the local server on the updated trailhead binary");
                    stop_local(&http, &base, true).await?;
                    start_local_server(&http, &base).await?;
                }
            }
        }
        Ok(Client { http, base: base.clone(), token, local: is_local(&base), ours, creds })
    }

    /// Sends `req`. If the local server has gone away since `connect` (stopped, or
    /// killed along with whatever started it), starts it again and retries once, so
    /// a long-lived `trailhead mcp` bridge outlives the server it started with.
    async fn send(&self, req: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let retry = req.try_clone();
        let err = match req.send().await {
            Ok(res) => return Ok(res),
            Err(e) => e,
        };
        match retry {
            Some(retry) if self.ours && err.is_connect() => {
                eprintln!("note: the local server isn't running; starting it");
                start_local_server(&self.http, &self.base).await?;
                retry.send().await.context("request to Trailhead failed")
            }
            _ => Err(err).context("request to Trailhead failed"),
        }
    }

    pub async fn request(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> Result<Value> {
        let url = self.base.join(path.trim_start_matches('/'))?;
        let mut req = self.http.request(method, url).bearer_auth(&self.token).query(query);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = self.send(req).await?;
        let status = res.status();
        if status == StatusCode::NO_CONTENT {
            return Ok(Value::Null);
        }
        let v: Value = res.json().await.context("bad response from server")?;
        if status.is_success() {
            return Ok(v);
        }
        Err(ApiError::from_body(&v).into())
    }

    /// Follows `GET /v1/events/stream`, calling `on_event` with each event's JSON.
    /// Reconnects from the last id seen if the connection drops.
    pub async fn watch(&self, query: &[(&str, String)], mut on_event: impl FnMut(Value)) -> Result<()> {
        let url = self.base.join("events/stream")?;
        let mut last_id: Option<String> = None;
        loop {
            let mut req = self.http.get(url.clone()).bearer_auth(&self.token).query(query);
            // The client's 30s default would cut the stream off.
            req = req.timeout(Duration::from_secs(60 * 60 * 24 * 365));
            if let Some(id) = &last_id {
                req = req.header("last-event-id", id);
            }
            let mut res = match req.send().await {
                Ok(r) if r.status().is_success() => r,
                Ok(r) => {
                    let v: Value = r.json().await.unwrap_or_default();
                    let err = &v["error"];
                    return Err(ApiError {
                        code: err["code"].as_str().unwrap_or("unknown").into(),
                        message: err["message"].as_str().unwrap_or("stream failed").into(),
                    }
                    .into());
                }
                Err(_) => {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };
            // Buffer bytes, not text: a character can span two chunks.
            let mut buf = Vec::new();
            while let Ok(Some(chunk)) = res.chunk().await {
                buf.extend_from_slice(&chunk);
                while let Some(frame) = take_sse_frame(&mut buf) {
                    if let Some(id) = frame.id {
                        last_id = Some(id);
                    }
                    if let Ok(v) = serde_json::from_str(&frame.data) {
                        on_event(v);
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    /// Forwards one JSON-RPC message to `/mcp` and returns the messages that come
    /// back: none for a notification, otherwise the response (plus anything the
    /// server streamed before it).
    pub async fn mcp_post(&self, message: &str, protocol_version: Option<&str>) -> Result<Vec<String>> {
        let url = self.base.join("/mcp")?;
        let mut req = self
            .http
            .post(url)
            .bearer_auth(&self.token)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(message.to_string());
        if let Some(v) = protocol_version {
            req = req.header("mcp-protocol-version", v);
        }
        let res = self.send(req).await?;
        let status = res.status();
        let is_sse = res
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"));
        let body = res.text().await?;
        if !status.is_success() {
            let v: Value = serde_json::from_str(&body).unwrap_or_default();
            return Err(ApiError {
                code: v["error"]["code"].as_str().unwrap_or("http_error").into(),
                message: v["error"]["message"].as_str().map(String::from).unwrap_or_else(|| format!("HTTP {status}")),
            }
            .into());
        }
        if body.trim().is_empty() {
            return Ok(Vec::new());
        }
        if !is_sse {
            return Ok(vec![body.trim().to_string()]);
        }
        let mut buf = body.into_bytes();
        // The last frame may lack its blank line.
        buf.extend_from_slice(b"\n\n");
        let mut out = Vec::new();
        while let Some(frame) = take_sse_frame(&mut buf) {
            if !frame.data.is_empty() {
                out.push(frame.data);
            }
        }
        Ok(out)
    }

    pub async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        self.request(Method::GET, path, query, None).await
    }

    pub async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.request(Method::POST, path, &[], Some(body)).await
    }

    pub async fn patch(&self, path: &str, body: Value) -> Result<Value> {
        self.request(Method::PATCH, path, &[], Some(body)).await
    }

    pub async fn delete(&self, path: &str) -> Result<Value> {
        self.request(Method::DELETE, path, &[], None).await
    }
}

/// One server-sent-events frame: its `id:` and its `data:` lines joined by `\n`.
#[derive(Debug, Default, PartialEq)]
struct SseFrame {
    id: Option<String>,
    data: String,
}

/// Removes the first complete frame (ended by a blank line, with LF, CRLF or CR
/// line endings) from `buf` and parses it. Frames are decoded whole, so a
/// multi-byte character split across chunks survives.
fn take_sse_frame(buf: &mut Vec<u8>) -> Option<SseFrame> {
    let (pos, len) = [&b"\r\n\r\n"[..], b"\n\n", b"\r\r"]
        .into_iter()
        .filter_map(|sep| buf.windows(sep.len()).position(|w| w == sep).map(|p| (p, sep.len())))
        .min_by_key(|&(p, _)| p)?;
    let raw: Vec<u8> = buf.drain(..pos + len).collect();
    let text = String::from_utf8_lossy(&raw[..pos]);
    let mut frame = SseFrame::default();
    let mut lines = Vec::new();
    for line in text.replace("\r\n", "\n").replace('\r', "\n").split('\n') {
        let value = |v: &str| v.strip_prefix(' ').unwrap_or(v).to_string();
        if let Some(id) = line.strip_prefix("id:") {
            frame.id = Some(value(id));
        } else if let Some(d) = line.strip_prefix("data:") {
            lines.push(value(d));
        } else if line == "data" {
            lines.push(String::new());
        }
    }
    frame.data = lines.join("\n");
    Some(frame)
}

/// This machine's own server keeps its tokens at the top of the config directory;
/// any other server's go in a directory named after its instance id.
fn creds_for(paths: &Paths, info: &Health, ours: bool) -> Result<CredStore> {
    match (&info.instance, ours) {
        (Some(id), false) => {
            paths.creds().for_server(id).ok_or_else(|| anyhow!("the server's instance id {id:?} isn't valid"))
        }
        _ => Ok(paths.creds()),
    }
}

/// The token for `--token`, the agent, or the user, in that order. `remote` is the
/// URL of a server that isn't this machine's, whose tokens come from `login`.
fn saved_token(
    creds: &CredStore,
    token: Option<&str>,
    agent: Option<&Identity>,
    remote: Option<&str>,
) -> Result<String> {
    let login = match remote {
        Some(url) => format!(
            "\non the server's machine, run `trailhead agent add <name> --print-token`, then pipe or paste it into \
             `trailhead --url {url} login` here and act with `--as <name>`"
        ),
        None => String::new(),
    };
    Ok(match (token, agent) {
        (Some(t), _) => t.to_string(),
        (None, Some(id)) => match creds.read(Cred::Agent(&id.name)) {
            Some((secret, _)) => secret,
            None => {
                let file = creds.file(Cred::Agent(&id.name));
                let msg = format!("no saved token for agent {:?} in the keyring or at {}", id.name, file.display());
                let msg = msg + &login;
                match &id.from {
                    Some(f) => {
                        bail!("{msg}\n{} sets agent = {:?}; pass --as-me to act as yourself", f.display(), id.name)
                    }
                    None => bail!("{msg}"),
                }
            }
        },
        (None, None) => match creds.read(Cred::User) {
            Some((secret, _)) => secret,
            None if remote.is_some() => bail!("no saved token for this server{login}"),
            None => bail!(
                "no token: set TRAILHEAD_TOKEN, or run a local server to create {}",
                creds.file(Cred::User).display()
            ),
        },
    })
}

/// The local user's saved token, for the lifecycle checks that need the server's pid and paths.
pub fn user_token(paths: &Paths) -> Option<String> {
    paths.creds().read(Cred::User).map(|(secret, _)| secret)
}

/// Whether `info` came from the server for this CLI's data directory, rather than
/// another one answering on the same port, such as a tunnel to another machine.
pub fn is_ours(paths: &Paths, info: &Health) -> bool {
    match &info.instance {
        Some(id) => paths.instance().as_ref() == Some(id),
        // Servers from before instance ids told anyone their database instead.
        None => info.db.as_ref() == Some(&our_db(paths)),
    }
}

fn our_db(paths: &Paths) -> std::path::PathBuf {
    std::env::var_os("TRAILHEAD_DB").map(std::path::PathBuf::from).unwrap_or_else(|| paths.db())
}

/// Loopback URLs that last answered with another machine's server. If one stops
/// answering, the tunnel is down, and starting a server here would quietly send
/// every write to an empty database.
fn tunnels_file(paths: &Paths) -> std::path::PathBuf {
    paths.data_dir.join("tunnels")
}

fn tunnels(paths: &Paths) -> Vec<String> {
    std::fs::read_to_string(tunnels_file(paths)).unwrap_or_default().lines().map(str::to_string).collect()
}

fn is_tunnel(paths: &Paths, base: &Url) -> bool {
    tunnels(paths).iter().any(|t| t == base.as_str())
}

fn remember_tunnel(paths: &Paths, base: &Url, tunnel: bool) -> Result<()> {
    let mut list = tunnels(paths);
    if list.iter().any(|t| t == base.as_str()) == tunnel {
        return Ok(());
    }
    list.retain(|t| t != base.as_str());
    if tunnel {
        list.push(base.to_string());
    }
    std::fs::create_dir_all(&paths.data_dir)?;
    std::fs::write(tunnels_file(paths), list.iter().map(|t| format!("{t}\n")).collect::<String>())?;
    Ok(())
}

pub fn http_and_base(url: &str) -> Result<(reqwest::Client, Url)> {
    let base = Url::parse(url).with_context(|| format!("bad server URL {url}"))?.join("/v1/")?;
    let http = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;
    Ok((http, base))
}

/// The server's pid and paths come back only with a `token`.
pub async fn health(http: &reqwest::Client, base: &Url, token: Option<&str>) -> Option<Health> {
    let url = base.join("health").ok()?;
    let mut req = http.get(url).timeout(Duration::from_secs(2));
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let res = req.send().await.ok()?;
    if !res.status().is_success() {
        return None;
    }
    Some(res.json().await.unwrap_or_default())
}

/// True when the server runs from this same binary path but the file on disk has
/// changed since it started, e.g. after `cargo install`. A server started from a
/// different binary (a dev build, say) is left alone.
pub fn is_stale(info: &Health) -> bool {
    let Ok(me) = std::env::current_exe() else { return false };
    let same_binary = info.exe.as_deref() == Some(me.as_path());
    same_binary && matches!((modified_ms(&me), info.exe_modified_ms), (Some(now), Some(then)) if now > then)
}

/// What [`stop_local`] did.
pub enum Stop {
    NotRunning,
    Stopped(u32),
    /// Signalled once: the server is draining the factory's runs and exits when they finish.
    Draining {
        pid: u32,
        runs: usize,
    },
}

/// Asks the local server to stop and waits for it to go away. With factory runs in flight it only starts a drain
/// unless `now`, which cancels them (they're requeued on the next start) with a second signal.
pub async fn stop_local(http: &reqwest::Client, base: &Url, now: bool) -> Result<Stop> {
    let paths = Paths::from_env().context("no home directory")?;
    let running = health(http, base, user_token(&paths).as_deref()).await;
    // Only stop a server that serves this CLI's database, so stopping one
    // instance can't take down another that happens to answer on the port.
    if let Some(info) = &running
        && !is_ours(&paths, info)
    {
        bail!("the server at {base} belongs to another data directory or machine (a tunnel?); refusing to stop it");
    }
    let ours = our_db(&paths);
    if let Some(db) = running.as_ref().and_then(|h| h.db.as_ref())
        && db != &ours
    {
        bail!("the server at {base} serves {}, not this CLI's {}; refusing to stop it", db.display(), ours.display());
    }
    let pid_file = paths.pid_file();
    // A stale pid file may name a pid the OS has since reused for something else.
    let is_trailhead = |pid: u32| {
        std::fs::read(format!("/proc/{pid}/cmdline"))
            .map_or(true, |c| String::from_utf8_lossy(&c).contains("trailhead"))
    };
    let from_file =
        || std::fs::read_to_string(&pid_file).ok().and_then(|p| p.trim().parse().ok()).filter(|&pid| is_trailhead(pid));
    let pid = match (&running, running.as_ref().and_then(|h| h.pid).or_else(from_file)) {
        (_, Some(pid)) => pid,
        (None, None) => return Ok(Stop::NotRunning),
        (Some(_), None) => bail!(
            "a server is answering at {base} but didn't report its pid (it may predate `trailhead server`); stop it by hand"
        ),
    };
    let runs = running.as_ref().and_then(|h| h.runs_in_flight).unwrap_or(0);
    let draining = running.as_ref().is_some_and(|h| h.draining);
    let upgrading = running.as_ref().is_some_and(|h| h.upgrade_pending == Some(true));
    if runs > 0 && (!draining || upgrading) {
        // The first stop starts a drain, or turns an upgrade's drain into a stop's: the server exits once the runs
        // finish instead of restarting onto the new binary.
        if !signal_stop(pid)? {
            return Ok(Stop::NotRunning);
        }
        if !now {
            return Ok(Stop::Draining { pid, runs });
        }
        // The next signal only cancels once this one has landed; two at once could arrive as one.
        for _ in 0..50 {
            if health(http, base, None).await.is_none_or(|h| h.draining && h.upgrade_pending != Some(true)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    } else if !now && runs > 0 {
        return Ok(Stop::Draining { pid, runs });
    }
    if !signal_stop(pid)? {
        return Ok(Stop::NotRunning);
    }
    // Cancelled factory runs get up to 30 s to record and tear down, then requests 10 s to finish.
    for _ in 0..450 {
        if health(http, base, None).await.is_none() {
            // It stops answering before it has finished exiting; it removes its pid file last.
            let ours = || std::fs::read_to_string(&pid_file).is_ok_and(|p| p.trim() == pid.to_string());
            for _ in 0..50 {
                if !ours() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            return Ok(Stop::Stopped(pid));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(anyhow!("asked the server (pid {pid}) to stop but it's still answering"))
}

/// Asks the process to shut down gracefully. Returns false if it wasn't running.
fn signal_stop(pid: u32) -> Result<bool> {
    signal(pid, "-TERM")
}

/// Sends `sig` (`-TERM`, say) to `pid`. Returns false if it wasn't running.
fn signal(pid: u32, sig: &str) -> Result<bool> {
    Ok(std::process::Command::new("kill").args([sig, &pid.to_string()]).status()?.success())
}

/// This binary's path. On Linux a process whose file was replaced by a reinstall
/// sees `<path> (deleted)`; the new binary at the original path is the one to run.
pub(crate) fn current_exe() -> Result<std::path::PathBuf> {
    let exe = std::env::current_exe()?;
    if !exe.exists()
        && let Some(path) = exe.to_str().and_then(|s| s.strip_suffix(" (deleted)"))
    {
        return Ok(path.into());
    }
    Ok(exe)
}

fn systemd_run_on_path() -> bool {
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|d| d.join("systemd-run").is_file()))
}

/// The variables to set on `systemd-run --user` so it can reach the user's systemd manager.
/// `None` when there is no runtime directory to be found. A cron job has no `XDG_RUNTIME_DIR`,
/// but `/run/user/<uid>` is still there when the user has a manager, so use it if it is ours.
fn scope_env(
    xdg_runtime_dir: Option<&std::ffi::OsStr>,
    dbus_address: Option<&std::ffi::OsStr>,
    uid: u32,
    run_dir_is_ours: impl Fn(&std::path::Path) -> bool,
) -> Option<Vec<(&'static str, std::ffi::OsString)>> {
    if xdg_runtime_dir.is_some_and(|d| !d.is_empty()) {
        return Some(Vec::new());
    }
    let dir = std::path::PathBuf::from(format!("/run/user/{uid}"));
    if !run_dir_is_ours(&dir) {
        return None;
    }
    let mut vars = vec![("XDG_RUNTIME_DIR", dir.clone().into_os_string())];
    if dbus_address.is_none_or(|d| d.is_empty()) {
        vars.push(("DBUS_SESSION_BUS_ADDRESS", format!("unix:path={}/bus", dir.display()).into()));
    }
    Some(vars)
}

/// The environment for a scoped start from this process, if `systemd-run --user` can plausibly work.
fn current_scope_env() -> Option<Vec<(&'static str, std::ffi::OsString)>> {
    use std::os::unix::fs::MetadataExt;
    if !systemd_run_on_path() {
        return None;
    }
    let uid = std::fs::metadata("/proc/self").ok()?.uid();
    scope_env(
        std::env::var_os("XDG_RUNTIME_DIR").as_deref(),
        std::env::var_os("DBUS_SESSION_BUS_ADDRESS").as_deref(),
        uid,
        |dir| std::fs::metadata(dir).is_ok_and(|m| m.is_dir() && m.uid() == uid),
    )
}

/// True when a server started from here would run unscoped, in this process's cgroup, although
/// systemd is around to do better (a cron job with no user manager to reach): it would die with the job.
fn would_start_unscoped_under_systemd() -> bool {
    systemd_run_on_path() && current_scope_env().is_none()
}

/// Says in server.log that the server is running outside its own scope, so a death with its launcher can be traced.
fn note_unscoped(mut log: &std::fs::File, why: &str) {
    use std::io::Write;
    let _ = writeln!(
        log,
        "note: starting the server unscoped ({why}); it will stop if the process that started it is killed"
    );
}

/// What `log` gained after byte `from`, at most its last 40 lines.
fn log_since(log: &std::path::Path, from: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let mut bytes = Vec::new();
    if let Ok(mut file) = std::fs::File::open(log) {
        let _ = file.seek(SeekFrom::Start(from));
        let _ = file.take(1 << 20).read_to_end(&mut bytes);
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(40)..].join("\n")
}

/// Runs `trailhead serve` detached, logging to the data directory. It keeps running
/// after this command exits.
pub async fn start_local_server(http: &reqwest::Client, base: &Url) -> Result<()> {
    let paths = Paths::from_env().context("no home directory")?;
    std::fs::create_dir_all(&paths.data_dir)?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(paths.server_log())?;
    let bind = format!("{}:{}", base.host_str().unwrap_or("127.0.0.1"), base.port_or_known_default().unwrap_or(7241));
    let exe = current_exe()?;
    let scope_env = current_scope_env();
    let spawn = |scoped: bool| -> Result<std::process::Child> {
        let mut cmd = if scoped {
            let mut cmd = std::process::Command::new("systemd-run");
            cmd.envs(scope_env.iter().flatten().map(|(k, v)| (*k, v)));
            cmd.args([
                "--user",
                "--scope",
                "--quiet",
                "--collect",
                "--unit",
                &format!("trailhead-server-{}", std::process::id()),
            ]);
            cmd.arg(&exe);
            cmd
        } else {
            std::process::Command::new(&exe)
        };
        cmd.args(["serve", "--bind", &bind, "--background"])
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log.try_clone()?);
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        Ok(cmd.spawn()?)
    };
    // Under systemd the server would otherwise stay in the launching app's cgroup scope, and
    // systemd-oomd killing that app (an editor, say) would take the server with it. Its own
    // scope keeps it alive; `systemd-run --scope` execs the server, so the pid is unchanged.
    let mut scoped = scope_env.is_some();
    let mut started_at = log.metadata().map(|m| m.len()).unwrap_or(0);
    if !scoped && systemd_run_on_path() {
        note_unscoped(&log, "no user systemd runtime directory found");
    }
    let mut child = spawn(scoped).context("couldn't start the local server")?;
    // Generous, since a busy machine (a full `cargo test` in a factory sandbox) can take well over 10 s to start it.
    for _ in 0..300 {
        if health(http, base, None).await.is_some() {
            return remember_tunnel(&paths, base, false);
        }
        if scoped && child.try_wait()?.is_some() {
            // No usable user manager (a container, a bare ssh session): run it unscoped.
            scoped = false;
            note_unscoped(&log, "systemd-run --user failed");
            let from = log.metadata().map(|m| m.len()).unwrap_or(0);
            child = spawn(false).context("couldn't start the local server")?;
            started_at = from;
        } else if let Some(status) = child.try_wait()? {
            // Another CLI's server may have taken the port first.
            if health(http, base, None).await.is_some() {
                return remember_tunnel(&paths, base, false);
            }
            // It refused to start (a broken factory configuration, say): say why rather than wait for it.
            return Err(anyhow!(
                "the local server stopped ({status}):\n{}",
                log_since(&paths.server_log(), started_at).trim_end()
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(anyhow!("started a local server but it isn't answering; see {}", paths.server_log().display()))
}

#[cfg(test)]
mod scope_env_tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn xdg_runtime_dir_set_needs_nothing_more() {
        assert_eq!(scope_env(Some(OsStr::new("/run/user/7")), None, 7, |_| false), Some(vec![]));
    }

    #[test]
    fn cron_falls_back_to_our_run_user_dir() {
        let vars = scope_env(None, None, 1000, |d| d == std::path::Path::new("/run/user/1000")).unwrap();
        assert_eq!(vars[0], ("XDG_RUNTIME_DIR", "/run/user/1000".into()));
        assert_eq!(vars[1], ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus".into()));
        // An empty XDG_RUNTIME_DIR counts as unset; an existing bus address is kept.
        let vars = scope_env(Some(OsStr::new("")), Some(OsStr::new("unix:path=/x")), 1000, |_| true).unwrap();
        assert_eq!(vars.len(), 1);
    }

    #[test]
    fn no_run_user_dir_of_ours_means_unscoped() {
        assert_eq!(scope_env(None, None, 1000, |_| false), None);
    }
}

#[cfg(test)]
mod sse_tests {
    use super::*;

    #[test]
    fn frame_split_inside_a_character_is_intact() {
        let bytes = "data: {\"t\":\"héllo ✓\"}\nid: 7\n\n".as_bytes();
        let cut = bytes.iter().position(|&b| b == 0xC3).unwrap() + 1;
        let mut buf = bytes[..cut].to_vec();
        assert_eq!(take_sse_frame(&mut buf), None);
        buf.extend_from_slice(&bytes[cut..]);
        let f = take_sse_frame(&mut buf).unwrap();
        assert_eq!(f.data, "{\"t\":\"héllo ✓\"}");
        assert_eq!(f.id.as_deref(), Some("7"));
        assert!(buf.is_empty());
    }

    #[test]
    fn crlf_frames_and_multiline_data() {
        let mut buf = b"data: a\r\ndata: b\r\n\r\ndata: c\n\n".to_vec();
        assert_eq!(take_sse_frame(&mut buf).unwrap().data, "a\nb");
        assert_eq!(take_sse_frame(&mut buf).unwrap().data, "c");
        assert_eq!(take_sse_frame(&mut buf), None);
    }
}
