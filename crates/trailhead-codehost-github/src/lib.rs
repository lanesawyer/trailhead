//! `CodeHost` for GitHub's REST API (`docs/factory/sandbox.md`). Commits are published with the Git Data API
//! (blobs, a tree on top of the parent's, a commit, then the ref), because the Sprites connector forwards REST
//! paths, not git's protocol. Inside a sandbox the base URL is the connector's gateway and no token is sent; the
//! gateway adds it. Elsewhere it's `https://api.github.com` with a token.

use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use reqwest::{Method, RequestBuilder, StatusCode};
use serde_json::{Value, json};
use trailhead_factory::{
    BranchUpdate, ChangedFile, Ci, CiState, CodeHost, Commit, Error, FileStatus, MergeOutcome, NewPr, PrRef, PrState,
    Repo, Result,
};

pub const API_URL: &str = "https://api.github.com";

/// GitHub pages check runs 100 at a time; a commit with more than this many is read as far as this.
const MAX_CHECK_PAGES: u32 = 10;

/// GitHub pages a PR's files 100 at a time, up to 3,000.
const MAX_FILE_PAGES: u32 = 30;

/// Who commits are attributed to. Without one, GitHub uses the credential's user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub email: String,
}

#[derive(Clone)]
pub struct GitHub {
    http: reqwest::Client,
    base: String,
    token: Option<String>,
    author: Option<Identity>,
}

impl GitHub {
    /// Through a Sprites connector: the gateway authenticates, so no token.
    pub fn gateway(base: impl Into<String>) -> GitHub {
        GitHub::new(base, None)
    }

    pub fn new(base: impl Into<String>, token: Option<String>) -> GitHub {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .user_agent("trailhead-factory")
            .build()
            .expect("http client");
        GitHub { http, base: base.into().trim_end_matches('/').to_string(), token, author: None }
    }

    pub fn with_author(mut self, author: Identity) -> GitHub {
        self.author = Some(author);
        self
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
                Ok(res) => return Self::read(res, call).await,
                Err(e) => causes(&e),
            };
            tracing::warn!(%call, error = %failure, tries, "github call failed; retrying");
            tokio::time::sleep(wait).await;
            wait *= 2;
        }
        let res = self.http.execute(req).await.map_err(|e| Error::Other(format!("github {call}: {}", causes(&e))))?;
        Self::read(res, call).await
    }

    async fn read(res: reqwest::Response, call: String) -> Result<(StatusCode, Value, String)> {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
        Ok((status, body, call))
    }

    /// Sends and requires success, mapping GitHub's errors onto the factory's.
    async fn call(&self, req: RequestBuilder) -> Result<Value> {
        let (status, body, call) = self.send(req).await?;
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
            _ => Error::Other(msg),
        })
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
    ChangedFile {
        path: f["filename"].as_str().unwrap_or_default().to_string(),
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

fn pr_ref(repo: &Repo, p: &Value) -> PrRef {
    PrRef {
        repo: repo.clone(),
        number: p["number"].as_u64().unwrap_or_default(),
        url: p["html_url"].as_str().unwrap_or_default().to_string(),
    }
}

#[async_trait]
impl CodeHost for GitHub {
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

    async fn comment(&self, pr: &PrRef, body: &str) -> Result<()> {
        let path = format!("{}/issues/{}/comments", Self::repo_path(&pr.repo), pr.number);
        self.call(self.request(Method::POST, &path).json(&json!({ "body": body }))).await.map(drop)
    }

    async fn merge(&self, pr: &PrRef) -> Result<MergeOutcome> {
        let path = format!("{}/pulls/{}/merge", Self::repo_path(&pr.repo), pr.number);
        let (status, body, _) =
            self.send(self.request(Method::PUT, &path).json(&json!({ "merge_method": "squash" }))).await?;
        match status {
            s if s.is_success() => Ok(MergeOutcome::Merged),
            // Not mergeable, or the head moved: the task goes back to running.
            StatusCode::METHOD_NOT_ALLOWED | StatusCode::CONFLICT => Ok(MergeOutcome::Conflict),
            s => Err(Error::Other(format!("github {s}: {}", body["message"].as_str().unwrap_or_default()))),
        }
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
            s => Err(Error::Other(format!("github {s}: {message}"))),
        }
    }

    async fn pr_state(&self, pr: &PrRef) -> Result<PrState> {
        let path = format!("{}/pulls/{}", Self::repo_path(&pr.repo), pr.number);
        let p = self.call(self.request(Method::GET, &path)).await?;
        Ok(if p["merged"].as_bool().unwrap_or(false) {
            PrState::Merged { commit: p["merge_commit_sha"].as_str().map(str::to_string) }
        } else if p["state"].as_str() == Some("closed") {
            PrState::Closed
        } else {
            // Null while GitHub is still working it out; asking again later gets the answer.
            PrState::Open { mergeable: p["mergeable"].as_bool() }
        })
    }

    async fn title(&self, pr: &PrRef) -> Result<String> {
        let path = format!("{}/pulls/{}", Self::repo_path(&pr.repo), pr.number);
        let p = self.call(self.request(Method::GET, &path)).await?;
        Ok(p["title"].as_str().unwrap_or_default().to_string())
    }

    async fn head(&self, pr: &PrRef) -> Result<String> {
        let path = format!("{}/pulls/{}", Self::repo_path(&pr.repo), pr.number);
        let p = self.call(self.request(Method::GET, &path)).await?;
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
            let found = self.call(req).await?;
            let found = found["check_runs"].as_array().map(Vec::as_slice).unwrap_or_default();
            runs.extend_from_slice(found);
            if found.len() < 100 {
                break;
            }
        }
        let combined = self.call(self.request(Method::GET, &format!("{r}/commits/{head}/status"))).await?;
        let statuses = combined["statuses"].as_array().map(Vec::as_slice).unwrap_or_default();
        Ok(Ci { state: summarize_ci(&runs, statuses), head })
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

#[cfg(test)]
mod tests {
    use super::*;

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
