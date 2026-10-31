//! The gateway's control protocol (`docs/factory/sandbox.md#the-gateway-credentials-and-egress-for-docker-sandboxes`).
//! `DockerCompute` sends one [`GatewayRequest`] per line of JSON to the gateway's control socket and reads one
//! [`GatewayResponse`] line back.

use std::collections::BTreeMap;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::{Egress, Labels};

/// An update to the gateway.
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
    /// Adds a connection, or replaces the one with the same provider and id. Held in memory only.
    SetConnection(GatewayConnection),
    /// Forgets a connection.
    RemoveConnection { provider: String, id: String },
    /// Asks for everything the gateway knows.
    State,
}

/// A secret that never prints: `Debug` redacts it. It still serializes, because the control socket has to carry it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Secret {
        Secret(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// How the gateway presents a connection's credential upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthScheme {
    /// `Authorization: Bearer <credential>`.
    Bearer,
    /// `x-api-key: <credential>`.
    XApiKey,
}

/// A connection: an upstream the gateway may forward to for sandboxes whose role allows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayConnection {
    /// `github`, `anthropic`, or `model` for a model server. The role policy is keyed by it.
    pub provider: String,
    pub id: String,
    /// The upstream's base URL, such as `https://api.github.com`.
    pub upstream: String,
    pub auth: AuthScheme,
    pub credential: Secret,
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
    /// The connections held, without their credentials.
    #[serde(default)]
    pub connections: Vec<GatewayConnectionInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayConnectionInfo {
    pub provider: String,
    pub id: String,
    pub upstream: String,
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
