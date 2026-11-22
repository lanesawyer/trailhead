//! A repo's factory file: stack, standard checks, agent profiles and gate policy.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{Check, DEFAULT_TARGET, Error, Grade, Grader, ModelAlias, PrepareRequest, Repo, Result, Shots, Usd};

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
    pub compute: ComputeConfig,
    /// `[factory.targets.<name>]`: several computes at once. Not with `[factory.compute]`, which means one target.
    #[serde(default)]
    pub targets: BTreeMap<String, TargetConfig>,
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub merge: MergeConfig,
    /// `[factory.shots]`: the command that draws the repo's views to PNGs in `$SHOTS_DIR`, and the `paths` a change
    /// has to touch for a worker to run it, before the change and after (`docs/factory/sandbox.md#pictures`). Without
    /// it no pictures are taken.
    #[serde(default)]
    pub shots: Option<ScopedCheck>,
}

/// `[factory.merge]`: what the merger does with a PR whose gates passed (`docs/factory/review.md#the-merger`).
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

/// `[factory.grades]`: path globs that raise a PR to at least grade 2, 3 or 4 (keyed by the grade), and the diff size over
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
                ("3".to_string(), globs(&["**/Cargo.toml", "**/migrations/**"])),
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
                        // A new package gets a person's look, since workers may fetch them (TRAIL-704).
                        "**/Cargo.lock",
                        "**/package-lock.json",
                        "**/pnpm-lock.yaml",
                        "**/yarn.lock",
                        "**/uv.lock",
                        "**/poetry.lock",
                        "**/go.sum",
                        "**/Gemfile.lock",
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
            self.rules.iter().filter_map(|(k, globs)| Some((k.parse::<Grade>().ok()?, globs.as_slice()))).collect();
        Grader { rules, max_lines: self.max_lines, within: None }
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

/// 16 GiB. For scale, Trailhead's own `trim` check caps its cargo `target` at 12 GiB (docs/factory/sandbox.md).
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
        /// concurrent builds can OOM-kill each other. Also sets `CARGO_BUILD_JOBS` to one per GiB of it. At least
        /// [`MIN_MEMORY_MIB`].
        #[serde(default, deserialize_with = "memory_mib_at_least_min")]
        memory_mib: Option<u64>,
        /// The least free disk, in bytes, the engine may have when a run starts; a floor, since a big sandbox needs
        /// more. Defaults to 10 GiB.
        min_free_bytes: Option<u64>,
        /// How many sandboxes may checkpoint at once, which the free-disk check reserves room for. Defaults to 2; set it
        /// to the pool size (`max_sandboxes`) for a pool that is not two. At least 1.
        #[serde(default, deserialize_with = "checkpoints_at_least_one")]
        concurrent_checkpoints: Option<u64>,
    },
}

/// The least `memory_mib` that can run a sandbox. Docker reads 0 as unlimited, and a limit much below this
/// OOM-kills every sandbox, which comes back as endless infrastructure retries.
pub const MIN_MEMORY_MIB: u64 = 512;

fn check_memory_mib(mib: Option<u64>) -> std::result::Result<Option<u64>, String> {
    match mib {
        Some(m) if m < MIN_MEMORY_MIB => Err(format!(
            "`memory_mib` is {m}, too small to run a sandbox; it must be at least {MIN_MEMORY_MIB} (omit it for no limit)"
        )),
        other => Ok(other),
    }
}

fn memory_mib_at_least_min<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<u64>, D::Error> {
    check_memory_mib(Option::<u64>::deserialize(d)?).map_err(serde::de::Error::custom)
}

fn check_concurrent_checkpoints(n: Option<u64>) -> std::result::Result<Option<u64>, String> {
    match n {
        Some(0) => Err("`concurrent_checkpoints` is 0, which would turn off the disk reservation for checkpoints; \
                        it must be at least 1 (omit it for the default)"
            .into()),
        other => Ok(other),
    }
}

fn checkpoints_at_least_one<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<u64>, D::Error> {
    check_concurrent_checkpoints(Option::<u64>::deserialize(d)?).map_err(serde::de::Error::custom)
}

/// `[factory.targets.<name>]`: one place sandboxes can run, with its compute's settings and its limits
/// (`docs/factory/targets.md`). `kind` and the compute's own keys are those of `[factory.compute]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawTarget")]
pub struct TargetConfig {
    #[serde(flatten)]
    pub compute: ComputeConfig,
    /// Lower is preferred; ties go to the name that sorts first.
    pub priority: u32,
    /// The most sandboxes the repo holds here at once; without one only the repo's own cap applies.
    pub max_sandboxes: Option<usize>,
    /// The daily spend cap and the rates it is priced at.
    pub spend: Option<crate::Spend>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTarget {
    kind: String,
    #[serde(default)]
    priority: u32,
    max_sandboxes: Option<usize>,
    daily_usd: Option<f64>,
    usd_per_vcpu_hour: Option<f64>,
    usd_per_gb_hour: Option<f64>,
    image: Option<String>,
    network: Option<String>,
    runtime: Option<String>,
    memory_mib: Option<u64>,
    min_free_bytes: Option<u64>,
    concurrent_checkpoints: Option<u64>,
}

impl TryFrom<RawTarget> for TargetConfig {
    type Error = String;

    fn try_from(raw: RawTarget) -> std::result::Result<TargetConfig, String> {
        let spend = raw_spend(&raw)?;
        let compute = match raw.kind.as_str() {
            "sprites" => {
                let docker_only = [
                    ("image", raw.image.is_some()),
                    ("network", raw.network.is_some()),
                    ("runtime", raw.runtime.is_some()),
                    ("memory_mib", raw.memory_mib.is_some()),
                    ("min_free_bytes", raw.min_free_bytes.is_some()),
                    ("concurrent_checkpoints", raw.concurrent_checkpoints.is_some()),
                ];
                if let Some((key, _)) = docker_only.iter().find(|(_, set)| *set) {
                    return Err(format!("`{key}` is a setting of kind = \"docker\", not \"sprites\""));
                }
                ComputeConfig::Sprites
            }
            "docker" => ComputeConfig::Docker {
                image: raw.image,
                network: raw.network,
                runtime: raw.runtime,
                memory_mib: check_memory_mib(raw.memory_mib)?,
                min_free_bytes: raw.min_free_bytes,
                concurrent_checkpoints: check_concurrent_checkpoints(raw.concurrent_checkpoints)?,
            },
            other => return Err(format!("unknown kind `{other}`; the kinds are sprites and docker")),
        };
        Ok(TargetConfig { compute, priority: raw.priority, max_sandboxes: raw.max_sandboxes, spend })
    }
}

/// The spend cap a target's keys set, with Fly's rates for any left out.
fn raw_spend(raw: &RawTarget) -> std::result::Result<Option<crate::Spend>, String> {
    let amount = |key: &str, v: f64, least: f64| {
        if v.is_finite() && v >= least {
            Ok(crate::Usd::dollars(v))
        } else {
            Err(format!("`{key}` is not a usable amount: {v}"))
        }
    };
    let Some(daily) = raw.daily_usd else {
        if raw.usd_per_vcpu_hour.is_some() || raw.usd_per_gb_hour.is_some() {
            return Err("`usd_per_vcpu_hour` and `usd_per_gb_hour` price a spend cap, so they need `daily_usd`".into());
        }
        return Ok(None);
    };
    let rate = |key: &str, v: Option<f64>, default: crate::Usd| v.map_or(Ok(default), |v| amount(key, v, 0.0));
    Ok(Some(crate::Spend {
        daily: amount("daily_usd", daily, 0.000_001)?,
        per_vcpu_hour: rate("usd_per_vcpu_hour", raw.usd_per_vcpu_hour, crate::Spend::VCPU_HOUR)?,
        per_gb_hour: rate("usd_per_gb_hour", raw.usd_per_gb_hour, crate::Spend::GB_HOUR)?,
    }))
}

/// A target as the factory runs it: [`TargetConfig`] with its name. [`FactoryConfig::target_specs`] lists them in
/// placement order.
#[derive(Debug, Clone, PartialEq)]
pub struct TargetSpec {
    pub name: String,
    pub compute: ComputeConfig,
    pub priority: u32,
    pub max_sandboxes: Option<usize>,
    pub spend: Option<crate::Spend>,
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
/// Deliberately equal to [`GATEWAY_GITHUB_ID`]: connections are keyed by (provider, id), so the two don't collide.
pub const GATEWAY_GIT_ID: &str = "github";

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
    /// Minutes a task may sit parked (`needs_input`) before its pooled sandbox is given up for ready tasks that are
    /// waiting for a place, when releasing loses nothing: no unpublished work in the checkout. 0 never releases.
    pub release_parked_after: u32,
    /// Minutes a spec task may wait on a person's reply before its planner's sandbox, which holds the planner's
    /// session, is given up whether or not other work waits; the next turn then starts a new session. 0 never does.
    pub release_planner_after: u32,
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
            release_parked_after: default_release_parked_after(),
            release_planner_after: default_release_planner_after(),
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
fn default_release_parked_after() -> u32 {
    15
}
fn default_release_planner_after() -> u32 {
    6 * 60
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
    /// Variables for the agent and every check (`setup` and `trim` too) in a sandbox, on top of mise's environment:
    /// `{ CARGO_BUILD_JOBS = "2" }`. Not the names the factory sets itself (`RESERVED_ENV`).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// Names `[factory.stack] env` can't set: the proxy variables the sandboxes' egress depends on, the harness's
/// credential and settings, and `PATH` and `HOME`, which are mise's and the sandbox user's.
const RESERVED_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
    "DISABLE_AUTOUPDATER",
];
const RESERVED_ENV_PREFIXES: &[&str] = &["TRAILHEAD_", "ANTHROPIC_", "MISE_"];

impl Stack {
    /// A short stable tag, which pooled sandboxes carry so a changed `[stack]` reinstalls (FNV-1a;
    /// `DefaultHasher` isn't stable across builds). Empty for no tools and no `env`. The `env` is in it because
    /// `setup` and `trim` run with it, so it can change what the warm checkpoint holds (`RUSTFLAGS`, say); a stack
    /// with only tools hashes as it did before `env` existed.
    pub fn tag(&self) -> String {
        if self.tools.is_empty() && self.env.is_empty() {
            return String::new();
        }
        let mut hash: u64 = 0xcbf29ce484222325;
        let mut feed = |text: String| {
            for byte in text.bytes() {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
            }
        };
        for (k, v) in &self.tools {
            feed(format!("{k}={v};"));
        }
        if !self.env.is_empty() {
            feed("env;".into());
            for (k, v) in &self.env {
                feed(format!("{k}={v};"));
            }
        }
        format!("{hash:016x}")
    }

    /// Refuses an `env` name that is malformed or that the factory sets itself.
    fn validate(&self) -> Result<()> {
        for name in self.env.keys() {
            if name.is_empty() || name.contains(['=', '\0']) {
                return Err(Error::Config(format!("[factory.stack] env name `{name}` isn't a variable name")));
            }
            let upper = name.to_ascii_uppercase();
            if RESERVED_ENV.contains(&upper.as_str()) || RESERVED_ENV_PREFIXES.iter().any(|p| upper.starts_with(p)) {
                return Err(Error::Config(format!(
                    "[factory.stack] env can't set `{name}`: the factory sets it itself (proxy variables, PATH, HOME, \
                     TRAILHEAD_*, ANTHROPIC_*, MISE_* and the harness's credential)"
                )));
            }
        }
        Ok(())
    }
}

/// A verifying check in `[factory.checks]`: a command, or a table of `run` and the `paths` it's limited to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CheckSpec {
    /// Runs on every task.
    Command(String),
    /// Runs when the change against the base branch touches one of the globs (as in `[factory.grades]`).
    Scoped(ScopedCheck),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopedCheck {
    pub run: String,
    #[serde(default)]
    pub paths: Vec<String>,
}

impl CheckSpec {
    pub fn run(&self) -> &str {
        match self {
            CheckSpec::Command(c) => c,
            CheckSpec::Scoped(s) => &s.run,
        }
    }

    /// The path globs the check is limited to; empty runs it always.
    pub fn paths(&self) -> &[String] {
        match self {
            CheckSpec::Command(_) => &[],
            CheckSpec::Scoped(s) => &s.paths,
        }
    }
}

/// The named checks. `fmt`, `lint`, `typecheck`, `test` and `build` are the usual ones; any other name is a further
/// verifying check, which runs after them in name order. A repo leaves out what it doesn't have; exit code 0 passes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Checks {
    /// Never path-scoped: it prepares every checkout.
    pub setup: Option<String>,
    /// Runs after `setup` and before the warm checkpoint, to cut build output and caches down to a size worth
    /// exporting on every task (`docs/factory/sandbox.md` has suggestions per stack). `$TRAILHEAD_DAEMON` is the
    /// daemon binary, whose `trim-cargo` does it for cargo.
    pub trim: Option<String>,
    pub fmt: Option<CheckSpec>,
    pub fmt_fix: Option<String>,
    pub lint: Option<CheckSpec>,
    pub typecheck: Option<CheckSpec>,
    pub test: Option<CheckSpec>,
    pub build: Option<CheckSpec>,
    #[serde(flatten)]
    pub more: BTreeMap<String, CheckSpec>,
}

/// Names with a meaning of their own in `[factory.checks]`; an extra check's name must not be a near miss of one.
const RESERVED_CHECKS: [&str; 8] = ["setup", "trim", "fmt", "fmt_fix", "lint", "typecheck", "test", "build"];

/// Whether two names differ by at most one insertion, deletion, substitution or swap of two adjacent characters.
fn within_one_edit(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    let skip = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let (a, b) = (&a[skip..], &b[skip..]);
    match a.len().cmp(&b.len()) {
        std::cmp::Ordering::Equal => {
            a.is_empty() || a[1..] == b[1..] || (a.len() >= 2 && a[0] == b[1] && a[1] == b[0] && a[2..] == b[2..])
        }
        std::cmp::Ordering::Greater => a[1..] == *b,
        std::cmp::Ordering::Less => *a == b[1..],
    }
}

impl Checks {
    /// Any name is a verifying check, so a typo of a reserved key (`lnt`, `fmt-fix`, `setp`) would silently become
    /// one, and `fmt-fix = "cargo fmt"` would rewrite the tree during checks. Reject names a typo away from one.
    fn validate_names(&self) -> Result<()> {
        for name in self.more.keys() {
            let normal = name.to_ascii_lowercase().replace('-', "_");
            if let Some(near) = RESERVED_CHECKS.iter().find(|r| within_one_edit(&normal, r)) {
                return Err(Error::Config(format!(
                    "[factory.checks] `{name}` looks like a misspelling of `{near}`; fix the key, or pick a name that isn't close to one of {}",
                    RESERVED_CHECKS.join(", ")
                )));
            }
        }
        Ok(())
    }

    /// The verification checks, in the order they run. `setup`, `trim` and `fmt_fix` change the tree, so they're left out.
    pub fn verifying(&self) -> Vec<(&str, &CheckSpec)> {
        let named = [
            ("fmt", &self.fmt),
            ("lint", &self.lint),
            ("typecheck", &self.typecheck),
            ("test", &self.test),
            ("build", &self.build),
        ]
        .into_iter()
        .filter_map(|(name, spec)| spec.as_ref().map(|s| (name, s)));
        named.chain(self.more.iter().map(|(n, s)| (n.as_str(), s))).collect()
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
    /// Download hosts per mise tool (`zig = ["ziglang.org"]`), for a `[stack]` tool the daemon's table
    /// doesn't know. Opened only while that tool installs; they take precedence over the table.
    #[serde(default)]
    pub tools: BTreeMap<String, Vec<String>>,
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
            #[expect(dead_code)]
            agent: Option<String>,
            factory: Option<toml::Table>,
        }
        let file: File = toml::from_str(toml_src).map_err(|e| Error::Config(e.to_string()))?;
        let table = file.factory.ok_or_else(|| Error::Config("no [factory] table".into()))?;
        let project = file.project.ok_or_else(|| Error::Config("no top-level `project`".into()))?;
        let config = Self::read(table)?;
        config.finish(project, remote)
    }

    /// The contents of the `[factory]` table alone, for `project`.
    pub fn parse_table(factory_src: &str, project: &str, remote: Option<Repo>) -> Result<FactoryConfig> {
        let table: toml::Table = toml::from_str(factory_src).map_err(|e| Error::Config(e.to_string()))?;
        let config = Self::read(table)?;
        config.finish(project.to_string(), remote)
    }

    /// The settings in `table`, refusing a file that lists both `[factory.compute]` and `[factory.targets]`.
    fn read(table: toml::Table) -> Result<FactoryConfig> {
        if table.contains_key("compute") && table.contains_key("targets") {
            return Err(Error::Config(
                "[factory.compute] and [factory.targets] are both set; [factory.compute] means one target, so use one or the other".into(),
            ));
        }
        FactoryConfig::deserialize(table).map_err(|e| Error::Config(e.to_string()))
    }

    /// Where sandboxes can run, in placement order: lowest priority number first, then by name. A file with
    /// `[factory.compute]` (or neither) has the one target, [`DEFAULT_TARGET`].
    pub fn target_specs(&self) -> Vec<TargetSpec> {
        if self.targets.is_empty() {
            return vec![TargetSpec {
                name: DEFAULT_TARGET.into(),
                compute: self.compute.clone(),
                priority: 0,
                max_sandboxes: None,
                spend: None,
            }];
        }
        let mut specs: Vec<TargetSpec> = self
            .targets
            .iter()
            .map(|(name, t)| TargetSpec {
                name: name.clone(),
                compute: t.compute.clone(),
                priority: t.priority,
                max_sandboxes: t.max_sandboxes,
                spend: t.spend,
            })
            .collect();
        specs.sort_by(|a, b| (a.priority, &a.name).cmp(&(b.priority, &b.name)));
        specs
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
        self.stack.validate()?;
        if self.shots.as_ref().is_some_and(|s| s.run.trim().is_empty()) {
            return Err(Error::Config("[factory.shots] run is empty; give the command, or drop the table".into()));
        }
        if let Some(log) = &self.daemon.log
            && let Err(e) = tracing_subscriber::EnvFilter::try_new(log)
        {
            return Err(Error::Config(format!("[factory.daemon] log `{log}` isn't a valid tracing filter: {e}")));
        }
        for (name, target) in &self.targets {
            // Names go into sandbox names and labels.
            let ok = !name.is_empty()
                && name.split('-').all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric()))
                && !name.chars().any(|c| c.is_ascii_uppercase());
            if !ok || name == DEFAULT_TARGET {
                return Err(Error::Config(format!(
                    "[factory.targets.{name}]: a target's name is lowercase letters, digits and single hyphens, and not `{DEFAULT_TARGET}`"
                )));
            }
            if target.max_sandboxes == Some(0) {
                return Err(Error::Config(format!(
                    "[factory.targets.{name}] max_sandboxes is 0, so no run could go there"
                )));
            }
        }
        for (want, role) in [(&self.repo.worker, Role::Worker), (&self.repo.reviewer, Role::Reviewer)] {
            match self.profiles.get(want) {
                None => {
                    return Err(Error::Config(format!(
                        "[factory.repo] {}: no profile named {want}; define [factory.profiles.{want}]",
                        role.as_str()
                    )));
                }
                Some(p) if p.role != role => {
                    return Err(Error::Config(format!(
                        "[factory.repo] {key} is `{want}`, but [factory.profiles.{want}] has role {}, not {key}",
                        p.role.as_str(),
                        key = role.as_str()
                    )));
                }
                Some(_) => {}
            }
        }
        for (name, profile) in &self.profiles {
            if profile.stall_timeout == 0 {
                return Err(Error::Config(format!(
                    "[factory.profiles.{name}] stall_timeout is 0; it's minutes of silence, 1 or more"
                )));
            }
            if let Some(next) = &profile.escalate_to
                && !self.profiles.contains_key(next)
            {
                return Err(Error::Config(format!(
                    "[factory.profiles.{name}] escalate_to is `{next}`, but there is no [factory.profiles.{next}]"
                )));
            }
        }
        for grade in self.grades.rules.keys() {
            if !matches!(grade.as_str(), "2" | "3" | "4") {
                return Err(Error::Config(format!(
                    "[factory.grades] has `{grade}`; path rules are for grades 2, 3 and 4"
                )));
            }
        }
        for (grade, globs) in &self.grades.rules {
            for glob in globs {
                if let Err(e) = crate::grade::compile_glob(glob) {
                    return Err(Error::Config(format!("[factory.grades] `{grade}`: bad glob `{glob}`: {e}")));
                }
            }
        }
        self.checks.validate_names()?;
        let checks: Vec<&str> = self.checks.verifying().into_iter().map(|(n, _)| n).collect();
        for (grade, gate) in &self.gates {
            if !matches!(grade.as_str(), "1" | "2" | "3" | "4") {
                return Err(Error::Config(format!("[factory.gates.{grade}]: gates are per grade, 1 to 4")));
            }
            for r in &gate.require {
                if let GateKind::Named(name) = r
                    && !checks.contains(&name.as_str())
                {
                    return Err(Error::Config(format!(
                        "[factory.gates.{grade}] requires unknown {r}; it must name a check in [factory.checks]"
                    )));
                }
            }
            if gate.require.contains(&GateKind::Quorum) && gate.quorum.is_none() {
                return Err(Error::Config(format!("[factory.gates.{grade}] requires a quorum but doesn't define one")));
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

    /// The repo's planner profile, by name: the first with `role = "planner"`. `None` when it has none, and then no
    /// spec task is planned.
    pub fn planner(&self) -> Option<(&str, &Profile)> {
        self.profiles.iter().find(|(_, p)| p.role == Role::Planner).map(|(n, p)| (n.as_str(), p))
    }

    /// The gate a PR of this grade must pass: the repo's `[factory.gates.N]`, or the table in `docs/factory/review.md#grades`.
    pub fn gate(&self, grade: Grade) -> Gate {
        use GateKind::{AgentReview, Checks, Ci, Human};
        if let Some(gate) = self.gates.get(&grade.to_string()) {
            return gate.clone();
        }
        let (require, auto_merge): (&[GateKind], bool) = match grade.get() {
            1 => (&[Checks, Ci], true),
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
            .map(|(n, c)| Check { name: n.to_string(), command: c.run().to_string(), paths: c.paths().to_vec() })
            .collect()
    }

    /// The repo's `[factory.shots]`, as a run request carries it.
    pub fn shots(&self) -> Option<Shots> {
        self.shots.as_ref().map(|s| Shots { command: s.run.clone(), paths: s.paths.clone() })
    }

    /// Readies a checkout, on `branch` when given: a worker's rework, or a reviewer's look at the PR's head. Only
    /// workers build: a reviewer reads the diff, so it skips `setup` and the stack's tools, saving a cold install of
    /// mise and the toolchain on every fresh reviewer Sprite.
    pub fn prepare_request(&self, role: Role, branch: Option<String>) -> PrepareRequest {
        let named = |name: &str, c: &Option<String>| {
            c.as_ref().filter(|_| role == Role::Worker).map(|c| Check::new(name, c.clone()))
        };
        let some = |b: u64| Some(b).filter(|&b| b > 0);
        PrepareRequest {
            repo: self.repo.repo(),
            base_branch: self.repo.base_branch.clone(),
            branch,
            merge_base: false,
            branch_if_present: false,
            setup: named("setup", &self.checks.setup),
            trim: named("trim", &self.checks.trim),
            disk_warn_bytes: some(self.daemon.disk_warn_bytes),
            disk_limit_bytes: some(self.daemon.disk_limit_bytes),
            tools: if role == Role::Worker { self.stack.tools.clone() } else { Default::default() },
            tool_hosts: if role == Role::Worker { self.egress.tools.clone() } else { Default::default() },
            env: if role == Role::Worker { self.stack.env.clone() } else { Default::default() },
        }
    }
}

#[cfg(test)]
mod tests;
