//! The factory's keys and who may reach its UI: the endpoint's secret key and the daemon contract's master key, kept
//! in the credential store, and the allowlist of endpoint ids `trailhead factory allow` edits.

use iroh::SecretKey;
use serde::{Deserialize, Serialize};
use trailhead_client::creds::{Cred, CredStore};
use trailhead_client::paths::Paths;
use trailhead_wire::ui::UiAllowlist;

use crate::{Error, Result};

/// Where the control plane's iroh key and daemon master key are kept.
pub(crate) const KEYS: Cred<'static> = Cred::Secret("factory-keys");

#[derive(Serialize, Deserialize)]
pub(crate) struct Keys {
    endpoint: String,
    master: String,
}

/// The control plane's endpoint id, if it has run here and saved its keys. Read-only, for `status`.
pub(crate) fn saved_endpoint_id(creds: &CredStore) -> Option<iroh::EndpointId> {
    let (saved, _) = creds.read(KEYS)?;
    let keys: Keys = serde_json::from_str(&saved).ok()?;
    Some(trailhead_wire::parse_secret_key(&keys.endpoint).ok()?.public())
}

pub(crate) fn load_or_create_keys(creds: &CredStore) -> Result<(SecretKey, [u8; 32])> {
    if let Some((saved, _)) = creds.read(KEYS) {
        let keys: Keys = serde_json::from_str(&saved).map_err(Error::BadKeys)?;
        let endpoint = trailhead_wire::parse_secret_key(&keys.endpoint).map_err(Error::backend)?;
        let master = hex::decode(&keys.master).ok().and_then(|b| b.try_into().ok()).ok_or(Error::BadMasterKey)?;
        return Ok((endpoint, master));
    }
    let (endpoint, master) = (SecretKey::generate(), SecretKey::generate().to_bytes());
    let keys = Keys { endpoint: hex::encode(endpoint.to_bytes()), master: hex::encode(master) };
    creds.save(KEYS, &serde_json::to_string(&keys).expect("keys serialize")).map_err(Error::backend)?;
    tracing::info!("created the factory's keys; losing them strands its running sandboxes");
    Ok((endpoint, master))
}

pub(crate) fn ui_allowlist(paths: &Paths) -> UiAllowlist {
    UiAllowlist::new(paths.data_dir.join("factory").join("ui-allowed"))
}

pub(crate) fn allowlist() -> Result<UiAllowlist> {
    let paths = Paths::from_env().ok_or(Error::NoDirs)?;
    Ok(ui_allowlist(&paths))
}

/// `allow` (`add`) or `disallow` a UI endpoint id.
pub fn allow(endpoint_id: &str, add: bool) -> Result<()> {
    let id = trailhead_wire::parse_endpoint_id(endpoint_id).map_err(Error::backend)?;
    let list = allowlist()?;
    let changed = if add { list.add(id) } else { list.remove(&id) }.map_err(Error::backend)?;
    match (add, changed) {
        (true, true) => println!("allowed {id}"),
        (true, false) => println!("{id} was already allowed"),
        (false, true) => println!("no longer allowed: {id}"),
        (false, false) => println!("{id} wasn't allowed"),
    }
    Ok(())
}

/// Prints the UI endpoint ids allowed to connect.
pub fn allowed() -> Result<()> {
    let ids = allowlist()?.list().map_err(Error::backend)?;
    if ids.is_empty() {
        println!("no UI endpoints are allowed; add one with `trailhead factory allow <endpoint-id>`");
    }
    ids.iter().for_each(|id| println!("{id}"));
    Ok(())
}
