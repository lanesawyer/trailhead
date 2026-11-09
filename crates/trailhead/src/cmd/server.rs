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
                if local
                    && client::is_ours(&paths, &info)
                    && let Some(unit) = info.pid.and_then(client::server_unit)
                {
                    println!("scope {unit}");
                    if client::is_app_scope(&unit) {
                        println!(
                            "warning: the server shares an app's scope; killing that app would stop it (`trailhead server restart` starts it in its own)"
                        );
                    }
                }
                if let Some(runs) = info.runs_in_flight {
                    println!("factory runs in flight: {runs}");
                }
                if info.upgrade_pending == Some(true) {
                    println!(
                        "restarting onto the new binary once those finish (`trailhead server restart --now` cancels them)"
                    );
                } else if info.draining {
                    println!("draining: stops once those finish (`trailhead server stop --now` cancels them)");
                }
                if local && !client::is_ours(&paths, &info) {
                    println!("served from another data directory or machine (a tunnel?), not this CLI's");
                } else if local && client::is_stale(&info) && info.upgrade_pending != Some(true) {
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
        ServerCmd::Stop { now } => match client::stop_local(&http, &base, *now).await? {
            client::Stop::Stopped(pid) => println!("stopped (pid {pid})"),
            client::Stop::NotRunning => println!("not running"),
            client::Stop::Draining { pid, runs } => println!(
                "draining {runs} factory run(s) (pid {pid}); it stops when they finish. `trailhead server stop --now` cancels them"
            ),
        },
        ServerCmd::Restart { now } => {
            let runs = health().await.and_then(|h| h.runs_in_flight).unwrap_or(0);
            if runs > 0 && !now {
                bail!(
                    "{runs} factory run(s) in flight; `trailhead server restart --now` cancels them (they're requeued on the next start)"
                );
            }
            client::stop_local(&http, &base, true).await?;
            client::start_local_server(&http, &base).await?;
            let pid = health().await.and_then(|i| i.pid).map(|p| p.to_string()).unwrap_or_default();
            println!("restarted (pid {pid})");
        }
    }
    Ok(())
}

/// Replaces this process with the newly installed binary, same arguments, so the pid (and the scope the server runs
/// in) stays.
fn restart_onto_new_binary() -> Result<()> {
    use std::os::unix::process::CommandExt;
    let exe = client::current_exe()?;
    let err = std::process::Command::new(&exe).args(std::env::args_os().skip(1)).exec();
    bail!("restarting onto {}: {err}", exe.display())
}

/// Runs the server in this process until stopped. `factory_on_terminal` also prints the factory's log lines, which
/// otherwise only go to `factory.log` (`trailhead factory run`).
#[cfg_attr(not(feature = "factory"), allow(unused_variables))]
pub(crate) async fn serve(bind: &str, factory_on_terminal: bool, foreground: bool) -> Result<()> {
    let paths = trailhead_server::Paths::from_env().context("no home directory")?;
    #[cfg(feature = "factory")]
    trailhead_factory_server::init_server_logging(&paths, "info,tower_http=warn", factory_on_terminal);
    #[cfg(not(feature = "factory"))]
    trailhead_log::init_stdout(trailhead_log::env_filter_or("info,tower_http=warn"));
    let cfg = trailhead_server::LocalConfig {
        bind: bind.parse().with_context(|| format!("bad bind address {bind}"))?,
        db: std::env::var_os("TRAILHEAD_DB").map(PathBuf::from).unwrap_or_else(|| paths.db()),
        creds: paths.creds(),
        user_name: std::env::var("USER").unwrap_or_else(|_| "me".into()),
        pid_file: Some(paths.pid_file()),
        lock_file: Some(paths.lock_file()),
        foreground,
        instance: Some(paths.instance_or_create()?),
        factory: Some(paths.clone()),
    };
    match trailhead_server::serve_local(cfg).await.map_err(|e| anyhow::anyhow!("{e}"))? {
        trailhead_server::Exit::Stopped => Ok(()),
        trailhead_server::Exit::Restart => restart_onto_new_binary(),
    }
}
