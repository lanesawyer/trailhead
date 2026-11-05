//! The daemon contract (`docs/factory/agents.md`): the control plane's
//! [`Daemon`] trait and the messages it exchanges with the daemon in a sandbox.
//! `trailhead-daemon` carries them over iroh; the testkit runs a daemon in-process.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{
    Credential, Endpoint, Error, EventSink, Feedback, HarnessSecrets, ModelId, PrGrade, PrRef, Repo, Result, Role,
    RunEvent, SandboxId, TaskSpec, Usage, Verdict,
};

/// A named check and the shell command that runs it in the repo (`docs/factory/sandbox.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub command: String,
    /// Path globs (as in `[factory.grades]`): when non-empty, the check runs only if the checkout's changes against the
    /// base branch touch one of them. Empty means it always runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

impl Check {
    /// A check that always runs.
    pub fn new(name: impl Into<String>, command: impl Into<String>) -> Self {
        Check { name: name.into(), command: command.into(), paths: Vec::new() }
    }

    /// Whether the check applies to a change touching `files`.
    pub fn applies_to(&self, files: &[String]) -> bool {
        self.paths.is_empty() || files.iter().any(|f| self.paths.iter().any(|g| crate::grade::glob_match(g, f)))
    }
}

/// Gets a sandbox's checkout ready before any agent runs: clone or refresh the
/// repo at the base branch, then run the repo's `setup` and `trim` checks. Runs while the
/// sandbox has install-phase egress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrepareRequest {
    pub repo: Repo,
    pub base_branch: String,
    /// A branch to check out at its remote head instead of staying on the base: a rework's PR branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Merge the base branch into `branch` after setup, leaving the merge uncommitted for the agent to finish: a
    /// rework of a PR that conflicts with it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub merge_base: bool,
    pub setup: Option<Check>,
    /// The repo's `[stack]` tools (`rust = "stable"`, `node = "26"`), which the daemon installs with mise before
    /// `setup` (`docs/factory/sandbox.md`). Empty for a repo that asks for none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, String>,
    /// `[factory.egress.tools]`: download hosts per mise tool, for tools the daemon has no hosts for.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_hosts: BTreeMap<String, Vec<String>>,
    /// `[factory.stack] env`: variables for `setup` and `trim`, on top of mise's environment.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// The repo's `trim` check, run after setup to cut build output and caches down so the warm checkpoint taken
    /// next stays small. A failure is logged and the sandbox goes on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trim: Option<Check>,
    /// Log a warning when the checkout's filesystem uses more than this after setup and trim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_warn_bytes: Option<u64>,
    /// Fail the prepare, so no warm checkpoint is taken, when the checkout's filesystem uses more than this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_limit_bytes: Option<u64>,
}

/// A host the install step needs open, and the tool that needs it (`docs/factory/sandbox.md`). The daemon works these
/// out from the `[stack]`; the control plane opens each and logs who asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallHost {
    pub host: String,
    /// The mise tool that needs the host (`node`, `npm:prettier`), or `mise` for its own installer and version lists.
    pub tool: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRequest {
    /// Names the run, so a control plane that loses its connection can reattach to it (`ToDaemon::Attach`).
    pub run: String,
    pub role: Role,
    pub task: TaskSpec,
    pub repo: Repo,
    pub base_branch: String,
    /// The branch a worker publishes to; a reviewer reads the PR instead.
    pub branch: String,
    pub pr: Option<PrRef>,
    /// For a reviewer: the PR's grade, which its comment states.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade: Option<PrGrade>,
    /// For a rework: the review that asked for changes, which the worker is told.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<Feedback>,
    pub model: ModelId,
    pub endpoint: Endpoint,
    pub max_steps: u32,
    /// The checks that must pass before a worker opens a PR.
    pub required_checks: Vec<Check>,
    /// For a reviewer: the worker's check results, passed on so it needn't build or test.
    #[serde(default)]
    pub worker_checks: Vec<CheckResult>,
    /// Hosts that egress was just narrowed to exclude. Policy changes take effect a moment after the call that makes
    /// them, so the daemon waits until these stop resolving before the agent starts.
    pub blocked_hosts: Vec<String>,
    /// Hosts the agent may reach, which the daemon keeps resolving: a Sprite only lets an IP through while its own
    /// DNS has recently resolved it, and a long-lived connection whose allowance lapses gets reset.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    /// `[factory.stack] env`: variables for the agent and every check, on top of mise's environment.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// The sandbox's own Trailhead sub-agent.
    pub credential: Credential,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    pub name: String,
    /// True for a skipped check too, so "every check passed" means every check that applied did.
    pub passed: bool,
    pub output: String,
    /// The check is scoped to paths the change didn't touch, so it didn't run. Not a pass.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skipped: bool,
}

impl CheckResult {
    pub fn skipped(name: impl Into<String>) -> Self {
        CheckResult { name: name.into(), passed: true, output: String::new(), skipped: true }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunResult {
    /// A worker published its branch, opened a PR and moved the task to review.
    PrOpened(PrRef),
    /// A reviewer posted its verdict on the PR and recorded it in Trailhead.
    Reviewed(Verdict),
    /// The agent parked the task with a question.
    NeedsInput(String),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunReport {
    pub result: RunResult,
    pub checks: Vec<CheckResult>,
    pub usage: Usage,
}

/// Control plane to daemon. One iroh stream carries one request: `Prepare`, or
/// `Run` or `Attach` followed by `Cancel`s.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToDaemon {
    Prepare(PrepareRequest),
    /// Asks which hosts installing the request's tools needs (`FromDaemon::InstallHosts`). The daemon installs nothing.
    InstallHosts(PrepareRequest),
    Run(Box<RunRequest>),
    /// Resumes following a run after a dropped connection: the daemon sends the events after the first `seen`,
    /// then the rest of the run as usual. A run the daemon doesn't know ends with `Failed(NotFound)`.
    Attach {
        run: String,
        seen: u64,
    },
    Cancel,
    /// Asks which build of the daemon is answering. A daemon that predates this fails the request.
    Version,
    /// Asks whether the checkout has work that isn't on `origin/<remote_branch>` (`FromDaemon::Unpublished`).
    Unpublished {
        remote_branch: String,
    },
}

/// Which build of the daemon is running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonVersion {
    /// The daemon crate's version.
    pub version: String,
    /// SHA-256 of the daemon's executable, hex. The control plane compares it with the binary it uploads, so
    /// it catches a stale process as well as a stale protocol.
    pub build: String,
    /// SHA-256 of the secrets file the daemon started with, hex; empty from a daemon that predates it. The
    /// control plane compares it with the file it would write, since a daemon reads that file once.
    #[serde(default)]
    pub secrets: String,
}

/// Daemon to control plane: `Event`s while a run streams, then exactly one of the rest.
#[derive(Debug, Serialize, Deserialize)]
pub enum FromDaemon {
    Event(RunEvent),
    Prepared,
    InstallHosts(Vec<InstallHost>),
    Version(DaemonVersion),
    Unpublished(bool),
    Report(RunReport),
    Failed(Error),
}

/// What the control plane hands a daemon at start, as a `0600` file: its iroh
/// key, and the one peer it accepts. The file stays, because a supervised
/// restart (a Sprite's cold wake) reads it again.
#[derive(Clone, Serialize, Deserialize)]
#[serde(from = "RawSecrets")]
pub struct DaemonSecrets {
    /// The daemon's iroh secret key, hex. Derived per sandbox by the control
    /// plane, so it can dial the daemon by id without being told it.
    pub secret_key: String,
    /// The control plane's iroh endpoint id, hex.
    pub control_plane: String,
    /// The `adapter` the daemon builds its harness from (`HarnessKind`). Secrets files written before this field
    /// were all Claude Code's.
    pub harness: String,
    /// Credentials for that harness, by the harness's own keys. Subscription mode (`docs/factory/sandbox.md`) is
    /// one: the one exception to sandboxes holding no model credential.
    #[serde(skip_serializing_if = "HarnessSecrets::is_empty")]
    pub harness_secrets: HarnessSecrets,
    /// Where the daemon lists its connectors: Fly's gateway on a Sprite, ours under Docker. Defaults to Fly's, so
    /// secrets files written before this field still parse.
    pub gateway_list: String,
}

/// The wire form of [`DaemonSecrets`], which also reads the files older control planes wrote.
#[derive(Deserialize)]
struct RawSecrets {
    secret_key: String,
    control_plane: String,
    #[serde(default = "legacy_harness")]
    harness: String,
    #[serde(default)]
    harness_secrets: HarnessSecrets,
    /// Before `harness_secrets`, the one harness's token had its own field.
    #[serde(default)]
    claude_oauth_token: Option<String>,
    #[serde(default = "default_gateway_list")]
    gateway_list: String,
}

impl From<RawSecrets> for DaemonSecrets {
    fn from(raw: RawSecrets) -> DaemonSecrets {
        let mut harness_secrets = raw.harness_secrets;
        if let Some(token) = raw.claude_oauth_token {
            harness_secrets.entry(LEGACY_TOKEN_KEY.into()).or_insert(token);
        }
        DaemonSecrets {
            secret_key: raw.secret_key,
            control_plane: raw.control_plane,
            harness: raw.harness,
            harness_secrets,
            gateway_list: raw.gateway_list,
        }
    }
}

/// Compatibility only: secrets files written before `harness` existed were all Claude Code's, whose token key is
/// the one its harness reads.
const LEGACY_HARNESS: &str = "claude-code";
const LEGACY_TOKEN_KEY: &str = "oauth_token";

fn legacy_harness() -> String {
    LEGACY_HARNESS.into()
}

/// Compatibility only: secrets files written before `gateway_list` existed were all Sprites', so they take its list.
/// New files always carry the compute's own (`Target::gateway_list`).
const LEGACY_GATEWAY_LIST: &str = "https://api.sprites.dev/v1/gateway/list";

fn default_gateway_list() -> String {
    LEGACY_GATEWAY_LIST.into()
}

impl std::fmt::Debug for DaemonSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonSecrets")
            .field("control_plane", &self.control_plane)
            .field("harness", &self.harness)
            .field("harness_secrets", &self.harness_secrets.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

#[async_trait]
pub trait Daemon: Send + Sync {
    /// Starts the sandbox's daemon if needed and says which hosts `prepare` needs open to
    /// install the request's tools, each with the tool that asked. Fails if a `[stack]` tool has no known hosts. The
    /// control plane opens them before `prepare` and closes them after.
    async fn install_hosts(&self, _sandbox: &SandboxId, _req: &PrepareRequest) -> Result<Vec<InstallHost>> {
        Ok(Vec::new())
    }
    /// Starts the sandbox's daemon if needed and readies its checkout.
    async fn prepare(&self, sandbox: &SandboxId, req: PrepareRequest) -> Result<()>;
    /// Whether the sandbox's checkout holds work that exists nowhere else: uncommitted changes, or commits that are not on
    /// `origin/<remote_branch>` (the base branch, or a rework's PR branch). Defaults to true, so a daemon that can't
    /// say is never taken to have nothing to lose (TRAIL-601).
    async fn unpublished(&self, _sandbox: &SandboxId, _remote_branch: &str) -> Result<bool> {
        Ok(true)
    }
    /// Drops any connection to the sandbox's daemon, after something restarted it or is about to destroy it.
    async fn forget(&self, _sandbox: &SandboxId) {}
    /// Drops any connection to the sandbox's daemon, after a restore: its image may start a daemon by itself.
    async fn restored(&self, sandbox: &SandboxId) {
        self.forget(sandbox).await;
    }
    /// Runs one agent step in the sandbox, streaming events, until it finishes or is cancelled.
    async fn run(&self, sandbox: &SandboxId, req: RunRequest, events: EventSink) -> Result<RunReport>;
    /// Follows a run this control plane didn't start, or lost track of in a restart: the daemon replays the run's
    /// events from the first, then the rest as usual. A run the daemon doesn't know fails with `Error::NotFound`,
    /// which is also what a daemon without reattach says.
    async fn attach(&self, _sandbox: &SandboxId, run: &str, _events: EventSink) -> Result<RunReport> {
        Err(Error::NotFound(format!("run {run}: this daemon can't reattach")))
    }
    /// Stops the running agent. A cancelled `run` returns `Error::Cancelled`.
    async fn cancel(&self, sandbox: &SandboxId) -> Result<()>;
    /// Cancels every run in flight, because the control plane is stopping on purpose. Without this, a daemon would
    /// keep an unobserved run going while it waits for a reattach.
    async fn shutdown(&self) {}
}

const GITHUB: &[&str] =
    &["github.com", "api.github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"];
const PYPI: &[&str] = &["pypi.org", "files.pythonhosted.org"];
pub const MISE_VERSION_HOSTS: &[&str] = &["mise.jdx.dev", "mise-versions.jdx.dev"];

/// The hosts installing a mise tool needs, by tool or backend (`docs/factory/sandbox.md`): the one table, so what a
/// sandbox opens for an install is reviewable in one place. `None` for a tool it doesn't know, which is never guessed.
/// `tool` is a mise tool key: a name (`node`) or `backend:name` (`npm:prettier`, `core:node`).
pub fn hosts_for(tool: &str) -> Option<Vec<&'static str>> {
    let (backend, name) = match tool.split_once(':') {
        Some(("core", name)) => ("", name),
        Some((backend, _)) => (backend, ""),
        None => ("", tool),
    };
    let hosts: &[&[&str]] = match (backend, name) {
        ("npm", _) => &[&["registry.npmjs.org"]],
        ("pipx" | "pypi", _) => &[PYPI],
        ("github" | "aqua" | "ubi", _) => &[GITHUB],
        ("", "node") => &[&["nodejs.org"]],
        ("", "rust") => &[&["static.rust-lang.org"]],
        ("", "go") => &[&["go.dev", "dl.google.com"]],
        // Prebuilt interpreters are GitHub release assets; pip comes from PyPI.
        ("", "python") => &[GITHUB, PYPI],
        // aqua's registry fetches pnpm from its GitHub releases.
        ("", "pnpm") => &[GITHUB],
        _ => return None,
    };
    Some(hosts.concat())
}

pub const MISE_INSTALLER_HOSTS: &[&str] = &[
    "mise.run",
    "github.com",
    "api.github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

/// Whether the control plane accepts `need` for `req`, whatever the daemon says: a second line of defence, so a
/// compromised or confused daemon can't widen install egress to arbitrary domains. Accepted are mise's own hosts for
/// `mise`, and for a `[stack]` tool its `[factory.egress.tools]` hosts, else its row in `hosts_for`.
pub fn install_host_allowed(req: &PrepareRequest, need: &InstallHost) -> bool {
    if need.tool == "mise" {
        let host = need.host.as_str();
        return MISE_VERSION_HOSTS.contains(&host) || MISE_INSTALLER_HOSTS.contains(&host);
    }
    if !req.tools.contains_key(&need.tool) {
        return false;
    }
    match req.tool_hosts.get(&need.tool) {
        Some(hosts) => hosts.contains(&need.host),
        None => hosts_for(&need.tool).is_some_and(|hosts| hosts.contains(&need.host.as_str())),
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn request(tools: &[&str], hosts: &[(&str, &[&str])]) -> PrepareRequest {
        PrepareRequest {
            repo: Repo { owner: "o".into(), name: "r".into() },
            base_branch: "main".into(),
            branch: None,
            merge_base: false,
            setup: None,
            tools: tools.iter().map(|t| (t.to_string(), "1".to_string())).collect(),
            tool_hosts: hosts.iter().map(|(t, h)| (t.to_string(), h.iter().map(|h| h.to_string()).collect())).collect(),
            env: Default::default(),
            trim: None,
            disk_warn_bytes: None,
            disk_limit_bytes: None,
        }
    }

    fn need(host: &str, tool: &str) -> InstallHost {
        InstallHost { host: host.into(), tool: tool.into() }
    }

    #[test]
    fn install_hosts_come_only_from_the_table_the_config_or_mise() {
        let req = request(&["node", "zig"], &[("zig", &["ziglang.org"])]);
        assert!(install_host_allowed(&req, &need("nodejs.org", "node")));
        assert!(install_host_allowed(&req, &need("ziglang.org", "zig")));
        assert!(install_host_allowed(&req, &need("mise.run", "mise")));
        assert!(install_host_allowed(&req, &need("mise-versions.jdx.dev", "mise")));
        assert!(!install_host_allowed(&req, &need("evil.example", "node")));
        assert!(!install_host_allowed(&req, &need("evil.example", "mise")));
        assert!(!install_host_allowed(&req, &need("nodejs.org", "rust")), "a tool outside the stack");
        assert!(!install_host_allowed(&req, &need("registry.npmjs.org", "node")), "another tool's host");
    }

    #[test]
    fn configured_hosts_replace_the_table_row() {
        let req = request(&["node"], &[("node", &["mirror.example"])]);
        assert!(install_host_allowed(&req, &need("mirror.example", "node")));
        assert!(!install_host_allowed(&req, &need("nodejs.org", "node")));
    }

    #[test]
    fn secrets_parse_with_and_without_a_gateway_list() {
        let old: DaemonSecrets = serde_json::from_str(r#"{"secret_key":"aa","control_plane":"bb"}"#).unwrap();
        assert_eq!(old.gateway_list, LEGACY_GATEWAY_LIST);
        let new: DaemonSecrets =
            serde_json::from_str(r#"{"secret_key":"aa","control_plane":"bb","gateway_list":"http://g/list"}"#).unwrap();
        assert_eq!(new.gateway_list, "http://g/list");
    }

    #[test]
    fn secrets_files_from_before_harness_secrets_migrate() {
        let old: DaemonSecrets =
            serde_json::from_str(r#"{"secret_key":"aa","control_plane":"bb","claude_oauth_token":"tok"}"#).unwrap();
        assert_eq!(old.harness, "claude-code");
        assert_eq!(old.harness_secrets.get("oauth_token").map(String::as_str), Some("tok"));
        let json = serde_json::to_string(&old).unwrap();
        assert!(!json.contains("claude_oauth_token"), "{json}");
        let back: DaemonSecrets = serde_json::from_str(&json).unwrap();
        assert_eq!(back.harness_secrets, old.harness_secrets);
    }
}
