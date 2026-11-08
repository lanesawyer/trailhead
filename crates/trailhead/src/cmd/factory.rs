//! `trailhead factory <cmd>`: the factory's own commands, built in with the `factory` feature, and `enable`/`disable`,
//! which edit the user config and need no server, so they work when a broken factory configuration keeps it from
//! starting.

use anyhow::{Context, Result, anyhow};

use crate::cli::FactoryCmd;
use crate::{client, config};

pub(crate) async fn run(cmd: FactoryCmd) -> Result<()> {
    match cmd {
        FactoryCmd::Control(cmd) => trailhead_factory_server::command(*cmd).await.map_err(|e| anyhow!(e)),
        FactoryCmd::Enable => set_enabled(true).await,
        FactoryCmd::Disable => set_enabled(false).await,
    }
}

/// Sets or clears `disabled` in `[factory]` of the user config. Needs no server, and touches no other factory key.
async fn set_enabled(enable: bool) -> Result<()> {
    let paths = trailhead_server::Paths::from_env().context("no home directory")?;
    let mut cfg = config::load(&paths)?;
    let table = cfg.factory.get_or_insert_with(Default::default);
    let was_enabled = !table.get("disabled").and_then(toml::Value::as_bool).unwrap_or(false);
    if enable {
        table.remove("disabled");
    } else {
        table.insert("disabled".into(), toml::Value::Boolean(true));
    }
    config::save(&paths, &cfg)?;
    let word = if enable { "enabled" } else { "disabled" };
    if was_enabled == enable {
        println!("the factory was already {word}");
        return Ok(());
    }
    println!("the factory is {word} ({})", config::path(&paths).display());
    let url = std::env::var("TRAILHEAD_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .or(cfg.url.clone())
        .unwrap_or_else(|| config::DEFAULT_URL.into());
    let (http, base) = client::http_and_base(&url)?;
    if client::is_local(&base) && client::health(&http, &base, None).await.is_some() {
        println!("the running server picks this up when it restarts: `trailhead server restart`");
    }
    Ok(())
}
