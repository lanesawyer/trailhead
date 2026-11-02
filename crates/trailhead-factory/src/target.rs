//! What differs between computes for the daemon and the model: where the daemon lives in a sandbox, where its
//! connectors are listed, and where model calls go. Each compute crate has one function returning its `Target`, so
//! neutral code holds none of these facts. A target also has a name, a priority and limits (`docs/factory/targets.md`),
//! which the factory file gives it.

/// The name of the one target a `[factory.compute]` file means, and of the one a test factory has. Its pooled
/// sandboxes keep the names they had before targets: `<repo>-w<n>`.
pub const DEFAULT_TARGET: &str = "default";

/// One compute's launch facts, and where it stands among a factory's targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// What the factory file calls it. Carried on the target's sandboxes as a label, and in their pool names.
    pub name: String,
    /// Lower is preferred: a run goes to the lowest priority number with room.
    pub priority: u32,
    /// The most sandboxes the repo holds on this target at once, workers and reviewers together. `None` leaves the
    /// repo's own cap to decide.
    pub max_sandboxes: Option<usize>,
    /// The directory in a sandbox that holds the daemon binary and its secrets file.
    pub dir: String,
    /// Where the repo is checked out in a sandbox.
    pub workdir: String,
    /// The connector list URL for each daemon's secrets.
    pub gateway_list: String,
    /// Where runs send model calls, or `None` for the configured `--model-endpoint`.
    pub model_endpoint: Option<String>,
    /// The socket a daemon holds its sandbox awake through, when the compute pauses idle sandboxes.
    pub hold_socket: Option<String>,
}

impl Default for Target {
    fn default() -> Target {
        Target {
            name: DEFAULT_TARGET.into(),
            priority: 0,
            max_sandboxes: None,
            dir: String::new(),
            workdir: String::new(),
            gateway_list: String::new(),
            model_endpoint: None,
            hold_socket: None,
        }
    }
}

impl Target {
    /// The configured model endpoint unless the compute brings its own.
    pub fn model_endpoint<'a>(&'a self, configured: &'a str) -> &'a str {
        self.model_endpoint.as_deref().unwrap_or(configured)
    }

    /// This target as the factory file lists it: the file's name, priority and limit over the compute's launch facts.
    pub fn listed(self, spec: &crate::TargetSpec) -> Target {
        Target { name: spec.name.clone(), priority: spec.priority, max_sandboxes: spec.max_sandboxes, ..self }
    }
}
