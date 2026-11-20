//! Who you are: agents, tokens and logging in.

use anyhow::{Context, Result, bail};
use trailhead_client::creds::{Cred, Saved};
use trailhead_client::http::{NewAgent, NewToken, TokenUpdate};
use trailhead_core::Principal;
use uuid::Uuid;

use super::{Ctx, print_json, to_json};
use crate::cli::{AgentCmd, TokenCmd};
use crate::client::{Client, api_err};
use crate::config;
use crate::output::short_time;

pub(crate) async fn whoami(cx: &Ctx) -> Result<()> {
    let me = cx.c.api().me().await.map_err(api_err)?;
    cx.emit(&me, |m| match &m.agent {
        Some(a) => format!("agent {} acting for {}", a.name, m.user.name),
        None => format!("{} ({})", m.user.name, m.user.id),
    });
    Ok(())
}

pub(crate) async fn agent(cx: &Ctx, cmd: AgentCmd) -> Result<()> {
    let c = &cx.c;
    match cmd {
        AgentCmd::Ls => {
            let agents = c.api().list_agents().await.map_err(api_err)?;
            if cx.json {
                print_json(&to_json(&agents));
            } else {
                for a in &agents {
                    let revoked = if a.revoked_at.is_none() { "" } else { "  (revoked)" };
                    println!("{}  {}{revoked}", a.id, a.name);
                }
            }
        }
        AgentCmd::Add { name, projects, rate_limit, print_token } => {
            let new = NewAgent { name, projects, rate_limit_per_minute: rate_limit };
            let created = c.api().create_agent(&new).await.map_err(api_err)?;
            if print_token {
                println!("{}", created.secret);
                return Ok(());
            }
            // The server names sub-agents `<parent>/<name>`; save under the full name.
            let name = created.agent.name.clone();
            let safe_name = name.split('/').all(|seg| {
                !seg.is_empty() && seg.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
            });
            let saved =
                if c.local && safe_name { Some(c.creds.save(Cred::Agent(&name), &created.secret)?) } else { None };
            if cx.json {
                print_json(&to_json(&created));
            } else {
                println!("agent {} ({})", created.agent.name, created.agent.id);
                match saved {
                    Some(Saved::Keyring) => println!("token saved to the OS keyring; use `trailhead --as {name}`"),
                    Some(Saved::File(path)) => {
                        println!("token saved to {}; use `trailhead --as {name}`", path.display())
                    }
                    None => println!("token (shown once): {}", created.secret),
                }
            }
        }
        AgentCmd::Revoke { name } => {
            let agent = c.api().revoke_agent(&name).await.map_err(api_err)?;
            cx.emit(&agent, |a| format!("revoked {} and its sub-agents", a.name));
        }
    }
    Ok(())
}

pub(crate) async fn token(cx: &Ctx, cmd: TokenCmd) -> Result<()> {
    let api = cx.c.api();
    match cmd {
        TokenCmd::Ls => {
            let tokens = api.list_tokens().await.map_err(api_err)?;
            if cx.json {
                print_json(&to_json(&tokens));
                return Ok(());
            }
            let projects = api.list_projects(true).await.map_err(api_err)?;
            let key_of = |id: &Uuid| projects.iter().find(|p| p.id == *id).map_or(id.to_string(), |p| p.key.clone());
            for t in &tokens {
                let revoked = if t.revoked_at.is_none() { "" } else { "  (revoked)" };
                let scope = match &t.projects {
                    Some(ids) => ids.iter().map(key_of).collect::<Vec<_>>().join(","),
                    None => "all".into(),
                };
                let limit = match t.rate_limit_per_minute {
                    Some(n) => format!("{n}/min"),
                    None => "unlimited".into(),
                };
                let expires = match t.expires_at {
                    Some(at) => format!("  expires={}", short_time(&at.to_string())),
                    None => String::new(),
                };
                let (kind, id) = match t.principal {
                    Principal::User(id) => ("user", id),
                    Principal::Agent(id) => ("agent", id),
                };
                println!("{}  {kind} {id}  {}  projects={scope}  rate={limit}{expires}{revoked}", t.id, t.name);
            }
        }
        TokenCmd::Add { name, agent, projects, rate_limit, expires } => {
            let new =
                NewToken { name, agent, projects, rate_limit_per_minute: rate_limit, expires_in_seconds: expires };
            let created = api.create_token(&new).await.map_err(api_err)?;
            cx.emit(&created, |c| c.secret.clone());
        }
        TokenCmd::Revoke { id } => {
            let token = api.revoke_token(&id).await.map_err(api_err)?;
            cx.emit(&token, |_| "revoked".into());
        }
        TokenCmd::Edit { id, projects, rate_limit } => {
            let mut update = TokenUpdate::default();
            if let Some(p) = projects {
                let all = p.iter().all(|k| k.trim().is_empty() || k.eq_ignore_ascii_case("all"));
                update.projects = Some((!all).then_some(p));
            }
            if let Some(r) = rate_limit {
                update.rate_limit_per_minute = Some(match r.as_str() {
                    "none" => None,
                    n => Some(n.parse::<i64>().with_context(|| format!("{n:?} isn't a number or \"none\""))?),
                });
            }
            if update == TokenUpdate::default() {
                bail!("nothing to change: pass --projects and/or --rate-limit");
            }
            let token = api.update_token(&id, &update).await.map_err(api_err)?;
            cx.emit(&token, |_| "updated".into());
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
    {
        let _quiet = trailhead_client::echo::NoEcho::new();
        std::io::stdin().read_line(&mut secret)?;
    }
    let secret = secret.trim();
    if secret.is_empty() {
        bail!("no token on stdin");
    }
    let c = Client::connect(url, Some(secret.to_string()), None).await?;
    let me = c.api().me().await.map_err(api_err)?;
    // An agent token whose agent doesn't resolve must not be saved as the user's.
    if let (Some(id), None) = (me.agent_id, &me.agent) {
        bail!("this token belongs to agent {id}, but the server can't look that agent up, so it can't be saved");
    }
    let agent = me.agent.as_ref().map(|a| a.name.as_str());
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
        print_json(&to_json(&me));
        return Ok(());
    }
    let to = match saved {
        Saved::Keyring => "the OS keyring".to_string(),
        Saved::File(path) => path.display().to_string(),
    };
    match agent {
        Some(name) => println!("logged in as agent {name}, token saved to {to}; use `trailhead --as {name}`"),
        None => println!("logged in as {}, token saved to {to}", me.user.name),
    }
    if new_default {
        println!("{url} is now this machine's default server (in {})", config::path(paths).display());
    }
    Ok(())
}
