//! `trailhead connector <cmd>`: the connectors the server knows, turning one on and off, its settings, running it now,
//! and giving it its secrets on the server's machine. Every one acts as the person: only a person may.

use std::io::{BufRead, IsTerminal};

use anyhow::{Result, bail};
use serde_json::{Map, Value};
use trailhead_client::creds::{Cred, CredStore, Saved};
use trailhead_core::connector::{ConnectorInfo, ConnectorPatch, ConnectorRun, secret_cred};

use super::Ctx;
use crate::cli::ConnectorCmd;
use crate::client::api_err;

pub(crate) async fn connector(cx: &Ctx, cmd: ConnectorCmd) -> Result<()> {
    let api = cx.c.api();
    match cmd {
        ConnectorCmd::Ls => {
            let all = api.list_connectors().await.map_err(api_err)?;
            cx.emit(&all, |all| match all.as_slice() {
                [] => "no connectors: the apps repo's last build holds none".to_string(),
                all => all.iter().map(line).collect::<Vec<_>>().join("\n"),
            });
        }
        ConnectorCmd::Show { id } => {
            let info = api.show_connector(&id).await.map_err(api_err)?;
            cx.emit(&info, detail);
        }
        ConnectorCmd::Enable { id } => patch(cx, &id, ConnectorPatch { enabled: Some(true), settings: None }).await?,
        ConnectorCmd::Disable { id } => patch(cx, &id, ConnectorPatch { enabled: Some(false), settings: None }).await?,
        ConnectorCmd::Set { id, setting, value, unset } => {
            let info = api.show_connector(&id).await.map_err(api_err)?;
            let Some(spec) = info.settings.iter().find(|s| s.name == setting) else {
                let known: Vec<&str> = info.settings.iter().map(|s| s.name.as_str()).collect();
                bail!(
                    "{id} has no setting `{setting}`; it has: {}",
                    if known.is_empty() { "none".into() } else { known.join(", ") }
                );
            };
            let value = if unset {
                Value::Null
            } else {
                spec.kind.from_words(&value).map_err(|e| anyhow::anyhow!("`{setting}` is a {}: {e}", spec.kind))?
            };
            let settings = Map::from_iter([(setting, value)]);
            patch(cx, &id, ConnectorPatch { enabled: None, settings: Some(settings) }).await?;
        }
        ConnectorCmd::Run { id } => {
            let info = api.run_connector(&id).await.map_err(api_err)?;
            cx.emit(&info, |i| format!("{} is running; `trailhead connector show {}` shows how it went", i.id, i.id));
        }
        ConnectorCmd::Login { id, secret } => {
            let creds = server_creds(cx)?;
            let names = secrets(cx, &id, secret).await?;
            let stdin = std::io::stdin();
            let interactive = stdin.is_terminal();
            let mut input = stdin.lock();
            for name in names {
                let value = read_secret(&format!("{id}'s {name}"), &mut input, interactive)?;
                let at = match creds.save(Cred::Secret(&secret_cred(&id, &name)), &value)? {
                    Saved::Keyring => "the OS keyring".to_string(),
                    Saved::File(path) => path.display().to_string(),
                };
                println!("saved {id}'s {name} in {at}");
            }
        }
        ConnectorCmd::Logout { id, secret } => {
            let creds = server_creds(cx)?;
            for name in secrets(cx, &id, secret).await? {
                let removed = creds.remove(Cred::Secret(&secret_cred(&id, &name)))?;
                println!(
                    "{}",
                    if removed { format!("removed {id}'s {name}") } else { format!("{id} had no {name} saved") }
                );
            }
        }
    }
    Ok(())
}

async fn patch(cx: &Ctx, id: &str, patch: ConnectorPatch) -> Result<()> {
    let info = cx.c.api().update_connector(id, patch).await.map_err(api_err)?;
    cx.emit(&info, line);
    Ok(())
}

/// Where the server keeps secrets: only this machine's own server, since a secret is never sent over the API.
fn server_creds(cx: &Ctx) -> Result<&CredStore> {
    if !cx.c.ours {
        bail!("a connector's secrets are given on the server's own machine: run this there");
    }
    Ok(&cx.c.creds)
}

/// The secrets to ask for: the one named, which the connector must declare, or all it declares.
async fn secrets(cx: &Ctx, id: &str, secret: Option<String>) -> Result<Vec<String>> {
    let info = cx.c.api().show_connector(id).await.map_err(api_err)?;
    let declared: Vec<String> = info.secrets.into_iter().map(|s| s.name).collect();
    match secret {
        Some(name) if declared.contains(&name) => Ok(vec![name]),
        Some(name) => bail!("{id} declares no secret `{name}`; it declares: {}", list_or_none(&declared)),
        None if declared.is_empty() => bail!("{id} declares no secrets"),
        None => Ok(declared),
    }
}

fn list_or_none(names: &[String]) -> String {
    if names.is_empty() { "none".into() } else { names.join(", ") }
}

/// The first line of `input`, trimmed; prompts on stderr first, with echo off, when stdin is a terminal.
fn read_secret(prompt: &str, input: &mut impl BufRead, interactive: bool) -> Result<String> {
    if interactive {
        eprint!("{prompt}: ");
    }
    let mut line = String::new();
    {
        let _quiet = interactive.then(trailhead_client::echo::NoEcho::new);
        input.read_line(&mut line)?;
    }
    if interactive {
        eprintln!();
    }
    let secret = line.trim().to_string();
    if secret.is_empty() {
        bail!("no value given for {prompt}");
    }
    Ok(secret)
}

fn run_words(run: &ConnectorRun) -> String {
    let when = run.started_at.strftime("%Y-%m-%d %H:%M");
    match &run.error {
        None => format!("ran {when}, ok in {}s", run.duration_ms.div_ceil(1000)),
        Some(e) => format!("ran {when}, failed: {}", e.lines().last().unwrap_or_default()),
    }
}

/// One line: its id and name, on or off, its last run, and what it still needs.
pub(crate) fn line(i: &ConnectorInfo) -> String {
    let state = match (i.enabled, i.running) {
        (_, true) => "running",
        (true, false) => "on",
        (false, false) => "off",
    };
    let last = i.last_run.as_ref().map_or_else(|| "never run".to_string(), run_words);
    let needs = if i.missing.is_empty() { String::new() } else { format!("; needs {}", i.missing.join(", ")) };
    format!("{}  {}  {state}, {last}{needs}", i.id, i.name)
}

/// Everything `show` says.
pub(crate) fn detail(i: &ConnectorInfo) -> String {
    let mut out = vec![line(i)];
    if let Some(e) = &i.error {
        out.push(format!("  broken: {e}"));
        return out.join("\n");
    }
    out.push(format!("  every {}s, at most {}s a run; reaches {}", i.every_secs, i.timeout_secs, i.hosts.join(", ")));
    if let Some(next) = i.next_run {
        out.push(format!("  next run {}", next.strftime("%Y-%m-%d %H:%M:%S")));
    }
    for s in &i.settings {
        let value = s.value.as_ref().map_or_else(|| "(not set)".to_string(), Value::to_string);
        let required = if s.required { ", required" } else { "" };
        out.push(format!("  setting {} ({}{required}): {value}  {}", s.name, s.kind, s.label));
    }
    for s in &i.secrets {
        out.push(format!("  secret {}: {}", s.name, if s.set { "set" } else { "not set" }));
    }
    for f in &i.feeds {
        let into = if f.collections.is_empty() { "no app".to_string() } else { f.collections.join(", ") };
        out.push(format!("  feed {} into {into}", f.name));
    }
    if let Some(run) = &i.last_run {
        for (feed, r) in &run.feeds {
            out.push(format!(
                "  last run's {feed}: {} records; {} created, {} changed, {} removed, {} unchanged",
                r.records, r.created, r.changed, r.removed, r.unchanged
            ));
            for e in &r.errors {
                out.push(format!("    {e}"));
            }
        }
        if let Some(e) = &run.error {
            out.push(format!(
                "  last error:\n{}",
                e.lines().map(|l| format!("    {l}")).collect::<Vec<_>>().join("\n")
            ));
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_is_the_first_line_and_an_empty_one_is_refused() {
        let mut input = std::io::Cursor::new("  hunter2 \nnext\n");
        assert_eq!(read_secret("x", &mut input, false).unwrap(), "hunter2");
        assert_eq!(read_secret("x", &mut input, false).unwrap(), "next");
        assert!(read_secret("x", &mut std::io::Cursor::new("\n"), false).is_err());
    }

    #[test]
    fn a_line_says_whether_it_is_on_its_last_run_and_what_it_needs() {
        let mut info = ConnectorInfo { id: "github".into(), name: "GitHub".into(), ..Default::default() };
        assert_eq!(line(&info), "github  GitHub  off, never run");
        info.enabled = true;
        info.missing = vec!["the secret `token`".into()];
        info.last_run = Some(ConnectorRun {
            started_at: "2026-11-22T10:00:00Z".parse().unwrap(),
            error: Some("first\nthe API said no".into()),
            ..Default::default()
        });
        assert_eq!(
            line(&info),
            "github  GitHub  on, ran 2026-11-22 10:00, failed: the API said no; needs the secret `token`"
        );
    }
}
