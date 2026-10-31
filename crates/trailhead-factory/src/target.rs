//! What differs between computes for the daemon and the model: where the daemon lives in a sandbox, where its
//! connectors are listed, and where model calls go. Each compute crate has one function returning its `Target`, so
//! neutral code holds none of these facts. `docs/factory/targets.md` extends it with a name, priority and limits.

/// One compute's launch facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
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

impl Target {
    /// The configured model endpoint unless the compute brings its own.
    pub fn model_endpoint<'a>(&'a self, configured: &'a str) -> &'a str {
        self.model_endpoint.as_deref().unwrap_or(configured)
    }
}
