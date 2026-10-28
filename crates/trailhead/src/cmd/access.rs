//! Who you are: agents, tokens and logging in.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use trailhead_server::creds::{Cred, Saved};

use super::{Ctx, print_json};
use crate::cli::{AgentCmd, TokenCmd};
use crate::client::Client;
use crate::config;
use crate::output::{s, short_time};

pub(crate) async fn whoami(cx: &Ctx) -> Result<()> {
    let v = cx.c.get("me", &[]).await?;
    cx.show(&v, |m| match m["agent"].as_object() {
        Some(_) => format!("agent {} acting for {}", s(&m["agent"], "name"), s(&m["user"], "name")),
        None => format!("{} ({})", s(&m["user"], "name"), s(&m["user"], "id")),
    });
    Ok(())
}

pub(crate) async fn agent(cx: &Ctx, cmd: AgentCmd) -> Result<()> {
    let c = &cx.c;
    match cmd {
        AgentCmd::Ls => {
            let v = c.get("agents", &[]).await?;
            if cx.json {
                print_json(&v);
            } else {
                for a in v.as_array().into_iter().flatten() {
                    let revoked = if a["revoked_at"].is_null() { "" } else { "  (revoked)" };
                    println!("{}  {}{revoked}", s(a, "id"), s(a, "name"));
                }
            }
        }
        AgentCmd::Add { name, projects, rate_limit, print_token } => {
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
            if cx.json {
                print_json(&v);
            } else {
                println!("agent {} ({})", s(&v["agent"], "name"), s(&v["agent"], "id"));
                match saved {
                    Some(Saved::Keyring) => println!("token saved to the OS keyring; use `trailhead --as {name}`"),
                    Some(Saved::File(path)) => {
                        println!("token saved to {}; use `trailhead --as {name}`", path.display())
                    }
                    None => println!("token (shown once): {}", s(&v, "secret")),
                }
            }
        }
        AgentCmd::Revoke { name } => {
            let v = c.post(&format!("agents/{}/revoke", name.replace('/', "%2F")), json!({})).await?;
            cx.show(&v, |a| format!("revoked {} and its sub-agents", s(a, "name")));
        }
    }
    Ok(())
}

pub(crate) async fn token(cx: &Ctx, cmd: TokenCmd) -> Result<()> {
    let c = &cx.c;
    match cmd {
        TokenCmd::Ls => {
            let v = c.get("tokens", &[]).await?;
            if cx.json {
                print_json(&v);
                return Ok(());
            }
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
        TokenCmd::Add { name, agent, projects, rate_limit, expires } => {
            let body = json!({
                "name": name,
                "agent": agent,
                "projects": projects,
                "rate_limit_per_minute": rate_limit,
                "expires_in_seconds": expires,
            });
            cx.show(&c.post("tokens", body).await?, |v| s(v, "secret").to_string());
        }
        TokenCmd::Revoke { id } => cx.show(&c.delete(&format!("tokens/{id}")).await?, |_| "revoked".into()),
        TokenCmd::Edit { id, projects, rate_limit } => {
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
            cx.show(&c.patch(&format!("tokens/{id}"), body).await?, |_| "updated".into());
        }
    }
    Ok(())
}

/// Reads a token from stdin, checks it against the server and saves it for the
/// agent (or user) it belongs to, in that server's credential store.
pub(crate) async fn login(paths: &trailhead_server::Paths, url: &str, json: bool) -> Result<()> {
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
