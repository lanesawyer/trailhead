//! One module per group of commands, like the server's routes.

pub(crate) mod access;
pub(crate) mod claims;
pub(crate) mod comments;
pub(crate) mod events;
#[cfg(feature = "factory")]
pub(crate) mod factory;
pub(crate) mod mcp;
pub(crate) mod projects;
pub(crate) mod server;
pub(crate) mod tasks;
pub(crate) mod webhooks;

use std::collections::HashMap;

use anyhow::Result;
use serde_json::Value;
use uuid::Uuid;

use crate::client::{Client, api_err};

/// What every command against a server needs.
pub(crate) struct Ctx {
    pub(crate) c: Client,
    pub(crate) json: bool,
    /// The project in `.trailhead.toml`, used when `-p` isn't given.
    pub(crate) project: Option<String>,
}

impl Ctx {
    /// A typed result: the full JSON with `--json`, else one line made from it.
    pub(crate) fn emit<T: serde::Serialize>(&self, v: &T, line: impl Fn(&T) -> String) {
        if self.json { print_json(&to_json(v)) } else { println!("{}", line(v)) }
    }
}

/// A typed value as the JSON the server sent it as.
pub(crate) fn to_json<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).expect("valid json")
}

pub(crate) fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).expect("valid json"));
}

/// Query parameters, leaving out the ones not given.
pub(crate) fn params<const N: usize>(pairs: [(&'static str, Option<String>); N]) -> Vec<(&'static str, String)> {
    pairs.into_iter().filter_map(|(k, v)| v.map(|v| (k, v))).collect()
}

/// A `--limit` for the typed client, which takes no negative counts.
pub(crate) fn count<T: TryFrom<i64>>(limit: Option<i64>) -> Result<Option<T>> {
    limit.map(|n| T::try_from(n).map_err(|_| anyhow::anyhow!("--limit must be 0 or more, not {n}"))).transpose()
}

/// Maps user and agent ids to names for display.
pub(crate) async fn names(c: &Client) -> Result<HashMap<Uuid, String>> {
    let me = c.api().me().await.map_err(api_err)?;
    let agents = c.api().list_agents().await.map_err(api_err)?;
    Ok(std::iter::once((me.user.id, me.user.name)).chain(agents.into_iter().map(|a| (a.id, a.name))).collect())
}
