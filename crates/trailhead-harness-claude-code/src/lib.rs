//! `Harness` for the Claude Code CLI (`docs/factory/agents.md`). Each run is one `claude --print` session in the
//! sandbox's checkout, streaming `stream-json` events. The final message is structured output against a schema, so
//! the factory acts on a typed outcome rather than parsing prose. Model calls go to the run's endpoint (the
//! connector gateway), which injects the real key; the CLI only sees a placeholder.
//!
//! A worker's changes are whatever differs in the checkout from where the run started, committed or not, collected
//! into one commit for the daemon to publish.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use trailhead_factory::{
    Commit, Error, EventSink, Feedback, FileChange, Harness, ModelId, Result, Role, RunContext, RunEvent, RunOutcome,
    Usage, VerdictKind,
};

/// What `ANTHROPIC_API_KEY` is set to. The gateway replaces credentials, so this only has to be non-empty.
pub const PLACEHOLDER_KEY: &str = "sprites-connector";

pub struct ClaudeCode {
    binary: PathBuf,
    oauth_token: Option<String>,
}

impl Default for ClaudeCode {
    fn default() -> ClaudeCode {
        ClaudeCode::new("claude")
    }
}

impl ClaudeCode {
    pub fn new(binary: impl Into<PathBuf>) -> ClaudeCode {
        ClaudeCode { binary: binary.into(), oauth_token: None }
    }

    /// Subscription mode: Claude Code authenticates with this token (`claude setup-token`) against Anthropic's API
    /// directly, and the run's endpoint is ignored.
    pub fn with_oauth_token(mut self, token: String) -> ClaudeCode {
        // Tokens never contain whitespace, but one copied from a wrapped terminal line can.
        self.oauth_token = Some(token.split_whitespace().collect());
        self
    }

    async fn session(&self, ctx: RunContext, prompt: String, events: EventSink) -> Result<RunOutcome> {
        let start = git_head(&ctx.workdir).await?;
        let mut stream = Stream::new(ctx.model.clone(), ctx.max_steps);
        let mut resume: Option<String> = None;
        let mut resumes = 0;
        loop {
            let prompt = match &resume {
                None => prompt.clone(),
                Some(_) => RESUME_PROMPT.to_string(),
            };
            match self.invoke(&ctx, &prompt, resume.as_deref(), &mut stream, &events).await? {
                Invocation::StepLimit => {
                    return Ok(RunOutcome::Failed { reason: format!("hit the step limit of {}", ctx.max_steps) });
                }
                Invocation::NoResult(why) => return Ok(RunOutcome::Failed { reason: why }),
                Invocation::Result(result) => {
                    // A dropped or failed API call ends Claude Code's session, but the session itself is saved, so the
                    // agent can pick up where it was instead of the whole attempt starting over.
                    if is_transient_api_error(&result)
                        && resumes < MAX_RESUMES
                        && let Some(id) = stream.session_id.clone()
                    {
                        resumes += 1;
                        tracing::warn!(error = %result["result"], resumes, "claude's API call failed; resuming the session");
                        tokio::time::sleep(Duration::from_secs(5 * resumes as u64)).await;
                        resume = Some(id);
                        stream.result = None;
                        continue;
                    }
                    return outcome(&ctx, &start, &result).await;
                }
            }
        }
    }

    /// One `claude --print` process, fresh or resuming a session, feeding `stream` until it exits.
    async fn invoke(
        &self,
        ctx: &RunContext,
        prompt: &str,
        resume: Option<&str>,
        stream: &mut Stream,
        events: &EventSink,
    ) -> Result<Invocation> {
        let mut cmd = Command::new(&self.binary);
        match &self.oauth_token {
            // An API key or base URL would take precedence over the token, so none may leak in from the environment.
            Some(token) => cmd
                .env("CLAUDE_CODE_OAUTH_TOKEN", token)
                .env_remove("ANTHROPIC_API_KEY")
                .env_remove("ANTHROPIC_AUTH_TOKEN")
                .env_remove("ANTHROPIC_BASE_URL"),
            None => cmd
                .env("ANTHROPIC_BASE_URL", &ctx.endpoint.base_url)
                .env("ANTHROPIC_API_KEY", PLACEHOLDER_KEY)
                .env_remove("CLAUDE_CODE_OAUTH_TOKEN"),
        };
        cmd.args(["--print", "--output-format", "stream-json", "--verbose", "--dangerously-skip-permissions"])
            .arg("--model")
            .arg(&ctx.model.0)
            .arg("--json-schema")
            .arg(schema(ctx.role).to_string());
        if let Some(id) = resume {
            cmd.arg("--resume").arg(id);
        }
        let mut child = cmd
            .arg(prompt)
            .current_dir(&ctx.workdir)
            .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
            .env("DISABLE_AUTOUPDATER", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Cancelling the run drops this future; the agent must not outlive it.
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| Error::Other(format!("can't start {}: {e}", self.binary.display())))?;
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        let stderr = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            let mut tail = Vec::new();
            while let Ok(Some(line)) = lines.next_line().await {
                tail.push(line);
                if tail.len() > 20 {
                    tail.remove(0);
                }
            }
            tail.join("\n")
        });

        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines.next_line().await.map_err(|e| Error::Other(e.to_string()))? {
            match serde_json::from_str::<Value>(&line) {
                Ok(msg) => {
                    for ev in stream.read(&msg) {
                        let _ = events.send(ev);
                    }
                }
                Err(_) => tracing::debug!(%line, "non-JSON line from claude"),
            }
            if stream.over_limit() {
                let _ = child.kill().await;
                return Ok(Invocation::StepLimit);
            }
        }
        let status = child.wait().await.map_err(|e| Error::Other(e.to_string()))?;
        let stderr = stderr.await.unwrap_or_default();
        Ok(match stream.result.clone() {
            Some(result) => Invocation::Result(result),
            None => Invocation::NoResult(format!("claude exited ({status}) without a result: {stderr}")),
        })
    }
}

enum Invocation {
    Result(Value),
    StepLimit,
    NoResult(String),
}

/// How many times one attempt resumes Claude Code after a failed API call before giving up.
pub const MAX_RESUMES: usize = 3;

const RESUME_PROMPT: &str = "Your previous request failed with a network or API error. Carry on with the task from \
where you left off, then finish with the structured answer as before.";

/// An error result from a failed or dropped API call, which resuming can get past: connection resets and timeouts,
/// overload and server errors. Not auth, billing or invalid-request errors.
fn is_transient_api_error(result: &Value) -> bool {
    if result["is_error"] != true {
        return false;
    }
    let text = result["result"].as_str().unwrap_or_default();
    text.starts_with("API Error")
        && [
            "ECONNRESET",
            "ETIMEDOUT",
            "EPIPE",
            "Connection",
            "connection",
            "socket",
            "timed out",
            "Overloaded",
            "overloaded",
            "529",
            "500",
            "502",
            "503",
            "504",
        ]
        .iter()
        .any(|m| text.contains(m))
}

#[async_trait]
impl Harness for ClaudeCode {
    async fn run(&self, ctx: RunContext, events: EventSink) -> Result<RunOutcome> {
        let prompt = prompt(&ctx, None);
        self.session(ctx, prompt, events).await
    }

    /// Requested changes start a fresh sandbox from the PR branch (`docs/factory/mvp.md`), so resuming is a new
    /// session with the reviewer's feedback in the prompt.
    async fn resume(&self, ctx: RunContext, feedback: Feedback, events: EventSink) -> Result<RunOutcome> {
        let prompt = prompt(&ctx, Some(&feedback));
        self.session(ctx, prompt, events).await
    }
}

/// The structured final answer Claude Code must give, per role.
pub fn schema(role: Role) -> Value {
    let outcomes = match role {
        Role::Reviewer => json!(["approve", "request_changes"]),
        _ => json!(["done", "needs_input", "failed"]),
    };
    json!({
        "type": "object",
        "properties": {
            "outcome": { "enum": outcomes },
            "summary": {
                "type": "string",
                "description": "For done: the PR description. For a review: the review comment. Otherwise: why.",
            },
            "question": { "type": "string", "description": "For needs_input: the one question for a human." },
        },
        "required": ["outcome", "summary"],
    })
}

pub fn prompt(ctx: &RunContext, feedback: Option<&Feedback>) -> String {
    let task = &ctx.task;
    let mut p = match ctx.role {
        Role::Reviewer => format!(
            "You are reviewing a pull request for task {key}: {title}.\n\n\
             Task:\n{desc}\n\n\
             The change under review:\n```diff\n{diff}\n```\n\n\
             Check it against the task and the repo's conventions (AGENTS.md). Read code in the checkout as needed, \
             but don't edit anything. Approve only if it does what the task asks, correctly and with tests where \
             they belong. Your summary is posted on the PR as the review.",
            key = task.key.0,
            title = task.title,
            desc = task.description,
            diff = ctx.diff.as_deref().unwrap_or("(no diff)"),
        ),
        _ => format!(
            "Implement task {key}: {title}.\n\n\
             Task:\n{desc}\n\n\
             Work in this checkout and follow the repo's conventions (AGENTS.md). Make the change, add or update \
             tests, and run the repo's formatter, linter and tests. You don't need to commit or push; everything \
             you change in the checkout is published as one commit and a PR. If only a human can answer something \
             you need, stop and give needs_input with the question. If the task can't be done, give failed and \
             say why.",
            key = task.key.0,
            title = task.title,
            desc = task.description,
        ),
    };
    if let Some(f) = feedback {
        p.push_str(&format!("\n\nA reviewer asked for changes:\n{}\n\nAddress them.", f.body));
    }
    p
}

/// Turns the `stream-json` messages into run events and keeps the final result.
struct Stream {
    model: ModelId,
    max_steps: u32,
    steps: u32,
    seen: HashSet<String>,
    result: Option<Value>,
    session_id: Option<String>,
}

impl Stream {
    fn new(model: ModelId, max_steps: u32) -> Stream {
        Stream { model, max_steps, steps: 0, seen: HashSet::new(), result: None, session_id: None }
    }

    fn over_limit(&self) -> bool {
        self.max_steps > 0 && self.steps > self.max_steps
    }

    fn read(&mut self, msg: &Value) -> Vec<RunEvent> {
        let mut out = Vec::new();
        if let Some(id) = msg["session_id"].as_str() {
            self.session_id = Some(id.to_string());
        }
        match msg["type"].as_str() {
            // Claude Code's own messages (errors, notices) are labelled `<synthetic>` and cost nothing.
            Some("assistant") if msg["message"]["model"] == "<synthetic>" => {}
            Some("assistant") => {
                let m = &msg["message"];
                // One model response can arrive as several messages, one per content block, each with its usage.
                let id = m["id"].as_str().unwrap_or_default().to_string();
                if self.seen.insert(id) {
                    self.steps += 1;
                    out.push(RunEvent::StepStarted { step: self.steps });
                    let model = m["model"].as_str().map(|s| ModelId(s.into())).unwrap_or_else(|| self.model.clone());
                    out.push(RunEvent::Usage { model, usage: usage(&m["usage"]) });
                }
                for block in m["content"].as_array().into_iter().flatten() {
                    if block["type"] == "tool_use" {
                        let name = block["name"].as_str().unwrap_or("unknown").to_string();
                        out.push(RunEvent::ToolCall { name, detail: detail(&block["input"]) });
                    }
                }
            }
            Some("result") => self.result = Some(msg.clone()),
            _ => {}
        }
        out
    }
}

/// What a tool call is about, in one short line: a Bash command, a file path or a search pattern.
fn detail(input: &Value) -> Option<String> {
    let raw =
        ["command", "file_path", "path", "pattern", "url", "description"].iter().find_map(|k| input[*k].as_str())?;
    let line: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(if line.chars().count() > 120 { format!("{}…", line.chars().take(120).collect::<String>()) } else { line })
}

fn usage(u: &Value) -> Usage {
    let n = |k: &str| u[k].as_u64().unwrap_or(0);
    Usage {
        input_tokens: n("input_tokens"),
        output_tokens: n("output_tokens"),
        cache_read_tokens: n("cache_read_input_tokens"),
        cache_write_tokens: n("cache_creation_input_tokens"),
    }
}

/// The schema answer: `structured_output` when the CLI provides it, else the result text parsed as JSON.
fn answer(result: &Value) -> Option<Value> {
    if result["structured_output"].is_object() {
        return Some(result["structured_output"].clone());
    }
    let text = result["result"].as_str()?;
    serde_json::from_str(text.trim().trim_start_matches("```json").trim_matches('`').trim()).ok()
}

async fn outcome(ctx: &RunContext, start: &str, result: &Value) -> Result<RunOutcome> {
    if result["is_error"] == true || result["subtype"].as_str().is_some_and(|s| s != "success") {
        let why = result["result"].as_str().or(result["subtype"].as_str()).unwrap_or("unknown error");
        return Ok(RunOutcome::Failed { reason: format!("claude: {why}") });
    }
    let Some(answer) = answer(result) else {
        return Ok(RunOutcome::Failed { reason: "claude finished without a structured answer".into() });
    };
    let summary = answer["summary"].as_str().unwrap_or_default().to_string();
    Ok(match answer["outcome"].as_str().unwrap_or_default() {
        "approve" => RunOutcome::Verdict { kind: VerdictKind::Approve, body: summary },
        "request_changes" => RunOutcome::Verdict { kind: VerdictKind::RequestChanges, body: summary },
        "needs_input" => {
            let question = answer["question"].as_str().filter(|q| !q.is_empty()).unwrap_or(&summary).to_string();
            RunOutcome::NeedsInput { question }
        }
        "done" => {
            let changes = changes_since(&ctx.workdir, start).await?;
            if changes.is_empty() {
                RunOutcome::Failed { reason: "claude reported done but changed nothing".into() }
            } else {
                let message = format!("{}: {}", ctx.task.key.0, ctx.task.title);
                RunOutcome::Changes { commits: vec![Commit { message, changes }], summary }
            }
        }
        other => RunOutcome::Failed { reason: format!("claude gave up ({other}): {summary}") },
    })
}

async fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git").args(args).current_dir(dir).stdin(Stdio::null()).output().await;
    let out = out.map_err(|e| Error::Other(format!("git {}: {e}", args.join(" "))))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(Error::Other(format!("git {} failed: {}", args.join(" "), stderr.trim())));
    }
    Ok(out.stdout)
}

async fn git_head(dir: &Path) -> Result<String> {
    Ok(String::from_utf8_lossy(&git(dir, &["rev-parse", "HEAD"]).await?).trim().to_string())
}

/// Every path that differs from `start`, committed or not, including new files, with its contents now.
async fn changes_since(dir: &Path, start: &str) -> Result<Vec<FileChange>> {
    git(dir, &["add", "--all"]).await?;
    let names = git(dir, &["diff", "--cached", "--name-only", "--no-renames", "-z", start]).await?;
    let mut changes = Vec::new();
    for path in names.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let path = String::from_utf8_lossy(path).into_owned();
        let contents = match tokio::fs::read(dir.join(&path)).await {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(Error::Other(format!("{path}: {e}"))),
        };
        changes.push(FileChange { path, contents });
    }
    Ok(changes)
}
