//! MCP at `/mcp` for agents without a shell. Tools mirror the CLI and answer in
//! the same terse one-line-per-task text, because agents pay for every token.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http::request::Parts;
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
use serde::Deserialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use trailhead_core::{
    Comment, CommentKind, Ctx, Error, EventFilter, NewComment, NewTask, Status, Task, TaskDetail, TaskFilter, TaskPatch,
};
use uuid::Uuid;

use crate::AppState;
use crate::error::ApiError;

const BRIEF_PREFIX: &str = "trailhead://projects/";

const INSTRUCTIONS: &str = "Trailhead is a shared task engine. Workflow: `ready` to see what's next, \
read the project's brief resource, `claim` a task before working on it (the last handoff comes back \
with the claim), `renew` on long work, `comment` with kind `decision` for choices and `question` for \
anything only a human can answer, and finish with `update_task` status `done` after leaving a \
`handoff` comment. File follow-ups with `create_task` instead of widening scope.";

pub fn service(state: Arc<AppState>) -> StreamableHttpService<TrailheadMcp, LocalSessionManager> {
    let config = StreamableHttpServerConfig::default().with_legacy_session_mode(false).with_json_response(true);
    StreamableHttpService::new(move || Ok(TrailheadMcp::new(state.clone())), Default::default(), config)
}

/// Authenticates `/mcp` requests with the same bearer tokens as REST. The context
/// rides along in the request extensions, which rmcp hands to each tool.
pub async fn auth(State(state): State<Arc<AppState>>, mut req: Request, next: Next) -> Response {
    let secret = req
        .headers()
        .get(http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string());
    let ctx = match secret {
        Some(s) => state.store.authenticate_token(&s).await.and_then(|(ctx, token)| {
            state.limits.check(&token)?;
            Ok(ctx)
        }),
        None => Err(Error::Unauthorized),
    };
    match ctx {
        Ok(ctx) => {
            req.extensions_mut().insert(ctx);
            next.run(req).await
        }
        Err(e) => ApiError(e).into_response(),
    }
}

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

fn date(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).map(|s| s[..10].to_string()).unwrap_or_default()
}

fn task_line(t: &Task) -> String {
    let mut line = format!("{} {} p{} v{} {}", t.key, t.status, t.priority, t.version, t.title);
    if let Some(due) = t.due_at {
        line.push_str(&format!(" (due {})", date(due)));
    }
    line
}

fn task_list(tasks: &[Task], empty: &str) -> String {
    if tasks.is_empty() { empty.to_string() } else { tasks.iter().map(task_line).collect::<Vec<_>>().join("\n") }
}

fn comment_line(c: &Comment) -> String {
    let mut tags = c.kind.to_string();
    if c.kind == CommentKind::Question {
        tags.push_str(if c.resolved_at.is_some() { ", resolved" } else { ", open" });
    }
    format!("[{tags}] id {}: {}", c.id, c.body)
}

fn detail_text(d: &TaskDetail) -> String {
    let t = &d.task;
    let mut out = vec![task_line(t)];
    if let Some(c) = &d.claim {
        out.push(format!("claimed by {} until {}", c.holder_id, c.expires_at.format(&Rfc3339).unwrap_or_default()));
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
        out.extend(d.comments.iter().map(comment_line));
    }
    out.join("\n")
}

fn parse_time(s: &str) -> Result<OffsetDateTime, Error> {
    OffsetDateTime::parse(s, &Rfc3339).map_err(|_| Error::Invalid(format!("{s:?} is not an RFC 3339 timestamp")))
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ReadyArgs {
    /// Project key, e.g. AUTH. Omit for every project.
    project: Option<String>,
    /// Defaults to 20.
    limit: Option<i64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ListArgs {
    project: Option<String>,
    /// Comma-separated statuses: open, in_progress, needs_input, in_review, done, cancelled.
    /// Defaults to everything not done or cancelled.
    status: Option<String>,
    /// Parent task key, to list subtasks.
    parent: Option<String>,
    limit: Option<i64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct TaskArg {
    /// Task key like AUTH-42, or its id.
    task: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct CreateArgs {
    /// Project key.
    project: String,
    title: String,
    description: Option<String>,
    /// 1 (urgent) to 4 (low). Defaults to 3.
    priority: Option<i64>,
    /// Parent task key, for subtasks.
    parent: Option<String>,
    /// Task keys that must finish before this one.
    #[serde(default)]
    blocked_by: Vec<String>,
    /// Task keys this one blocks. To get help on your own task, create a subtask that blocks it.
    #[serde(default)]
    blocks: Vec<String>,
    /// RFC 3339 timestamp.
    due_at: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct UpdateArgs {
    task: String,
    title: Option<String>,
    description: Option<String>,
    /// open, in_progress, needs_input, in_review, done or cancelled.
    status: Option<String>,
    priority: Option<i64>,
    /// The version you last saw; the update fails if the task changed since.
    version: Option<i64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ClaimArgs {
    task: String,
    /// Lease length in seconds. Defaults to the project's or workspace's TTL (30 minutes).
    ttl: Option<i64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct CommentArgs {
    task: String,
    body: String,
    /// handoff, progress, question, decision or note (default). A question parks the task until answered.
    kind: Option<String>,
    /// User or agent id the comment is for.
    #[schemars(with = "Option<String>")]
    addressed_to: Option<Uuid>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ResolveArgs {
    #[schemars(with = "String")]
    comment_id: Uuid,
    /// Posted as a note addressed to whoever asked.
    answer: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct SearchArgs {
    query: String,
    limit: Option<i64>,
}

#[tool_router]
impl TrailheadMcp {
    fn new(state: Arc<AppState>) -> Self {
        TrailheadMcp { state, tool_router: Self::tool_router() }
    }

    #[tool(
        description = "Open, unclaimed, unblocked tasks in priority order. One line per task: KEY status priority version title."
    )]
    async fn ready(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<ReadyArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = ctx_of(&parts)?;
        let tasks = attempt!(self.state.store.ready(&ctx, a.project.as_deref(), a.limit).await);
        text(task_list(&tasks, "nothing ready"))
    }

    #[tool(description = "List tasks with filters. Defaults to everything not done or cancelled.")]
    async fn list_tasks(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<ListArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = ctx_of(&parts)?;
        let statuses = attempt!(
            a.status
                .as_deref()
                .unwrap_or("")
                .split(',')
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().parse())
                .collect::<Result<Vec<Status>, _>>()
        );
        let filter =
            TaskFilter { project: a.project, statuses, parent: a.parent, limit: a.limit, ..Default::default() };
        let tasks = attempt!(self.state.store.list_tasks(&ctx, filter).await);
        text(task_list(&tasks, "no tasks"))
    }

    #[tool(description = "A task with its claim, dependencies, description and comments.")]
    async fn show(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<TaskArg>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = ctx_of(&parts)?;
        text(detail_text(&attempt!(self.state.store.task_detail(&ctx, &a.task).await)))
    }

    #[tool(description = "Create a task. Use blocks to make a subtask that blocks your own task.")]
    async fn create_task(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<CreateArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = ctx_of(&parts)?;
        let due_at = attempt!(a.due_at.as_deref().map(parse_time).transpose());
        let new = NewTask {
            project: a.project,
            title: a.title,
            description: a.description.unwrap_or_default(),
            priority: a.priority,
            due_at,
            parent: a.parent,
            ..Default::default()
        };
        let store = &self.state.store;
        let task = attempt!(store.create_task(&ctx, new).await);
        for blocker in &a.blocked_by {
            attempt!(store.add_dependency(&ctx, blocker, &task.key).await);
        }
        for blocked in &a.blocks {
            attempt!(store.add_dependency(&ctx, &task.key, blocked).await);
        }
        text(task_line(&task))
    }

    #[tool(description = "Change a task's fields or status. Setting status done or in_review releases your claim.")]
    async fn update_task(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<UpdateArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = ctx_of(&parts)?;
        let status = attempt!(a.status.as_deref().map(str::parse::<Status>).transpose());
        let patch = TaskPatch {
            title: a.title,
            description: a.description,
            status,
            priority: a.priority,
            ..Default::default()
        };
        text(task_line(&attempt!(self.state.store.update_task(&ctx, &a.task, patch, a.version).await)))
    }

    #[tool(description = "Take a lease on a task and start it. Returns the last handoff left on it.")]
    async fn claim(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<ClaimArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = ctx_of(&parts)?;
        let r = attempt!(self.state.store.claim(&ctx, &a.task, a.ttl).await);
        let mut out = format!(
            "{}\nclaimed until {}",
            task_line(&r.task),
            r.claim.expires_at.format(&Rfc3339).unwrap_or_default()
        );
        if let Some(h) = r.handoff {
            out.push_str(&format!("\n\nlast handoff:\n{}", h.body));
        }
        text(out)
    }

    #[tool(description = "Extend your lease. Renew about every third of the TTL.")]
    async fn renew(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<ClaimArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = ctx_of(&parts)?;
        let c = attempt!(self.state.store.renew(&ctx, &a.task, a.ttl).await);
        text(format!("claimed until {}", c.expires_at.format(&Rfc3339).unwrap_or_default()))
    }

    #[tool(description = "Give a task back without finishing it. Leave a handoff comment first.")]
    async fn release(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<TaskArg>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = ctx_of(&parts)?;
        text(task_line(&attempt!(self.state.store.release(&ctx, &a.task).await)))
    }

    #[tool(description = "Comment on a task. Kinds: handoff, progress, question, decision, note.")]
    async fn comment(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<CommentArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = ctx_of(&parts)?;
        let kind = attempt!(a.kind.as_deref().map(str::parse::<CommentKind>).transpose()).unwrap_or(CommentKind::Note);
        let new = NewComment { kind, body: a.body, addressed_to: a.addressed_to };
        let c = attempt!(self.state.store.add_comment(&ctx, &a.task, new).await);
        text(format!("{} {}", c.kind, c.id))
    }

    #[tool(description = "Resolve a question comment, optionally answering it.")]
    async fn resolve_question(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<ResolveArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = ctx_of(&parts)?;
        attempt!(self.state.store.resolve_comment(&ctx, a.comment_id, a.answer).await);
        text("resolved")
    }

    #[tool(
        description = "Undo your own most recent change (everything from your last tool call that wrote something). Fails if someone changed the same thing since."
    )]
    async fn undo_last(&self, Extension(parts): Extension<Parts>) -> Result<CallToolResult, ErrorData> {
        let ctx = ctx_of(&parts)?;
        let store = &self.state.store;
        let filter =
            EventFilter {
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

    #[tool(description = "Full-text search over task titles, descriptions and comments.")]
    async fn search(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(a): Parameters<SearchArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = ctx_of(&parts)?;
        let tasks = attempt!(self.state.store.search(&ctx, &a.query, a.limit).await);
        text(task_list(&tasks, "no matches"))
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
