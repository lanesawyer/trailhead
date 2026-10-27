use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::Result;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SandboxId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CheckpointId(pub String);

pub type Labels = BTreeMap<String, String>;

/// Label keys every factory sandbox carries; the reconciler lists by them.
pub mod label {
    pub const FACTORY: &str = "factory";
    pub const REPO: &str = "repo";
    pub const TASK: &str = "task";
    /// `worker` or `reviewer`. Connector access is granted by role, so removing
    /// it is the kill switch.
    pub const ROLE: &str = "role";
}

/// Domains a sandbox may reach. Everything else is denied.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Egress {
    pub domains: Vec<String>,
}

impl Egress {
    pub fn none() -> Egress {
        Egress::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxSpec {
    pub name: String,
    pub labels: Labels,
    pub egress: Egress,
    pub base: Option<CheckpointId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sandbox {
    pub id: SandboxId,
    pub name: String,
    pub labels: Labels,
}

#[async_trait]
pub trait Compute: Send + Sync {
    /// Creates a sandbox, or returns the existing one with the same name.
    async fn create(&self, spec: &SandboxSpec) -> Result<Sandbox>;
    /// Sandboxes carrying every given label.
    async fn list(&self, labels: &Labels) -> Result<Vec<Sandbox>>;
    async fn checkpoint(&self, id: &SandboxId, note: &str) -> Result<CheckpointId>;
    async fn restore(&self, id: &SandboxId, cp: &CheckpointId) -> Result<()>;
    async fn set_egress(&self, id: &SandboxId, egress: &Egress) -> Result<()>;
    async fn set_labels(&self, id: &SandboxId, labels: &Labels) -> Result<()>;
    async fn destroy(&self, id: &SandboxId) -> Result<()>;
}
