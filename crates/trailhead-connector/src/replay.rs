//! Recorded responses: the format, loading a directory of them, and writing one as it passes.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::http::{HttpError, Response};

/// One recorded answer. In a file it is a JSON object (or an array of them); `method` defaults to GET, `status` to
/// 200, and the body is `body` as text or `json` as a value, not both:
///
/// ```json
/// { "url": "https://api.github.com/user", "status": 200, "headers": { "etag": "\"v1\"" }, "json": { "login": "lane" } }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Recorded {
    #[serde(default = "get")]
    pub method: String,
    pub url: String,
    #[serde(default = "ok")]
    pub status: u16,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json: Option<Value>,
}

fn get() -> String {
    "GET".into()
}

fn ok() -> u16 {
    200
}

impl Recorded {
    pub fn new(method: &str, url: &str, status: u16, body: &str) -> Recorded {
        Recorded {
            method: method.to_string(),
            url: url.to_string(),
            status,
            headers: BTreeMap::new(),
            body: Some(body.to_string()),
            json: None,
        }
    }

    pub fn with_header(mut self, name: &str, value: &str) -> Recorded {
        self.headers.insert(name.to_ascii_lowercase(), value.to_string());
        self
    }

    pub(crate) fn key(&self) -> Result<(String, String), HttpError> {
        let url = Url::parse(&self.url)
            .map_err(|e| HttpError::Replay(format!("recorded url `{}` is not a URL: {e}", self.url)))?;
        Ok((self.method.to_ascii_uppercase(), url.to_string()))
    }

    pub(crate) fn into_response(self) -> Result<Response, HttpError> {
        let body = match (self.body, self.json) {
            (Some(_), Some(_)) => {
                return Err(HttpError::Replay(format!("recorded {} sets both `body` and `json`", self.url)));
            }
            (Some(text), None) => text.into_bytes(),
            (None, Some(value)) => value.to_string().into_bytes(),
            (None, None) => Vec::new(),
        };
        let headers = self.headers.into_iter().map(|(k, v)| (k.to_ascii_lowercase(), v)).collect();
        Ok(Response::new(self.status, headers, body, self.url))
    }
}

struct Slot {
    recorded: Recorded,
    key: (String, String),
    used: bool,
}

pub(crate) struct Replay {
    slots: Mutex<Vec<Slot>>,
}

impl Replay {
    pub(crate) fn new(entries: Vec<Recorded>) -> Result<Replay, HttpError> {
        let slots = entries
            .into_iter()
            .map(|recorded| recorded.key().map(|key| Slot { recorded, key, used: false }))
            .collect::<Result<_, _>>()?;
        Ok(Replay { slots: Mutex::new(slots) })
    }

    pub(crate) fn from_dir(dir: &Path) -> Result<Replay, HttpError> {
        let bad = |p: &Path, e: &dyn std::fmt::Display| HttpError::Replay(format!("{}: {e}", p.display()));
        let mut files: Vec<PathBuf> = fs::read_dir(dir)
            .map_err(|e| bad(dir, &e))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        files.sort();
        let mut entries = Vec::new();
        for file in files {
            let text = fs::read_to_string(&file).map_err(|e| bad(&file, &e))?;
            let value: Value = serde_json::from_str(&text).map_err(|e| bad(&file, &e))?;
            let items = match value {
                Value::Array(items) => items,
                one => vec![one],
            };
            for item in items {
                entries.push(serde_json::from_value::<Recorded>(item).map_err(|e| bad(&file, &e))?);
            }
        }
        Replay::new(entries)
    }

    /// The first matching answer not yet given; once all have been, the last one again.
    pub(crate) fn answer(&self, method: &str, url: &Url) -> Result<Response, HttpError> {
        let want = (method.to_string(), url.to_string());
        let mut slots = self.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let fresh = slots.iter().position(|s| s.key == want && !s.used);
        let any = slots.iter().rposition(|s| s.key == want);
        let Some(i) = fresh.or(any) else {
            let known: Vec<String> = slots.iter().map(|s| format!("{} {}", s.key.0, s.key.1)).collect();
            return Err(HttpError::NotRecorded { method: want.0, url: want.1, known });
        };
        slots[i].used = true;
        slots[i].recorded.clone().into_response()
    }
}

pub(crate) struct Recorder {
    dir: PathBuf,
    next: AtomicUsize,
}

impl Recorder {
    pub(crate) fn new(dir: PathBuf) -> Recorder {
        Recorder { dir, next: AtomicUsize::new(1) }
    }

    /// Keeps only the headers that shape a connector's reading of a response, so no cookie or token is saved.
    pub(crate) fn save(&self, method: &str, url: &Url, response: &Response) -> Result<(), HttpError> {
        let headers = ["content-type", "link", "etag", "retry-after"]
            .iter()
            .filter_map(|h| response.header(h).map(|v| ((*h).to_string(), v.to_string())))
            .collect();
        let text = String::from_utf8_lossy(response.bytes()).into_owned();
        let json = serde_json::from_str::<Value>(&text).ok();
        let record = Recorded {
            method: method.to_string(),
            url: url.to_string(),
            status: response.status,
            headers,
            body: if json.is_some() { None } else { Some(text) },
            json,
        };
        let fail = |e: &dyn std::fmt::Display| HttpError::Record(format!("{}: {e}", self.dir.display()));
        fs::create_dir_all(&self.dir).map_err(|e| fail(&e))?;
        let n = self.next.fetch_add(1, Ordering::SeqCst);
        let name = format!("{n:04}-{}-{}.json", method.to_ascii_lowercase(), url.host_str().unwrap_or("host"));
        let body = serde_json::to_string_pretty(&record).map_err(|e| fail(&e))?;
        fs::write(self.dir.join(name), body).map_err(|e| fail(&e))
    }
}
