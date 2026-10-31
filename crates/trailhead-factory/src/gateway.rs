//! The gateway's control protocol (`docs/factory/sandbox.md#the-gateway-credentials-and-egress-for-docker-sandboxes`).
//! `DockerCompute` sends one [`GatewayRequest`] per line of JSON to the gateway's control socket and reads one
//! [`GatewayResponse`] line back.

use std::collections::BTreeMap;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::{Egress, Labels};

/// An update to the gateway. Credentials (TRAIL-175) arrive as further variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GatewayRequest {
    /// Adds a sandbox, or updates the address and labels of one already registered (its egress stays).
    /// A new sandbox starts with no egress.
    Register {
        name: String,
        /// The gateway's own address on the sandbox's network: requests that arrive on it came from this sandbox.
        address: IpAddr,
        #[serde(default)]
        labels: Labels,
    },
    /// Forgets a sandbox and closes its open tunnels.
    Unregister { name: String },
    /// Replaces a sandbox's allowlist. Returns once new requests see it and tunnels it no longer allows are closed.
    SetEgress { name: String, egress: Egress },
    /// Asks for everything the gateway knows.
    State,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GatewayResponse {
    Ok,
    State(GatewayState),
    Error { message: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayState {
    pub sandboxes: Vec<GatewaySandbox>,
    /// Requests refused because they arrived on a network with no sandbox, by the local address they arrived on.
    #[serde(default)]
    pub unregistered_refusals: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewaySandbox {
    pub name: String,
    pub address: IpAddr,
    pub labels: Labels,
    pub egress: Egress,
    /// Refused requests by host, since the sandbox was registered.
    pub refused: BTreeMap<String, u64>,
    pub open_tunnels: usize,
}
