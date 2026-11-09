//! `CodeHost` for GitHub's REST API (`docs/factory/sandbox.md`). Commits are published with the Git Data API
//! (blobs, a tree on top of the parent's, a commit, then the ref), because the Sprites connector forwards REST
//! paths, not git's protocol. Inside a sandbox the base URL is the connector's gateway and no token is sent; the
//! gateway adds it. Elsewhere it's `https://api.github.com` with a token.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use reqwest::{Method, RequestBuilder, StatusCode};
use serde_json::{Value, json};
use trailhead_factory::{
    BranchUpdate, ChangedFile, Ci, CiState, CodeHost, Commit, Error, FileStatus, MergeOutcome, NewPr, OpenPr, PrRef,
    PrState, RateLimit, Repo, Result,
};

pub const API_URL: &str = "https://api.github.com";

/// GitHub pages check runs 100 at a time; a commit with more than this many is read as far as this.
const MAX_CHECK_PAGES: u32 = 10;
/// Pages of open PRs listed, a hundred each.
const MAX_PR_PAGES: u32 = 5;

/// GitHub pages a PR's files 100 at a time, up to 3,000.
const MAX_FILE_PAGES: u32 = 30;

/// Who commits are attributed to. Without one, GitHub uses the credential's user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub email: String,
}

/// How many conditional-GET answers a `GitHub` keeps.
const ETAG_CACHE_CAP: usize = 256;

/// A small least-recently-used cache of conditional-GET answers, by URL: the ETag and the body.
struct EtagCache {
    cap: usize,
    clock: u64,
    entries: HashMap<String, (u64, String, Value)>,
}

impl Default for EtagCache {
    fn default() -> Self {
        EtagCache::new(ETAG_CACHE_CAP)
    }
}

impl EtagCache {
    fn new(cap: usize) -> EtagCache {
        EtagCache { cap, clock: 0, entries: HashMap::new() }
    }

    fn get(&mut self, key: &str) -> Option<(String, Value)> {
        self.clock += 1;
        let entry = self.entries.get_mut(key)?;
        entry.0 = self.clock;
        Some((entry.1.clone(), entry.2.clone()))
    }

    fn insert(&mut self, key: String, etag: String, body: Value) {
        self.clock += 1;
        self.entries.insert(key, (self.clock, etag, body));
        while self.entries.len() > self.cap {
            let oldest = self.entries.iter().min_by_key(|(_, v)| v.0).map(|(k, _)| k.clone());
            match oldest {
                Some(k) => self.entries.remove(&k),
                None => break,
            };
        }
    }

    fn remove(&mut self, key: &str) {
        self.entries.remove(key);
    }
}

#[derive(Clone)]
pub struct GitHub {
    http: reqwest::Client,
    base: String,
    token: Option<String>,
    author: Option<Identity>,
    /// The last answer to each conditional GET, by URL: its ETag and body. A `304 Not Modified` isn't counted
    /// against GitHub's rate limit, so asking again about an unchanged PR is almost free. Bounded: the least
    /// recently used answers are dropped, since the host keeps one `GitHub` for the life of the process.
    etags: Arc<Mutex<EtagCache>>,
    /// What the last response said of GitHub's rate limit (`x-ratelimit-*`), if it said.
    rate: Arc<Mutex<Option<RateLimit>>>,
}

impl GitHub {
    /// Through a Sprites connector: the gateway authenticates, so no token.
    pub fn gateway(base: impl Into<String>) -> GitHub {
        GitHub::new(base, None)
    }

    pub fn new(base: impl Into<String>, token: Option<String>) -> GitHub {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            // Drop idle pooled connections before GitHub closes them, or a reconcile pass's first call finds one
            // dead ("connection closed before message completed").
            .pool_idle_timeout(Duration::from_secs(30))
            .user_agent("trailhead-factory")
            .build()
            .expect("http client");
        GitHub {
            http,
            base: base.into().trim_end_matches('/').to_string(),
            token,
            author: None,
            etags: Arc::default(),
            rate: Arc::default(),
        }
    }

    pub fn with_author(mut self, author: Identity) -> GitHub {
        self.author = Some(author);
        self
    }

    /// Checks the token: it must authenticate (`GET /user`) and read the repo's pulls. Returns the user's login.
    pub async fn check_token(&self, repo: &Repo) -> Result<String> {
        let user = self.call(self.request(Method::GET, "/user")).await?;
        let pulls = format!("{}/pulls", Self::repo_path(repo));
        self.call(self.request(Method::GET, &pulls).query(&[("per_page", "1")])).await?;
        Ok(user["login"].as_str().unwrap_or("?").to_string())
    }

    /// Checks a token meant to write: it must authenticate and have push access to the repo (the `permissions`
    /// GitHub reports for the token on `GET /repos/o/r`), which holds for classic and fine-grained tokens alike.
    /// GitHub doesn't report pull request or issue write there, so those can't be checked without writing.
    /// Returns the user's login.
    pub async fn check_writer(&self, repo: &Repo) -> Result<String> {
        let user = self.call(self.request(Method::GET, "/user")).await?;
        let info = self.call(self.request(Method::GET, &Self::repo_path(repo))).await?;
        if info["permissions"]["push"].as_bool() != Some(true) {
            return Err(Error::Forbidden(format!(
                "the token can't push to {}/{} (needs contents, pull requests and issues write)",
                repo.owner, repo.name
            )));
        }
        Ok(user["login"].as_str().unwrap_or("?").to_string())
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let req = self
            .http
            .request(method, format!("{}{path}", self.base))
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28");
        match &self.token {
            Some(t) => req.bearer_auth(t),
            None => req,
        }
    }

    /// Sends, returning the status, the body, and which call this was (`POST /repos/o/r/git/refs`) for error messages.
    async fn send(&self, req: RequestBuilder) -> Result<(StatusCode, Value, String)> {
        let (status, body, call, _) = self.send_full(req).await?;
        Ok((status, body, call))
    }

    /// `send`, and the response's ETag.
    async fn send_full(&self, req: RequestBuilder) -> Result<(StatusCode, Value, String, Option<String>)> {
        let req = req.build().map_err(|e| Error::Other(format!("github: {e}")))?;
        let path = req.url().path();
        let path = path.find("/repos/").map_or(path, |i| &path[i..]);
        let call = format!("{} {path}", req.method());
        let mut wait = Duration::from_millis(500);
        for tries in 1..=RETRIES {
            if req.method() != Method::GET {
                break;
            }
            let Some(again) = req.try_clone() else { break };
            let failure = match self.http.execute(again).await {
                Ok(res) if transient(res.status()) => res.status().to_string(),
                Ok(res) => return self.read(res, call).await,
                Err(e) => causes(&e),
            };
            tracing::warn!(%call, error = %failure, tries, "github call failed; retrying");
            tokio::time::sleep(wait).await;
            wait *= 2;
        }
        let res = self.http.execute(req).await.map_err(|e| Error::Other(format!("github {call}: {}", causes(&e))))?;
        self.read(res, call).await
    }

    async fn read(&self, res: reqwest::Response, call: String) -> Result<(StatusCode, Value, String, Option<String>)> {
        if let Some(rate) = rate_limit(res.headers()).filter(|_| is_core(res.headers())) {
            *self.rate.lock().unwrap() = Some(rate);
        }
        let status = res.status();
        let etag = res.headers().get(reqwest::header::ETAG).and_then(|v| v.to_str().ok()).map(str::to_string);
        let text = res.text().await.unwrap_or_default();
        let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
        Ok((status, body, call, etag))
    }

    /// Sends and requires success, mapping GitHub's errors onto the factory's.
    async fn call(&self, req: RequestBuilder) -> Result<Value> {
        let (status, body, call) = self.send(req).await?;
        Self::checked(status, body, call)
    }

    fn checked(status: StatusCode, body: Value, call: String) -> Result<Value> {
        if status.is_success() {
            return Ok(body);
        }
        let mut msg = format!("github {status} on {call}: {}", body["message"].as_str().unwrap_or(&body.to_string()));
        // GitHub answers a token without the `workflow` scope with a bare 404 when a commit touches a workflow.
        if status == StatusCode::NOT_FOUND && call.contains("/git/") {
            msg.push_str(" (if the change touches .github/workflows, the credential needs the `workflow` scope)");
        }
        Err(match status {
            StatusCode::NOT_FOUND => Error::NotFound(msg),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Error::Forbidden(msg),
            StatusCode::CONFLICT | StatusCode::UNPROCESSABLE_ENTITY => Error::Conflict(msg),
            s => Error::from_status(s.as_u16(), msg),
        })
    }

    /// `call` for a GET, conditional on the ETag of the last answer to the same URL; a `304` returns that answer.
    async fn call_cached(&self, req: RequestBuilder) -> Result<Value> {
        let mut req = req.build().map_err(|e| Error::Other(format!("github: {e}")))?;
        let key = req.url().to_string();
        let known = self.etags.lock().unwrap().get(&key);
        if let Some((etag, _)) = &known
            && let Ok(value) = etag.parse()
        {
            req.headers_mut().insert(reqwest::header::IF_NONE_MATCH, value);
        }
        let (status, body, call, etag) = self.send_full(RequestBuilder::from_parts(self.http.clone(), req)).await?;
        if status == StatusCode::NOT_MODIFIED
            && let Some((_, cached)) = known
        {
            return Ok(cached);
        }
        let body = Self::checked(status, body, call)?;
        let mut etags = self.etags.lock().unwrap();
        match etag {
            Some(etag) => etags.insert(key, etag, body.clone()),
            None => etags.remove(&key),
        }
        Ok(body)
    }

    fn repo_path(repo: &Repo) -> String {
        format!("/repos/{}/{}", repo.owner, repo.name)
    }

    /// The commit a branch points at, or `None` if there's no such branch.
    async fn head(&self, repo: &Repo, branch: &str) -> Result<Option<String>> {
        let path = format!("{}/git/ref/heads/{branch}", Self::repo_path(repo));
        match self.call(self.request(Method::GET, &path)).await {
            Ok(r) => Ok(r["object"]["sha"].as_str().map(str::to_string)),
            Err(Error::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Writes one commit on top of `parent` and returns its sha.
    async fn write_commit(&self, repo: &Repo, parent: &str, commit: &Commit) -> Result<String> {
        let r = Self::repo_path(repo);
        let parent_commit = self.call(self.request(Method::GET, &format!("{r}/git/commits/{parent}"))).await?;
        let base_tree =
            parent_commit["tree"]["sha"].as_str().ok_or_else(|| Error::Other("parent without a tree".into()))?;
        let mut entries = Vec::with_capacity(commit.changes.len());
        for change in &commit.changes {
            let sha = match &change.contents {
                Some(bytes) => {
                    let content = base64::engine::general_purpose::STANDARD.encode(bytes);
                    let blob = json!({ "content": content, "encoding": "base64" });
                    let blob = self.call(self.request(Method::POST, &format!("{r}/git/blobs")).json(&blob)).await?;
                    Value::String(blob["sha"].as_str().unwrap_or_default().to_string())
                }
                // A null sha deletes the path from the base tree.
                None => Value::Null,
            };
            entries.push(json!({ "path": change.path, "mode": "100644", "type": "blob", "sha": sha }));
        }
        let tree = json!({ "base_tree": base_tree, "tree": entries });
        let tree = self.call(self.request(Method::POST, &format!("{r}/git/trees")).json(&tree)).await?;
        let parents: Vec<&str> = std::iter::once(parent).chain(commit.merges.iter().map(String::as_str)).collect();
        let mut body = json!({ "message": commit.message, "tree": tree["sha"], "parents": parents });
        if let Some(a) = &self.author {
            body["author"] = json!({ "name": a.name, "email": a.email });
            body["committer"] = body["author"].clone();
        }
        let created = self.call(self.request(Method::POST, &format!("{r}/git/commits")).json(&body)).await?;
        created["sha"].as_str().map(str::to_string).ok_or_else(|| Error::Other("commit without a sha".into()))
    }

    async fn find_pr(&self, repo: &Repo, branch: &str) -> Result<Option<PrRef>> {
        let path = format!("{}/pulls", Self::repo_path(repo));
        let head = format!("{}:{branch}", repo.owner);
        let prs =
            self.call(self.request(Method::GET, &path).query(&[("head", head.as_str()), ("state", "open")])).await?;
        Ok(prs.as_array().and_then(|a| a.first()).map(|p| pr_ref(repo, p)))
    }
}

/// Whether a response counts against the core rate limit, the one that gates this client. GitHub names the pool in
/// `x-ratelimit-resource`; a response that doesn't say is taken as core. Search (30 a minute) and graphql have
/// pools of their own, and their readings must not stand in for core's.
fn is_core(headers: &reqwest::header::HeaderMap) -> bool {
    headers.get("x-ratelimit-resource").and_then(|v| v.to_str().ok()).is_none_or(|r| r.trim() == "core")
}

/// The rate limit a response's headers report, if they all do.
fn rate_limit(headers: &reqwest::header::HeaderMap) -> Option<RateLimit> {
    let get = |name: &str| headers.get(name)?.to_str().ok()?.trim().parse::<i64>().ok();
    Some(RateLimit {
        limit: u64::try_from(get("x-ratelimit-limit")?).ok()?,
        remaining: u64::try_from(get("x-ratelimit-remaining")?).ok()?,
        resets_at: get("x-ratelimit-reset")?,
    })
}

/// Extra tries a GET makes after a transient failure.
const RETRIES: u32 = 3;

/// Whether GitHub's (or the gateway's) answer is worth asking again for: a server error, or told to slow down.
fn transient(status: StatusCode) -> bool {
    status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS
}

/// The error with its sources: reqwest's own message ("error sending request for url ...") leaves out why.
fn causes(e: &reqwest::Error) -> String {
    let mut out = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(cause) = source {
        out.push_str(&format!(": {cause}"));
        source = cause.source();
    }
    out
}

/// One file of a PR as a unified diff section. GitHub omits `patch` for binary and very large files.
fn file_diff(f: &Value) -> String {
    let name = f["filename"].as_str().unwrap_or_default();
    let old = f["previous_filename"].as_str().unwrap_or(name);
    let status = f["status"].as_str().unwrap_or_default();
    let from = if status == "added" { "/dev/null".to_string() } else { format!("a/{old}") };
    let to = if status == "removed" { "/dev/null".to_string() } else { format!("b/{name}") };
    let body = match f["patch"].as_str() {
        Some(patch) => format!("{patch}\n"),
        None if status == "removed" => "(deleted)\n".to_string(),
        None => "(binary or too large to show)\n".to_string(),
    };
    format!("diff --git a/{old} b/{name}\n--- {from}\n+++ {to}\n{body}")
}

fn changed_file(f: &Value) -> ChangedFile {
    let status = match f["status"].as_str().unwrap_or_default() {
        "added" => FileStatus::Added,
        "modified" => FileStatus::Modified,
        "removed" => FileStatus::Removed,
        "renamed" => FileStatus::Renamed,
        _ => FileStatus::Other,
    };
    let count = |k: &str| u32::try_from(f[k].as_u64().unwrap_or_default()).unwrap_or(u32::MAX);
    let path = f["filename"].as_str().unwrap_or_default().to_string();
    ChangedFile {
        // Only a lockfile's patch is kept: the grader reads it to tell a new package from an edited dependency list.
        patch: f["patch"].as_str().filter(|_| path.ends_with("Cargo.lock")).map(str::to_string),
        path,
        status,
        additions: count("additions"),
        deletions: count("deletions"),
        previous_path: f["previous_filename"].as_str().map(str::to_string),
    }
}

/// CI from a commit's check runs and its commit statuses (the combined status's list, since its own `state` reads
/// pending when there are none). Any failure fails it, naming what failed; otherwise it's pending until something has
/// reported and everything has finished.
fn summarize_ci(runs: &[Value], statuses: &[Value]) -> CiState {
    let mut failed = Vec::new();
    let mut pending = runs.is_empty() && statuses.is_empty();
    for run in runs {
        let name = run["name"].as_str().unwrap_or("check").to_string();
        if run["status"].as_str() != Some("completed") {
            pending = true;
            continue;
        }
        match run["conclusion"].as_str() {
            Some("success" | "neutral" | "skipped") => {}
            _ => failed.push(name),
        }
    }
    for status in statuses {
        let name = status["context"].as_str().unwrap_or("status").to_string();
        match status["state"].as_str() {
            Some("success") => {}
            Some("failure" | "error") => failed.push(name),
            _ => pending = true,
        }
    }
    if !failed.is_empty() {
        failed.dedup();
        CiState::Failed { checks: failed }
    } else if pending {
        CiState::Pending
    } else {
        CiState::Passed
    }
}

/// The end of a job log: its last lines, within a size that fits in a prompt.
fn log_tail(log: &str) -> String {
    const LINES: usize = 80;
    const BYTES: usize = 6000;
    let lines: Vec<&str> = log.lines().collect();
    let tail = lines[lines.len().saturating_sub(LINES)..].join("\n");
    let mut start = tail.len().saturating_sub(BYTES);
    while !tail.is_char_boundary(start) {
        start += 1;
    }
    tail[start..].to_string()
}

fn pr_ref(repo: &Repo, p: &Value) -> PrRef {
    PrRef {
        repo: repo.clone(),
        number: p["number"].as_u64().unwrap_or_default(),
        url: p["html_url"].as_str().unwrap_or_default().to_string(),
    }
}

#[async_trait]
impl CodeHost for GitHub {
    fn rate_limit(&self) -> Option<RateLimit> {
        *self.rate.lock().unwrap()
    }

    async fn publish(&self, repo: &Repo, start: &str, branch: &str, commits: &[Commit]) -> Result<()> {
        let existing = self.head(repo, branch).await?;
        if let Some(head) = existing.as_deref().filter(|head| *head != start) {
            return Err(Error::Conflict(format!(
                "branch {branch} is at {head}, not at {start} where this work started; publishing would overwrite it"
            )));
        }
        let mut sha = start.to_string();
        for commit in commits {
            sha = self.write_commit(repo, &sha, commit).await?;
        }
        let r = Self::repo_path(repo);
        match existing {
            // Not forced: if someone else moved the branch meanwhile, this fails instead of dropping their work.
            Some(_) => {
                let req = self.request(Method::PATCH, &format!("{r}/git/refs/heads/{branch}"));
                self.call(req.json(&json!({ "sha": sha, "force": false }))).await?;
            }
            None => {
                let body = json!({ "ref": format!("refs/heads/{branch}"), "sha": sha });
                self.call(self.request(Method::POST, &format!("{r}/git/refs")).json(&body)).await?;
            }
        }
        Ok(())
    }

    /// Opens the PR, or returns the open one for this branch, so a retry after a crash doesn't fail.
    async fn open_pr(&self, repo: &Repo, pr: NewPr) -> Result<PrRef> {
        let body = json!({ "title": pr.title, "head": pr.branch, "base": pr.base, "body": pr.body });
        let req = self.request(Method::POST, &format!("{}/pulls", Self::repo_path(repo))).json(&body);
        match self.call(req).await {
            Ok(created) => Ok(pr_ref(repo, &created)),
            Err(Error::Conflict(msg)) => match self.find_pr(repo, &pr.branch).await? {
                Some(existing) => Ok(existing),
                None => Err(Error::Conflict(msg)),
            },
            Err(e) => Err(e),
        }
    }

    /// A unified diff assembled from the PR's files. GitHub's diff media type would be simpler, but the Sprites
    /// gateway doesn't pass `Accept` through, so this sticks to JSON.
    async fn diff(&self, pr: &PrRef) -> Result<String> {
        let path = format!("{}/pulls/{}/files", Self::repo_path(&pr.repo), pr.number);
        let mut out = String::new();
        for page in 1..=MAX_FILE_PAGES {
            let req = self.request(Method::GET, &path).query(&[("per_page", "100"), ("page", &page.to_string())]);
            let files = self.call(req).await?;
            let files = files.as_array().cloned().unwrap_or_default();
            for f in &files {
                out.push_str(&file_diff(f));
            }
            if files.len() < 100 {
                return Ok(out);
            }
        }
        out.push_str("(diff truncated: GitHub lists at most 3,000 files)\n");
        Ok(out)
    }

    async fn comment(&self, pr: &PrRef, body: &str) -> Result<u64> {
        let path = format!("{}/issues/{}/comments", Self::repo_path(&pr.repo), pr.number);
        let made = self.call(self.request(Method::POST, &path).json(&json!({ "body": body }))).await?;
        made["id"].as_u64().ok_or_else(|| Error::Other("github answered a comment without an id".into()))
    }

    async fn edit_comment(&self, pr: &PrRef, id: u64, body: &str) -> Result<()> {
        let path = format!("{}/issues/comments/{id}", Self::repo_path(&pr.repo));
        self.call(self.request(Method::PATCH, &path).json(&json!({ "body": body }))).await.map(drop)
    }

    async fn merge(&self, pr: &PrRef, head: &str) -> Result<MergeOutcome> {
        let path = format!("{}/pulls/{}/merge", Self::repo_path(&pr.repo), pr.number);
        let req = self.request(Method::PUT, &path).json(&json!({ "merge_method": "squash", "sha": head }));
        let (status, body, _) = self.send(req).await?;
        let message = body["message"].as_str().unwrap_or_default().to_string();
        match status {
            s if s.is_success() => Ok(MergeOutcome::Merged),
            StatusCode::CONFLICT => Ok(MergeOutcome::HeadMoved),
            // GitHub says 405 for a conflict and for a merge its rules refuse (a required check, say) alike.
            StatusCode::METHOD_NOT_ALLOWED => match self.pr_state(pr).await? {
                PrState::Open { mergeable: Some(false), .. } => Ok(MergeOutcome::Conflict),
                _ => Ok(MergeOutcome::Refused(message)),
            },
            s => Err(Error::from_status(s.as_u16(), format!("github {s}: {message}"))),
        }
    }

    async fn open_prs(&self, repo: &Repo, base: &str) -> Result<Vec<OpenPr>> {
        let path = format!("{}/pulls", Self::repo_path(repo));
        let mut out = Vec::new();
        for page in 1..=MAX_PR_PAGES {
            let req = self.request(Method::GET, &path).query(&[
                ("state", "open"),
                ("base", base),
                ("sort", "created"),
                ("direction", "asc"),
                ("per_page", "100"),
                ("page", &page.to_string()),
            ]);
            let found = self.call_cached(req).await?;
            let found = found.as_array().map(Vec::as_slice).unwrap_or_default();
            out.extend(found.iter().map(|p| OpenPr { pr: pr_ref(repo, p), auto_merge: !p["auto_merge"].is_null() }));
            if found.len() < 100 {
                break;
            }
        }
        Ok(out)
    }

    async fn behind(&self, pr: &PrRef) -> Result<bool> {
        let r = Self::repo_path(&pr.repo);
        let p = self.call(self.request(Method::GET, &format!("{r}/pulls/{}", pr.number))).await?;
        let (Some(base), Some(head)) = (p["base"]["ref"].as_str(), p["head"]["sha"].as_str()) else {
            return Err(Error::Other(format!("github: PR {} has no base or head", pr.number)));
        };
        let cmp = self.call(self.request(Method::GET, &format!("{r}/compare/{base}...{head}"))).await?;
        // `behind` gates merging, so a response without the count is an error, not "up to date".
        let behind_by = cmp["behind_by"]
            .as_u64()
            .ok_or_else(|| Error::Other(format!("github: comparing PR {} with {base} has no behind_by", pr.number)))?;
        Ok(behind_by > 0)
    }

    async fn update_branch(&self, pr: &PrRef) -> Result<BranchUpdate> {
        let r = Self::repo_path(&pr.repo);
        let p = self.call(self.request(Method::GET, &format!("{r}/pulls/{}", pr.number))).await?;
        let Some(head) = p["head"]["sha"].as_str() else {
            return Err(Error::Other(format!("github: PR {} has no head", pr.number)));
        };
        // Naming the head we read makes a push that raced us fail instead of getting merged over.
        let request = self
            .request(Method::PUT, &format!("{r}/pulls/{}/update-branch", pr.number))
            .json(&json!({ "expected_head_sha": head }));
        let (status, body, _) = self.send(request).await?;
        let message = body["message"].as_str().unwrap_or_default();
        match status {
            // GitHub queues the merge and answers 202.
            s if s.is_success() => Ok(BranchUpdate::Updated),
            // 422 is also nothing to merge in (a queued update landed already) and a head that moved; only a real
            // merge conflict sends the task back to running.
            StatusCode::UNPROCESSABLE_ENTITY => {
                let lower = message.to_lowercase();
                if lower.contains("merge conflict") {
                    Ok(BranchUpdate::Conflict)
                } else if lower.contains("no new commits") {
                    Ok(BranchUpdate::UpToDate)
                } else {
                    Err(Error::Other(format!("github {status}: {message}")))
                }
            }
            s => Err(Error::from_status(s.as_u16(), format!("github {s}: {message}"))),
        }
    }

    async fn carries_verdict(&self, pr: &PrRef, reviewed: &str) -> Result<bool> {
        let r = Self::repo_path(&pr.repo);
        let p = self.call(self.request(Method::GET, &format!("{r}/pulls/{}", pr.number))).await?;
        let (Some(base), Some(head)) = (p["base"]["ref"].as_str(), p["head"]["sha"].as_str()) else {
            return Err(Error::Other(format!("github: PR {} has no base or head", pr.number)));
        };
        if head == reviewed {
            return Ok(true);
        }
        // What the PR has that its base lacks: its own commits. A merge of the base brings in base commits, which
        // aren't among them, so a commit since `reviewed` that is one of them must be a merge of the base itself.
        let own = self.call(self.request(Method::GET, &format!("{r}/compare/{base}...{head}"))).await?;
        // A reviewed sha the host no longer has (a force push, then GC) 404s: it can't be vouched for.
        let since = match self.call(self.request(Method::GET, &format!("{r}/compare/{reviewed}...{head}"))).await {
            Ok(since) => since,
            Err(Error::NotFound(_)) => return Ok(false),
            Err(e) => return Err(e),
        };
        // `reviewed` must lead to the head, and a listing cut short can't be vouched for.
        if since["status"].as_str() != Some("ahead") {
            return Ok(false);
        }
        let (Some(own), Some(since)) = (complete_commits(&own), complete_commits(&since)) else { return Ok(false) };
        let own_shas: Vec<&str> = own.iter().filter_map(|c| c["sha"].as_str()).collect();
        Ok(since.iter().filter(|c| c["sha"].as_str().is_some_and(|s| own_shas.contains(&s))).all(|c| {
            let parents = c["parents"].as_array().map(Vec::as_slice).unwrap_or_default();
            parents.len() == 2 && parents[1]["sha"].as_str().is_some_and(|second| !own_shas.contains(&second))
        }))
    }

    async fn pr_state(&self, pr: &PrRef) -> Result<PrState> {
        let path = format!("{}/pulls/{}", Self::repo_path(&pr.repo), pr.number);
        let p = self.call_cached(self.request(Method::GET, &path)).await?;
        Ok(if p["merged"].as_bool().unwrap_or(false) {
            PrState::Merged { commit: p["merge_commit_sha"].as_str().map(str::to_string) }
        } else if p["state"].as_str() == Some("closed") {
            PrState::Closed
        } else {
            // Null while GitHub is still working it out; asking again later gets the answer.
            PrState::Open { mergeable: p["mergeable"].as_bool(), head: p["head"]["sha"].as_str().map(str::to_string) }
        })
    }

    async fn title(&self, pr: &PrRef) -> Result<String> {
        let path = format!("{}/pulls/{}", Self::repo_path(&pr.repo), pr.number);
        let p = self.call_cached(self.request(Method::GET, &path)).await?;
        Ok(p["title"].as_str().unwrap_or_default().to_string())
    }

    async fn head(&self, pr: &PrRef) -> Result<String> {
        let path = format!("{}/pulls/{}", Self::repo_path(&pr.repo), pr.number);
        let p = self.call_cached(self.request(Method::GET, &path)).await?;
        p["head"]["sha"].as_str().map(str::to_string).ok_or_else(|| Error::Other("PR without a head sha".into()))
    }

    async fn ci(&self, pr: &PrRef) -> Result<Ci> {
        let head = CodeHost::head(self, pr).await?;
        let r = Self::repo_path(&pr.repo);
        let mut runs = Vec::new();
        for page in 1..=MAX_CHECK_PAGES {
            let req = self
                .request(Method::GET, &format!("{r}/commits/{head}/check-runs"))
                .query(&[("per_page", "100"), ("page", &page.to_string())]);
            let found = self.call_cached(req).await?;
            let found = found["check_runs"].as_array().map(Vec::as_slice).unwrap_or_default();
            runs.extend_from_slice(found);
            if found.len() < 100 {
                break;
            }
        }
        let combined = self.call_cached(self.request(Method::GET, &format!("{r}/commits/{head}/status"))).await?;
        let statuses = combined["statuses"].as_array().map(Vec::as_slice).unwrap_or_default();
        Ok(Ci { state: summarize_ci(&runs, statuses), head })
    }

    async fn ci_logs(&self, pr: &PrRef, checks: &[String]) -> Result<Vec<(String, String)>> {
        let r = Self::repo_path(&pr.repo);
        let head = CodeHost::head(self, pr).await?;
        let req = self.request(Method::GET, &format!("{r}/commits/{head}/check-runs")).query(&[("per_page", "100")]);
        let found = self.call(req).await?;
        let mut out = Vec::new();
        for run in found["check_runs"].as_array().map(Vec::as_slice).unwrap_or_default() {
            let name = run["name"].as_str().unwrap_or_default();
            let (Some(id), true) = (run["id"].as_u64(), checks.iter().any(|c| c == name)) else { continue };
            if out.iter().any(|(n, _): &(String, String)| n == name) {
                continue;
            }
            // An Actions job's id is its check run's; a check from elsewhere has no such log, which is left out.
            if let Ok(Value::String(log)) =
                self.call(self.request(Method::GET, &format!("{r}/actions/jobs/{id}/logs"))).await
            {
                out.push((name.to_string(), log_tail(&log)));
            }
        }
        Ok(out)
    }

    async fn files(&self, pr: &PrRef) -> Result<Vec<ChangedFile>> {
        let path = format!("{}/pulls/{}/files", Self::repo_path(&pr.repo), pr.number);
        let mut out = Vec::new();
        for page in 1..=MAX_FILE_PAGES {
            let req = self.request(Method::GET, &path).query(&[("per_page", "100"), ("page", &page.to_string())]);
            let files = self.call(req).await?;
            let files = files.as_array().map(Vec::as_slice).unwrap_or_default();
            out.extend(files.iter().map(changed_file));
            if files.len() < 100 {
                return Ok(out);
            }
        }
        Ok(out)
    }
}

/// The commits of a compare response, unless the host cut the list short (it lists at most 250).
fn complete_commits(compare: &Value) -> Option<&Vec<Value>> {
    let commits = compare["commits"].as_array()?;
    (compare["total_commits"].as_u64()? == commits.len() as u64).then_some(commits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etag_cache_evicts_the_least_recently_used() {
        let mut c = EtagCache::new(2);
        c.insert("a".into(), "1".into(), json!("a"));
        c.insert("b".into(), "2".into(), json!("b"));
        assert!(c.get("a").is_some());
        c.insert("c".into(), "3".into(), json!("c"));
        assert!(c.get("b").is_none());
        assert_eq!(c.get("a"), Some(("1".into(), json!("a"))));
        assert!(c.get("c").is_some());
        c.remove("c");
        assert!(c.get("c").is_none());
        assert_eq!(c.entries.len(), 1);
    }

    fn run(name: &str, status: &str, conclusion: Option<&str>) -> Value {
        json!({ "name": name, "status": status, "conclusion": conclusion })
    }

    fn status(context: &str, state: &str) -> Value {
        json!({ "context": context, "state": state })
    }

    #[test]
    fn ci_with_nothing_reported_is_pending() {
        assert_eq!(summarize_ci(&[], &[]), CiState::Pending);
    }

    #[test]
    fn ci_passes_when_every_run_and_status_succeeded_or_was_skipped() {
        let runs = [run("test", "completed", Some("success")), run("docs", "completed", Some("skipped"))];
        assert_eq!(summarize_ci(&runs, &[status("deploy", "success")]), CiState::Passed);
        assert_eq!(summarize_ci(&[], &[status("deploy", "success")]), CiState::Passed);
    }

    #[test]
    fn ci_is_pending_while_a_run_or_status_hasnt_finished() {
        let runs = [run("test", "completed", Some("success")), run("lint", "in_progress", None)];
        assert_eq!(summarize_ci(&runs, &[]), CiState::Pending);
        let done = [run("test", "completed", Some("success"))];
        assert_eq!(summarize_ci(&done, &[status("deploy", "pending")]), CiState::Pending);
    }

    #[test]
    fn ci_fails_naming_what_failed_even_while_other_checks_run() {
        let runs = [
            run("test", "completed", Some("failure")),
            run("lint", "in_progress", None),
            run("fmt", "completed", Some("timed_out")),
        ];
        assert_eq!(
            summarize_ci(&runs, &[status("deploy", "error")]),
            CiState::Failed { checks: vec!["test".into(), "fmt".into(), "deploy".into()] }
        );
    }
}
