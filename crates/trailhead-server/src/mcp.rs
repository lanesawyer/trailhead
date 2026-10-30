//! MCP at `/mcp` for agents without a shell. Tools mirror the CLI and answer in
//! the same terse one-line-per-task text, because agents pay for every token.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http::request::Parts;
use jiff::Timestamp;
use jiff::civil::Date;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ListResourcesResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource, ResourceContents,
    ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};
use trailhead_core::{
    Claim, Comment, CommentKind, Ctx, Error, EventFilter, GradeFilter, NewComment, NewProject, NewTask, Project,
    ProjectPatch, ProjectStatus, Status, StoredResponse, Task, TaskDetail, TaskFilter, TaskPatch, TaskSort, Verdict,
    idempotency_fingerprint, parse_grade,
};
use uuid::Uuid;

use crate::AppState;
use crate::error::ApiError;
use crate::params::{parse_date, parse_list, parse_opt_at, parse_opt_date};

const BRIEF_PREFIX: &str = "trailhead://projects/";

const INSTRUCTIONS: &str = "Trailhead is a shared task engine. Workflow: `ready` to see what's next, \
read the project's brief (`show_project`), `claim` a task before working on it (the last handoff comes back \
with the claim), `renew` on long work, `comment` with kind `decision` for choices and `question` for \
anything only a human can answer, and finish with `update_task` status `done` and a `handoff`. File follow-ups with `create_task` instead of widening scope. When you `create_project`, give it a brief. If several of your agents share this connection, give each its own identity: `register_agent` once per agent, then pass `as_agent` on every call.";

pub fn service(state: Arc<AppState>) -> StreamableHttpService<TrailheadMcp, LocalSessionManager> {
    let config = StreamableHttpServerConfig::default().with_legacy_session_mode(false).with_json_response(true);
    StreamableHttpService::new(move || Ok(TrailheadMcp::new(state.clone())), Default::default(), config)
}

/// Authenticates `/mcp` requests with the same bearer tokens as REST. The context
/// rides along in the request extensions, which rmcp hands to each tool.
pub async fn auth(State(state): State<Arc<AppState>>, mut req: Request, next: Next) -> Response {
    match crate::auth::authenticate(&state, req.headers()).await {
        Ok((ctx, token)) => {
            req.extensions_mut().insert(ctx);
            req.extensions_mut().insert(TokenId(token.id));
            next.run(req).await
        }
        Err(e) => ApiError(e).into_response(),
    }
}

/// The authenticating token, so idempotency keys belong to it.
#[derive(Clone, Copy)]
struct TokenId(Uuid);

#[derive(Clone)]
pub struct TrailheadMcp {
    state: Arc<AppState>,
    tool_router: ToolRouter<Self>,
}

/// Each tool call is its own batch, so it can be undone on its own.
fn ctx_of(parts: &Parts) -> Result<Ctx, ErrorData> {
    parts
        .extensions
        .get::<Ctx>()
        .map(Ctx::next_batch)
        .ok_or_else(|| ErrorData::invalid_request("not authenticated", None))
}

fn text(s: impl Into<String>) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::success(vec![ContentBlock::text(s)]))
}

/// Domain errors go back as tool errors the agent can read and act on, not as
/// protocol failures.
fn fail(e: Error) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::error(vec![ContentBlock::text(format!("error[{}]: {e}", e.code()))]))
}

macro_rules! attempt {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => return fail(e),
        }
    };
}

/// `2026-10-17 13:00 PDT` in the zone it was planned in, else at UTC.
fn local_time(t: Timestamp, tz: Option<&str>) -> String {
    let zone = tz.and_then(|z| jiff::tz::TimeZone::get(z).ok()).unwrap_or(jiff::tz::TimeZone::UTC);
    t.to_zoned(zone).strftime("%Y-%m-%d %H:%M %Z").to_string()
}

/// A day, or a day and time: `2026-10-17` or `2026-10-17 13:00 PDT`.
fn when(on: Option<Date>, at: Option<Timestamp>, tz: Option<&str>) -> Option<String> {
    match (at, on) {
        (Some(at), _) => Some(local_time(at, tz)),
        (None, Some(day)) => Some(day.to_string()),
        (None, None) => None,
    }
}

/// An update argument where an empty string clears the field.
fn clearable<T>(
    s: Option<&str>,
    parse: impl Fn(&str) -> trailhead_core::Result<T>,
) -> trailhead_core::Result<Option<Option<T>>> {
    s.map(str::trim).map(|s| (!s.is_empty()).then(|| parse(s)).transpose()).transpose()
}

fn parse_grade_filter(s: Option<&str>) -> trailhead_core::Result<GradeFilter> {
    Ok(s.map(GradeFilter::parse).transpose()?.unwrap_or_default())
}

fn task_line(t: &Task) -> String {
    let mut line = format!("{} {} p{} v{} {}", t.key, t.status, t.priority, t.version, t.title);
    if let Some(g) = t.grade {
        line.push_str(&format!(" (grade {g})"));
    }
    if let Some(w) = when(t.scheduled_on, t.scheduled_at, t.scheduled_tz.as_deref()) {
        line.push_str(&format!(" (scheduled {w})"));
    }
    if let Some(w) = when(t.due_on, t.due_at, t.due_tz.as_deref()) {
        line.push_str(&format!(" (due {w})"));
    }
    line
}

fn task_list(tasks: &[Task], empty: &str) -> String {
    if tasks.is_empty() { empty.to_string() } else { tasks.iter().map(task_line).collect::<Vec<_>>().join("\n") }
}

fn project_line(p: &Project) -> String {
    let mut line = format!("{} v{} {}", p.key, p.version, p.name);
    if p.status == ProjectStatus::Archived {
        line.push_str(" (archived)");
    }
    if p.require_review {
        line.push_str(" [review]");
    }
    line
}

fn project_text(p: &Project) -> String {
    let brief = if p.brief.is_empty() { "(no brief yet)" } else { &p.brief };
    format!("{}\n\n{brief}", project_line(p))
}

/// `2026-10-29 14:03 UTC`, the server having no local zone.
fn short_time(ts: Timestamp) -> String {
    ts.strftime("%Y-%m-%d %H:%M UTC").to_string()
}

/// Same shape as the CLI's comment line, with the time in UTC.
fn comment_line(c: &Comment, names: &dyn Fn(Uuid) -> String) -> String {
    let mut tags = vec![c.kind.to_string()];
    if let Some(to) = c.addressed_to {
        tags.push(format!("to {}", names(to)));
    }
    if c.kind == CommentKind::Question {
        tags.push(if c.resolved_at.is_some() { "resolved".into() } else { "open".into() });
    }
    if let Some(v) = c.verdict {
        tags.push(v.to_string().replace('_', " "));
    }
    if let Some(m) = &c.model {
        tags.push(m.clone());
    }
    format!(
        "[{}] {} {}: {}\n    id {}",
        tags.join(", "),
        short_time(c.created_at),
        names(c.author_id),
        c.body.replace('\n', "\n    "),
        c.id
    )
}

/// "until <time>", or "since <time>, no expiry" for a claim that never lapses.
fn lease(c: &Claim) -> String {
    match c.expires_at {
        Some(e) => format!("until {e}"),
        None => format!("since {}, no expiry", c.claimed_at),
    }
}

fn detail_text(d: &TaskDetail, names: &dyn Fn(Uuid) -> String) -> String {
    let t = &d.task;
    let mut out = vec![task_line(t)];
    if let Some(c) = &d.claim {
        out.push(format!("claimed by {} {}", names(c.holder_id), lease(c)));
    }
    if !d.blocked_by.is_empty() {
        out.push(format!("blocked by {}", d.blocked_by.join(", ")));
    }
    if !d.blocks.is_empty() {
        out.push(format!("blocks {}", d.blocks.join(", ")));
    }
    if t.metadata.as_object().is_some_and(|m| !m.is_empty()) {
        out.push(format!("metadata {}", t.metadata));
    }
    if !t.description.is_empty() {
        out.push(String::new());
        out.push(t.description.clone());
    }
    if !d.comments.is_empty() {
        out.push(String::new());
        out.extend(d.comments.iter().map(|c| comment_line(c, names)));
    }
    out.join("\n")
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct AsArgs {
    /// Act as one of your registered sub-agents (e.g. `researcher` for `<you>/researcher`).
    as_agent: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct RegisterArgs {
    /// The sub-agent's own name: letters, digits, '-' or '_'. It becomes `<you>/<name>`.
    name: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ReadyArgs {
    /// Project key, e.g. AUTH. Omit for every project.
    project: Option<String>,
    /// Defaults to 20.
    limit: Option<i64>,
    /// Only tasks with this grade: 1 to 4, `graded` or `ungraded`.
    grade: Option<String>,
    /// Act as one of your registered sub-agents (e.g. `researcher` for `<you>/researcher`).
    as_agent: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ListArgs {
    project: Option<String>,
    /// Comma-separated statuses: open, in_progress, needs_input, in_review, approved, done, cancelled.
    /// Defaults to everything not done or cancelled.
    status: Option<String>,
    /// Parent task key, to list subtasks.
    parent: Option<String>,
    /// YYYY-MM-DD. Only tasks scheduled on or before this day; pass today's date for today's plan.
    scheduled_through: Option<String>,
    /// Only tasks with this grade: 1 to 4, `graded` or `ungraded`.
    grade: Option<String>,
    limit: Option<i64>,
    /// Act as one of your registered sub-agents (e.g. `researcher` for `<you>/researcher`).
    as_agent: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct TaskArg {
    /// Task key like AUTH-42, or its id.
    task: String,
    /// Act as one of your registered sub-agents (e.g. `researcher` for `<you>/researcher`).
    as_agent: Option<String>,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
pub struct CreateArgs {
    /// Project key.
    project: String,
    title: String,
    description: Option<String>,
    /// 1 (urgent) to 4 (low). Defaults to 3.
    priority: Option<i64>,
    /// Risk grade, 1 to 4. Only a human can create a task with a grade.
    grade: Option<i64>,
    /// Parent task key, for subtasks.
    parent: Option<String>,
    /// Task keys that must finish before this one.
    #[serde(default)]
    blocked_by: Vec<String>,
    /// Task keys this one blocks. To get help on your own task, create a subtask that blocks it.
    #[serde(default)]
    blocks: Vec<String>,
    /// YYYY-MM-DD: the day it's due, by the end of that day.
    due_on: Option<String>,
    /// RFC 3339 timestamp with the local offset, e.g. 2026-10-24T17:00:00-07:00: a time it's
    /// due by. Sets the day too; due_on can be left out.
    due_at: Option<String>,
    /// IANA zone for the deadline, e.g. America/Los_Angeles. Decides when a due day ends.
    due_tz: Option<String>,
    /// YYYY-MM-DD: the day to work on it, separate from the deadline.
    scheduled_on: Option<String>,
    /// RFC 3339 timestamp with the local offset, e.g. 2026-10-17T13:00:00-07:00: a time
    /// to work on it. Sets the day too; scheduled_on can be left out.
    scheduled_at: Option<String>,
    /// IANA zone for scheduled_at, e.g. America/Los_Angeles.
    scheduled_tz: Option<String>,
    /// Optional. Retrying with the same key and arguments returns the first call's
    /// result instead of doing it twice. Keys last 24 hours.
    #[serde(default, skip_serializing)]
    idempotency_key: Option<String>,
    /// Act as one of your registered sub-agents (e.g. `researcher` for `<you>/researcher`).
    #[serde(default)]
    as_agent: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct UpdateArgs {
    task: String,
    title: Option<String>,
    description: Option<String>,
    /// open, in_progress, needs_input, in_review, approved, done or cancelled.
    status: Option<String>,
    priority: Option<i64>,
    /// Risk grade: 1 to 4, or `none` to clear it. Agents can only raise an existing grade.
    grade: Option<String>,
    /// YYYY-MM-DD: the day it's due, without a time. An empty string removes the deadline.
    due_on: Option<String>,
    /// RFC 3339 timestamp with the local offset: a time it's due by, moving the day to
    /// match. An empty string drops the time and keeps the day.
    due_at: Option<String>,
    /// IANA zone for the deadline, e.g. America/Los_Angeles.
    due_tz: Option<String>,
    /// YYYY-MM-DD: the day to work on it, without a time. An empty string unschedules it.
    scheduled_on: Option<String>,
    /// RFC 3339 timestamp with the local offset: a time to work on it, moving the day to
    /// match. An empty string drops the time and keeps the day.
    scheduled_at: Option<String>,
    /// IANA zone for scheduled_at, e.g. America/Los_Angeles.
    scheduled_tz: Option<String>,
    /// Parent task key: moves the task under it as a subtask. An empty string makes it top-level.
    parent: Option<String>,
    /// A JSON merge patch (RFC 7386) for the task's metadata: keys you give are set, a
    /// `null` value removes a key, keys you leave out stay.
    metadata: Option<serde_json::Map<String, serde_json::Value>>,
    /// The version you last saw; the update fails if the task changed since.
    version: Option<i64>,
    /// Posted as a `handoff` comment once the update succeeds: what changed, what's
    /// left, gotchas. Use it when finishing or stepping away.
    handoff: Option<String>,
    /// Act as one of your registered sub-agents (e.g. `researcher` for `<you>/researcher`).
    as_agent: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ClaimArgs {
    task: String,
    /// Lease length in seconds. Defaults to the project's or workspace's TTL (30 minutes).
    ttl: Option<i64>,
    /// Act as one of your registered sub-agents (e.g. `researcher` for `<you>/researcher`).
    as_agent: Option<String>,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
pub struct CommentArgs {
    task: String,
    body: String,
    /// handoff, progress, question, decision, review or note (default). A question parks the task until answered.
    kind: Option<String>,
    /// For a review, required: approve or request_changes.
    verdict: Option<String>,
    /// For a review, optional: the model that reviewed.
    model: Option<String>,
    /// Who the comment is for: a user or agent name as `show` prints it (`lane`, `claude/openapi`), or an id.
    addressed_to: Option<String>,
    /// Optional. Retrying with the same key and arguments returns the first call's
    /// result instead of doing it twice. Keys last 24 hours.
    #[serde(default, skip_serializing)]
    idempotency_key: Option<String>,
    /// Act as one of your registered sub-agents (e.g. `researcher` for `<you>/researcher`).
    #[serde(default)]
    as_agent: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ResolveArgs {
    #[schemars(with = "String")]
    comment_id: Uuid,
    /// Posted as a note addressed to whoever asked.
    answer: Option<String>,
    /// Act as one of your registered sub-agents (e.g. `researcher` for `<you>/researcher`).
    as_agent: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ListProjectsArgs {
    /// Include archived projects.
    #[serde(default)]
    archived: bool,
    /// Act as one of your registered sub-agents (e.g. `researcher` for `<you>/researcher`).
    as_agent: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ProjectArg {
    /// Project key, e.g. AUTH.
    project: String,
    /// Act as one of your registered sub-agents (e.g. `researcher` for `<you>/researcher`).
    as_agent: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct CreateProjectArgs {
    /// 2-10 uppercase letters or digits, starting with a letter. Task keys become KEY-1, KEY-2, ...
    key: String,
    name: String,
    /// Goals, conventions and definition of done, for agents to read before claiming work.
    brief: Option<String>,
    /// Agents must send finished work to `in_review` for a human to approve.
    #[serde(default)]
    require_review: bool,
    /// Act as one of your registered sub-agents (e.g. `researcher` for `<you>/researcher`).
    as_agent: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct UpdateProjectArgs {
    /// Project key, e.g. AUTH.
    project: String,
    name: Option<String>,
    /// Replaces the whole brief.
    brief: Option<String>,
    /// The version you last saw; the update fails if the project changed since.
    version: Option<i64>,
    /// Act as one of your registered sub-agents (e.g. `researcher` for `<you>/researcher`).
    as_agent: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct SearchArgs {
    query: String,
    limit: Option<i64>,
    /// Act as one of your registered sub-agents (e.g. `researcher` for `<you>/researcher`).
    as_agent: Option<String>,
}

#[tool_router]
impl TrailheadMcp {
    fn new(state: Arc<AppState>) -> Self {
        TrailheadMcp { state, tool_router: Self::tool_router() }
    }

    /// The caller's context, or one of its registered sub-agents' when `as_agent` is set.
    async fn ctx_for(&self, parts: &Parts, as_agent: Option<&str>) -> trailhead_core::Result<Ctx> {
        let ctx = ctx_of(parts).map_err(|_| Error::Unauthorized)?;
        match as_agent.map(str::trim).filter(|n| !n.is_empty()) {
            None => Ok(ctx),
            Some(name) => self.state.store.sub_agent_ctx(&ctx, name).await,
        }
    }

    /// The names `show` can print: the acting user and the workspace's agents. Only cosmetic, so a lookup that fails
    /// leaves its names out rather than failing the call.
    async fn known_names(&self, ctx: &Ctx) -> HashMap<Uuid, String> {
        let mut known = HashMap::new();
        match self.state.store.get_user(ctx, ctx.user_id).await {
            Ok(u) => {
                known.insert(u.id, u.name);
            }
            Err(e) => tracing::warn!(error = %e, "couldn't look up the user's name"),
        }
        match self.state.store.list_agents(ctx).await {
            Ok(agents) => known.extend(agents.into_iter().map(|a| (a.id, a.name))),
            Err(e) => tracing::warn!(error = %e, "couldn't look up agent names"),
        }
        known
    }

    /// An id, or the user or agent with a name `show` prints.
    async fn resolve_name(&self, ctx: &Ctx, name: &str) -> trailhead_core::Result<Uuid> {
        if let Ok(id) = name.parse::<Uuid>() {
            return Ok(id);
        }
        let user = self.state.store.get_user(ctx, ctx.user_id).await?;
        if user.name == name {
            return Ok(user.id);
        }
        let agents = self.state.store.list_agents(ctx).await?;
        agents
            .into_iter()
            .find(|a| a.name == name)
            .map(|a| a.id)
            .ok_or_else(|| Error::NotFound(format!("no user or agent {name}")))
    }

    async fn do_create_task(&self, ctx: &Ctx, a: CreateArgs) -> Result<CallToolResult, ErrorData> {
        let due_at = attempt!(parse_opt_at(a.due_at.as_deref()));
        let scheduled_on = attempt!(parse_opt_date(a.scheduled_on.as_deref()));
        let scheduled_at = attempt!(parse_opt_at(a.scheduled_at.as_deref()));
        let new = NewTask {
            project: a.project,
            title: a.title,
            description: a.description.unwrap_or_default(),
            priority: a.priority,
            grade: a.grade,
            due_on: attempt!(parse_opt_date(a.due_on.as_deref())),
            due_at,
            due_tz: a.due_tz,
            scheduled_on,
            scheduled_at,
            scheduled_tz: a.scheduled_tz,
            parent: a.parent,
            ..Default::default()
        };
        let store = &self.state.store;
        let task = attempt!(store.create_task(ctx, new).await);
        for blocker in &a.blocked_by {
            attempt!(store.add_dependency(ctx, blocker, &task.key).await);
        }
        for blocked in &a.blocks {
            attempt!(store.add_dependency(ctx, &task.key, blocked).await);
        }
        text(task_line(&task))
    }

    async fn do_comment(&self, ctx: &Ctx, a: CommentArgs) -> Result<CallToolResult, ErrorData> {
        let kind = attempt!(a.kind.as_deref().map(str::parse::<CommentKind>).transpose()).unwrap_or(CommentKind::Note);
        let verdict = attempt!(a.verdict.as_deref().map(str::parse::<Verdict>).transpose());
        let addressed_to = match a.addressed_to.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            None => None,
            Some(to) => Some(attempt!(self.resolve_name(ctx, to).await)),
        };
        let new = NewComment { kind, body: a.body, addressed_to, verdict, model: a.model };
        let c = attempt!(self.state.store.add_comment(ctx, &a.task, new).await);
        text(format!("{} {}", c.kind, c.id))
    }

    /// Runs `run` at most once per idempotency key: a retry with the same key and
    /// arguments gets the stored result back. Keys share REST's table, namespaced
    /// by token and by `mcp/<tool>`.
    async fn idempotent(
        &self,
        parts: &Parts,
        ctx: &Ctx,
        key: Option<String>,
        tool: &str,
        args: &[u8],
        run: impl Future<Output = Result<CallToolResult, ErrorData>>,
    ) -> Result<CallToolResult, ErrorData> {
        let Some(key) = key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty()) else { return run.await };
        if key.len() > 255 {
            return fail(Error::Invalid("idempotency_key must be 1-255 characters".into()));
        }
        let token = parts.extensions.get::<TokenId>().map_or_else(String::new, |t| t.0.to_string());
        let key = format!("{token}:mcp/{tool}:{key}");
        let fingerprint = idempotency_fingerprint("MCP", tool, args);
        let store = &self.state.store;
        if let Some(stored) = attempt!(store.idempotency_begin(ctx, &key, &fingerprint).await) {
            return text(String::from_utf8_lossy(&stored.body).into_owned());
        }
        let result = run.await;
        let stored = match &result {
            Ok(r) if r.is_error != Some(true) => Some(
                r.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect::<Vec<_>>().join("\n"),
            ),
            _ => None,
        };
        let saved = match stored {
            Some(out) => {
                let res = StoredResponse { status: 200, headers: Vec::new(), body: out.into_bytes() };
                store.idempotency_complete(ctx, &key, &res).await
            }
            None => store.idempotency_abandon(ctx, &key).await,
        };
        if let Err(e) = saved {
            tracing::warn!(error = %e, "saving MCP idempotency result failed");
        }
        result
    }

    #[tool(
        description = "Open, unclaimed, unblocked tasks in priority order. One line per task: KEY status priority version title.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn ready(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<ReadyArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = attempt!(self.ctx_for(&parts, a.as_agent.as_deref()).await);
        let tasks = attempt!(
            self.state
                .store
                .ready(&ctx, a.project.as_deref(), a.limit, attempt!(parse_grade_filter(a.grade.as_deref())))
                .await
        );
        text(task_list(&tasks, "nothing ready"))
    }

    #[tool(
        description = "List tasks with filters. Defaults to everything not done or cancelled.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_tasks(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<ListArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = attempt!(self.ctx_for(&parts, a.as_agent.as_deref()).await);
        let statuses = attempt!(parse_list(a.status.as_deref()));
        let scheduled_through = attempt!(parse_opt_date(a.scheduled_through.as_deref()));
        let filter = TaskFilter {
            project: a.project,
            statuses,
            parent: a.parent,
            sort: if scheduled_through.is_some() { TaskSort::Scheduled } else { TaskSort::Priority },
            scheduled_through,
            grade: attempt!(parse_grade_filter(a.grade.as_deref())),
            limit: a.limit,
            ..Default::default()
        };
        let tasks = attempt!(self.state.store.list_tasks(&ctx, filter).await);
        text(task_list(&tasks, "no tasks"))
    }

    #[tool(
        description = "Register a sub-agent named `<you>/<name>`, so work done as it (pass `as_agent` on any tool) is attributed to it. Each of your agents should have its own; names must be registered before use.",
        annotations(read_only_hint = false, destructive_hint = false, open_world_hint = false)
    )]
    async fn register_agent(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<RegisterArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = ctx_of(&parts)?;
        let agent = attempt!(self.state.store.create_agent(&ctx, &a.name).await);
        text(format!("registered {}; pass as_agent: {:?} to act as it", agent.name, a.name.trim()))
    }

    #[tool(
        description = "A task with its claim, dependencies, description and comments.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn show(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<TaskArg>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = attempt!(self.ctx_for(&parts, a.as_agent.as_deref()).await);
        let detail = attempt!(self.state.store.task_detail(&ctx, &a.task).await);
        let known = self.known_names(&ctx).await;
        let names = |id: Uuid| known.get(&id).cloned().unwrap_or_else(|| id.to_string().chars().take(8).collect());
        text(detail_text(&detail, &names))
    }

    #[tool(
        description = "Create a task. Use blocks to make a subtask that blocks your own task.",
        annotations(read_only_hint = false, destructive_hint = false, open_world_hint = false)
    )]
    async fn create_task(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<CreateArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = attempt!(self.ctx_for(&parts, a.as_agent.as_deref()).await);
        let key = a.idempotency_key.clone();
        let args = serde_json::to_vec(&a).unwrap_or_default();
        self.idempotent(&parts, &ctx, key, "create_task", &args, self.do_create_task(&ctx, a)).await
    }

    #[tool(
        description = "Change a task's fields or status. Setting status done, approved or in_review releases your claim. To finish, set status done with a handoff in one call.",
        annotations(read_only_hint = false, open_world_hint = false)
    )]
    async fn update_task(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<UpdateArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = attempt!(self.ctx_for(&parts, a.as_agent.as_deref()).await);
        let status = attempt!(a.status.as_deref().map(str::parse::<Status>).transpose());
        let patch = TaskPatch {
            title: a.title,
            description: a.description,
            status,
            priority: a.priority,
            grade: attempt!(a.grade.as_deref().map(parse_grade).transpose()),
            due_on: attempt!(clearable(a.due_on.as_deref(), parse_date)),
            due_at: attempt!(clearable(a.due_at.as_deref(), trailhead_core::parse_at)),
            due_tz: a.due_tz.map(Some),
            scheduled_on: attempt!(clearable(a.scheduled_on.as_deref(), parse_date)),
            scheduled_at: attempt!(clearable(a.scheduled_at.as_deref(), trailhead_core::parse_at)),
            scheduled_tz: a.scheduled_tz.map(Some),
            parent: attempt!(clearable(a.parent.as_deref(), |p| Ok(p.to_owned()))),
            metadata: a.metadata.map(serde_json::Value::Object),
        };
        let task = attempt!(self.state.store.update_task(&ctx, &a.task, patch, a.version).await);
        if let Some(body) = a.handoff.filter(|b| !b.trim().is_empty()) {
            let new = NewComment::new(CommentKind::Handoff, body);
            attempt!(self.state.store.add_comment(&ctx, &task.key, new).await);
        }
        text(task_line(&task))
    }

    #[tool(
        description = "Take a lease on a task and start it. Returns the last handoff left on it.",
        annotations(read_only_hint = false, destructive_hint = false, open_world_hint = false)
    )]
    async fn claim(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<ClaimArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = attempt!(self.ctx_for(&parts, a.as_agent.as_deref()).await);
        let r = attempt!(self.state.store.claim(&ctx, &a.task, a.ttl).await);
        let mut out = format!("{}\nclaimed {}", task_line(&r.task), lease(&r.claim));
        if let Some(h) = r.handoff {
            out.push_str(&format!("\n\nlast handoff:\n{}", h.body));
        }
        text(out)
    }

    #[tool(
        description = "Claims you currently hold and when they expire. Check this after a pause: a lapsed claim means someone else may own the task now.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn my_claims(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<AsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = attempt!(self.ctx_for(&parts, a.as_agent.as_deref()).await);
        let held = attempt!(self.state.store.my_claims(&ctx).await);
        if held.is_empty() {
            return text("you hold no claims");
        }
        text(
            held.iter()
                .map(|h| format!("{} {} {} {}", h.key, h.status, lease(&h.claim), h.title))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    #[tool(
        description = "Extend your lease. Renew about every third of the TTL.",
        annotations(read_only_hint = false, destructive_hint = false, open_world_hint = false)
    )]
    async fn renew(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<ClaimArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = attempt!(self.ctx_for(&parts, a.as_agent.as_deref()).await);
        let c = attempt!(self.state.store.renew(&ctx, &a.task, a.ttl).await);
        text(format!("claimed {}", lease(&c)))
    }

    #[tool(
        description = "Give a task back without finishing it. Leave a handoff comment first.",
        annotations(read_only_hint = false, destructive_hint = false, open_world_hint = false)
    )]
    async fn release(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<TaskArg>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = attempt!(self.ctx_for(&parts, a.as_agent.as_deref()).await);
        text(task_line(&attempt!(self.state.store.release(&ctx, &a.task).await)))
    }

    #[tool(
        description = "Comment on a task. Kinds: handoff, progress, question, decision, note.",
        annotations(read_only_hint = false, destructive_hint = false, open_world_hint = false)
    )]
    async fn comment(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<CommentArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = attempt!(self.ctx_for(&parts, a.as_agent.as_deref()).await);
        let key = a.idempotency_key.clone();
        let args = serde_json::to_vec(&a).unwrap_or_default();
        self.idempotent(&parts, &ctx, key, "comment", &args, self.do_comment(&ctx, a)).await
    }

    #[tool(
        description = "Resolve a question comment, optionally answering it.",
        annotations(read_only_hint = false, destructive_hint = false, open_world_hint = false)
    )]
    async fn resolve_question(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<ResolveArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = attempt!(self.ctx_for(&parts, a.as_agent.as_deref()).await);
        attempt!(self.state.store.resolve_comment(&ctx, a.comment_id, a.answer).await);
        text("resolved")
    }

    #[tool(
        description = "Undo your own most recent change (everything from your last tool call that wrote something). Fails if someone changed the same thing since.",
        annotations(read_only_hint = false, open_world_hint = false)
    )]
    async fn undo_last(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<AsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = attempt!(self.ctx_for(&parts, a.as_agent.as_deref()).await);
        let store = &self.state.store;
        let filter = EventFilter {
            actor_id: Some(ctx.actor_id()),
            newest_first: true,
            changes_only: true,
            limit: Some(1),
            ..Default::default()
        };
        let Some(last) = attempt!(store.list_events(&ctx, filter).await).pop() else {
            return text("nothing to undo");
        };
        let report = attempt!(store.undo_batch(&ctx, last.batch_id).await);
        text(format!("reverted {} change(s)", report.reverted.len()))
    }

    #[tool(
        description = "Full-text search over task titles, descriptions and comments.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn search(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<SearchArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = attempt!(self.ctx_for(&parts, a.as_agent.as_deref()).await);
        let tasks = attempt!(self.state.store.search(&ctx, &a.query, a.limit).await);
        text(task_list(&tasks, "no matches"))
    }

    #[tool(
        description = "Projects you can see. One line per project: KEY version name, with (archived) and [review] when they apply.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_projects(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<ListProjectsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = attempt!(self.ctx_for(&parts, a.as_agent.as_deref()).await);
        let projects = attempt!(self.state.store.list_projects(&ctx, a.archived).await);
        if projects.is_empty() {
            return text("no projects");
        }
        text(projects.iter().map(project_line).collect::<Vec<_>>().join("\n"))
    }

    #[tool(
        description = "A project and its brief: goals, conventions and definition of done. Read it before claiming work in the project.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn show_project(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<ProjectArg>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = attempt!(self.ctx_for(&parts, a.as_agent.as_deref()).await);
        text(project_text(&attempt!(self.state.store.get_project(&ctx, &a.project).await)))
    }

    #[tool(
        description = "Create a project. Write a brief so other agents know the goals, conventions and definition of done. Tokens limited to certain projects can't create projects.",
        annotations(read_only_hint = false, destructive_hint = false, open_world_hint = false)
    )]
    async fn create_project(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<CreateProjectArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = attempt!(self.ctx_for(&parts, a.as_agent.as_deref()).await);
        let new = NewProject {
            key: a.key,
            name: a.name,
            brief: a.brief.unwrap_or_default(),
            require_review: a.require_review,
            ..Default::default()
        };
        text(project_line(&attempt!(self.state.store.create_project(&ctx, new).await)))
    }

    #[tool(
        description = "Rename a project or replace its brief. Review settings and archiving are left to humans.",
        annotations(read_only_hint = false, open_world_hint = false)
    )]
    async fn update_project(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<UpdateProjectArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = attempt!(self.ctx_for(&parts, a.as_agent.as_deref()).await);
        let patch = ProjectPatch { name: a.name, brief: a.brief, ..Default::default() };
        text(project_line(&attempt!(self.state.store.update_project(&ctx, &a.project, patch, a.version).await)))
    }
}

fn request_ctx(context: &RequestContext<RoleServer>) -> Result<Ctx, ErrorData> {
    context
        .extensions
        .get::<Parts>()
        .ok_or_else(|| ErrorData::invalid_request("not authenticated", None))
        .and_then(ctx_of)
}

fn internal(e: Error) -> ErrorData {
    ErrorData::internal_error(e.to_string(), None)
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for TrailheadMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().enable_resources().build())
            .with_server_info(Implementation::new("trailhead", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let ctx = request_ctx(&context)?;
        let projects = self.state.store.list_projects(&ctx, false).await.map_err(internal)?;
        let resources = projects
            .into_iter()
            .map(|p| {
                Resource::new(format!("{BRIEF_PREFIX}{}/brief", p.key), format!("{} brief", p.key))
                    .with_title(format!("{} project brief", p.name))
                    .with_description("Goals, conventions and definition of done. Read before claiming work.")
            })
            .collect();
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let ctx = request_ctx(&context)?;
        let key = request
            .uri
            .strip_prefix(BRIEF_PREFIX)
            .and_then(|rest| rest.strip_suffix("/brief"))
            .ok_or_else(|| ErrorData::resource_not_found(format!("no resource {}", request.uri), None))?;
        let project = match self.state.store.get_project(&ctx, key).await {
            Ok(p) => p,
            Err(Error::NotFound(_)) => {
                return Err(ErrorData::resource_not_found(format!("no project {key}"), None));
            }
            Err(e) => return Err(internal(e)),
        };
        let body = if project.brief.is_empty() { "(no brief yet)".to_string() } else { project.brief };
        Ok(ReadResourceResult::new(vec![ResourceContents::text(body, request.uri)]).into())
    }
}
