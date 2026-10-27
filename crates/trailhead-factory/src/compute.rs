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

/// Domains iroh needs under a deny-by-default policy: n0's relays (which CNAME
/// to `iroh-svc.com`) and DNS discovery. Measured in M0 (`docs/factory/sandbox.md`).
pub const IROH_DOMAINS: &[&str] = &["iroh.link", "*.iroh.link", "*.iroh-svc.com"];

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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sandbox {
    pub id: SandboxId,
    pub name: String,
    pub labels: Labels,
}

/// A file the control plane puts in a sandbox before starting a program.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxFile {
    pub path: String,
    pub contents: Vec<u8>,
    /// Unix permissions, e.g. `0o755` for a binary or `0o600` for a secret.
    pub mode: u32,
}

impl std::fmt::Debug for SandboxFile {
    // Contents may be a secret or a whole binary.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SandboxFile")
            .field("path", &self.path)
            .field("bytes", &self.contents.len())
            .field("mode", &format_args!("{:o}", self.mode))
            .finish()
    }
}

/// A long-running program in a sandbox, supervised by the sandbox host so it
/// survives the call that started it. This is how the daemon gets in: its
/// binary and a secrets file it reads and deletes are among `files`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Program {
    pub name: String,
    pub files: Vec<SandboxFile>,
    pub cmd: String,
    pub args: Vec<String>,
}

#[async_trait]
pub trait Compute: Send + Sync {
    /// Creates a sandbox, or returns the existing one with the same name.
    async fn create(&self, spec: &SandboxSpec) -> Result<Sandbox>;
    /// Sandboxes carrying every given label.
    async fn list(&self, labels: &Labels) -> Result<Vec<Sandbox>>;
    async fn checkpoint(&self, id: &SandboxId, note: &str) -> Result<CheckpointId>;
    async fn restore(&self, id: &SandboxId, cp: &CheckpointId) -> Result<()>;
    /// Writes the program's files and starts it, replacing a program of the same name.
    async fn start(&self, id: &SandboxId, program: &Program) -> Result<()>;
    async fn set_egress(&self, id: &SandboxId, egress: &Egress) -> Result<()>;
    async fn set_labels(&self, id: &SandboxId, labels: &Labels) -> Result<()>;
    async fn destroy(&self, id: &SandboxId) -> Result<()>;
}
