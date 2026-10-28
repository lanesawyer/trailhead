use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use reqwest::{Method, StatusCode, Url};
use serde_json::Value;
use trailhead_server::{Paths, modified_ms};

use trailhead_server::creds::{Cred, CredStore};

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

/// An API error with its stable code, so the CLI can print `error[claim_held]: ...`.
#[derive(Debug)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "error[{}]: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

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
        {
            eprintln!("note: restarting the local server on the updated trailhead binary");
            stop_local(&http, &base).await?;
            start_local_server(&http, &base).await?;
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
        let err = &v["error"];
        Err(ApiError {
            code: err["code"].as_str().unwrap_or("unknown").into(),
            message: err["message"].as_str().unwrap_or("request failed").into(),
        }
        .into())
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
            let mut buf = String::new();
            while let Ok(Some(chunk)) = res.chunk().await {
                buf.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(end) = buf.find("\n\n") {
                    let frame: String = buf.drain(..end + 2).collect();
                    let mut data = String::new();
                    for line in frame.lines() {
                        if let Some(id) = line.strip_prefix("id:") {
                            last_id = Some(id.trim().to_string());
                        } else if let Some(d) = line.strip_prefix("data:") {
                            data.push_str(d.trim_start());
                        }
                    }
                    if let Ok(v) = serde_json::from_str(&data) {
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
        Ok(body
            .split("\n\n")
            .map(|frame| {
                frame.lines().filter_map(|l| l.strip_prefix("data:")).map(str::trim_start).collect::<Vec<_>>().join("")
            })
            .filter(|data| !data.is_empty())
            .collect())
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

/// `/v1/health` as the CLI reads it. Every field is optional so an older or newer
/// server still counts as running.
#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct Health {
    pub version: Option<String>,
    pub instance: Option<String>,
    pub pid: Option<u32>,
    pub exe: Option<std::path::PathBuf>,
    pub exe_modified_ms: Option<u64>,
    pub db: Option<std::path::PathBuf>,
    pub started_at: Option<String>,
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

/// Asks the local server to stop and waits for it to go away. Returns the pid
/// it stopped, or `None` if nothing was running.
pub async fn stop_local(http: &reqwest::Client, base: &Url) -> Result<Option<u32>> {
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
        (None, None) => return Ok(None),
        (Some(_), None) => bail!(
            "a server is answering at {base} but didn't report its pid (it may predate `trailhead server`); stop it by hand"
        ),
    };
    if !signal_stop(pid)? {
        return Ok(None);
    }
    for _ in 0..100 {
        if health(http, base, None).await.is_none() {
            // It stops answering before it has finished exiting; it removes its pid file last.
            let ours = || std::fs::read_to_string(&pid_file).is_ok_and(|p| p.trim() == pid.to_string());
            for _ in 0..50 {
                if !ours() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            return Ok(Some(pid));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(anyhow!("asked the server (pid {pid}) to stop but it's still answering"))
}

/// Asks the process to shut down gracefully. Returns false if it wasn't running.
fn signal_stop(pid: u32) -> Result<bool> {
    Ok(std::process::Command::new("kill").args(["-TERM", &pid.to_string()]).status()?.success())
}

/// This binary's path. On Linux a process whose file was replaced by a reinstall
/// sees `<path> (deleted)`; the new binary at the original path is the one to run.
fn current_exe() -> Result<std::path::PathBuf> {
    let exe = std::env::current_exe()?;
    if !exe.exists()
        && let Some(path) = exe.to_str().and_then(|s| s.strip_suffix(" (deleted)"))
    {
        return Ok(path.into());
    }
    Ok(exe)
}

/// Runs `trailhead serve` detached, logging to the data directory. It keeps running
/// after this command exits.
pub async fn start_local_server(http: &reqwest::Client, base: &Url) -> Result<()> {
    let paths = Paths::from_env().context("no home directory")?;
    std::fs::create_dir_all(&paths.data_dir)?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(paths.server_log())?;
    let bind = format!("{}:{}", base.host_str().unwrap_or("127.0.0.1"), base.port_or_known_default().unwrap_or(7241));
    let exe = current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["serve", "--bind", &bind]).stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    cmd.spawn().context("couldn't start the local server")?;
    // Generous, since a busy machine (a full `cargo test` in a factory sandbox) can take well over 10 s to start it.
    for _ in 0..300 {
        if health(http, base, None).await.is_some() {
            return remember_tunnel(&paths, base, false);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(anyhow!("started a local server but it isn't answering; see {}", paths.server_log().display()))
}
