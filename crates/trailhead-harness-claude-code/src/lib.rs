//! `Harness` for the Claude Code CLI (`docs/factory/agents.md`). Each run is one `claude --print` session in the
//! sandbox's checkout, streaming `stream-json` events. The final message is structured output against a schema, so
//! the factory acts on a typed outcome rather than parsing prose. Model calls go to the run's endpoint (the
//! connector gateway), which injects the real key; the CLI only sees a placeholder.
//!
//! A worker's changes are whatever differs in the checkout from where the run started, committed or not, collected
//! into one commit for the daemon to publish.
//!
//! This file runs the CLI and turns its answer into an outcome. Beside it: `prompt` (what the agent is told and
//! the schema its answer fits), `stream` (its output read into the run's events) and `git` (what it changed).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use trailhead_factory::{
    Commit, Error, EventSink, Feedback, FollowUp, Harness, PlanTurn, Priority, RaiseGrade, Result, Role, RunContext,
    RunOutcome, VerdictKind, conventional_title,
};

mod git;
mod prompt;
mod stream;

pub use prompt::{DIFF_CAP, planner_prompt, prompt, schema};

use git::{changes_since, git_head};
use stream::Stream;

/// The `adapter` name profiles use for this harness.
pub const NAME: &str = "claude-code";

/// The `HarnessSecrets` key for subscription mode: a token from `claude setup-token`.
pub const OAUTH_TOKEN: &str = "oauth_token";

/// The host Claude Code reaches directly in subscription mode.
pub const API_HOST: &str = "api.anthropic.com";

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

/// How the factory's registry builds this harness and which egress it needs (`trailhead-plugins` lists it).
pub fn kind() -> trailhead_factory::HarnessKind {
    trailhead_factory::HarnessKind {
        build: std::sync::Arc::new(|setup| {
            let claude = ClaudeCode::new(setup.executable.clone());
            std::sync::Arc::new(match setup.secrets.get(OAUTH_TOKEN) {
                Some(token) => claude.with_oauth_token(token.expose().to_string()),
                None => claude,
            })
        }),
        egress: |secrets| if secrets.contains_key(OAUTH_TOKEN) { vec![API_HOST.into()] } else { Vec::new() },
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
        match self.converse(&ctx, prompt, None, &mut stream, &events).await? {
            Invocation::StepLimit => Ok(step_limit(&ctx)),
            Invocation::NoResult(why) => Ok(RunOutcome::Failed { reason: why }),
            Invocation::Result(result) => outcome(&ctx, &start, &result).await,
        }
    }

    /// Runs Claude Code on `prompt`, resuming the session `resume` when given, until it ends. A dropped or failed API
    /// call ends Claude Code's session, but the session itself is saved, so the agent picks up where it was instead of
    /// the whole attempt starting over.
    async fn converse(
        &self,
        ctx: &RunContext,
        prompt: String,
        mut resume: Option<String>,
        stream: &mut Stream,
        events: &EventSink,
    ) -> Result<Invocation> {
        let mut prompt = prompt;
        let mut resumes = 0;
        loop {
            let ended = self.invoke(ctx, &prompt, resume.as_deref(), stream, events).await?;
            if let Invocation::Result(result) = &ended
                && is_transient_api_error(result)
                && resumes < MAX_RESUMES
                && let Some(id) = stream.session_id.clone()
            {
                resumes += 1;
                tracing::warn!(error = %result["result"], resumes, "claude's API call failed; resuming the session");
                tokio::time::sleep(Duration::from_secs(5 * resumes as u64)).await;
                resume = Some(id);
                prompt = if ctx.plan.is_some() { PLAN_RESUME_PROMPT } else { RESUME_PROMPT }.to_string();
                stream.result = None;
                continue;
            }
            return Ok(ended);
        }
    }

    /// A planner's turn (`docs/app-platform.md`, "Planning an ask"). The session the last turn kept is resumed with
    /// only what is new; without one, or when it is gone (a fresh sandbox), a new session gets the whole prompt. The
    /// turn is the file the agent writes; a missing or unreadable one fails the run.
    async fn plan_turn(&self, ctx: RunContext, events: EventSink) -> Result<RunOutcome> {
        let Some(plan) = ctx.plan.clone() else {
            return Ok(RunOutcome::Failed { reason: "a planner run without its plan".into() });
        };
        let files = &plan.files;
        if let Some(dir) = files.turn.parent() {
            tokio::fs::create_dir_all(dir).await.map_err(|e| Error::Other(format!("{}: {e}", dir.display())))?;
        }
        match tokio::fs::remove_file(&files.turn).await {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(Error::Other(format!("{}: {e}", files.turn.display())));
            }
            _ => {}
        }
        let mut stream = Stream::new(ctx.model.clone(), ctx.max_steps);
        let mut ended = None;
        if let Some(id) = kept_session(&files.session).await {
            let next = planner_prompt(&ctx, &plan, true);
            match self.converse(&ctx, next, Some(id.clone()), &mut stream, &events).await? {
                Invocation::Result(result) if !session_gone(&result) => ended = Some(Invocation::Result(result)),
                Invocation::StepLimit => ended = Some(Invocation::StepLimit),
                gone => {
                    tracing::info!(session = %id, ended = ?gone, "the planner's session couldn't be resumed; starting a new one");
                    stream.result = None;
                    stream.session_id = None;
                }
            }
        }
        let ended = match ended {
            Some(ended) => ended,
            None => self.converse(&ctx, planner_prompt(&ctx, &plan, false), None, &mut stream, &events).await?,
        };
        if let Some(id) = &stream.session_id
            && let Err(e) = tokio::fs::write(&files.session, id).await
        {
            tracing::warn!(error = %e, "couldn't keep the planner's session; the next turn starts a new one");
        }
        match ended {
            Invocation::StepLimit => Ok(step_limit(&ctx)),
            Invocation::NoResult(why) => Ok(RunOutcome::Failed { reason: why }),
            Invocation::Result(result) if result["is_error"] == true => {
                let why = result["result"].as_str().unwrap_or("unknown error");
                Ok(RunOutcome::Failed { reason: format!("claude: {why}") })
            }
            Invocation::Result(_) => Ok(read_turn(&files.turn).await),
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
        cmd.envs(&ctx.env);
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
        // The repo's own MCP servers (.mcp.json) are for interactive sessions; the factory tracks the task itself.
        cmd.args(["--print", "--output-format", "stream-json", "--verbose", "--dangerously-skip-permissions"])
            .arg("--strict-mcp-config")
            .env("TRAILHEAD_FACTORY_TASK", &ctx.task.key.0)
            .arg("--model")
            .arg(&ctx.model.0);
        // A planner's answer is the turn file it writes, not a structured result.
        match &ctx.plan {
            Some(plan) => {
                cmd.env(PLAN_TURN_ENV, &plan.files.turn);
            }
            None => {
                cmd.arg("--json-schema").arg(schema(ctx.role).to_string());
            }
        }
        if let Some(id) = resume {
            cmd.arg("--resume").arg(id);
        }
        // The prompt goes on stdin: one argv argument is capped at 128 KiB on Linux, and a reviewer's diff can exceed it.
        let mut child = cmd
            .current_dir(&ctx.workdir)
            .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
            .env("DISABLE_AUTOUPDATER", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Cancelling the run drops this future; the agent must not outlive it.
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| Error::Other(format!("can't start {}: {e}", self.binary.display())))?;
        let mut stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        // Written from its own task so a prompt bigger than the pipe can't deadlock against output we haven't read.
        // A write error (the agent exited early) shows up as its missing result.
        let input = prompt.as_bytes().to_vec();
        tokio::spawn(async move {
            let _ = stdin.write_all(&input).await;
        });
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

#[derive(Debug)]
enum Invocation {
    Result(Value),
    StepLimit,
    NoResult(String),
}

/// The environment variable that names the file a planner writes its turn to.
pub const PLAN_TURN_ENV: &str = "TRAILHEAD_PLAN_TURN";

fn step_limit(ctx: &RunContext) -> RunOutcome {
    RunOutcome::Failed { reason: format!("hit the step limit of {}", ctx.max_steps) }
}

/// The session id the last planner turn kept, if it kept one.
async fn kept_session(path: &Path) -> Option<String> {
    let id = tokio::fs::read_to_string(path).await.ok()?;
    Some(id.trim().to_string()).filter(|id| !id.is_empty())
}

/// Whether Claude Code refused to resume because it no longer has the session.
fn session_gone(result: &Value) -> bool {
    result["is_error"] == true && result["result"].as_str().is_some_and(|r| r.contains("No conversation found"))
}

/// The turn a planner wrote, or its failure.
async fn read_turn(path: &Path) -> RunOutcome {
    let read = match tokio::fs::read_to_string(path).await {
        Ok(text) => PlanTurn::parse(&text),
        Err(e) => Err(format!("{}: {e}", path.display())),
    };
    match read {
        Ok(turn) => RunOutcome::PlanTurn(turn),
        Err(why) => RunOutcome::Failed { reason: trailhead_factory::messages::no_plan_turn(&why) },
    }
}

/// How many times one attempt resumes Claude Code after a failed API call before giving up.
pub const MAX_RESUMES: usize = 3;

const PLAN_RESUME_PROMPT: &str = "Your previous request failed with a network or API error. Carry on with the turn \
from where you left off, then end it by writing the turn file as before.";

const RESUME_PROMPT: &str = "Your previous request failed with a network or API error. Carry on with the task from \
where you left off, then finish with the structured answer as before.";

/// An error result from a failed or dropped API call, which resuming can get past: connection resets and timeouts,
/// overload and server errors. Not auth, billing or invalid-request errors.
fn is_transient_api_error(result: &Value) -> bool {
    result["is_error"] == true
        && trailhead_factory::harness::is_transient_api_error(result["result"].as_str().unwrap_or_default())
}

#[async_trait]
impl Harness for ClaudeCode {
    async fn run(&self, ctx: RunContext, events: EventSink) -> Result<RunOutcome> {
        if ctx.role == Role::Planner {
            return self.plan_turn(ctx, events).await;
        }
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

/// The schema answer: `structured_output` when the CLI provides it, else the result text parsed as JSON.
fn answer(result: &Value) -> Option<Value> {
    if result["structured_output"].is_object() {
        return Some(result["structured_output"].clone());
    }
    let text = result["result"].as_str()?;
    serde_json::from_str(text.trim().trim_start_matches("```json").trim_matches('`').trim()).ok()
}

/// The reviewer's call for a higher grade, if it made a well-formed one.
fn raise_grade(answer: &Value) -> Option<RaiseGrade> {
    let raise = serde_json::from_value::<RaiseGrade>(answer["raise_grade"].clone()).ok()?;
    (!raise.reason.trim().is_empty()).then_some(raise)
}

/// The reviewer's follow-ups, leaving out any without a title. A priority outside 1 to 4 is dropped, so the task is
/// filed at the project's default rather than rejected.
fn follow_ups(answer: &Value) -> Vec<FollowUp> {
    let items = answer["follow_ups"].as_array().into_iter().flatten();
    items
        .filter_map(|v| {
            let mut v = v.clone();
            if v["priority"].as_i64().and_then(|p| Priority::try_from(p).ok()).is_none()
                && let Some(fields) = v.as_object_mut()
            {
                fields.remove("priority");
            }
            serde_json::from_value::<FollowUp>(v).ok()
        })
        .filter(|f| !f.title.trim().is_empty())
        .collect()
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
        "approve" => RunOutcome::Verdict {
            kind: VerdictKind::Approve,
            body: summary,
            follow_ups: follow_ups(&answer),
            raise_grade: raise_grade(&answer),
        },
        "request_changes" => RunOutcome::Verdict {
            kind: VerdictKind::RequestChanges,
            body: summary,
            follow_ups: Vec::new(),
            raise_grade: raise_grade(&answer),
        },
        "needs_input" => {
            let question = answer["question"].as_str().filter(|q| !q.is_empty()).unwrap_or(&summary).to_string();
            RunOutcome::NeedsInput { question }
        }
        "already_done" => {
            RunOutcome::NeedsInput { question: trailhead_factory::messages::already_done_question(&summary) }
        }
        "done" => {
            let changes = changes_since(&ctx.workdir, start).await?;
            if changes.is_empty() {
                RunOutcome::Failed { reason: "claude reported done but changed nothing".into() }
            } else {
                let (message, valid) = conventional_title(answer["title"].as_str(), &ctx.task.key, &ctx.task.title);
                if !valid {
                    tracing::warn!(task = %ctx.task.key.0, title = ?answer["title"], "no conventional title from the agent; using chore");
                }
                RunOutcome::Changes { commits: vec![Commit { message, changes, merges: Vec::new() }], summary }
            }
        }
        other => RunOutcome::Failed { reason: format!("claude gave up ({other}): {summary}") },
    })
}

#[cfg(test)]
mod tests;
