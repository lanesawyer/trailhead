//! A repo's factory file: stack, standard checks, agent profiles and gate policy.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{Check, Error, Grader, ModelAlias, PrepareRequest, Repo, Result, Usd};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Planner,
    Worker,
    Reviewer,
    Utility,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Planner => "planner",
            Role::Worker => "worker",
            Role::Reviewer => "reviewer",
            Role::Utility => "utility",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactoryConfig {
    #[serde(default)]
    pub repo: RepoConfig,
    #[serde(default)]
    pub stack: Stack,
    #[serde(default)]
    pub checks: Checks,
    pub profiles: BTreeMap<String, Profile>,
    #[serde(default)]
    pub gates: BTreeMap<String, Gate>,
    #[serde(default)]
    pub grades: GradesConfig,
    #[serde(default)]
    pub egress: EgressConfig,
    #[serde(default)]
    pub usage: UsageConfig,
    #[serde(default)]
    pub compute: ComputeConfig,
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub merge: MergeConfig,
}

/// `[merge]`: what the merger does with a PR whose gates passed (`docs/factory/review.md#the-merger`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeConfig {
    #[serde(default)]
    pub mode: MergeMode,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MergeMode {
    /// Nothing: Lane merges every PR, and the PR isn't commented on.
    #[default]
    Off,
    /// Says on the PR what it would do, and doesn't merge.
    Shadow,
    /// Squash-merges approved PRs: grades with `auto_merge`, and any a person approved. Falls back to `Shadow` when
    /// the factory isn't a delegated reviewer. Re-read from the file before each merge decision, so
    /// switching it takes effect on the next pass.
    On,
}

/// `[grades]`: path globs that raise a PR to at least grade 2, 3 or 4 (keyed by the grade), and the diff size over
/// which it is at least grade 3 (`docs/factory/review.md#grades`). Without the section, the defaults of the example
/// there; with it, the rules it lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GradesConfig {
    #[serde(default = "default_max_lines")]
    pub max_lines: u32,
    #[serde(flatten)]
    pub rules: BTreeMap<String, Vec<String>>,
}

fn default_max_lines() -> u32 {
    600
}

impl Default for GradesConfig {
    fn default() -> GradesConfig {
        let globs = |g: &[&str]| g.iter().map(|s| s.to_string()).collect();
        GradesConfig {
            max_lines: default_max_lines(),
            rules: BTreeMap::from([
                // Code, not its tests: a PR that only adds or changes tests stays at grade 1.
                (
                    "2".to_string(),
                    globs(&[
                        "crates/*/src/**",
                        "crates/*/build.rs",
                        "crates/*/benches/**",
                        "crates/*/examples/**",
                        "cliff.toml",
                        "rustfmt.toml",
                    ]),
                ),
                ("3".to_string(), globs(&["**/Cargo.toml", "Cargo.lock", "**/migrations/**"])),
                // What steers the agents themselves, as well as CI and the factory's own policy.
                (
                    "4".to_string(),
                    globs(&[
                        ".github/**",
                        "fnox.toml",
                        "mise.toml",
                        "AGENTS.md",
                        ".claude/**",
                        ".mcp.json",
                        "skills/**",
                        ".trailhead.toml",
                        "LICENSE",
                    ]),
                ),
            ]),
        }
    }
}

impl GradesConfig {
    /// The classifier for these rules. Keys are checked when the file is parsed.
    pub fn grader(&self) -> Grader<'_> {
        let rules =
            self.rules.iter().filter_map(|(k, globs)| Some((k.parse::<u8>().ok()?, globs.as_slice()))).collect();
        Grader { rules, max_lines: self.max_lines }
    }
}

/// How the daemon in each sandbox is started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonConfig {
    /// A `tracing` filter for the daemon's log, such as `info,iroh=debug`. The daemon's default when unset.
    pub log: Option<String>,
    /// Warn when the sandbox's filesystem uses more than this after setup and trim, before the warm checkpoint is
    /// taken; 0 for no warning. Stack-neutral, so a new stack's caches can't fill the disk unnoticed.
    #[serde(default = "default_disk_warn_bytes")]
    pub disk_warn_bytes: u64,
    /// Refuse to take the warm checkpoint (the prepare fails and the sandbox is destroyed) when the filesystem uses
    /// more than this; 0 for no limit, the default.
    #[serde(default)]
    pub disk_limit_bytes: u64,
}

/// 16 GiB: a Trailhead sandbox with its cargo `target` trimmed to 6 GiB uses about 10 GiB (docs/factory/sandbox.md).
pub const DEFAULT_DISK_WARN_BYTES: u64 = 16 << 30;

fn default_disk_warn_bytes() -> u64 {
    DEFAULT_DISK_WARN_BYTES
}

impl Default for DaemonConfig {
    fn default() -> Self {
        DaemonConfig { log: None, disk_warn_bytes: DEFAULT_DISK_WARN_BYTES, disk_limit_bytes: 0 }
    }
}

/// Where sandboxes run. Sprites unless the factory file says otherwise.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum ComputeConfig {
    #[default]
    Sprites,
    /// A Docker engine on this machine or reachable through `DOCKER_HOST`. Unset fields take the Docker Compute's
    /// defaults.
    Docker {
        image: Option<String>,
        /// The bridge network the gateway gets out on. Sandboxes aren't on it.
        network: Option<String>,
        /// The OCI runtime, such as `runsc` (gVisor).
        runtime: Option<String>,
        /// Each sandbox's memory limit, in MiB, swap included. Without one, sandboxes share the engine's memory and
        /// concurrent builds can OOM-kill each other. Also sets `CARGO_BUILD_JOBS` to one per GiB of it.
        memory_mib: Option<u64>,
    },
}

/// The Docker gateway's name on each sandbox's network (`docs/factory/sandbox.md`).
pub const GATEWAY_HOST: &str = "trailhead-gateway";
/// The port the Docker gateway listens on.
pub const GATEWAY_PORT: u16 = 8080;
/// The Anthropic connection's id in the Docker gateway's list.
pub const GATEWAY_ANTHROPIC_ID: &str = "anthropic";
/// The GitHub connection's id in the Docker gateway's list.
pub const GATEWAY_GITHUB_ID: &str = "github";
/// The git connection's id in the Docker gateway's list: smart-HTTP to `https://github.com`, read only.
pub const GATEWAY_GIT_ID: &str = "github";

/// Subscription usage windows (`docs/factory/budgets.md`): at or past `pause_at[window]` percent, no new runs
/// start until that window resets, leaving room for whatever else shares the subscription.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageConfig {
    #[serde(default)]
    pub pause_at: BTreeMap<String, u32>,
}

/// The usage windows `pause_at` can name.
pub const USAGE_WINDOWS: [&str; 2] = ["five_hour", "seven_day"];

/// Reject `pause_at` windows that aren't known (a typo would never trigger) and percentages over 100.
pub(crate) fn validate_pause_at(pause_at: &BTreeMap<String, u32>, table: &str) -> Result<()> {
    for (window, percent) in pause_at {
        if !USAGE_WINDOWS.contains(&window.as_str()) {
            return Err(Error::Config(format!(
                "{table} pause_at has unknown window `{window}`; the windows are {}",
                USAGE_WINDOWS.join(" and ")
            )));
        }
        if *percent > 100 {
            return Err(Error::Config(format!("{table} pause_at for {window} is {percent}; it's a percent, 0 to 100")));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RepoConfig {
    /// Default to the git remote's.
    pub owner: String,
    pub name: String,
    pub base_branch: String,
    /// The Trailhead project whose tasks run against this repo: the file's top-level `project`, shared with the CLI.
    #[serde(skip_deserializing)]
    pub project: String,
    pub max_sandboxes: usize,
    /// The profile tasks run on unless they name another.
    pub worker: String,
    pub reviewer: String,
    /// Tasks whose direct subtasks are the factory's work: one key or a list of them. Without any, every ready task in
    /// the project is.
    #[serde(deserialize_with = "string_or_list")]
    pub queue: Vec<String>,
    /// Attempts a task gets before it's parked for a human instead of retried.
    pub max_attempts: u32,
    /// Infrastructure failures in a row (sandbox, daemon or API trouble, not the task) at which a task is parked for a
    /// human instead of retried after a backoff.
    pub max_infra_failures: u32,
    /// Reviews of one task that may ask for changes before it's parked for a human instead of reworked again.
    pub max_review_rounds: u32,
    /// Reworks for a PR that conflicts with the base branch before the task is parked for a human. They don't use up
    /// review rounds.
    pub max_conflict_reworks: u32,
}

impl Default for RepoConfig {
    fn default() -> RepoConfig {
        RepoConfig {
            owner: String::new(),
            name: String::new(),
            base_branch: default_base_branch(),
            project: String::new(),
            max_sandboxes: default_max_sandboxes(),
            worker: default_worker(),
            reviewer: default_reviewer(),
            queue: Vec::new(),
            max_attempts: default_max_attempts(),
            max_infra_failures: default_max_infra_failures(),
            max_review_rounds: default_max_review_rounds(),
            max_conflict_reworks: default_max_conflict_reworks(),
        }
    }
}

/// A key as a string, or a list of them.
fn string_or_list<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Keys {
        One(String),
        Many(Vec<String>),
    }
    match Keys::deserialize(d)? {
        Keys::One(s) => Ok(vec![s]),
        Keys::Many(v) if v.is_empty() => Err(serde::de::Error::custom(
            "queue = [] is empty: list at least one task key, or leave `queue` out to make every ready task in the project the factory's",
        )),
        Keys::Many(v) => Ok(v),
    }
}

fn default_base_branch() -> String {
    "main".into()
}
fn default_max_sandboxes() -> usize {
    1
}
fn default_max_attempts() -> u32 {
    3
}
fn default_max_infra_failures() -> u32 {
    10
}
fn default_max_review_rounds() -> u32 {
    3
}
fn default_max_conflict_reworks() -> u32 {
    5
}
fn default_worker() -> String {
    "worker".into()
}
fn default_reviewer() -> String {
    "reviewer".into()
}

impl RepoConfig {
    pub fn repo(&self) -> Repo {
        Repo { owner: self.owner.clone(), name: self.name.clone() }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stack {
    #[serde(default)]
    pub tools: BTreeMap<String, String>,
}

/// The fixed set of named checks. A repo leaves out what it doesn't have; exit code 0 passes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checks {
    pub setup: Option<String>,
    /// Runs after `setup` and before the warm checkpoint, to cut build output and caches down to a size worth
    /// exporting on every task (`docs/factory/sandbox.md` has suggestions per stack). `$TRAILHEAD_DAEMON` is the
    /// daemon binary, whose `trim-cargo` does it for cargo.
    pub trim: Option<String>,
    pub fmt: Option<String>,
    pub fmt_fix: Option<String>,
    pub lint: Option<String>,
    pub typecheck: Option<String>,
    pub test: Option<String>,
    pub build: Option<String>,
}

impl Checks {
    /// The verification checks, in the order they run. `setup`, `trim` and `fmt_fix` change the tree, so they're left out.
    pub fn verifying(&self) -> Vec<(&'static str, &str)> {
        [
            ("fmt", &self.fmt),
            ("lint", &self.lint),
            ("typecheck", &self.typecheck),
            ("test", &self.test),
            ("build", &self.build),
        ]
        .into_iter()
        .filter_map(|(name, cmd)| cmd.as_deref().map(|c| (name, c)))
        .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub role: Role,
    pub adapter: String,
    pub provider: String,
    pub model: ModelAlias,
    pub budget: Budget,
    pub escalate_to: Option<String>,
    /// Minutes of silence from the agent (no steps, tool calls or output, not counting a tool call that's still
    /// running) after which a run is stopped and fails as a stall.
    #[serde(default = "default_stall_timeout")]
    pub stall_timeout: u32,
}

fn default_stall_timeout() -> u32 {
    5
}

impl Profile {
    pub fn stall_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(u64::from(self.stall_timeout) * 60)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub usd: f64,
    pub minutes: u32,
    pub steps: u32,
}

impl Budget {
    pub fn usd(&self) -> Usd {
        Usd::dollars(self.usd)
    }
}

/// One requirement of a grade's gate. It is written as a string in `.trailhead.toml` and in a task's stored
/// `MergeDecision`: one of the five fixed names, or the name of a verifying check.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GateKind {
    /// Every verifying check passed, which the worker ran before opening the PR.
    Checks,
    Ci,
    AgentReview,
    Human,
    Quorum,
    /// A verifying check the repo names; config validation rejects a name that isn't one.
    Named(String),
}

impl GateKind {
    pub fn as_str(&self) -> &str {
        match self {
            GateKind::Checks => "checks",
            GateKind::Ci => "ci",
            GateKind::AgentReview => "agent_review",
            GateKind::Human => "human",
            GateKind::Quorum => "quorum",
            GateKind::Named(name) => name,
        }
    }
}

impl From<&str> for GateKind {
    fn from(s: &str) -> Self {
        match s {
            "checks" => GateKind::Checks,
            "ci" => GateKind::Ci,
            "agent_review" => GateKind::AgentReview,
            "human" => GateKind::Human,
            "quorum" => GateKind::Quorum,
            name => GateKind::Named(name.to_string()),
        }
    }
}

impl std::fmt::Display for GateKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PartialEq<&str> for GateKind {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl Serialize for GateKind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for GateKind {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Ok(GateKind::from(String::deserialize(d)?.as_str()))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gate {
    #[serde(default)]
    pub require: Vec<GateKind>,
    #[serde(default)]
    pub auto_merge: bool,
    pub quorum: Option<Quorum>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quorum {
    pub reviewers: u32,
    pub agree: u32,
    #[serde(default)]
    pub distinct_models: bool,
}

/// Domains each phase may reach, on top of the iroh relays and connector gateway every phase gets.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EgressConfig {
    #[serde(default)]
    pub always: Vec<String>,
    #[serde(default)]
    pub install: Vec<String>,
}

impl FactoryConfig {
    /// The factory's settings from a repo's `.trailhead.toml`: the top-level `project` and the `[factory]` table. The
    /// CLI's own keys (`agent`) are left alone. `remote` is the git remote's owner and name, which `[factory.repo]`
    /// falls back to.
    pub fn from_repo_file(toml_src: &str, remote: Option<Repo>) -> Result<FactoryConfig> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct File {
            project: Option<String>,
            #[allow(dead_code)]
            agent: Option<String>,
            factory: Option<toml::Table>,
        }
        let file: File = toml::from_str(toml_src).map_err(|e| Error::Config(e.to_string()))?;
        let table = file.factory.ok_or_else(|| Error::Config("no [factory] table".into()))?;
        let project = file.project.ok_or_else(|| Error::Config("no top-level `project`".into()))?;
        let config = FactoryConfig::deserialize(table).map_err(|e| Error::Config(e.to_string()))?;
        config.finish(project, remote)
    }

    /// The contents of the `[factory]` table alone, for `project`.
    pub fn parse_table(factory_src: &str, project: &str, remote: Option<Repo>) -> Result<FactoryConfig> {
        let config: FactoryConfig = toml::from_str(factory_src).map_err(|e| Error::Config(e.to_string()))?;
        config.finish(project.to_string(), remote)
    }

    fn finish(mut self, project: String, remote: Option<Repo>) -> Result<FactoryConfig> {
        self.repo.project = project;
        for (field, from_remote) in [
            (&mut self.repo.owner, remote.as_ref().map(|r| &r.owner)),
            (&mut self.repo.name, remote.as_ref().map(|r| &r.name)),
        ] {
            if field.is_empty() {
                *field = from_remote.cloned().ok_or_else(|| {
                    Error::Config("[factory.repo] needs owner and name: there's no git remote to read them from".into())
                })?;
            }
        }
        self.validate()?;
        Ok(self)
    }

    fn validate(&self) -> Result<()> {
        validate_pause_at(&self.usage.pause_at, "[factory.usage]")?;
        for (want, role) in [(&self.repo.worker, Role::Worker), (&self.repo.reviewer, Role::Reviewer)] {
            match self.profiles.get(want) {
                None => return Err(Error::Config(format!("no profile named {want}"))),
                Some(p) if p.role != role => {
                    return Err(Error::Config(format!(
                        "profile {want} has role {}, not {}",
                        p.role.as_str(),
                        role.as_str()
                    )));
                }
                Some(_) => {}
            }
        }
        for (name, profile) in &self.profiles {
            if let Some(next) = &profile.escalate_to
                && !self.profiles.contains_key(next)
            {
                return Err(Error::Config(format!("profile {name} escalates to unknown profile {next}")));
            }
        }
        for grade in self.grades.rules.keys() {
            if !matches!(grade.as_str(), "2" | "3" | "4") {
                return Err(Error::Config(format!("[grades] has `{grade}`; path rules are for grades 2, 3 and 4")));
            }
        }
        let checks: Vec<&str> = self.checks.verifying().into_iter().map(|(n, _)| n).collect();
        for (grade, gate) in &self.gates {
            if !matches!(grade.as_str(), "1" | "2" | "3" | "4") {
                return Err(Error::Config(format!("[gates.{grade}]: gates are per grade, 1 to 4")));
            }
            for r in &gate.require {
                if let GateKind::Named(name) = r
                    && !checks.contains(&name.as_str())
                {
                    return Err(Error::Config(format!("gate {grade} requires unknown {r}")));
                }
            }
            if gate.require.contains(&GateKind::Quorum) && gate.quorum.is_none() {
                return Err(Error::Config(format!("gate {grade} requires a quorum but doesn't define one")));
            }
        }
        Ok(())
    }

    pub fn worker(&self) -> &Profile {
        &self.profiles[&self.repo.worker]
    }

    pub fn reviewer(&self) -> &Profile {
        &self.profiles[&self.repo.reviewer]
    }

    /// The gate a PR of this grade must pass: the repo's `[gates.N]`, or the table in `docs/factory/review.md#grades`.
    pub fn gate(&self, grade: u8) -> Gate {
        use GateKind::{AgentReview, Checks, Ci, Human};
        if let Some(gate) = self.gates.get(&grade.to_string()) {
            return gate.clone();
        }
        let (require, auto_merge): (&[GateKind], bool) = match grade {
            0 | 1 => (&[Checks, Ci], true),
            2 => (&[Checks, Ci, AgentReview], true),
            3 => (&[Checks, Ci, AgentReview, Human], false),
            _ => (&[Checks, Ci, Human], false),
        };
        Gate { require: require.to_vec(), auto_merge, quorum: None }
    }

    /// The checks a worker must pass before opening a PR: every verifying check the repo defines. The grade isn't
    /// known until the PR exists, and any grade's gate may require `checks` or a named check, so the worker runs them
    /// all and the merger can count a gate's check requirements as met.
    pub fn required_checks(&self) -> Vec<Check> {
        self.checks
            .verifying()
            .into_iter()
            .map(|(n, c)| Check { name: n.to_string(), command: c.to_string() })
            .collect()
    }

    /// Readies a checkout, on `branch` when given (a rework). Only workers build: a reviewer reads the diff, so it
    /// skips `setup`.
    pub fn prepare_request(&self, role: Role, branch: Option<String>) -> PrepareRequest {
        let named = |name: &str, c: &Option<String>| {
            c.as_ref().filter(|_| role == Role::Worker).map(|c| Check { name: name.into(), command: c.clone() })
        };
        let some = |b: u64| Some(b).filter(|&b| b > 0);
        PrepareRequest {
            repo: self.repo.repo(),
            base_branch: self.repo.base_branch.clone(),
            branch,
            merge_base: false,
            setup: named("setup", &self.checks.setup),
            trim: named("trim", &self.checks.trim),
            disk_warn_bytes: some(self.daemon.disk_warn_bytes),
            disk_limit_bytes: some(self.daemon.disk_limit_bytes),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_kinds_keep_their_string_form() {
        let kinds = [
            (GateKind::Checks, "checks"),
            (GateKind::Ci, "ci"),
            (GateKind::AgentReview, "agent_review"),
            (GateKind::Human, "human"),
            (GateKind::Quorum, "quorum"),
            (GateKind::Named("lint".into()), "lint"),
        ];
        for (kind, text) in kinds {
            assert_eq!(serde_json::to_string(&kind).unwrap(), format!("\"{text}\""));
            assert_eq!(serde_json::from_str::<GateKind>(&format!("\"{text}\"")).unwrap(), kind);
        }
    }

    fn parse(factory_src: &str) -> Result<FactoryConfig> {
        FactoryConfig::parse_table(factory_src, "TRAIL", None)
    }

    #[test]
    fn the_repos_own_trailhead_toml_parses() {
        let remote = Repo { owner: "lanesawyer".into(), name: "trailhead".into() };
        let config = FactoryConfig::from_repo_file(include_str!("../../../.trailhead.toml"), Some(remote)).unwrap();
        assert_eq!(config.repo.project, "TRAIL");
        let prepare = config.prepare_request(Role::Worker, None);
        assert!(prepare.trim.unwrap().command.contains("trim-cargo"));
        assert_eq!(prepare.disk_warn_bytes, Some(DEFAULT_DISK_WARN_BYTES));
        assert_eq!(prepare.disk_limit_bytes, None);
        assert!(config.prepare_request(Role::Reviewer, None).trim.is_none(), "reviewers don't build");
        assert_eq!(config.usage.pause_at.get("five_hour"), Some(&80));
        assert_eq!(config.grades, GradesConfig::default(), "this repo's rules are the documented example");
        assert_eq!(config.merge.mode, MergeMode::On);
        assert!(config.gate(1).auto_merge);
        assert!(config.gate(2).auto_merge);
        assert!(!config.gate(3).auto_merge);
        assert_eq!(config.gate(3).require, ["checks", "ci", "agent_review", "human"]);
        let raised = |path: &str| {
            config
                .grades
                .rules
                .iter()
                .filter(|(_, g)| g.iter().any(|g| crate::grade::glob_match(g, path)))
                .map(|(n, _)| n.as_str())
                .max()
        };
        assert_eq!(raised("crates/trailhead-core/tests/domain.rs"), None, "test-only changes stay at the task's grade");
        assert_eq!(raised("crates/trailhead-core/src/tasks.rs"), Some("2"));
        assert_eq!(raised("AGENTS.md"), Some("4"));
        assert_eq!(raised(".claude/settings.json"), Some("4"));
    }

    #[test]
    fn queue_is_a_key_or_a_list_of_them() {
        let with = |q: &str| parse(&SAMPLE.replace("[repo]\n", &format!("[repo]\n{q}\n"))).unwrap().repo.queue;
        assert!(parse(SAMPLE).unwrap().repo.queue.is_empty());
        assert_eq!(with("queue = \"TRAIL-102\""), ["TRAIL-102"]);
        assert_eq!(with("queue = [\"TRAIL-102\", \"TRAIL-167\"]"), ["TRAIL-102", "TRAIL-167"]);
        let err = parse(&SAMPLE.replace("[repo]\n", "[repo]\nqueue = []\n")).unwrap_err();
        assert!(err.to_string().contains("queue = [] is empty"), "{err}");
    }
    #[test]
    fn grades_default_to_the_documented_rules_and_parse_from_the_file() {
        let base = SAMPLE;
        let config = parse(base).unwrap();
        assert_eq!(config.grades, GradesConfig::default());

        let custom = format!("{base}\n[grades]\n3 = [\"Cargo.lock\"]\nmax_lines = 50\n");
        let config = parse(&custom).unwrap();
        assert_eq!(config.grades.max_lines, 50);
        assert_eq!(config.grades.rules.keys().collect::<Vec<_>>(), ["3"]);
        assert_eq!(config.grades.grader().rules.len(), 1);

        let no_limit = format!("{base}\n[grades]\n2 = [\"src/**\"]\n");
        assert_eq!(parse(&no_limit).unwrap().grades.max_lines, 600);

        for bad in ["1 = [\"a\"]", "5 = [\"a\"]", "x = [\"a\"]", "max_line = 5"] {
            let err = parse(&format!("{base}\n[grades]\n{bad}\n")).unwrap_err();
            assert!(matches!(err, Error::Config(_)), "{bad}: {err}");
        }
    }

    const SAMPLE: &str = r#"
[repo]
owner = "lanesawyer"
name = "trailhead"

[stack]
tools = { rust = "stable" }

[checks]
fmt = "cargo fmt --check"
lint = "cargo clippy -- -D warnings"
test = "cargo test"

[profiles.worker]
role = "worker"
adapter = "claude-code"
provider = "anthropic"
model = "sonnet"
budget = { usd = 5, minutes = 60, steps = 40 }
escalate_to = "worker-opus"

[profiles.worker-opus]
role = "worker"
adapter = "claude-code"
provider = "anthropic"
model = "opus"
budget = { usd = 15, minutes = 90, steps = 60 }

[profiles.reviewer]
role = "reviewer"
adapter = "claude-code"
provider = "anthropic"
model = "opus"
budget = { usd = 2, minutes = 20, steps = 20 }

[gates.1]
require = ["checks", "agent_review", "human"]
"#;

    #[test]
    fn parses_the_documented_shape() {
        let c = parse(SAMPLE).unwrap();
        assert_eq!(c.repo.base_branch, "main");
        assert_eq!(c.worker().model, ModelAlias::Sonnet);
        assert_eq!(c.worker().budget.usd(), Usd::dollars(5.0));
        let names: Vec<_> = c.required_checks().into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["fmt", "lint", "test"]);
    }

    #[test]
    fn repo_pause_at_rejects_unknown_windows_and_percents_over_100() {
        for bad in ["five_hours = 80", "five_hour = 101"] {
            let err = parse(&format!("{SAMPLE}\n[usage]\npause_at = {{ {bad} }}\n")).unwrap_err();
            assert!(err.to_string().contains("pause_at"), "{err}");
        }
        assert!(parse(&format!("{SAMPLE}\n[usage]\npause_at = {{ seven_day = 100 }}\n")).is_ok());
    }

    #[test]
    fn workers_run_every_check_whatever_the_gates_require() {
        let c = parse(&SAMPLE.replace("require = [\"checks\", \"agent_review\", \"human\"]", "require = [\"ci\"]"))
            .unwrap();
        let names: Vec<_> = c.required_checks().into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["fmt", "lint", "test"]);
    }

    #[test]
    fn rejects_unknown_checks_and_profiles() {
        let bad = SAMPLE.replace("lint = ", "lnt = ");
        assert!(matches!(parse(&bad), Err(Error::Config(_))));
        let bad = SAMPLE.replace("escalate_to = \"worker-opus\"", "escalate_to = \"nope\"");
        assert!(matches!(parse(&bad), Err(Error::Config(_))));
        let bad = SAMPLE.replace("\"agent_review\", \"human\"", "\"agent_review\", \"vibes\"");
        assert!(matches!(parse(&bad), Err(Error::Config(_))));
    }

    #[test]
    fn gates_default_to_the_table_and_merge_mode_to_off() {
        let mut c =
            parse(&SAMPLE.replace("[gates.1]\nrequire = [\"checks\", \"agent_review\", \"human\"]", "")).unwrap();
        assert_eq!(c.merge.mode, MergeMode::Off);
        assert_eq!(c.gate(1).require, ["checks", "ci"]);
        assert!(c.gate(2).auto_merge);
        assert_eq!(c.gate(3).require, ["checks", "ci", "agent_review", "human"]);
        assert!(!c.gate(3).auto_merge);
        assert_eq!(c.gate(4).require, ["checks", "ci", "human"]);

        let custom =
            format!("{SAMPLE}\n[gates.2]\nrequire = [\"ci\"]\nauto_merge = true\n[merge]\nmode = \"shadow\"\n");
        c = parse(&custom).unwrap();
        assert_eq!(c.gate(2).require, ["ci"]);
        assert_eq!(c.gate(1).require, ["checks", "agent_review", "human"]);
        assert_eq!(c.merge.mode, MergeMode::Shadow);
    }

    #[test]
    fn merge_mode_on_is_accepted_and_gates_for_unknown_grades_are_rejected() {
        let on = format!("{SAMPLE}\n[merge]\nmode = \"on\"\n");
        assert_eq!(parse(&on).unwrap().merge.mode, MergeMode::On);
        for bad in ["[merge]\nmode = \"yes\"", "[gates.t1]\nrequire = []", "[gates.5]\nrequire = []"] {
            let err = parse(&format!("{SAMPLE}\n{bad}\n")).unwrap_err();
            assert!(matches!(err, Error::Config(_)), "{bad}: {err}");
        }
    }

    #[test]
    fn compute_defaults_to_sprites() {
        assert_eq!(parse(SAMPLE).unwrap().compute, ComputeConfig::Sprites);
        let docker = format!("{SAMPLE}\n[compute]\nkind = \"docker\"\nruntime = \"runsc\"\n");
        assert_eq!(
            parse(&docker).unwrap().compute,
            ComputeConfig::Docker { image: None, network: None, runtime: Some("runsc".into()), memory_mib: None }
        );
        let sized = format!("{SAMPLE}\n[compute]\nkind = \"docker\"\nmemory_mib = 1536\n");
        assert_eq!(
            parse(&sized).unwrap().compute,
            ComputeConfig::Docker { image: None, network: None, runtime: None, memory_mib: Some(1536) }
        );
        let bad = format!("{SAMPLE}\n[compute]\nkind = \"docker\"\nimgae = \"x\"\n");
        assert!(matches!(parse(&bad), Err(Error::Config(_))));
    }

    #[test]
    fn daemon_log_is_optional() {
        assert_eq!(parse(SAMPLE).unwrap().daemon, DaemonConfig::default());
        let with = format!("{SAMPLE}\n[daemon]\nlog = \"info,iroh=debug\"\n");
        assert_eq!(parse(&with).unwrap().daemon.log.as_deref(), Some("info,iroh=debug"));
        let bad = format!("{SAMPLE}\n[daemon]\nlogg = \"x\"\n");
        assert!(matches!(parse(&bad), Err(Error::Config(_))));
    }

    #[test]
    fn worker_profile_must_have_worker_role() {
        let bad = SAMPLE.replace("[repo]\n", "[repo]\nworker = \"reviewer\"\n");
        assert!(matches!(parse(&bad), Err(Error::Config(_))));
    }

    #[test]
    fn the_repo_file_shares_project_and_defaults_owner_and_name_from_the_remote() {
        let file = format!("agent = \"claude\"\nproject = \"TRAIL\"\n{}", factory_table(SAMPLE));
        let file = file.replace("owner = \"lanesawyer\"\nname = \"trailhead\"\n", "");
        let remote = Repo { owner: "o".into(), name: "n".into() };
        let c = FactoryConfig::from_repo_file(&file, Some(remote.clone())).unwrap();
        assert_eq!((c.repo.project.as_str(), c.repo.repo()), ("TRAIL", remote.clone()));
        assert!(matches!(FactoryConfig::from_repo_file(&file, None), Err(Error::Config(_))));

        let err = |src: &str| match FactoryConfig::from_repo_file(src, Some(remote.clone())) {
            Err(Error::Config(m)) => m,
            other => panic!("expected a config error, got {other:?}"),
        };
        let no_project = file.replace("project = \"TRAIL\"\n", "");
        assert!(err(&no_project).contains("project"), "{}", err(&no_project));
        let repeated = file.replace("[factory.repo]\n", "[factory.repo]\nproject = \"TRAIL\"\n");
        assert!(err(&repeated).contains("project"), "{}", err(&repeated));
        let typo = file.replace("agent =", "agnt =");
        assert!(err(&typo).contains("agnt"), "{}", err(&typo));
    }

    /// `src` (a bare factory table) with every table header under `[factory.`.
    fn factory_table(src: &str) -> String {
        let src = src.replace("\n[", "\n[factory.");
        match src.strip_prefix('[') {
            Some(rest) => format!("[factory.{rest}"),
            None => src,
        }
    }
}
