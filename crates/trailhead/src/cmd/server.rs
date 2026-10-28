//! The local server: running it in the foreground, and managing it in the background.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde_json::json;

use super::print_json;
use crate::cli::ServerCmd;
use crate::client;
use crate::output::short_time;

pub(crate) async fn server(url: &str, cmd: &ServerCmd, json: bool) -> Result<()> {
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
                if let Some(started) = info.started_at.as_deref().and_then(|s| s.parse::<jiff::Timestamp>().ok()) {
                    let up = jiff::Timestamp::now().duration_since(started);
                    let since = short_time(&started.to_string());
                    parts.push(format!("up {}h {}m (since {since})", up.as_hours(), up.as_mins() % 60));
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

pub(crate) async fn serve(bind: &str) -> Result<()> {
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
