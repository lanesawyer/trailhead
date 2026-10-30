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
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use trailhead_factory::{
    CheckResult, Commit, Error, EventSink, Feedback, FileChange, FollowUp, Harness, ModelId, RaiseGrade, Result, Role,
    RunContext, RunEvent, RunOutcome, Usage, VerdictKind, conventional_title,
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
        // The repo's own MCP servers (.mcp.json) are for interactive sessions; the factory tracks the task itself.
        cmd.args(["--print", "--output-format", "stream-json", "--verbose", "--dangerously-skip-permissions"])
            .arg("--strict-mcp-config")
            .env("TRAILHEAD_FACTORY_TASK", &ctx.task.key.0)
            .arg("--model")
            .arg(&ctx.model.0)
            .arg("--json-schema")
            .arg(schema(ctx.role).to_string());
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
    result["is_error"] == true
        && trailhead_factory::harness::is_transient_api_error(result["result"].as_str().unwrap_or_default())
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
            "title": {
                "type": "string",
                "description": "For done: a conventional commit title for the change, `type(scope): message`, with type one of feat, fix, perf, refactor, docs, test, chore, ci, build and scope the crate or area (factory, core, server, cli, daemon, docs). The task key is appended for you. E.g. `fix(factory): send the prompt on stdin so big PRs can be reviewed`.",
            },
            "question": { "type": "string", "description": "For needs_input: the one question for a human." },
            "follow_ups": {
                "type": "array",
                "description": "For an approving review: non-blocking suggestions, each filed as its own task. Leave out anything the change should fix first.",
                "items": {
                    "type": "object",
                    "properties": {
                        "title": { "type": "string" },
                        "description": { "type": "string" },
                        "priority": { "type": "integer", "minimum": 1, "maximum": 4, "description": "1 (most urgent) to 4." },
                    },
                    "required": ["title"],
                },
            },
            "raise_grade": {
                "type": "object",
                "description": "For a review: only if the change is riskier than its grade (1 walk, 2 ordinary code, 3 dependencies, schema or public API, 4 auth, secrets, egress, CI or the factory's policy), the grade it deserves and why. Never to lower one.",
                "properties": {
                    "grade": { "type": "integer", "minimum": 2, "maximum": 4 },
                    "reason": { "type": "string" },
                },
                "required": ["grade", "reason"],
            },
        },
        "required": ["outcome", "summary"],
    })
}

/// The most the human-context section of a prompt may take, in bytes.
const HUMAN_CONTEXT_CAP: usize = 6000;

/// The most the answers may take of the human-context section, so notes keep the rest.
const ANSWERS_CAP: usize = 4000;

/// The answers and the task's decisions and notes, for after the description. Empty when there are none. Past
/// the caps the oldest answers and notes are dropped; each entry is cut short if very long, and the newest answer
/// is always kept.
fn human_section(task: &trailhead_factory::TaskSpec) -> String {
    let ctx = &task.context;
    let mut budget = HUMAN_CONTEXT_CAP;
    let mut out = String::new();
    let mut answers = Vec::new();
    let mut answers_budget = ANSWERS_CAP;
    for a in ctx.answers.iter().rev() {
        let entry =
            format!("\n- Q: {}\n  A ({}, {}): {}", clip(&a.question, 1000), a.by, a.date, clip(&a.answer, 2000));
        if entry.len() > answers_budget && !answers.is_empty() {
            break;
        }
        answers_budget = answers_budget.saturating_sub(entry.len());
        budget = budget.saturating_sub(entry.len());
        answers.push(entry);
    }
    if !answers.is_empty() {
        out.push_str("\n\nQuestions you asked earlier and the answers you got:");
        answers.iter().rev().for_each(|e| out.push_str(e));
    }
    let mut kept = Vec::new();
    for n in ctx.notes.iter().rev() {
        let entry = format!("\n- {}, {}: {}", n.author, n.date, clip(&n.body, 2000));
        if entry.len() > budget {
            break;
        }
        budget -= entry.len();
        kept.push(entry);
    }
    if !kept.is_empty() {
        out.push_str("\n\nDecisions and notes on this task, oldest first:");
        kept.iter().rev().for_each(|e| out.push_str(e));
    }
    out
}

/// What a reviewer is told about building and the checks the worker already ran. The don't-build line goes in even
/// with no results, as when reconcile starts the review after a restart: this sandbox can't build either way.
fn checks_section(checks: &[CheckResult]) -> String {
    let mut out = String::from(
        "\n\nThis sandbox can't fetch dependencies, so don't build or run tests: review the diff and read code instead.",
    );
    if checks.is_empty() {
        return out;
    }
    out.push_str(" The worker already ran these checks in its own sandbox, where the dependencies are installed:\n");
    for c in checks {
        out.push_str(&format!("- {}: {}\n", c.name, if c.passed { "passed" } else { "FAILED" }));
        if !c.passed {
            out.push_str(&format!("  Output (tail):\n```\n{}\n```\n", clip_tail(&c.output, 2000)));
        }
    }
    out
}

/// The last `max` bytes of `s`, on a character boundary, marked when cut.
fn clip_tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    format!("(cut) …{}", &s[start..])
}

/// `s` cut to at most `max` bytes on a character boundary, marked when cut.
fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… (cut)", &s[..end])
}

/// The most of the diff a reviewer's prompt embeds, in bytes. The rest it reads from the checkout.
pub const DIFF_CAP: usize = 100 * 1024;

/// The diff cut to `cap` bytes at a file boundary (a `diff --git` line), with a note saying how to read the rest.
fn capped_diff(diff: &str, cap: usize, base: Option<&str>) -> String {
    if diff.len() <= cap {
        return diff.to_string();
    }
    let mut end = cap;
    while !diff.is_char_boundary(end) {
        end -= 1;
    }
    // Back up to the start of the file whose diff straddles the cap; one huge first file is cut mid-file.
    let at = diff[..end].rfind("\ndiff --git ").map(|i| i + 1).unwrap_or(end);
    let command = match base {
        Some(base) => format!("`git diff origin/{base}...HEAD` in the checkout"),
        None => "`git diff origin/<base>...HEAD` in the checkout, where <base> is the PR's base branch".into(),
    };
    format!(
        "{}\n[The diff is {} bytes and was cut here at {} bytes. Read the rest with {command}.]",
        diff[..at].trim_end(),
        diff.len(),
        at
    )
}

/// What a reviewer is told about the PR's title, which a squash merge makes the commit subject on main.
fn pr_title_section(title: Option<&str>, key: &str) -> String {
    let shown = match title {
        Some(t) => format!("The PR's title is `{t}`."),
        None => "The PR's title couldn't be read, so don't judge it.".into(),
    };
    format!(
        "\n\n{shown} It must be a conventional commit (`type(scope): message`, which the repo's changelog \
         requires) that fits the change: a fix titled feat, or a wrong scope, is a reason to request changes. The \
         task key comes last in square brackets (`... [{key}]`), not parentheses."
    )
}

pub fn prompt(ctx: &RunContext, feedback: Option<&Feedback>) -> String {
    let task = &ctx.task;
    let mut p = match ctx.role {
        Role::Reviewer => format!(
            "You are reviewing a pull request for task {key}: {title}.\n\n\
             Task:\n{desc}{human}\n\n\
             The change under review:\n```diff\n{diff}\n```{checks}\n\n\
             Check it against the task and the repo's conventions (AGENTS.md). Read code in the checkout as needed, \
             but don't edit anything. Approve only if it does what the task asks, correctly and with tests where \
             they belong. Your summary is posted on the PR as the review. When you approve, put anything worth doing \
             later that shouldn't hold this change up in `follow_ups`, each with a title and description: the factory \
             files them as tasks, so don't repeat them in the summary. If the change is riskier than its grade \
             says (a dependency, schema or public API change is grade 3; auth, secrets, sandbox egress, CI or the \
             factory's own policy is grade 4), say so in `raise_grade` with the reason; never to lower it.{pr_title}",
            key = task.key.0,
            title = task.title,
            desc = task.description,
            human = human_section(task),
            diff = ctx.diff.as_deref().map_or("(no diff)".into(), |d| capped_diff(
                d,
                DIFF_CAP,
                ctx.base_branch.as_deref()
            )),
            checks = checks_section(&ctx.checks),
            pr_title = pr_title_section(ctx.title.as_deref(), &task.key.0),
        ),
        _ => format!(
            "Implement task {key}: {title}.\n\n\
             Task:\n{desc}{human}\n\n\
             Work in this checkout and follow the repo's conventions (AGENTS.md). Make the change, add or update \
             tests, and run the repo's formatter, linter and tests. You don't need to commit or push; everything \
             you change in the checkout is published as one commit and a PR. If only a human can answer something \
             you need, stop and give needs_input with the question. If the task can't be done, give failed and \
             say why. When you finish, give `title`: a conventional commit title for the change, like \
             `fix(factory): send the prompt on stdin so big PRs can be reviewed`, with the type (feat, fix, \
             perf, refactor, docs, test, chore, ci or build) that fits what you did and the crate or area as \
             scope; the factory appends the task key. The factory tracks this task in Trailhead for you, so skip \
             AGENTS.md's \"Tracking work\" steps: don't claim, comment on or close tasks. Put follow-ups in your summary.",
            key = task.key.0,
            title = task.title,
            desc = task.description,
            human = human_section(task),
        ),
    };
    if let Some(failure) = &task.previous_failure {
        p.push_str(&format!("\n\nA previous attempt at this task failed:\n{failure}\n\nFix what broke."));
    }
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
            // A tool's result going back to the model: the call is over.
            Some("user") => {
                for block in msg["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"] == "tool_result" {
                        out.push(RunEvent::ToolResult);
                    }
                }
            }
            Some("result") => self.result = Some(msg.clone()),
            Some("rate_limit_event") => out.extend(quota(&msg["rate_limit_info"])),
            _ => {}
        }
        out
    }
}

/// A subscription's usage windows from Claude Code's `rate_limit_event`. Observed, not documented: anything
/// missing or renamed yields no reading, and the factory carries on as if it had none.
fn quota(info: &Value) -> Vec<RunEvent> {
    let Some(windows) = info["unifiedWindows"].as_object() else { return Vec::new() };
    windows
        .iter()
        .filter_map(|(window, w)| {
            let used = w["utilization"].as_f64()?;
            Some(RunEvent::Quota {
                window: window.clone(),
                used_percent: (used * 100.0).round().clamp(0.0, 1000.0) as u32,
                resets_at: w["resetsAt"].as_i64()?,
            })
        })
        .collect()
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

/// The reviewer's call for a higher grade, if it made a well-formed one.
fn raise_grade(answer: &Value) -> Option<RaiseGrade> {
    let raise = serde_json::from_value::<RaiseGrade>(answer["raise_grade"].clone()).ok()?;
    ((1..=4).contains(&raise.grade) && !raise.reason.trim().is_empty()).then_some(raise)
}

/// The reviewer's follow-ups, leaving out any without a title. A priority outside 1 to 4 is dropped, so the task is
/// filed at the project's default rather than rejected.
fn follow_ups(answer: &Value) -> Vec<FollowUp> {
    let items = answer["follow_ups"].as_array().into_iter().flatten();
    items
        .filter_map(|v| serde_json::from_value::<FollowUp>(v.clone()).ok())
        .filter(|f| !f.title.trim().is_empty())
        .map(|mut f| {
            f.priority = f.priority.filter(|p| (1..=4).contains(p));
            f
        })
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
