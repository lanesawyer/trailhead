//! A repo's factory file: stack, standard checks, agent profiles and gate policy.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{Check, Error, ModelAlias, PrepareRequest, Repo, Result, Usd};

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
    pub egress: EgressConfig,
    #[serde(default)]
    pub usage: UsageConfig,
    #[serde(default)]
    pub compute: ComputeConfig,
    #[serde(default)]
    pub daemon: DaemonConfig,
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
    /// Infrastructure failures in a row (sandbox, daemon or API trouble, not the task) before a task is parked for a
    /// human instead of retried after a backoff.
    #[serde(default = "default_max_infra_failures")]
    pub max_infra_failures: u32,
    /// Reviews of one task that may ask for changes before it's parked for a human instead of reworked again.
    #[serde(default = "default_max_review_rounds")]
    pub max_review_rounds: u32,
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
        let known = ["checks", "agent_review", "human", "quorum"];
        let checks: Vec<&str> = self.checks.verifying().into_iter().map(|(n, _)| n).collect();
        for (tier, gate) in &self.gates {
            for r in &gate.require {
                if !known.contains(&r.as_str()) && !checks.contains(&r.as_str()) {
                    return Err(Error::Config(format!("gate {tier} requires unknown {r}")));
                }
            }
            if gate.require.iter().any(|r| r == "quorum") && gate.quorum.is_none() {
                return Err(Error::Config(format!("gate {tier} requires a quorum but doesn't define one")));
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

    /// The default tier's gate, `t1` unless the repo defines no tiers.
    pub fn default_gate(&self) -> Gate {
        self.gates.get("t1").cloned().unwrap_or(Gate {
            require: vec!["checks".into(), "agent_review".into()],
            auto_merge: false,
            quorum: None,
        })
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
        PrepareRequest { repo: self.repo.repo(), base_branch: self.repo.base_branch.clone(), branch, setup }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_repos_own_factory_toml_parses() {
        let config = FactoryConfig::parse(include_str!("../../../factory.toml")).unwrap();
        assert_eq!(config.usage.pause_at.get("five_hour"), Some(&80));
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

[gates.t1]
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
