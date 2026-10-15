use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use reqwest::{Method, StatusCode, Url};
use serde_json::Value;
use trailhead_server::Paths;

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

fn is_local(url: &Url) -> bool {
    matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
}

impl Client {
    /// Connects to the server, starting a local one in the background if needed.
    pub async fn connect(url: &str, token: Option<String>, agent: Option<&str>) -> Result<Client> {
        let base = Url::parse(url).with_context(|| format!("bad server URL {url}"))?.join("/v1/")?;
        let http = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;
        if !healthy(&http, &base).await {
            if !is_local(&base) {
                bail!("can't reach the Trailhead server at {url}");
            }
            start_local_server(&http, &base).await?;
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

async fn healthy(http: &reqwest::Client, base: &Url) -> bool {
    let Ok(url) = base.join("health") else { return false };
    matches!(http.get(url).timeout(Duration::from_secs(2)).send().await, Ok(r) if r.status().is_success())
}

/// Runs `trailhead serve` detached, logging to the data directory. It keeps running
/// after this command exits.
async fn start_local_server(http: &reqwest::Client, base: &Url) -> Result<()> {
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
        if healthy(http, base).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(anyhow!("started a local server but it isn't answering; see {}", paths.server_log().display()))
}
