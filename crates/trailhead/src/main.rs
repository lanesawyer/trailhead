mod client;
mod output;

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use client::{ApiError, Client};
use output::{comment_line, event_line, project_line, s, short_time, task_line, task_list};

/// Trailhead: a headless task engine for humans and agents.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Print full JSON instead of terse lines.
    #[arg(long, global = true)]
    json: bool,
    #[arg(long, global = true, env = "TRAILHEAD_URL", default_value = "http://127.0.0.1:7241")]
    url: String,
    #[arg(long, global = true, env = "TRAILHEAD_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// Act as a local agent, using its saved token.
    #[arg(long = "as", global = true, env = "TRAILHEAD_AGENT")]
    agent: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open, unclaimed, unblocked tasks in priority order.
    Ready {
        #[arg(short, long, env = "TRAILHEAD_PROJECT")]
        project: Option<String>,
        #[arg(short = 'n', long)]
        limit: Option<i64>,
    },
    /// List tasks. Defaults to everything not done or cancelled.
    Ls {
        #[arg(short, long, env = "TRAILHEAD_PROJECT")]
        project: Option<String>,
        /// Comma-separated statuses.
        #[arg(short, long)]
        status: Option<String>,
        #[arg(long)]
        parent: Option<String>,
        /// Include done and cancelled.
        #[arg(short, long)]
        all: bool,
        #[arg(short = 'n', long)]
        limit: Option<i64>,
    },
    /// Show a task with its claim, dependencies and comments.
    Show {
        task: String,
    },
    /// Create a task.
    Add {
        title: String,
        #[arg(short, long, env = "TRAILHEAD_PROJECT")]
        project: String,
        #[arg(short, long)]
        description: Option<String>,
        /// 1 (urgent) to 4 (low).
        #[arg(short = 'P', long)]
        priority: Option<i64>,
        #[arg(long)]
        parent: Option<String>,
        /// YYYY-MM-DD or an RFC 3339 timestamp.
        #[arg(long)]
        due: Option<String>,
        /// Tasks that must finish first.
        #[arg(long, value_delimiter = ',')]
        after: Vec<String>,
        /// Tasks this one blocks.
        #[arg(long, value_delimiter = ',')]
        blocks: Vec<String>,
    },
    /// Change a task's fields.
    Edit {
        task: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(short, long)]
        description: Option<String>,
        #[arg(short = 'P', long)]
        priority: Option<i64>,
        /// YYYY-MM-DD, RFC 3339, or "none".
        #[arg(long)]
        due: Option<String>,
        #[arg(short, long)]
        status: Option<String>,
        /// Fail if the task changed since this version.
        #[arg(long)]
        version: Option<i64>,
    },
    /// Take a lease on a task and start it. Prints the last handoff.
    Claim {
        task: String,
        /// Lease length in seconds.
        #[arg(long)]
        ttl: Option<i64>,
    },
    /// Extend your lease.
    Renew {
        task: String,
        #[arg(long)]
        ttl: Option<i64>,
    },
    /// Give a task back.
    Release {
        task: String,
    },
    /// Mark done, optionally leaving a handoff first.
    Done {
        task: String,
        #[arg(short, long)]
        message: Option<String>,
    },
    /// Send for human review.
    Review {
        task: String,
        #[arg(short, long)]
        message: Option<String>,
    },
    Cancel {
        task: String,
    },
    Reopen {
        task: String,
    },
    /// Delete a task (undoable).
    Rm {
        task: String,
    },
    /// Comment on a task.
    Comment {
        task: String,
        body: String,
        /// handoff, progress, question, decision or note.
        #[arg(short, long, default_value = "note")]
        kind: String,
        /// User or agent id to address.
        #[arg(long)]
        to: Option<String>,
    },
    /// Leave a handoff: what's done, what's left, gotchas.
    Handoff {
        task: String,
        body: String,
    },
    /// Ask a question; parks the task until answered.
    Ask {
        task: String,
        body: String,
        #[arg(long)]
        to: Option<String>,
    },
    /// Resolve a question, optionally answering it.
    Answer {
        comment_id: String,
        body: Option<String>,
    },
    /// BLOCKER must finish before BLOCKED.
    Block {
        blocker: String,
        blocked: String,
    },
    Unblock {
        blocker: String,
        blocked: String,
    },
    /// Full-text search over tasks and comments.
    Search {
        query: Vec<String>,
        #[arg(short = 'n', long)]
        limit: Option<i64>,
    },
    /// The event log, newest first.
    Log {
        task: Option<String>,
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: i64,
    },
    /// Follow events live as they happen.
    Watch {
        /// Only this task's events.
        task: Option<String>,
        /// Comma-separated kind prefixes, e.g. `task.,comment.`.
        #[arg(short, long)]
        kinds: Option<String>,
        /// Replay from after this event id instead of starting now.
        #[arg(long)]
        after: Option<i64>,
    },
    /// Undo an event or batch. With no arguments, undoes your last batch.
    Undo {
        event_id: Option<i64>,
        #[arg(long)]
        batch: Option<String>,
    },
    /// Revert everything an agent did since a time.
    Revert {
        actor_id: String,
        /// RFC 3339 timestamp or YYYY-MM-DD.
        #[arg(long)]
        since: String,
    },
    #[command(subcommand)]
    Project(ProjectCmd),
    #[command(subcommand)]
    Agent(AgentCmd),
    #[command(subcommand)]
    Token(TokenCmd),
    Whoami,
    /// Manage the local background server.
    #[command(subcommand)]
    Server(ServerCmd),
    /// Run the server in the foreground.
    Serve {
        #[arg(long, env = "TRAILHEAD_BIND", default_value = trailhead_server::DEFAULT_BIND)]
        bind: String,
    },
}

#[derive(Subcommand)]
enum ProjectCmd {
    Ls {
        #[arg(short, long)]
        all: bool,
    },
    Add {
        key: String,
        name: String,
        #[arg(long)]
        brief: Option<String>,
        #[arg(long)]
        brief_file: Option<PathBuf>,
        /// Agents must send work for review.
        #[arg(long)]
        review: bool,
    },
    Show {
        project: String,
    },
    Edit {
        project: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        brief: Option<String>,
        #[arg(long)]
        brief_file: Option<PathBuf>,
        #[arg(long)]
        review: Option<bool>,
        #[arg(long)]
        archive: bool,
        #[arg(long)]
        unarchive: bool,
    },
}

#[derive(Subcommand)]
enum ServerCmd {
    /// Is it running, on which binary, and since when.
    Status,
    /// Start it in the background if it isn't running.
    Start,
    /// Stop it gracefully.
    Stop,
    /// Stop it and start it again, e.g. after a rebuild.
    Restart,
}

#[derive(Subcommand)]
enum AgentCmd {
    Ls,
    /// Create an agent and print its token (shown once).
    Add {
        name: String,
    },
}

#[derive(Subcommand)]
enum TokenCmd {
    Ls,
    Revoke { id: String },
}

fn parse_when(s: &str) -> Result<String> {
    if OffsetDateTime::parse(s, &Rfc3339).is_ok() {
        return Ok(s.to_string());
    }
    let fmt = time::macros::format_description!("[year]-[month]-[day]");
    let date = time::Date::parse(s, &fmt).with_context(|| format!("{s:?} isn't YYYY-MM-DD or RFC 3339"))?;
    let end_of_day = date.with_hms(23, 59, 59)?.assume_offset(output::local_offset());
    Ok(end_of_day.format(&Rfc3339)?)
}

fn read_brief(brief: Option<String>, file: Option<PathBuf>) -> Result<Option<String>> {
    match (brief, file) {
        (Some(_), Some(_)) => bail!("use --brief or --brief-file, not both"),
        (b, None) => Ok(b),
        (None, Some(f)) => Ok(Some(std::fs::read_to_string(&f).with_context(|| format!("reading {}", f.display()))?)),
    }
}

/// Maps user and agent ids to names for display.
async fn names(c: &Client) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    let me = c.get("me", &[]).await?;
    map.insert(s(&me["user"], "id").to_string(), s(&me["user"], "name").to_string());
    for a in c.get("agents", &[]).await?.as_array().into_iter().flatten() {
        map.insert(s(a, "id").to_string(), s(a, "name").to_string());
    }
    Ok(map)
}

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).expect("valid json"));
}

fn main() {
    output::init_local_offset();
    let cli = Cli::parse();
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    if let Err(e) = rt.block_on(run(cli)) {
        match e.downcast_ref::<ApiError>() {
            Some(api) => eprintln!("{api}"),
            None => eprintln!("error: {e:#}"),
        }
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    match &cli.cmd {
        Cmd::Serve { bind } => return serve(bind).await,
        Cmd::Server(cmd) => return server(&cli.url, cmd, cli.json).await,
        _ => {}
    }
    let c = Client::connect(&cli.url, cli.token.clone(), cli.agent.as_deref()).await?;
    let json = cli.json;
    let show = |v: Value, line: &dyn Fn(&Value) -> String| {
        if json { print_json(&v) } else { println!("{}", line(&v)) }
    };

    match cli.cmd {
        Cmd::Serve { .. } | Cmd::Server(_) => unreachable!(),
        Cmd::Ready { project, limit } => {
            let mut q = vec![];
            if let Some(p) = project {
                q.push(("project", p));
            }
            if let Some(n) = limit {
                q.push(("limit", n.to_string()));
            }
            let v = c.get("ready", &q).await?;
            if json { print_json(&v) } else { task_list(&v, "nothing ready") }
        }
        Cmd::Ls { project, status, parent, all, limit } => {
            let mut q = vec![];
            if let Some(p) = project {
                q.push(("project", p));
            }
            match (status, all) {
                (Some(st), _) => q.push(("status", st)),
                (None, true) => q.push(("status", "open,in_progress,needs_input,in_review,done,cancelled".into())),
                _ => {}
            }
            if let Some(p) = parent {
                q.push(("parent", p));
            }
            if let Some(n) = limit {
                q.push(("limit", n.to_string()));
            }
            let v = c.get("tasks", &q).await?;
            if json { print_json(&v) } else { task_list(&v, "no tasks") }
        }
        Cmd::Show { task } => {
            let v = c.get(&format!("tasks/{task}"), &[]).await?;
            if json {
                print_json(&v);
            } else {
                print_detail(&c, &v).await?;
            }
        }
        Cmd::Add { title, project, description, priority, parent, due, after, blocks } => {
            let body = json!({
                "project": project,
                "title": title,
                "description": description.unwrap_or_default(),
                "priority": priority,
                "parent": parent,
                "due_at": due.as_deref().map(parse_when).transpose()?,
                "blocked_by": after,
                "blocks": blocks,
            });
            show(c.post("tasks", body).await?, &task_line);
        }
        Cmd::Edit { task, title, description, priority, due, status, version } => {
            let mut body = json!({});
            if let Some(t) = title {
                body["title"] = json!(t);
            }
            if let Some(d) = description {
                body["description"] = json!(d);
            }
            if let Some(p) = priority {
                body["priority"] = json!(p);
            }
            if let Some(d) = due {
                body["due_at"] = if d == "none" { Value::Null } else { json!(parse_when(&d)?) };
            }
            if let Some(st) = status {
                body["status"] = json!(st);
            }
            if let Some(v) = version {
                body["version"] = json!(v);
            }
            show(c.patch(&format!("tasks/{task}"), body).await?, &task_line);
        }
        Cmd::Claim { task, ttl } => {
            let v = c.post(&format!("tasks/{task}/claim"), json!({ "ttl": ttl })).await?;
            if json {
                print_json(&v);
            } else {
                println!("{}", task_line(&v["task"]));
                println!("claimed until {}", short_time(s(&v["claim"], "expires_at")));
                if v["handoff"].is_object() {
                    println!(
                        "\nlast handoff ({}):\n{}",
                        short_time(s(&v["handoff"], "created_at")),
                        s(&v["handoff"], "body")
                    );
                }
            }
        }
        Cmd::Renew { task, ttl } => {
            let v = c.post(&format!("tasks/{task}/renew"), json!({ "ttl": ttl })).await?;
            show(v, &|v| format!("claimed until {}", short_time(s(v, "expires_at"))));
        }
        Cmd::Release { task } => show(c.post(&format!("tasks/{task}/release"), json!({})).await?, &task_line),
        Cmd::Done { task, message } => finish(&c, &task, "done", message, json).await?,
        Cmd::Review { task, message } => finish(&c, &task, "in_review", message, json).await?,
        Cmd::Cancel { task } => finish(&c, &task, "cancelled", None, json).await?,
        Cmd::Reopen { task } => finish(&c, &task, "open", None, json).await?,
        Cmd::Rm { task } => show(c.delete(&format!("tasks/{task}")).await?, &|t| format!("deleted {}", s(t, "key"))),
        Cmd::Comment { task, body, kind, to } => comment(&c, &task, &kind, &body, to, json).await?,
        Cmd::Handoff { task, body } => comment(&c, &task, "handoff", &body, None, json).await?,
        Cmd::Ask { task, body, to } => comment(&c, &task, "question", &body, to, json).await?,
        Cmd::Answer { comment_id, body } => {
            let v = c.post(&format!("comments/{comment_id}/resolve"), json!({ "answer": body })).await?;
            show(v, &|_| "resolved".into());
        }
        Cmd::Block { blocker, blocked } => {
            let v = c.post(&format!("tasks/{blocked}/blockers"), json!({ "blocker": blocker })).await?;
            show(v, &|_| format!("{blocker} now blocks {blocked}"));
        }
        Cmd::Unblock { blocker, blocked } => {
            c.delete(&format!("tasks/{blocked}/blockers/{blocker}")).await?;
            if !json {
                println!("{blocker} no longer blocks {blocked}");
            }
        }
        Cmd::Search { query, limit } => {
            let mut q = vec![("q", query.join(" "))];
            if let Some(n) = limit {
                q.push(("limit", n.to_string()));
            }
            let v = c.get("search", &q).await?;
            if json { print_json(&v) } else { task_list(&v, "no matches") }
        }
        Cmd::Log { task, limit } => {
            let mut q = vec![("newest_first", "true".to_string()), ("limit", limit.to_string())];
            if let Some(t) = task {
                q.push(("task", t));
            }
            let v = c.get("events", &q).await?;
            if json {
                print_json(&v);
            } else {
                v.as_array().into_iter().flatten().for_each(|e| println!("{}", event_line(e)));
            }
        }
        Cmd::Watch { task, kinds, after } => {
            let mut q = vec![];
            if let Some(t) = task {
                q.push(("task", t));
            }
            if let Some(k) = kinds {
                q.push(("kinds", k));
            }
            if let Some(a) = after {
                q.push(("after", a.to_string()));
            }
            c.watch(&q, |e| {
                if json {
                    println!("{e}");
                } else {
                    println!("{}", event_line(&e));
                }
            })
            .await?;
        }
        Cmd::Undo { event_id, batch } => {
            let body = match (event_id, batch) {
                (Some(e), None) => json!({ "event_id": e }),
                (None, Some(b)) => json!({ "batch_id": b }),
                (None, None) => {
                    let me = c.get("me", &[]).await?;
                    let actor = me["agent"]["id"].as_str().or(me["user"]["id"].as_str()).unwrap_or("").to_string();
                    let q = [("actor", actor), ("newest_first", "true".into()), ("limit", "1".into())];
                    let last = c.get("events", &q).await?;
                    let Some(b) = last[0]["batch_id"].as_str() else { bail!("nothing to undo") };
                    json!({ "batch_id": b })
                }
                _ => bail!("give an event id or --batch, not both"),
            };
            let v = c.post("undo", body).await?;
            show(v, &|r| format!("reverted {} change(s)", r["reverted"].as_array().map_or(0, Vec::len)));
        }
        Cmd::Revert { actor_id, since } => {
            let v = c.post("revert", json!({ "actor_id": actor_id, "since": parse_when(&since)? })).await?;
            if json {
                print_json(&v);
            } else {
                println!("reverted {} change(s)", v["reverted"].as_array().map_or(0, Vec::len));
                for sk in v["skipped"].as_array().into_iter().flatten() {
                    println!(
                        "skipped {} {}: changed later by event #{}",
                        s(sk, "entity"),
                        s(sk, "entity_id"),
                        sk["later_event"]
                    );
                }
            }
        }
        Cmd::Project(p) => project(&c, p, json).await?,
        Cmd::Agent(AgentCmd::Ls) => {
            let v = c.get("agents", &[]).await?;
            if json {
                print_json(&v);
            } else {
                v.as_array().into_iter().flatten().for_each(|a| println!("{}  {}", s(a, "id"), s(a, "name")));
            }
        }
        Cmd::Agent(AgentCmd::Add { name }) => {
            let v = c.post("agents", json!({ "name": name })).await?;
            let safe_name = name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
            let saved = if c.local && safe_name {
                let path = trailhead_server::Paths::from_env().context("no home directory")?.agent_token(&name);
                trailhead_server::write_secret(&path, s(&v, "secret"))?;
                Some(path)
            } else {
                None
            };
            if json {
                print_json(&v);
            } else {
                println!("agent {} ({})", s(&v["agent"], "name"), s(&v["agent"], "id"));
                match saved {
                    Some(path) => println!("token saved to {}; use `trailhead --as {name}`", path.display()),
                    None => println!("token (shown once): {}", s(&v, "secret")),
                }
            }
        }
        Cmd::Token(TokenCmd::Ls) => {
            let v = c.get("tokens", &[]).await?;
            if json {
                print_json(&v);
            } else {
                for t in v.as_array().into_iter().flatten() {
                    let revoked = if t["revoked_at"].is_null() { "" } else { "  (revoked)" };
                    println!(
                        "{}  {} {}  {}{}",
                        s(t, "id"),
                        s(&t["principal"], "kind"),
                        s(&t["principal"], "id"),
                        s(t, "name"),
                        revoked
                    );
                }
            }
        }
        Cmd::Token(TokenCmd::Revoke { id }) => show(c.delete(&format!("tokens/{id}")).await?, &|_| "revoked".into()),
        Cmd::Whoami => {
            let v = c.get("me", &[]).await?;
            show(v, &|m| match m["agent"].as_object() {
                Some(_) => format!("agent {} acting for {}", s(&m["agent"], "name"), s(&m["user"], "name")),
                None => format!("{} ({})", s(&m["user"], "name"), s(&m["user"], "id")),
            });
        }
    }
    Ok(())
}

async fn finish(c: &Client, task: &str, status: &str, handoff: Option<String>, json: bool) -> Result<()> {
    if let Some(m) = handoff {
        c.post(&format!("tasks/{task}/comments"), json!({ "kind": "handoff", "body": m })).await?;
    }
    let v = c.patch(&format!("tasks/{task}"), json!({ "status": status })).await?;
    if json {
        print_json(&v)
    } else {
        println!("{}", task_line(&v))
    }
    Ok(())
}

async fn comment(c: &Client, task: &str, kind: &str, body: &str, to: Option<String>, json: bool) -> Result<()> {
    let v =
        c.post(&format!("tasks/{task}/comments"), json!({ "kind": kind, "body": body, "addressed_to": to })).await?;
    if json {
        print_json(&v)
    } else {
        println!("{} {}", s(&v, "kind"), s(&v, "id"))
    }
    Ok(())
}

async fn print_detail(c: &Client, v: &Value) -> Result<()> {
    let names = names(c).await?;
    let name = |id: &str| names.get(id).cloned().unwrap_or_else(|| id.chars().take(8).collect());
    let t = &v["task"];
    println!("{}  {}", s(t, "key"), s(t, "title"));
    let mut meta = vec![s(t, "status").to_string(), format!("p{}", t["priority"]), format!("v{}", t["version"])];
    if let Some(due) = t["due_at"].as_str() {
        meta.push(format!("due {}", short_time(due)));
    }
    println!("{}", meta.join(" · "));
    if v["claim"].is_object() {
        println!("claimed by {} until {}", name(s(&v["claim"], "holder_id")), short_time(s(&v["claim"], "expires_at")));
    }
    let keys = |k: &str| v[k].as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "));
    if let Some(b) = keys("blocked_by").filter(|b| !b.is_empty()) {
        println!("blocked by {b}");
    }
    if let Some(b) = keys("blocks").filter(|b| !b.is_empty()) {
        println!("blocks {b}");
    }
    if t["metadata"].as_object().is_some_and(|m| !m.is_empty()) {
        println!("metadata {}", t["metadata"]);
    }
    if !s(t, "description").is_empty() {
        println!("\n{}", s(t, "description"));
    }
    let comments = v["comments"].as_array().cloned().unwrap_or_default();
    if !comments.is_empty() {
        println!();
        for cm in &comments {
            println!("{}", comment_line(cm, &name));
        }
    }
    Ok(())
}

async fn project(c: &Client, cmd: ProjectCmd, json: bool) -> Result<()> {
    match cmd {
        ProjectCmd::Ls { all } => {
            let v = c.get("projects", &[("archived", all.to_string())]).await?;
            if json {
                print_json(&v);
            } else {
                v.as_array().into_iter().flatten().for_each(|p| println!("{}", project_line(p)));
            }
        }
        ProjectCmd::Add { key, name, brief, brief_file, review } => {
            let brief = read_brief(brief, brief_file)?.unwrap_or_default();
            let v = c
                .post("projects", json!({ "key": key, "name": name, "brief": brief, "require_review": review }))
                .await?;
            if json { print_json(&v) } else { println!("{}", project_line(&v)) }
        }
        ProjectCmd::Show { project } => {
            let v = c.get(&format!("projects/{project}"), &[]).await?;
            if json {
                print_json(&v);
            } else {
                println!("{}", project_line(&v));
                if !s(&v, "brief").is_empty() {
                    println!("\n{}", s(&v, "brief"));
                }
            }
        }
        ProjectCmd::Edit { project, name, brief, brief_file, review, archive, unarchive } => {
            let mut body = json!({});
            if let Some(n) = name {
                body["name"] = json!(n);
            }
            if let Some(b) = read_brief(brief, brief_file)? {
                body["brief"] = json!(b);
            }
            if let Some(r) = review {
                body["require_review"] = json!(r);
            }
            match (archive, unarchive) {
                (true, true) => bail!("--archive or --unarchive, not both"),
                (true, false) => body["status"] = json!("archived"),
                (false, true) => body["status"] = json!("active"),
                _ => {}
            }
            let v = c.patch(&format!("projects/{project}"), body).await?;
            if json { print_json(&v) } else { println!("{}", project_line(&v)) }
        }
    }
    Ok(())
}

async fn server(url: &str, cmd: &ServerCmd, json: bool) -> Result<()> {
    let (http, base) = client::http_and_base(url)?;
    let local = client::is_local(&base);
    if !local && !matches!(cmd, ServerCmd::Status) {
        bail!("{url} isn't a local server; only `trailhead server status` works against it");
    }
    match cmd {
        ServerCmd::Status => match client::health(&http, &base).await {
            Some(info) if json => print_json(&serde_json::to_value(&info)?),
            Some(info) => {
                let mut parts = vec!["running".to_string()];
                if let Some(pid) = info.pid {
                    parts.push(format!("pid {pid}"));
                }
                if let (Some(h), Some(p)) = (base.host_str(), base.port_or_known_default()) {
                    parts.push(format!("{h}:{p}"));
                }
                if let Some(v) = &info.version {
                    parts.push(format!("v{v}"));
                }
                if let Some(started) = info.started_at.as_deref().and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok())
                {
                    let up = OffsetDateTime::now_utc() - started;
                    let since = short_time(&started.format(&Rfc3339)?);
                    parts.push(format!("up {}h {}m (since {since})", up.whole_hours(), up.whole_minutes() % 60));
                }
                println!("{}", parts.join(" · "));
                if let Some(exe) = &info.exe {
                    println!("binary {}", exe.display());
                }
                if let Some(db) = &info.db {
                    println!("db {}", db.display());
                }
                if local && client::is_stale(&info) {
                    println!("the binary has been rebuilt since; the next command restarts the server");
                }
            }
            None if json => print_json(&json!({ "ok": false })),
            None => println!("not running"),
        },
        ServerCmd::Start => match client::health(&http, &base).await {
            Some(_) => println!("already running"),
            None => {
                client::start_local_server(&http, &base).await?;
                println!("started");
            }
        },
        ServerCmd::Stop => match client::stop_local(&http, &base).await? {
            Some(pid) => println!("stopped (pid {pid})"),
            None => println!("not running"),
        },
        ServerCmd::Restart => {
            client::stop_local(&http, &base).await?;
            client::start_local_server(&http, &base).await?;
            let pid = client::health(&http, &base).await.and_then(|i| i.pid).map(|p| p.to_string()).unwrap_or_default();
            println!("restarted (pid {pid})");
        }
    }
    Ok(())
}

async fn serve(bind: &str) -> Result<()> {
    tracing_subscriber::fmt()
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,tower_http=warn".into()),
        )
        .init();
    let paths = trailhead_server::Paths::from_env().context("no home directory")?;
    let cfg = trailhead_server::LocalConfig {
        bind: bind.parse().with_context(|| format!("bad bind address {bind}"))?,
        db: std::env::var_os("TRAILHEAD_DB").map(PathBuf::from).unwrap_or_else(|| paths.db()),
        credentials: paths.credentials(),
        user_name: std::env::var("USER").unwrap_or_else(|_| "me".into()),
        pid_file: Some(paths.pid_file()),
    };
    trailhead_server::serve_local(cfg).await.map_err(|e| anyhow::anyhow!("{e}"))
}
