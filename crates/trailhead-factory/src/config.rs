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
    /// the factory isn't a delegated reviewer. Read at startup, so switching it takes effect on the next restart.
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
                        "factory.toml",
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonConfig {
    /// A `tracing` filter for the daemon's log, such as `info,iroh=debug`. The daemon's default when unset.
    pub log: Option<String>,
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
        network: Option<String>,
        /// The OCI runtime, such as `runsc` (gVisor).
        runtime: Option<String>,
    },
}

/// Subscription usage windows (`docs/factory/budgets.md`): at or past `pause_at[window]` percent, no new runs
/// start until that window resets, leaving room for whatever else shares the subscription.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageConfig {
    #[serde(default)]
    pub pause_at: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    pub owner: String,
    pub name: String,
    #[serde(default = "default_base_branch")]
    pub base_branch: String,
    /// The Trailhead project whose tasks run against this repo.
    pub project: String,
    #[serde(default = "default_max_sandboxes")]
    pub max_sandboxes: usize,
    /// The profile tasks run on unless they name another.
    #[serde(default = "default_worker")]
    pub worker: String,
    #[serde(default = "default_reviewer")]
    pub reviewer: String,
    /// A task whose subtasks are the factory's work. Without one, every ready task in the project is.
    #[serde(default)]
    pub queue: Option<String>,
    /// Attempts a task gets before it's parked for a human instead of retried.
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    /// Infrastructure failures in a row (sandbox, daemon or API trouble, not the task) at which a task is parked for a
    /// human instead of retried after a backoff.
    #[serde(default = "default_max_infra_failures")]
    pub max_infra_failures: u32,
    /// Reviews of one task that may ask for changes before it's parked for a human instead of reworked again.
    #[serde(default = "default_max_review_rounds")]
    pub max_review_rounds: u32,
    /// Reworks for a PR that conflicts with the base branch before the task is parked for a human. They don't use up
    /// review rounds.
    #[serde(default = "default_max_conflict_reworks")]
    pub max_conflict_reworks: u32,
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
    pub fmt: Option<String>,
    pub fmt_fix: Option<String>,
    pub lint: Option<String>,
    pub typecheck: Option<String>,
    pub test: Option<String>,
    pub build: Option<String>,
}

impl Checks {
    /// The verification checks, in the order they run. `setup` and `fmt_fix` change the tree, so they're left out.
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gate {
    #[serde(default)]
    pub require: Vec<String>,
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
    pub fn parse(toml_src: &str) -> Result<FactoryConfig> {
        let config: FactoryConfig = toml::from_str(toml_src).map_err(|e| Error::Config(e.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
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
        let known = ["checks", "ci", "agent_review", "human", "quorum"];
        let checks: Vec<&str> = self.checks.verifying().into_iter().map(|(n, _)| n).collect();
        for (grade, gate) in &self.gates {
            if !matches!(grade.as_str(), "1" | "2" | "3" | "4") {
                return Err(Error::Config(format!("[gates.{grade}]: gates are per grade, 1 to 4")));
            }
            for r in &gate.require {
                if !known.contains(&r.as_str()) && !checks.contains(&r.as_str()) {
                    return Err(Error::Config(format!("gate {grade} requires unknown {r}")));
                }
            }
            if gate.require.iter().any(|r| r == "quorum") && gate.quorum.is_none() {
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
        if let Some(gate) = self.gates.get(&grade.to_string()) {
            return gate.clone();
        }
        let (require, auto_merge): (&[&str], bool) = match grade {
            0 | 1 => (&["checks", "ci"], true),
            2 => (&["checks", "ci", "agent_review"], true),
            3 => (&["checks", "ci", "agent_review", "human"], false),
            _ => (&["checks", "ci", "human"], false),
        };
        Gate { require: require.iter().map(|r| r.to_string()).collect(), auto_merge, quorum: None }
    }

    /// The lowest grade's gate, which decides what a worker's checks must cover before it opens a PR.
    pub fn default_gate(&self) -> Gate {
        self.gate(1)
    }

    /// The checks a worker must pass before opening a PR under the default gate.
    pub fn required_checks(&self) -> Vec<Check> {
        let gate = self.default_gate();
        let all =
            self.checks.verifying().into_iter().map(|(n, c)| Check { name: n.to_string(), command: c.to_string() });
        let everything = gate.require.iter().any(|r| r == "checks");
        all.filter(|c| everything || gate.require.contains(&c.name)).collect()
    }

    /// Readies a checkout, on `branch` when given (a rework). Only workers build: a reviewer reads the diff, so it
    /// skips `setup`.
    pub fn prepare_request(&self, role: Role, branch: Option<String>) -> PrepareRequest {
        let setup = match role {
            Role::Worker => self.checks.setup.as_ref().map(|c| Check { name: "setup".into(), command: c.clone() }),
            _ => None,
        };
        PrepareRequest {
            repo: self.repo.repo(),
            base_branch: self.repo.base_branch.clone(),
            branch,
            merge_base: false,
            setup,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_repos_own_factory_toml_parses() {
        let config = FactoryConfig::parse(include_str!("../../../factory.toml")).unwrap();
        assert_eq!(config.usage.pause_at.get("five_hour"), Some(&80));
        assert_eq!(config.grades, GradesConfig::default(), "this repo's rules are the documented example");
        assert_eq!(config.merge.mode, MergeMode::On);
        assert!(config.gate(1).auto_merge);
        assert!(!config.gate(2).auto_merge, "only grade 1 auto-merges, to start");
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
    fn grades_default_to_the_documented_rules_and_parse_from_the_file() {
        let base = SAMPLE;
        let config = FactoryConfig::parse(base).unwrap();
        assert_eq!(config.grades, GradesConfig::default());

        let custom = format!("{base}\n[grades]\n3 = [\"Cargo.lock\"]\nmax_lines = 50\n");
        let config = FactoryConfig::parse(&custom).unwrap();
        assert_eq!(config.grades.max_lines, 50);
        assert_eq!(config.grades.rules.keys().collect::<Vec<_>>(), ["3"]);
        assert_eq!(config.grades.grader().rules.len(), 1);

        let no_limit = format!("{base}\n[grades]\n2 = [\"src/**\"]\n");
        assert_eq!(FactoryConfig::parse(&no_limit).unwrap().grades.max_lines, 600);

        for bad in ["1 = [\"a\"]", "5 = [\"a\"]", "x = [\"a\"]", "max_line = 5"] {
            let err = FactoryConfig::parse(&format!("{base}\n[grades]\n{bad}\n")).unwrap_err();
            assert!(matches!(err, Error::Config(_)), "{bad}: {err}");
        }
    }

    const SAMPLE: &str = r#"
[repo]
owner = "lanesawyer"
name = "trailhead"
project = "TRAIL"

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
        let c = FactoryConfig::parse(SAMPLE).unwrap();
        assert_eq!(c.repo.base_branch, "main");
        assert_eq!(c.worker().model, ModelAlias::Sonnet);
        assert_eq!(c.worker().budget.usd(), Usd::dollars(5.0));
        let names: Vec<_> = c.required_checks().into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["fmt", "lint", "test"]);
    }

    #[test]
    fn rejects_unknown_checks_and_profiles() {
        let bad = SAMPLE.replace("lint = ", "lnt = ");
        assert!(matches!(FactoryConfig::parse(&bad), Err(Error::Config(_))));
        let bad = SAMPLE.replace("escalate_to = \"worker-opus\"", "escalate_to = \"nope\"");
        assert!(matches!(FactoryConfig::parse(&bad), Err(Error::Config(_))));
        let bad = SAMPLE.replace("\"agent_review\", \"human\"", "\"agent_review\", \"vibes\"");
        assert!(matches!(FactoryConfig::parse(&bad), Err(Error::Config(_))));
    }

    #[test]
    fn gates_default_to_the_table_and_merge_mode_to_off() {
        let mut c =
            FactoryConfig::parse(&SAMPLE.replace("[gates.1]\nrequire = [\"checks\", \"agent_review\", \"human\"]", ""))
                .unwrap();
        assert_eq!(c.merge.mode, MergeMode::Off);
        assert_eq!(c.gate(1).require, ["checks", "ci"]);
        assert!(c.gate(2).auto_merge);
        assert_eq!(c.gate(3).require, ["checks", "ci", "agent_review", "human"]);
        assert!(!c.gate(3).auto_merge);
        assert_eq!(c.gate(4).require, ["checks", "ci", "human"]);

        let custom =
            format!("{SAMPLE}\n[gates.2]\nrequire = [\"ci\"]\nauto_merge = true\n[merge]\nmode = \"shadow\"\n");
        c = FactoryConfig::parse(&custom).unwrap();
        assert_eq!(c.gate(2).require, ["ci"]);
        assert_eq!(c.gate(1).require, ["checks", "agent_review", "human"]);
        assert_eq!(c.merge.mode, MergeMode::Shadow);
    }

    #[test]
    fn merge_mode_on_is_accepted_and_gates_for_unknown_grades_are_rejected() {
        let on = format!("{SAMPLE}\n[merge]\nmode = \"on\"\n");
        assert_eq!(FactoryConfig::parse(&on).unwrap().merge.mode, MergeMode::On);
        for bad in ["[merge]\nmode = \"yes\"", "[gates.t1]\nrequire = []", "[gates.5]\nrequire = []"] {
            let err = FactoryConfig::parse(&format!("{SAMPLE}\n{bad}\n")).unwrap_err();
            assert!(matches!(err, Error::Config(_)), "{bad}: {err}");
        }
    }

    #[test]
    fn compute_defaults_to_sprites() {
        assert_eq!(FactoryConfig::parse(SAMPLE).unwrap().compute, ComputeConfig::Sprites);
        let docker = format!("{SAMPLE}\n[compute]\nkind = \"docker\"\nruntime = \"runsc\"\n");
        assert_eq!(
            FactoryConfig::parse(&docker).unwrap().compute,
            ComputeConfig::Docker { image: None, network: None, runtime: Some("runsc".into()) }
        );
        let bad = format!("{SAMPLE}\n[compute]\nkind = \"docker\"\nimgae = \"x\"\n");
        assert!(matches!(FactoryConfig::parse(&bad), Err(Error::Config(_))));
    }

    #[test]
    fn daemon_log_is_optional() {
        assert_eq!(FactoryConfig::parse(SAMPLE).unwrap().daemon, DaemonConfig::default());
        let with = format!("{SAMPLE}\n[daemon]\nlog = \"info,iroh=debug\"\n");
        assert_eq!(FactoryConfig::parse(&with).unwrap().daemon.log.as_deref(), Some("info,iroh=debug"));
        let bad = format!("{SAMPLE}\n[daemon]\nlogg = \"x\"\n");
        assert!(matches!(FactoryConfig::parse(&bad), Err(Error::Config(_))));
    }

    #[test]
    fn worker_profile_must_have_worker_role() {
        let bad = SAMPLE.replace("[repo]\n", "[repo]\nworker = \"reviewer\"\n");
        assert!(matches!(FactoryConfig::parse(&bad), Err(Error::Config(_))));
    }
}
