//! One module per group of commands, like the server's routes.

pub(crate) mod access;
pub(crate) mod claims;
pub(crate) mod comments;
pub(crate) mod events;
pub(crate) mod mcp;
pub(crate) mod projects;
pub(crate) mod server;
pub(crate) mod tasks;
pub(crate) mod webhooks;

use std::collections::HashMap;

use anyhow::Result;
use serde_json::Value;

use crate::client::Client;
use crate::output::s;

/// What every command against a server needs.
pub(crate) struct Ctx {
    pub(crate) c: Client,
    pub(crate) json: bool,
    /// The project in `.trailhead.toml`, used when `-p` isn't given.
    pub(crate) project: Option<String>,
}

impl Ctx {
    /// The full JSON with `--json`, else one line made from it.
    pub(crate) fn show(&self, v: &Value, line: impl Fn(&Value) -> String) {
        if self.json { print_json(v) } else { println!("{}", line(v)) }
    }
}

pub(crate) fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).expect("valid json"));
}

/// Query parameters, leaving out the ones not given.
pub(crate) fn params<const N: usize>(pairs: [(&'static str, Option<String>); N]) -> Vec<(&'static str, String)> {
    pairs.into_iter().filter_map(|(k, v)| v.map(|v| (k, v))).collect()
}

/// Maps user and agent ids to names for display.
pub(crate) async fn names(c: &Client) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    let me = c.get("me", &[]).await?;
    map.insert(s(&me["user"], "id").to_string(), s(&me["user"], "name").to_string());
    for a in c.get("agents", &[]).await?.as_array().into_iter().flatten() {
        map.insert(s(a, "id").to_string(), s(a, "name").to_string());
    }
    Ok(map)
}
