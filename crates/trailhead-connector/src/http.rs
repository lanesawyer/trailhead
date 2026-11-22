//! `Http`, the only way a connector reaches the network.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use reqwest::Url;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::manifest::Manifest;
use crate::replay::{Recorded, Recorder, Replay};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_REDIRECTS: usize = 5;

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("{url}: host `{host}` is not one this connector declares in `hosts`")]
    HostNotAllowed { host: String, url: String },
    #[error("{0}: only https is allowed")]
    NotHttps(String),
    #[error("`{0}` is not a usable URL")]
    InvalidUrl(String),
    #[error("{method} {url}: {message}")]
    Transport { method: String, url: String, message: String },
    #[error("{url} answered {status}: {body}")]
    Status { url: String, status: u16, body: String },
    #[error("{url}: the answer is not the JSON expected: {message}")]
    Decode { url: String, message: String },
    #[error("replay has no recorded answer for {method} {url}; recorded: [{}]", known.join(", "))]
    NotRecorded { method: String, url: String, known: Vec<String> },
    #[error("replay: {0}")]
    Replay(String),
    #[error("recording: {0}")]
    Record(String),
}

/// How hard to try again on a 429, a 5xx or a failed connection: a few tries, a doubling wait, never longer than `cap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub max_tries: u32,
    pub base: Duration,
    pub cap: Duration,
}

impl Default for RetryPolicy {
    fn default() -> RetryPolicy {
        RetryPolicy { max_tries: 4, base: Duration::from_millis(500), cap: Duration::from_secs(30) }
    }
}

impl RetryPolicy {
    pub const NONE: RetryPolicy = RetryPolicy { max_tries: 1, base: Duration::ZERO, cap: Duration::ZERO };

    fn wait(&self, tries_so_far: u32, retry_after: Option<Duration>) -> Duration {
        let backoff = self.base.saturating_mul(2u32.saturating_pow(tries_so_far.saturating_sub(1)));
        retry_after.unwrap_or(backoff).min(self.cap)
    }
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
    url: String,
}

impl Response {
    pub(crate) fn new(status: u16, headers: BTreeMap<String, String>, body: Vec<u8>, url: String) -> Response {
        Response { status, headers, body, url }
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// The answer to a request that sent `If-None-Match`: what the connector saved is still current.
    pub fn not_modified(&self) -> bool {
        self.status == 304
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(&name.to_ascii_lowercase()).map(String::as_str)
    }

    pub fn headers(&self) -> &BTreeMap<String, String> {
        &self.headers
    }

    pub fn etag(&self) -> Option<&str> {
        self.header("etag")
    }

    pub fn bytes(&self) -> &[u8] {
        &self.body
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json<T: DeserializeOwned>(&self) -> Result<T, HttpError> {
        serde_json::from_slice(&self.body)
            .map_err(|e| HttpError::Decode { url: self.url.clone(), message: e.to_string() })
    }

    /// An error for a 4xx or 5xx, carrying the start of the body; a 2xx or a 304 passes through.
    pub fn error_for_status(self) -> Result<Response, HttpError> {
        if self.status < 400 {
            return Ok(self);
        }
        let body: String = self.text().chars().take(300).collect();
        Err(HttpError::Status { url: self.url, status: self.status, body })
    }

    fn retry_after(&self) -> Option<Duration> {
        self.header("retry-after")?.trim().parse().ok().map(Duration::from_secs)
    }

    /// The `rel="next"` URL of a `Link` header, made absolute against the request's URL.
    pub fn next_link(&self) -> Option<String> {
        let mut rest = self.header("link")?;
        while let Some(start) = rest.find('<') {
            let after = &rest[start + 1..];
            let end = after.find('>')?;
            let target = &after[..end];
            let tail = &after[end + 1..];
            let params_end = tail.find('<').unwrap_or(tail.len());
            let is_next = tail[..params_end].split(';').any(|p| {
                p.split_once('=').is_some_and(|(k, v)| {
                    k.trim().eq_ignore_ascii_case("rel")
                        && v.trim()
                            .trim_end_matches(',')
                            .trim()
                            .trim_matches('"')
                            .split_whitespace()
                            .any(|t| t.eq_ignore_ascii_case("next"))
                })
            });
            if is_next {
                let base = Url::parse(&self.url).ok()?;
                return base.join(target).ok().map(String::from);
            }
            rest = &tail[params_end..];
        }
        None
    }
}

enum Backend {
    Network { client: reqwest::Client, recorder: Option<Arc<Recorder>> },
    Replay(Arc<Replay>),
}

impl Clone for Backend {
    fn clone(&self) -> Backend {
        match self {
            Backend::Network { client, recorder } => {
                Backend::Network { client: client.clone(), recorder: recorder.clone() }
            }
            Backend::Replay(r) => Backend::Replay(r.clone()),
        }
    }
}

#[derive(Clone)]
pub struct Http {
    hosts: Arc<[String]>,
    user_agent: String,
    backend: Backend,
    retry: RetryPolicy,
    allow_plain_http: bool,
}

impl std::fmt::Debug for Http {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Http").field("hosts", &self.hosts).field("user_agent", &self.user_agent).finish_non_exhaustive()
    }
}

fn host_allowed(hosts: &[String], url: &Url) -> bool {
    url.host_str().is_some_and(|h| hosts.iter().any(|a| a.eq_ignore_ascii_case(h)))
}

impl Http {
    pub fn new(manifest: &Manifest) -> Result<Http, HttpError> {
        Http::build(manifest, false)
    }

    fn build(manifest: &Manifest, allow_plain_http: bool) -> Result<Http, HttpError> {
        let hosts: Arc<[String]> = manifest.hosts.clone().into();
        let user_agent = user_agent(manifest);
        let policy_hosts = hosts.clone();
        let policy = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.error("too many redirects");
            }
            let url = attempt.url();
            let scheme_ok = url.scheme() == "https" || (allow_plain_http && url.scheme() == "http");
            if scheme_ok && host_allowed(&policy_hosts, url) {
                attempt.follow()
            } else {
                let msg = format!("redirect to {url} refused: not https or not a declared host");
                attempt.error(msg)
            }
        });
        let client = reqwest::Client::builder()
            .user_agent(user_agent.clone())
            .redirect(policy)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| HttpError::Replay(format!("the HTTP client could not be built: {e}")))?;
        Ok(Http {
            hosts,
            user_agent,
            backend: Backend::Network { client, recorder: None },
            retry: RetryPolicy::default(),
            allow_plain_http,
        })
    }

    #[cfg(test)]
    pub(crate) fn new_allowing_plain_http(manifest: &Manifest) -> Http {
        Http::build(manifest, true).expect("a client builds")
    }

    #[cfg(test)]
    pub(crate) fn allowing_plain_http(mut self) -> Http {
        self.allow_plain_http = true;
        self
    }

    /// Answers from `entries` instead of the network, with the same host and scheme rules as the real client.
    pub fn replay(manifest: &Manifest, entries: Vec<Recorded>) -> Result<Http, HttpError> {
        Ok(Http::replaying(manifest, Replay::new(entries)?))
    }

    /// Like [`Http::replay`], from the `*.json` files of a directory (see [`Recorded`]), in file-name order.
    pub fn replay_dir(manifest: &Manifest, dir: impl Into<PathBuf>) -> Result<Http, HttpError> {
        Ok(Http::replaying(manifest, Replay::from_dir(&dir.into())?))
    }

    fn replaying(manifest: &Manifest, replay: Replay) -> Http {
        Http {
            hosts: manifest.hosts.clone().into(),
            user_agent: user_agent(manifest),
            backend: Backend::Replay(Arc::new(replay)),
            retry: RetryPolicy::NONE,
            allow_plain_http: false,
        }
    }

    pub fn with_retry(mut self, retry: RetryPolicy) -> Http {
        self.retry = retry;
        self
    }

    /// Saves every answer the real network gives into `dir`, one file each, in the format [`Http::replay_dir`] reads.
    /// A person calls this on purpose to capture a connector's fixtures; it does nothing on a replaying client. Only
    /// `content-type`, `link`, `etag` and `retry-after` are kept of the headers, so a cookie or token is never saved,
    /// but a body may hold personal data: read what was recorded before committing it.
    pub fn record_to(mut self, dir: impl Into<PathBuf>) -> Http {
        if let Backend::Network { recorder, .. } = &mut self.backend {
            *recorder = Some(Arc::new(Recorder::new(dir.into())));
        }
        self
    }

    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }

    pub fn get(&self, url: &str) -> Request {
        Request::new(self.clone(), "GET", url)
    }

    pub fn post(&self, url: &str) -> Request {
        Request::new(self.clone(), "POST", url)
    }

    /// Every page from `first`, following `Link: rel="next"`, at most `max_pages`. A failing page is an error.
    pub async fn pages(&self, first: Request, max_pages: usize) -> Result<Vec<Response>, HttpError> {
        let mut pages = Vec::new();
        let mut request = first;
        while pages.len() < max_pages {
            let page = request.clone().send().await?.error_for_status()?;
            let next = page.next_link();
            pages.push(page);
            match next {
                Some(url) => request = request.at(url),
                None => break,
            }
        }
        Ok(pages)
    }

    fn check(&self, url: &Url) -> Result<(), HttpError> {
        let scheme_ok = url.scheme() == "https" || (self.allow_plain_http && url.scheme() == "http");
        if !scheme_ok {
            return Err(HttpError::NotHttps(url.to_string()));
        }
        if !host_allowed(&self.hosts, url) {
            let host = url.host_str().unwrap_or("").to_string();
            return Err(HttpError::HostNotAllowed { host, url: url.to_string() });
        }
        Ok(())
    }

    async fn once(&self, request: &Request, url: &Url) -> Result<Response, HttpError> {
        match &self.backend {
            Backend::Replay(replay) => replay.answer(request.method, url),
            Backend::Network { client, recorder } => {
                let method = reqwest::Method::from_bytes(request.method.as_bytes()).expect("GET and POST are methods");
                let mut call = client.request(method, url.clone());
                for (name, value) in &request.headers {
                    call = call.header(name, value);
                }
                if let Some(body) = &request.body {
                    call = call.header("content-type", "application/json").body(body.clone());
                }
                let transport = |e: reqwest::Error| HttpError::Transport {
                    method: request.method.to_string(),
                    url: url.to_string(),
                    message: e.without_url().to_string(),
                };
                let answer = call.send().await.map_err(transport)?;
                let status = answer.status().as_u16();
                let mut headers = BTreeMap::new();
                for (name, value) in answer.headers() {
                    if let Ok(v) = value.to_str() {
                        headers
                            .entry(name.as_str().to_string())
                            .and_modify(|old: &mut String| {
                                old.push_str(", ");
                                old.push_str(v);
                            })
                            .or_insert_with(|| v.to_string());
                    }
                }
                let body = answer.bytes().await.map_err(transport)?.to_vec();
                let response = Response::new(status, headers, body, url.to_string());
                if let Some(recorder) = recorder {
                    recorder.save(request.method, url, &response)?;
                }
                Ok(response)
            }
        }
    }
}

fn user_agent(manifest: &Manifest) -> String {
    format!("trailhead-connector/{} ({})", env!("CARGO_PKG_VERSION"), manifest.id)
}

#[derive(Clone)]
pub struct Request {
    http: Http,
    method: &'static str,
    url: String,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
}

impl Request {
    fn new(http: Http, method: &'static str, url: &str) -> Request {
        Request { http, method, url: url.to_string(), query: Vec::new(), headers: Vec::new(), body: None }
    }

    fn at(mut self, url: String) -> Request {
        self.url = url;
        self.query.clear();
        self.body = None;
        self
    }

    pub fn query(mut self, name: &str, value: impl ToString) -> Request {
        self.query.push((name.to_string(), value.to_string()));
        self
    }

    pub fn header(mut self, name: &str, value: &str) -> Request {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    pub fn bearer(self, token: &str) -> Request {
        self.header("authorization", &format!("Bearer {token}"))
    }

    pub fn basic(self, user: &str, password: &str) -> Request {
        let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
        self.header("authorization", &format!("Basic {encoded}"))
    }

    /// Sends `ETag` as `If-None-Match`; check [`Response::not_modified`] on the answer.
    pub fn if_none_match(self, etag: &str) -> Request {
        self.header("if-none-match", etag)
    }

    pub fn json<T: Serialize>(mut self, body: &T) -> Request {
        self.body = Some(serde_json::to_vec(body).expect("a body serializes to JSON"));
        self
    }

    /// Sends it, retrying a 429, a 5xx or a failed connection under the client's [`RetryPolicy`]. A status that
    /// still isn't a success after the last try comes back as a response; call [`Response::error_for_status`].
    pub async fn send(self) -> Result<Response, HttpError> {
        let mut url = Url::parse(&self.url).map_err(|_| HttpError::InvalidUrl(self.url.clone()))?;
        if !self.query.is_empty() {
            url.query_pairs_mut().extend_pairs(&self.query);
        }
        self.http.check(&url)?;
        let policy = self.http.retry;
        let mut tries = 0;
        loop {
            tries += 1;
            let result = self.http.once(&self, &url).await;
            let wait = match &result {
                Ok(r) if r.status == 429 || r.status >= 500 => Some(r.retry_after()),
                Err(HttpError::Transport { .. }) => Some(None),
                _ => None,
            };
            match wait {
                Some(retry_after) if tries < policy.max_tries => {
                    tokio::time::sleep(policy.wait(tries, retry_after)).await;
                }
                _ => {
                    return result.map(|mut r| {
                        r.url = url.to_string();
                        r
                    });
                }
            }
        }
    }
}
