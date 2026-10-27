mod cli;
mod client;
mod config;
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

fn local_today() -> jiff::civil::Date {
    jiff::Zoned::now().with_time_zone(output::local_tz().clone()).date()
}

/// Fields for `--on` (`scheduled_*`) or `--due` (`due_*`): a day, a day and a time,
/// a time today, or an RFC 3339 timestamp, in the local zone.
fn parse_day_time(field: Field, words: &[String]) -> Result<Value> {
    parse_day_time_in(field, &words.join(" "), local_today(), output::local_tz())
}

#[derive(Clone, Copy)]
enum Field {
    Scheduled,
    Due,
}

fn parse_day_time_in(field: Field, s: &str, today: jiff::civil::Date, tz: &jiff::tz::TimeZone) -> Result<Value> {
    let f = match field {
        Field::Scheduled => "scheduled",
        Field::Due => "due",
    };
    let at = |at: jiff::Zoned| {
        json!({
            format!("{f}_on"): at.date().to_string(),
            format!("{f}_at"): at.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string(),
            format!("{f}_tz"): tz.iana_name(),
        })
    };
    if let Ok(ts) = s.trim().parse::<jiff::Timestamp>() {
        return Ok(at(ts.to_zoned(tz.clone())));
    }
    let lower = s.to_lowercase();
    let mut words: Vec<&str> = lower.split_whitespace().filter(|w| *w != "at").collect();
    let day = match words.first().map(|w| parse_day(w, today)) {
        Some(Ok(day)) => {
            words.remove(0);
            day
        }
        _ => today,
    };
    if !words.is_empty() {
        return Ok(at(day.to_datetime(parse_clock(&words.concat())?).to_zoned(tz.clone())?));
    }
    let mut out = json!({ format!("{f}_on"): day.to_string() });
    // A day-only deadline ends at midnight in a zone; a scheduled day has no instant.
    if let (Field::Due, Some(name)) = (field, tz.iana_name()) {
        out["due_tz"] = json!(name);
    }
    Ok(out)
}

fn merge(body: &mut Value, fields: Value) {
    if let (Some(body), Value::Object(fields)) = (body.as_object_mut(), fields) {
        body.extend(fields);
    }
}

/// `YYYY-MM-DD`, `today`, `tomorrow`, or a weekday (`fri`, `friday`): the next one,
/// counting today.
fn parse_day(s: &str, today: jiff::civil::Date) -> Result<jiff::civil::Date> {
    use jiff::civil::Weekday::*;
    let weekday = match s {
        "mon" | "monday" => Some(Monday),
        "tue" | "tues" | "tuesday" => Some(Tuesday),
        "wed" | "wednesday" => Some(Wednesday),
        "thu" | "thur" | "thurs" | "thursday" => Some(Thursday),
        "fri" | "friday" => Some(Friday),
        "sat" | "saturday" => Some(Saturday),
        "sun" | "sunday" => Some(Sunday),
        _ => None,
    };
    if let Some(w) = weekday {
        return Ok(today.checked_add(jiff::Span::new().days(today.weekday().until(w)))?);
    }
    match s {
        "today" => Ok(today),
        "tomorrow" => Ok(today.tomorrow()?),
        _ => s.parse().with_context(|| format!("{s:?} isn't YYYY-MM-DD, today, tomorrow or a weekday")),
    }
}

/// `1pm`, `1:30pm`, `13:00` or `noon`.
fn parse_clock(s: &str) -> Result<jiff::civil::Time> {
    let bad = || anyhow::anyhow!("{s:?} isn't a day or a time like 1pm, 1:30pm or 13:00");
    if s == "noon" {
        return Ok(jiff::civil::time(12, 0, 0, 0));
    }
    let (clock, pm) = match (s.strip_suffix("am"), s.strip_suffix("pm")) {
        (Some(c), _) => (c, Some(false)),
        (_, Some(c)) => (c, Some(true)),
        _ => (s, None),
    };
    let (h, m) = clock.split_once(':').unwrap_or((clock, "0"));
    let mut hour: i8 = h.parse().map_err(|_| bad())?;
    let minute: i8 = m.parse().map_err(|_| bad())?;
    if let Some(pm) = pm {
        if !(1..=12).contains(&hour) {
            return Err(bad());
        }
        hour = hour % 12 + if pm { 12 } else { 0 };
    }
    jiff::civil::Time::new(hour, minute, 0, 0).map_err(|_| bad())
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
    if let Cmd::Serve { bind } = &cli.cmd {
        return serve(bind).await;
    }
    let paths = trailhead_server::Paths::from_env().context("no home directory")?;
    let url = match cli.url.clone() {
        Some(url) => url,
        None => config::load(&paths)?.url.unwrap_or_else(|| config::DEFAULT_URL.into()),
    };
    match &cli.cmd {
        Cmd::Server(cmd) => return server(&url, cmd, cli.json).await,
        Cmd::Login => return login(&paths, &url, cli.json).await,
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
    let c = Client::connect(&url, cli.token.clone(), agent.as_ref()).await?;
    let json = cli.json;
    let show = |v: Value, line: &dyn Fn(&Value) -> String| {
        if json { print_json(&v) } else { println!("{}", line(&v)) }
    };

    match cli.cmd {
        Cmd::Serve { .. } | Cmd::Server(_) | Cmd::Login => unreachable!(),
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
                ("scheduled_through", today.then(|| local_today().to_string())),
                ("sort", today.then(|| "scheduled".to_string())),
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
            let mut body = json!({
                "project": project,
                "title": title,
                "description": description.unwrap_or_default(),
                "priority": priority,
                "parent": parent,
                "blocked_by": after,
                "blocks": blocks,
            });
            if let Some(due) = due {
                merge(&mut body, parse_day_time(Field::Due, &due)?);
            }
            if let Some(on) = on {
                merge(&mut body, parse_day_time(Field::Scheduled, &on)?);
            }
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
                if d == ["none"] {
                    body["due_on"] = Value::Null;
                } else {
                    merge(&mut body, parse_day_time(Field::Due, &d)?);
                }
            }
            if let Some(d) = on {
                if d == ["none"] {
                    body["scheduled_on"] = Value::Null;
                } else {
                    merge(&mut body, parse_day_time(Field::Scheduled, &d)?);
                }
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
        Cmd::Verdict { task, verdict, body, model } => {
            let verdict = verdict.replace('-', "_");
            let body = json!({ "kind": "review", "body": body, "verdict": verdict, "model": model });
            let v = c.post(&format!("tasks/{task}/comments"), body).await?;
            if json { print_json(&v) } else { println!("review {} {}", s(&v, "verdict"), s(&v, "id")) }
        }
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
        Cmd::Agent(AgentCmd::Add { name, projects, rate_limit, print_token }) => {
            let body = json!({ "name": name, "projects": projects, "rate_limit_per_minute": rate_limit });
            let v = c.post("agents", body).await?;
            if print_token {
                println!("{}", s(&v, "secret"));
                return Ok(());
            }
            // The server names sub-agents `<parent>/<name>`; save under the full name.
            let name = s(&v["agent"], "name").to_string();
            let safe_name = name.split('/').all(|seg| {
                !seg.is_empty() && seg.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
            });
            let saved =
                if c.local && safe_name { Some(c.creds.save(Cred::Agent(&name), s(&v, "secret"))?) } else { None };
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
                    let expires = match t["expires_at"].as_str() {
                        Some(at) => format!("  expires={}", short_time(at)),
                        None => String::new(),
                    };
                    println!(
                        "{}  {} {}  {}  projects={scope}  rate={limit}{expires}{}",
                        s(t, "id"),
                        s(&t["principal"], "kind"),
                        s(&t["principal"], "id"),
                        s(t, "name"),
                        revoked
                    );
                }
            }
        }
        Cmd::Token(TokenCmd::Add { name, agent, projects, rate_limit, expires }) => {
            let body = json!({
                "name": name,
                "agent": agent,
                "projects": projects,
                "rate_limit_per_minute": rate_limit,
                "expires_in_seconds": expires,
            });
            show(c.post("tokens", body).await?, &|v| s(v, "secret").to_string());
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
    if let Some(w) = output::when(t, "scheduled", short_time) {
        meta.push(format!("scheduled {w}"));
    }
    if let Some(w) = output::when(t, "due", short_time) {
        meta.push(format!("due {w}"));
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

/// Reads a token from stdin, checks it against the server and saves it for the
/// agent (or user) it belongs to, in that server's credential store.
async fn login(paths: &trailhead_server::Paths, url: &str, json: bool) -> Result<()> {
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        eprint!("token: ");
    }
    let mut secret = String::new();
    std::io::stdin().read_line(&mut secret)?;
    let secret = secret.trim();
    if secret.is_empty() {
        bail!("no token on stdin");
    }
    let c = Client::connect(url, Some(secret.to_string()), None).await?;
    let me = c.get("me", &[]).await?;
    let agent = me["agent"]["name"].as_str();
    let saved = match agent {
        Some(name) => c.creds.save(Cred::Agent(name), secret)?,
        None => c.creds.save(Cred::User, secret)?,
    };
    // Logging in to another machine's server makes it this machine's default.
    let mut cfg = config::load(paths)?;
    let new_default = !c.ours && cfg.url.as_deref() != Some(url);
    if new_default {
        cfg.url = Some(url.to_string());
        config::save(paths, &cfg)?;
    }
    if json {
        print_json(&me);
        return Ok(());
    }
    let to = match saved {
        Saved::Keyring => "the OS keyring".to_string(),
        Saved::File(path) => path.display().to_string(),
    };
    match agent {
        Some(name) => println!("logged in as agent {name}, token saved to {to}; use `trailhead --as {name}`"),
        None => println!("logged in as {}, token saved to {to}", s(&me["user"], "name")),
    }
    if new_default {
        println!("{url} is now this machine's default server (in {})", config::path(paths).display());
    }
    Ok(())
}

async fn server(url: &str, cmd: &ServerCmd, json: bool) -> Result<()> {
    let (http, base) = client::http_and_base(url)?;
    let local = client::is_local(&base);
    if !local && !matches!(cmd, ServerCmd::Status) {
        bail!("{url} isn't a local server; only `trailhead server status` works against it");
    }
    let paths = trailhead_server::Paths::from_env().context("no home directory")?;
    let token = client::user_token(&paths);
    let health = || client::health(&http, &base, token.as_deref());
    match cmd {
        ServerCmd::Status => match health().await {
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
                if local && !client::is_ours(&paths, &info) {
                    println!("served from another data directory or machine (a tunnel?), not this CLI's");
                } else if local && client::is_stale(&info) {
                    println!("the binary has been rebuilt since; the next command restarts the server");
                }
            }
            None if json => print_json(&json!({ "ok": false })),
            None => println!("not running"),
        },
        ServerCmd::Start => match health().await {
            Some(info) if !client::is_ours(&paths, &info) => {
                bail!("a server from another data directory or machine (a tunnel?) is answering at {url}")
            }
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
            let pid = health().await.and_then(|i| i.pid).map(|p| p.to_string()).unwrap_or_default();
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
        instance: Some(paths.instance_or_create()?),
    };
    trailhead_server::serve_local(cfg).await.map_err(|e| anyhow::anyhow!("{e}"))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use serde_json::json;

    use super::{Cli, Cmd, Field, WebhookCmd, parse_day_time_in, parse_when_in};

    #[test]
    fn webhook_url_does_not_clobber_the_server_url() {
        let cli = Cli::try_parse_from(["trailhead", "--url", "http://127.0.0.1:1", "webhook", "add", "http://hook/x"])
            .unwrap();
        assert_eq!(cli.url.as_deref(), Some("http://127.0.0.1:1"));
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
    fn schedules_are_a_day_with_an_optional_local_time() {
        let la = jiff::tz::TimeZone::get("America/Los_Angeles").unwrap();
        let today = jiff::civil::date(2026, 10, 31);
        let on = |s| parse_day_time_in(Field::Scheduled, s, today, &la);
        assert_eq!(on("today").unwrap(), json!({"scheduled_on": "2026-10-31"}));
        assert_eq!(on("tomorrow").unwrap(), json!({"scheduled_on": "2026-11-01"}));
        assert_eq!(on("2026-12-01").unwrap(), json!({"scheduled_on": "2026-12-01"}));
        let at = |day: &str, at: &str| json!({"scheduled_on": day, "scheduled_at": at, "scheduled_tz": "America/Los_Angeles"});
        assert_eq!(on("today at 1pm").unwrap(), at("2026-10-31", "2026-10-31T13:00:00-07:00"));
        assert_eq!(on("1:30 PM").unwrap(), at("2026-10-31", "2026-10-31T13:30:00-07:00"));
        // DST ends 2026-11-01 in the US, so tomorrow's offset differs.
        assert_eq!(on("tomorrow 9am").unwrap(), at("2026-11-01", "2026-11-01T09:00:00-08:00"));
        assert_eq!(on("2026-12-01 13:00").unwrap(), at("2026-12-01", "2026-12-01T13:00:00-08:00"));
        assert_eq!(on("12am").unwrap(), at("2026-10-31", "2026-10-31T00:00:00-07:00"));
        assert_eq!(on("noon").unwrap(), at("2026-10-31", "2026-10-31T12:00:00-07:00"));
        assert!(on("13pm").is_err());
        assert!(on("next tuesday").is_err());
    }

    #[test]
    fn deadlines_take_the_same_input_and_keep_a_zone_for_day_only() {
        let la = jiff::tz::TimeZone::get("America/Los_Angeles").unwrap();
        // A Saturday.
        let today = jiff::civil::date(2026, 10, 31);
        let due = |s| parse_day_time_in(Field::Due, s, today, &la).unwrap();
        let day = |d: &str| json!({"due_on": d, "due_tz": "America/Los_Angeles"});
        assert_eq!(due("fri"), day("2026-11-06"));
        assert_eq!(due("saturday"), day("2026-10-31"));
        assert_eq!(due("2026-12-01"), day("2026-12-01"));
        assert_eq!(
            due("friday 5pm"),
            json!({"due_on": "2026-11-06", "due_at": "2026-11-06T17:00:00-08:00", "due_tz": "America/Los_Angeles"})
        );
        assert_eq!(
            due("2026-11-01T12:00:00Z"),
            json!({"due_on": "2026-11-01", "due_at": "2026-11-01T04:00:00-08:00", "due_tz": "America/Los_Angeles"})
        );
    }
}
