//! `Compute` for Fly.io Sprites, over the Sprites HTTP API (`https://api.sprites.dev/openapi.json`).
//!
//! Mapping, from the M0 spikes (`docs/factory/sandbox.md`):
//!
//! - A sandbox's id is its Sprite name, since every endpoint addresses Sprites by name.
//! - Labels are plain strings on a Sprite, so `key=value` pairs are stored as `"key=value"`. Connector access
//!   policies match on those exact strings (`sprite_labels: ["role=worker"]`).
//! - Egress becomes a deny-`*` policy plus one allow rule per domain. An empty rule list means unrestricted on
//!   Sprites, so this never sends one. Any egress other than none also allows the connector gateway.
//! - Checkpoints only restore the Sprite they came from; nothing here seeds one Sprite from another.

use std::time::Duration;

use async_trait::async_trait;
use reqwest::{Method, RequestBuilder, Response, StatusCode, Url};
use serde::Deserialize;
use serde_json::{Value, json};
use trailhead_factory::{
    CheckpointId, Compute, Egress, Error, Labels, Program, Result, Sandbox, SandboxId, SandboxSpec,
};

pub const API_URL: &str = "https://api.sprites.dev";

/// Where connectors are reached from inside a Sprite (`/v1/gateway/...`).
pub const GATEWAY_DOMAIN: &str = "api.sprites.dev";

const PAGE_SIZE: u32 = 500;

/// How long `start` watches a new service's output for an early exit.
const START_WATCH_SECS: u32 = 3;

#[derive(Clone)]
pub struct SpritesCompute {
    http: reqwest::Client,
    base: Url,
    token: String,
    prefix: Option<String>,
}

impl SpritesCompute {
    pub fn new(token: impl Into<String>) -> Result<SpritesCompute> {
        SpritesCompute::with_url(API_URL, token)
    }

    pub fn with_url(base: &str, token: impl Into<String>) -> Result<SpritesCompute> {
        let base = Url::parse(base).map_err(|e| Error::Config(format!("sprites url {base}: {e}")))?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .map_err(|e| Error::Other(e.to_string()))?;
        Ok(SpritesCompute { http, base, token: token.into(), prefix: None })
    }

    /// Narrows `list` to Sprites whose names start with `prefix`, so other Sprites in the organization aren't paged
    /// through. Sandbox names must then carry it.
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> SpritesCompute {
        self.prefix = Some(prefix.into());
        self
    }

    /// The recent output of a program started with `start`, for diagnosing a sandbox.
    pub async fn logs(&self, id: &SandboxId, program: &str) -> Result<String> {
        let mut url = self.url(&[&id.0, "services", program, "logs"]);
        url.query_pairs_mut().append_pair("lines", "3000");
        let body = ok(send(self.request(Method::GET, url)).await?).await?.text().await.map_err(http_err)?;
        let mut out = String::new();
        for line in body.lines().filter(|l| !l.trim().is_empty()) {
            match serde_json::from_str::<Value>(line) {
                Ok(ev) if ev["data"].is_string() => out.push_str(ev["data"].as_str().unwrap_or_default()),
                Ok(ev) => out.push_str(&format!("[{}]\n", ev["type"].as_str().unwrap_or("?"))),
                Err(_) => {
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
        Ok(out)
    }

    fn url(&self, segments: &[&str]) -> Url {
        let mut url = self.base.clone();
        url.path_segments_mut().expect("base url").pop_if_empty().push("v1").push("sprites").extend(segments);
        url
    }

    fn request(&self, method: Method, url: Url) -> RequestBuilder {
        self.http.request(method, url).bearer_auth(&self.token)
    }

    async fn get_sprite(&self, name: &str) -> Result<Option<Sandbox>> {
        let res = send(self.request(Method::GET, self.url(&[name]))).await?;
        if res.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let sprite: SpriteJson = json_body(ok(res).await?).await?;
        Ok(Some(sprite.into_sandbox()))
    }

    /// Runs a call that answers with an NDJSON progress stream, failing on its first `error` event and returning the
    /// `complete` event's message.
    async fn stream(&self, req: RequestBuilder) -> Result<String> {
        let body = ok(send(req).await?).await?.text().await.map_err(http_err)?;
        let mut done = None;
        for line in body.lines().filter(|l| !l.trim().is_empty()) {
            let event: StreamEvent = serde_json::from_str(line).map_err(|e| Error::Other(format!("{e}: {line}")))?;
            match event.kind.as_str() {
                "error" => {
                    let msg = event.error.unwrap_or_default();
                    return Err(if msg.contains("not found") { Error::NotFound(msg) } else { Error::Other(msg) });
                }
                "complete" => done = event.data,
                _ => {}
            }
        }
        done.ok_or_else(|| Error::Other("stream ended without completing".into()))
    }
}

#[async_trait]
impl Compute for SpritesCompute {
    /// Creation and the egress policy are two calls; a crash between them leaves a Sprite with open egress but
    /// nothing running in it, and the retry (or reconcile) applies the policy.
    async fn create(&self, spec: &SandboxSpec) -> Result<Sandbox> {
        let sandbox = match self.get_sprite(&spec.name).await? {
            Some(existing) => existing,
            None => {
                let body = json!({ "name": spec.name, "labels": encode_labels(&spec.labels) });
                let res = send(self.request(Method::POST, self.url(&[])).json(&body)).await?;
                if res.status().is_success() {
                    json_body::<SpriteJson>(res).await?.into_sandbox()
                } else {
                    // Lost a race with another create of the same name.
                    let err = ok(res).await.expect_err("not a success");
                    self.get_sprite(&spec.name).await?.ok_or(err)?
                }
            }
        };
        self.set_egress(&sandbox.id, &spec.egress).await?;
        Ok(sandbox)
    }

    async fn list(&self, labels: &Labels) -> Result<Vec<Sandbox>> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut url = self.url(&[]);
            {
                let mut q = url.query_pairs_mut();
                q.append_pair("max_results", &PAGE_SIZE.to_string());
                if let Some(prefix) = &self.prefix {
                    q.append_pair("prefix", prefix);
                }
                if let Some(token) = &token {
                    q.append_pair("continuation_token", token);
                }
            }
            let page: SpriteList = json_body(ok(send(self.request(Method::GET, url)).await?).await?).await?;
            out.extend(
                page.sprites
                    .into_iter()
                    .map(SpriteJson::into_sandbox)
                    .filter(|s| labels.iter().all(|(k, v)| s.labels.get(k) == Some(v))),
            );
            match page.next_continuation_token.filter(|t| page.has_more && !t.is_empty()) {
                Some(next) => token = Some(next),
                None => return Ok(out),
            }
        }
    }

    async fn checkpoint(&self, id: &SandboxId, note: &str) -> Result<CheckpointId> {
        let req = self.request(Method::POST, self.url(&[&id.0, "checkpoint"])).json(&json!({ "comment": note }));
        let done = self.stream(req).await?;
        // "Checkpoint v3 created successfully"; the API returns the new id only in this message.
        match done.split_whitespace().collect::<Vec<_>>().as_slice() {
            ["Checkpoint", cp, "created", ..] => Ok(CheckpointId(cp.to_string())),
            _ => Err(Error::Other(format!("unexpected checkpoint result: {done}"))),
        }
    }

    async fn restore(&self, id: &SandboxId, cp: &CheckpointId) -> Result<()> {
        let req = self.request(Method::POST, self.url(&[&id.0, "checkpoints", &cp.0, "restore"]));
        self.stream(req).await.map(drop)
    }

    /// Uploads the files, defines the program as a Sprites service (supervised, and restarted when the Sprite wakes),
    /// and starts it. A program that exits within the first few seconds is an error carrying its output.
    async fn start(&self, id: &SandboxId, program: &Program) -> Result<()> {
        for file in &program.files {
            // Written beside the target and renamed over it, so a running binary can be replaced.
            let tmp = format!("{}.new", file.path);
            let mut url = self.url(&[&id.0, "fs", "write"]);
            url.query_pairs_mut()
                .append_pair("path", &tmp)
                .append_pair("mode", &format!("{:04o}", file.mode))
                .append_pair("mkdir", "true");
            let req = self.request(Method::PUT, url).header("content-type", "application/octet-stream");
            ok(send(req.body(file.contents.clone())).await?).await?;
            let rename = json!({ "source": tmp, "dest": file.path, "workingDir": "/", "asRoot": false });
            ok(send(self.request(Method::POST, self.url(&[&id.0, "fs", "rename"])).json(&rename)).await?).await?;
        }
        let service = json!({ "cmd": program.cmd, "args": program.args, "needs": [] });
        let url = self.url(&[&id.0, "services", &program.name]);
        ok(send(self.request(Method::PUT, url).json(&service)).await?).await?;

        let mut url = self.url(&[&id.0, "services", &program.name, "start"]);
        url.query_pairs_mut().append_pair("duration", &START_WATCH_SECS.to_string());
        let body = ok(send(self.request(Method::POST, url)).await?).await?.text().await.map_err(http_err)?;
        service_started(&program.name, &body)
    }

    async fn set_egress(&self, id: &SandboxId, egress: &Egress) -> Result<()> {
        let body = json!({ "rules": egress_rules(egress) });
        let req = self.request(Method::POST, self.url(&[&id.0, "policy", "network"])).json(&body);
        ok(send(req).await?).await.map(drop)
    }

    /// Replaces the Sprite's labels. The API drops an empty list, so clearing them is its own flag.
    async fn set_labels(&self, id: &SandboxId, labels: &Labels) -> Result<()> {
        let body = if labels.is_empty() {
            json!({ "clear_labels": true })
        } else {
            json!({ "labels": encode_labels(labels) })
        };
        ok(send(self.request(Method::PUT, self.url(&[&id.0])).json(&body)).await?).await.map(drop)
    }

    async fn destroy(&self, id: &SandboxId) -> Result<()> {
        let res = send(self.request(Method::DELETE, self.url(&[&id.0]))).await?;
        if res.status() == StatusCode::NOT_FOUND {
            return Ok(());
        }
        ok(res).await.map(drop)
    }
}

/// Reads a service start's NDJSON log: an `error` event or an `exit` fails it, with the output seen so far.
fn service_started(name: &str, ndjson: &str) -> Result<()> {
    let mut output = String::new();
    for line in ndjson.lines().filter(|l| !l.trim().is_empty()) {
        let event: Value = serde_json::from_str(line).map_err(|e| Error::Other(format!("{e}: {line}")))?;
        let data = event["data"].as_str().unwrap_or_default();
        match event["type"].as_str().unwrap_or_default() {
            "stdout" | "stderr" => output.push_str(data),
            "error" => return Err(Error::Other(format!("service {name}: {data}"))),
            "exit" => {
                let code = &event["exit_code"];
                return Err(Error::Other(format!("service {name} exited ({code}): {}", output.trim())));
            }
            _ => {}
        }
    }
    Ok(())
}

pub fn encode_labels(labels: &Labels) -> Vec<String> {
    labels.iter().map(|(k, v)| format!("{k}={v}")).collect()
}

/// The inverse of [`encode_labels`]. A label set by hand without `=` becomes a key with an empty value.
pub fn decode_labels(labels: &[String]) -> Labels {
    labels
        .iter()
        .map(|l| match l.split_once('=') {
            Some((k, v)) => (k.to_string(), v.to_string()),
            None => (l.clone(), String::new()),
        })
        .collect()
}

pub fn egress_rules(egress: &Egress) -> Vec<Value> {
    let mut rules = vec![json!({ "domain": "*", "action": "deny" })];
    if egress.domains.is_empty() {
        return rules;
    }
    let mut domains: Vec<&str> = vec![GATEWAY_DOMAIN];
    for d in &egress.domains {
        if !domains.contains(&d.as_str()) {
            domains.push(d);
        }
    }
    rules.extend(domains.into_iter().map(|d| json!({ "domain": d, "action": "allow" })));
    rules
}

#[derive(Deserialize)]
struct SpriteJson {
    name: String,
    #[serde(default)]
    labels: Option<Vec<String>>,
}

impl SpriteJson {
    fn into_sandbox(self) -> Sandbox {
        Sandbox {
            id: SandboxId(self.name.clone()),
            labels: decode_labels(&self.labels.unwrap_or_default()),
            name: self.name,
        }
    }
}

#[derive(Deserialize)]
struct SpriteList {
    sprites: Vec<SpriteJson>,
    #[serde(default)]
    has_more: bool,
    #[serde(default)]
    next_continuation_token: Option<String>,
}

#[derive(Deserialize)]
struct StreamEvent {
    #[serde(rename = "type")]
    kind: String,
    data: Option<String>,
    error: Option<String>,
}

fn http_err(e: reqwest::Error) -> Error {
    Error::Other(format!("sprites api: {e}"))
}

async fn send(req: RequestBuilder) -> Result<Response> {
    req.send().await.map_err(http_err)
}

async fn ok(res: Response) -> Result<Response> {
    let status = res.status();
    if status.is_success() {
        return Ok(res);
    }
    let body = res.text().await.unwrap_or_default();
    let msg = format!("sprites api {status}: {}", body.trim());
    tracing::debug!("{msg}");
    Err(match status {
        StatusCode::NOT_FOUND => Error::NotFound(msg),
        StatusCode::CONFLICT => Error::Conflict(msg),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Error::Forbidden(msg),
        _ => Error::Other(msg),
    })
}

async fn json_body<T: serde::de::DeserializeOwned>(res: Response) -> Result<T> {
    res.json().await.map_err(http_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_round_trip() {
        let labels = Labels::from([("repo".into(), "lanesawyer/trailhead".into()), ("role".into(), "worker".into())]);
        let encoded = encode_labels(&labels);
        assert_eq!(encoded, ["repo=lanesawyer/trailhead", "role=worker"]);
        assert_eq!(decode_labels(&encoded), labels);
        assert_eq!(decode_labels(&["github".into()]).get("github").map(String::as_str), Some(""));
    }

    #[test]
    fn egress_denies_everything_else() {
        assert_eq!(egress_rules(&Egress::none()), [json!({ "domain": "*", "action": "deny" })]);
        let rules = egress_rules(&Egress { domains: vec!["*.iroh.link".into(), "api.sprites.dev".into()] });
        assert_eq!(
            rules,
            [
                json!({ "domain": "*", "action": "deny" }),
                json!({ "domain": "api.sprites.dev", "action": "allow" }),
                json!({ "domain": "*.iroh.link", "action": "allow" }),
            ]
        );
    }
}
