//! The factory daemon (`docs/factory/agents.md`): it runs in every sandbox, prepares the checkout, hosts the agent
//! harness, runs checks, publishes and opens the PR, and reports back to the control plane over iroh.
//!
//! [`Runner`] holds the daemon's duties and has no network code, so the testkit runs it in-process. The `net`
//! feature adds the iroh transport: [`net::DaemonProtocol`] in the sandbox and [`net::RemoteDaemon`] in the control
//! plane.

mod runner;
mod workspace;

#[cfg(feature = "net")]
pub mod awake;
#[cfg(feature = "net")]
pub mod gateway;
#[cfg(feature = "net")]
pub mod net;
#[cfg(feature = "net")]
mod rest;

#[cfg(feature = "net")]
pub use rest::RestTracker;

pub use runner::{Connect, HEARTBEAT_EVERY, RENEW_EVERY, Runner, Workspace};
pub use workspace::GitWorkspace;
