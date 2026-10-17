mod cli;
mod client;
mod output;
mod repo;

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use cli::{AgentCmd, Cli, Cmd, ProjectCmd, ServerCmd, TokenCmd, WebhookCmd};
use client::{ApiError, Client, Identity};
use output::{comment_line, event_line, project_line, s, short_time, task_line, task_list};
use trailhead_server::creds::{Cred, Saved};

/// RFC 3339 passes through; a bare date means the end of that day in the local
/// zone, using that day's offset (DST included).
fn parse_when(s: &str) -> Result<String> {
    parse_when_in(s, output::local_tz())
}

fn parse_when_in(s: &str, tz: &jiff::tz::TimeZone) -> Result<String> {
    if s.parse::<jiff::Timestamp>().is_ok() {
        return Ok(s.to_string());
    }
    let date: jiff::civil::Date = s.parse().with_context(|| format!("{s:?} isn't YYYY-MM-DD or RFC 3339"))?;
    let end_of_day = date.at(23, 59, 59, 0).to_zoned(tz.clone())?;
    Ok(end_of_day.timestamp().to_string())
}

/// A day to schedule work on: `YYYY-MM-DD`, `today` or `tomorrow`, in the local zone.
fn parse_day(s: &str) -> Result<String> {
    parse_day_from(s, jiff::Zoned::now().with_time_zone(output::local_tz().clone()).date())
}

fn parse_day_from(s: &str, today: jiff::civil::Date) -> Result<String> {
    let day = match s {
        "today" => today,
        "tomorrow" => today.tomorrow()?,
        _ => s.parse().with_context(|| format!("{s:?} isn't YYYY-MM-DD, today or tomorrow"))?,
    };
    Ok(day.to_string())
}

/// The local IANA zone name, e.g. `America/Los_Angeles`, if the system has one.
fn local_time_zone() -> Option<String> {
    output::local_tz().iana_name().map(String::from)
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

/// Query parameters, leaving out the ones not given.
fn params<const N: usize>(pairs: [(&'static str, Option<String>); N]) -> Vec<(&'static str, String)> {
    pairs.into_iter().filter_map(|(k, v)| v.map(|v| (k, v))).collect()
}

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).expect("valid json"));
}

/// Rust ignores SIGPIPE, so `println!` panics once a reader like `head` closes
/// the pipe. By then the command has done its work; exit like a Unix tool killed
/// by SIGPIPE instead of printing a panic.
fn exit_quietly_on_broken_pipe() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info
            .payload()
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| info.payload().downcast_ref::<&str>().copied())
            .unwrap_or_default();
        // EPIPE on Unix, ERROR_NO_DATA on Windows.
        let broken_pipe = msg.contains("(os error 32)") || msg.contains("(os error 232)");
        if msg.starts_with("failed printing to std") && broken_pipe {
            std::process::exit(141);
        }
        default(info);
    }));
}

fn main() {
    exit_quietly_on_broken_pipe();
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
    let repo = repo::find(&std::env::current_dir()?)?.unwrap_or_default();
    let env_agent = std::env::var("TRAILHEAD_AGENT").ok().filter(|a| !a.is_empty());
    let agent = match (cli.as_me, cli.agent.clone(), env_agent, repo.agent.clone()) {
        (true, ..) => None,
        (false, Some(a), ..) | (false, None, Some(a), _) => Some(Identity { name: a, from: None }),
        (false, None, None, Some(a)) => Some(Identity { name: a, from: Some(repo.path.clone()) }),
        (false, None, None, None) => None,
    };
    let default_project = repo.project.clone();
    let c = Client::connect(&cli.url, cli.token.clone(), agent.as_ref()).await?;
    let json = cli.json;
    let show = |v: Value, line: &dyn Fn(&Value) -> String| {
        if json { print_json(&v) } else { println!("{}", line(&v)) }
    };

    match cli.cmd {
        Cmd::Serve { .. } | Cmd::Server(_) => unreachable!(),
        Cmd::Ready { project, limit } => {
            let q = params([("project", project.or(default_project)), ("limit", limit.map(|n| n.to_string()))]);
            let v = c.get("ready", &q).await?;
            if json {
                print_json(&v)
            } else {
                task_list(&v, "nothing ready");
                // People's claims don't expire, so a forgotten one would hide work for good.
                let held = c.get("claims", &[]).await?;
                for h in held.as_array().into_iter().flatten() {
                    println!("you hold {} ({}), claimed {}", s(h, "key"), s(h, "status"), output::lease(&h["claim"]));
                }
            }
        }
        Cmd::Ls { project, status, parent, all, today, limit } => {
            let status = status.or(all.then(|| "open,in_progress,needs_input,in_review,done,cancelled".into()));
            let q = params([
                ("project", project.or(default_project)),
                ("status", status),
                ("parent", parent),
                ("scheduled_through", today.then(|| parse_day("today")).transpose()?),
                ("limit", limit.map(|n| n.to_string())),
            ]);
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
        Cmd::Add { title, project, description, priority, parent, due, on, after, blocks } => {
            let project =
                project.or(default_project).context("--project is required (or set project in .trailhead.toml)")?;
            let body = json!({
                "project": project,
                "title": title,
                "description": description.unwrap_or_default(),
                "priority": priority,
                "parent": parent,
                "due_at": due.as_deref().map(parse_when).transpose()?,
                "due_tz": due.as_ref().and_then(|_| local_time_zone()),
                "scheduled_on": on.as_deref().map(parse_day).transpose()?,
                "blocked_by": after,
                "blocks": blocks,
            });
            show(c.post("tasks", body).await?, &task_line);
        }
        Cmd::Edit { task, title, description, priority, due, on, status, version } => {
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
                if d == "none" {
                    body["due_at"] = Value::Null;
                } else {
                    body["due_at"] = json!(parse_when(&d)?);
                    body["due_tz"] = json!(local_time_zone());
                }
            }
            if let Some(d) = on {
                body["scheduled_on"] = if d == "none" { Value::Null } else { json!(parse_day(&d)?) };
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
                println!("claimed {}", output::lease(&v["claim"]));
                if v["handoff"].is_object() {
                    println!(
                        "\nlast handoff ({}):\n{}",
                        short_time(s(&v["handoff"], "created_at")),
                        s(&v["handoff"], "body")
                    );
                }
            }
        }
        Cmd::Mine => {
            let v = c.get("claims", &[]).await?;
            if json {
                print_json(&v);
            } else {
                match v.as_array() {
                    Some(held) if !held.is_empty() => {
                        for h in held {
                            println!(
                                "{:<10} {:<11} {}  {}",
                                s(h, "key"),
                                s(h, "status"),
                                output::lease(&h["claim"]),
                                s(h, "title")
                            );
                        }
                    }
                    _ => println!("you hold no claims"),
                }
            }
        }
        Cmd::Renew { task, ttl } => {
            let v = c.post(&format!("tasks/{task}/renew"), json!({ "ttl": ttl })).await?;
            show(v, &|v| format!("claimed {}", output::lease(v)));
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
            let id = match uuid::Uuid::parse_str(&comment_id) {
                Ok(id) => id.to_string(),
                Err(_) => {
                    let detail = c.get(&format!("tasks/{comment_id}"), &[]).await?;
                    let open: Vec<&Value> = detail["comments"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|q| q["kind"] == "question" && q["resolved_at"].is_null())
                        .collect();
                    match open.as_slice() {
                        [q] => s(q, "id").to_string(),
                        [] => bail!("{comment_id} has no open question"),
                        many => bail!(
                            "{comment_id} has {} open questions; answer one by id:\n{}",
                            many.len(),
                            many.iter()
                                .map(|q| format!("  {}  {}", s(q, "id"), s(q, "body")))
                                .collect::<Vec<_>>()
                                .join("\n")
                        ),
                    }
                }
            };
            let v = c.post(&format!("comments/{id}/resolve"), json!({ "answer": body })).await?;
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
            let q = params([("q", Some(query.join(" "))), ("limit", limit.map(|n| n.to_string()))]);
            let v = c.get("search", &q).await?;
            if json { print_json(&v) } else { task_list(&v, "no matches") }
        }
        Cmd::Log { task, limit } => {
            let q = params([("newest_first", Some("true".into())), ("limit", Some(limit.to_string())), ("task", task)]);
            let v = c.get("events", &q).await?;
            if json {
                print_json(&v);
            } else {
                v.as_array().into_iter().flatten().for_each(|e| println!("{}", event_line(e)));
            }
        }
        Cmd::Watch { task, kinds, after } => {
            let q = params([("task", task), ("kinds", kinds), ("after", after.map(|a| a.to_string()))]);
            c.watch(&q, |e| {
                if json {
                    println!("{e}");
                } else {
                    println!("{}", event_line(&e));
                }
            })
            .await?;
        }
        Cmd::Mcp => mcp_bridge(&c).await?,
        Cmd::Undo { event_id, batch } => {
            let body = match (event_id, batch) {
                (Some(e), None) => json!({ "event_id": e }),
                (None, Some(b)) => json!({ "batch_id": b }),
                (None, None) => {
                    let me = c.get("me", &[]).await?;
                    let actor = me["agent"]["id"].as_str().or(me["user"]["id"].as_str()).unwrap_or("").to_string();
                    let q = [
                        ("actor", actor),
                        ("newest_first", "true".into()),
                        ("changes_only", "true".into()),
                        ("limit", "1".into()),
                    ];
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
                for a in v.as_array().into_iter().flatten() {
                    let revoked = if a["revoked_at"].is_null() { "" } else { "  (revoked)" };
                    println!("{}  {}{revoked}", s(a, "id"), s(a, "name"));
                }
            }
        }
        Cmd::Agent(AgentCmd::Add { name, projects, rate_limit }) => {
            let body = json!({ "name": name, "projects": projects, "rate_limit_per_minute": rate_limit });
            let v = c.post("agents", body).await?;
            // The server names sub-agents `<parent>/<name>`; save under the full name.
            let name = s(&v["agent"], "name").to_string();
            let safe_name = name.split('/').all(|seg| {
                !seg.is_empty() && seg.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
            });
            let saved = if c.local && safe_name {
                let paths = trailhead_server::Paths::from_env().context("no home directory")?;
                Some(paths.creds().save(Cred::Agent(&name), s(&v, "secret"))?)
            } else {
                None
            };
            if json {
                print_json(&v);
            } else {
                println!("agent {} ({})", s(&v["agent"], "name"), s(&v["agent"], "id"));
                match saved {
                    Some(Saved::Keyring) => {
                        println!("token saved to the OS keyring; use `trailhead --as {name}`")
                    }
                    Some(Saved::File(path)) => {
                        println!("token saved to {}; use `trailhead --as {name}`", path.display())
                    }
                    None => println!("token (shown once): {}", s(&v, "secret")),
                }
            }
        }
        Cmd::Agent(AgentCmd::Revoke { name }) => {
            let v = c.post(&format!("agents/{}/revoke", name.replace('/', "%2F")), json!({})).await?;
            show(v, &|a| format!("revoked {} and its sub-agents", s(a, "name")));
        }
        Cmd::Token(TokenCmd::Ls) => {
            let v = c.get("tokens", &[]).await?;
            if json {
                print_json(&v);
            } else {
                let projects = c.get("projects", &[("archived", "true".into())]).await?;
                let key_of = |id: &Value| {
                    let id = id.as_str().unwrap_or("");
                    projects
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find(|p| s(p, "id") == id)
                        .map_or(id.to_string(), |p| s(p, "key").to_string())
                };
                for t in v.as_array().into_iter().flatten() {
                    let revoked = if t["revoked_at"].is_null() { "" } else { "  (revoked)" };
                    let scope = match t["projects"].as_array() {
                        Some(ids) => ids.iter().map(key_of).collect::<Vec<_>>().join(","),
                        None => "all".into(),
                    };
                    let limit = match t["rate_limit_per_minute"].as_i64() {
                        Some(n) => format!("{n}/min"),
                        None => "unlimited".into(),
                    };
                    println!(
                        "{}  {} {}  {}  projects={scope}  rate={limit}{}",
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
        Cmd::Token(TokenCmd::Edit { id, projects, rate_limit }) => {
            let mut body = json!({});
            if let Some(p) = projects {
                let all = p.iter().all(|k| k.trim().is_empty() || k.eq_ignore_ascii_case("all"));
                body["projects"] = if all { Value::Null } else { json!(p) };
            }
            if let Some(r) = rate_limit {
                body["rate_limit_per_minute"] = match r.as_str() {
                    "none" => Value::Null,
                    n => json!(n.parse::<i64>().with_context(|| format!("{n:?} isn't a number or \"none\""))?),
                };
            }
            if body.as_object().is_some_and(|b| b.is_empty()) {
                bail!("nothing to change: pass --projects and/or --rate-limit");
            }
            show(c.patch(&format!("tokens/{id}"), body).await?, &|_| "updated".into());
        }
        Cmd::Webhook(WebhookCmd::Ls) => {
            let v = c.get("webhooks", &[]).await?;
            if json {
                print_json(&v);
            } else {
                for w in v.as_array().into_iter().flatten() {
                    let kinds = w["event_kinds"]
                        .as_array()
                        .map(|k| k.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(","));
                    let state = match (w["disabled_at"].as_str(), w["failure_count"].as_i64()) {
                        (Some(_), _) => "disabled".to_string(),
                        (None, Some(n)) if n > 0 => format!("failing ({n})"),
                        _ => "ok".to_string(),
                    };
                    let kinds = kinds.filter(|k| !k.is_empty()).unwrap_or_else(|| "all events".into());
                    println!("{}  {}  [{kinds}]  {state}", s(w, "id"), s(w, "url"));
                }
            }
        }
        Cmd::Webhook(WebhookCmd::Add { target, kinds }) => {
            let v = c.post("webhooks", json!({ "url": target, "event_kinds": kinds })).await?;
            if json {
                print_json(&v);
            } else {
                println!("webhook {}", s(&v["webhook"], "id"));
                println!("signing secret (shown once): {}", s(&v, "secret"));
            }
        }
        Cmd::Webhook(WebhookCmd::Rm { id }) => {
            c.delete(&format!("webhooks/{id}")).await?;
            if !json {
                println!("deleted");
            }
        }
        Cmd::Webhook(WebhookCmd::Log { id, limit }) => {
            let v = c.get(&format!("webhooks/{id}/deliveries"), &[("limit", limit.to_string())]).await?;
            if json {
                print_json(&v);
            } else {
                for d in v.as_array().into_iter().flatten() {
                    let outcome = match (d["status_code"].as_i64(), d["error"].as_str()) {
                        (Some(code), None) => code.to_string(),
                        (code, Some(err)) => {
                            format!("failed {} {err}", code.map(|c| c.to_string()).unwrap_or_default())
                        }
                        _ => "?".into(),
                    };
                    println!("{}  event #{}  {outcome}", short_time(s(d, "at")), d["event_id"]);
                }
            }
        }
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
    if let Some(day) = t["scheduled_on"].as_str() {
        meta.push(format!("scheduled {day}"));
    }
    if let Some(due) = t["due_at"].as_str() {
        meta.push(format!("due {}", short_time(due)));
    }
    println!("{}", meta.join(" · "));
    if v["claim"].is_object() {
        println!("claimed by {} {}", name(s(&v["claim"], "holder_id")), output::lease(&v["claim"]));
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
        ProjectCmd::Edit { project, name, brief, brief_file, review, reviewers, archive, unarchive } => {
            let mut body = json!({});
            if let Some(r) = reviewers {
                body["reviewers"] = json!(r.into_iter().filter(|n| !n.trim().is_empty()).collect::<Vec<_>>());
            }
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

/// Newline-delimited JSON-RPC on stdio, one HTTP POST per message. Only JSON-RPC
/// goes to stdout; everything else goes to stderr.
async fn mcp_bridge(c: &Client) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    let mut protocol: Option<String> = None;
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("trailhead mcp: ignoring unparseable message: {e}");
                continue;
            }
        };
        let replies = match c.mcp_post(&line, protocol.as_deref()).await {
            Ok(r) => r,
            Err(e) if msg.get("id").is_some() => {
                let err = json!({ "jsonrpc": "2.0", "id": msg["id"], "error": { "code": -32603, "message": format!("{e}") } });
                vec![err.to_string()]
            }
            Err(e) => {
                eprintln!("trailhead mcp: {e}");
                continue;
            }
        };
        for reply in replies {
            if msg["method"] == "initialize"
                && let Some(v) = serde_json::from_str::<Value>(&reply)
                    .ok()
                    .and_then(|r| r["result"]["protocolVersion"].as_str().map(String::from))
            {
                protocol = Some(v);
            }
            stdout.write_all(reply.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
        }
        stdout.flush().await?;
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
        creds: paths.creds(),
        user_name: std::env::var("USER").unwrap_or_else(|_| "me".into()),
        pid_file: Some(paths.pid_file()),
    };
    trailhead_server::serve_local(cfg).await.map_err(|e| anyhow::anyhow!("{e}"))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Cli, Cmd, WebhookCmd, parse_day_from, parse_when_in};

    #[test]
    fn webhook_url_does_not_clobber_the_server_url() {
        let cli = Cli::try_parse_from(["trailhead", "--url", "http://127.0.0.1:1", "webhook", "add", "http://hook/x"])
            .unwrap();
        assert_eq!(cli.url, "http://127.0.0.1:1");
        assert!(matches!(cli.cmd, Cmd::Webhook(WebhookCmd::Add { ref target, .. }) if target == "http://hook/x"));
    }

    #[test]
    fn dates_end_at_local_midnight_with_that_days_offset() {
        let la = jiff::tz::TimeZone::get("America/Los_Angeles").unwrap();
        // DST ends 2026-11-01 in the US.
        assert_eq!(parse_when_in("2026-10-31", &la).unwrap(), "2026-11-01T06:59:59Z");
        assert_eq!(parse_when_in("2026-11-01", &la).unwrap(), "2026-11-02T07:59:59Z");
        assert_eq!(parse_when_in("2026-11-01T12:00:00Z", &la).unwrap(), "2026-11-01T12:00:00Z");
        assert!(parse_when_in("next tuesday", &la).is_err());
    }

    #[test]
    fn scheduled_days_are_plain_dates() {
        let today = jiff::civil::date(2026, 10, 31);
        assert_eq!(parse_day_from("today", today).unwrap(), "2026-10-31");
        assert_eq!(parse_day_from("tomorrow", today).unwrap(), "2026-11-01");
        assert_eq!(parse_day_from("2026-12-01", today).unwrap(), "2026-12-01");
        assert!(parse_day_from("2026-12-01T00:00:00Z", today).is_err());
    }
}
