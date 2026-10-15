use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use reqwest::{Method, StatusCode, Url};
use serde_json::Value;
use trailhead_server::{Paths, modified_ms};

pub struct Client {
    http: reqwest::Client,
    base: Url,
    token: String,
    pub local: bool,
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
    /// Connects to the server, starting a local one in the background if needed.
    pub async fn connect(url: &str, token: Option<String>, agent: Option<&str>) -> Result<Client> {
        let (http, base) = http_and_base(url)?;
        match health(&http, &base).await {
            Some(info) if is_local(&base) && is_stale(&info) => {
                eprintln!("note: restarting the local server on the updated trailhead binary");
                stop_local(&http, &base).await?;
                start_local_server(&http, &base).await?;
            }
            Some(_) => {}
            None if is_local(&base) => start_local_server(&http, &base).await?,
            None => bail!("can't reach the Trailhead server at {url}"),
        }
        let paths = Paths::from_env().context("no home directory")?;
        let token = match (token, agent) {
            (Some(t), _) => t,
            (None, Some(name)) => {
                let path = paths.agent_token(name);
                std::fs::read_to_string(&path)
                    .with_context(|| format!("no saved token for agent {name:?} at {}", path.display()))?
                    .trim()
                    .to_string()
            }
            (None, None) => {
                let path = paths.credentials();
                std::fs::read_to_string(&path)
                    .with_context(|| {
                        format!("no token: set TRAILHEAD_TOKEN or run a local server to create {}", path.display())
                    })?
                    .trim()
                    .to_string()
            }
        };
        let local = is_local(&base);
        Ok(Client { http, base, token, local })
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
        let res = req.send().await.context("request to Trailhead failed")?;
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
    pub pid: Option<u32>,
    pub exe: Option<std::path::PathBuf>,
    pub exe_modified_ms: Option<u64>,
    pub db: Option<std::path::PathBuf>,
    pub started_at: Option<String>,
}

pub async fn health(http: &reqwest::Client, base: &Url) -> Option<Health> {
    let url = base.join("health").ok()?;
    let res = http.get(url).timeout(Duration::from_secs(2)).send().await.ok()?;
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

/// Sends SIGTERM to the local server and waits for it to go away. Returns the pid
/// it stopped, or `None` if nothing was running.
pub async fn stop_local(http: &reqwest::Client, base: &Url) -> Result<Option<u32>> {
    let running = health(http, base).await;
    let pid_file = Paths::from_env().context("no home directory")?.pid_file();
    let from_file = || std::fs::read_to_string(&pid_file).ok().and_then(|p| p.trim().parse().ok());
    let pid = match (&running, running.as_ref().and_then(|h| h.pid).or_else(from_file)) {
        (_, Some(pid)) => pid,
        (None, None) => return Ok(None),
        (Some(_), None) => bail!(
            "a server is answering at {base} but didn't report its pid (it may predate `trailhead server`); stop it by hand"
        ),
    };
    let status = std::process::Command::new("kill").args(["-TERM", &pid.to_string()]).status()?;
    if !status.success() {
        return Ok(None);
    }
    for _ in 0..100 {
        if health(http, base).await.is_none() {
            return Ok(Some(pid));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(anyhow!("sent SIGTERM to the server (pid {pid}) but it's still answering"))
}

/// Runs `trailhead serve` detached, logging to the data directory. It keeps running
/// after this command exits.
pub async fn start_local_server(http: &reqwest::Client, base: &Url) -> Result<()> {
    let paths = Paths::from_env().context("no home directory")?;
    std::fs::create_dir_all(&paths.data_dir)?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(paths.server_log())?;
    let bind = format!("{}:{}", base.host_str().unwrap_or("127.0.0.1"), base.port_or_known_default().unwrap_or(7241));
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    cmd.args(["serve", "--bind", &bind]).stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    cmd.spawn().context("couldn't start the local server")?;
    for _ in 0..100 {
        if health(http, base).await.is_some() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(anyhow!("started a local server but it isn't answering; see {}", paths.server_log().display()))
}
