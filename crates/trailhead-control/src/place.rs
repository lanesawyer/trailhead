//! Where a run goes (`docs/factory/targets.md`): the first target, by priority, that is reachable and under its
//! limit. Workers and reviewers are placed one at a time and on their own, so a review needn't share a target with the
//! worker whose PR it checks.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use trailhead_factory::{Daemon, Labels, ModelProvider, Role, Sandbox, TargetSpend, Task, TaskKey, label};

use tokio::time::Instant;

use crate::spend::{self, Ledger};
use crate::{Factory, Placed};

/// How full each target is, read once and then added to as tasks are placed.
pub(crate) struct Survey {
    loads: HashMap<String, usize>,
    /// Targets whose compute didn't answer: nothing is placed there, and nothing is assumed about what is.
    pub(crate) down: HashSet<String>,
    /// Targets that have cost their daily cap: they take no new runs, and those running finish.
    capped: HashSet<String>,
    /// The target each task's worker sandbox is on, for the tasks that have one (a parked task's, say).
    holders: HashMap<TaskKey, String>,
}

/// A worker's or review's place on a target, held from the moment it is chosen until it ends, because its sandbox
/// isn't there to count until the run gets that far.
pub(crate) struct Placement {
    placed: Arc<Mutex<HashMap<(TaskKey, Role), String>>>,
    key: (TaskKey, Role),
    ledger: Arc<Ledger>,
}

impl Drop for Placement {
    fn drop(&mut self) {
        self.placed.lock().unwrap().remove(&self.key);
        self.ledger.end(&self.key, Instant::now(), &spend::today());
    }
}

impl Factory {
    /// The targets sandboxes can run on, in placement order.
    pub fn targets(&self) -> &[Placed] {
        &self.targets
    }

    /// The target `sandbox` runs on: the one its label names, else the first (a sandbox from before targets).
    pub(crate) fn target_of(&self, sandbox: &Sandbox) -> &Placed {
        sandbox
            .labels
            .get(label::TARGET)
            .and_then(|name| self.targets.iter().find(|p| p.target.name == *name))
            .unwrap_or(&self.targets[0])
    }

    pub(crate) fn daemon_of(&self, sandbox: &Sandbox) -> &Arc<dyn Daemon> {
        &self.target_of(sandbox).daemon
    }

    pub(crate) fn provider_of(&self, sandbox: &Sandbox) -> &Arc<dyn ModelProvider> {
        &self.target_of(sandbox).provider
    }

    /// Lists on every target. A target that can't be reached is named in the second half, and its sandboxes are
    /// missing from the first: they are unknown, not gone. An error only when no target answered.
    pub(crate) async fn list_all(&self, filter: &Labels) -> trailhead_factory::Result<(Vec<Sandbox>, HashSet<String>)> {
        let mut found = Vec::new();
        let mut down = HashSet::new();
        let mut last = None;
        for p in &self.targets {
            match p.list(filter).await {
                Ok(listed) => found.extend(listed),
                Err(e) => {
                    tracing::warn!(target = %p.target.name, error = %e, "can't reach the target; its sandboxes are unknown, not gone");
                    down.insert(p.target.name.clone());
                    last = Some(e);
                }
            }
        }
        match last {
            Some(e) if down.len() == self.targets.len() => Err(e),
            _ => Ok((found, down)),
        }
    }

    /// The sandboxes this repo holds on `p`: workers, reviewers and busy pooled ones (which include a parked task's).
    async fn held_on(&self, p: &Placed) -> trailhead_factory::Result<Vec<Sandbox>> {
        let mut filters = Vec::new();
        for role in [Role::Worker, Role::Reviewer] {
            let mut labels = self.base_labels();
            labels.insert(label::ROLE.into(), role.as_str().into());
            filters.push(labels);
        }
        let mut busy = self.pool_labels();
        busy.insert(label::STATE.into(), label::BUSY.into());
        filters.push(busy);
        let mut out: Vec<Sandbox> = Vec::new();
        for filter in filters {
            for s in p.list(&filter).await? {
                if !self.abandoned(&s.name) && !out.iter().any(|o| o.name == s.name) {
                    out.push(s);
                }
            }
        }
        Ok(out)
    }

    /// The targets that have cost their daily cap today. Reaching the cap and leaving it (a new day, or a raised cap)
    /// are each logged once.
    pub(crate) fn capped(&self) -> HashSet<String> {
        let (now, today) = (Instant::now(), spend::today());
        let mut capped = HashSet::new();
        let mut board = Vec::new();
        let mut logged = self.placing.capped.lock().unwrap();
        for p in &self.targets {
            let Some(spend) = &p.target.spend else { continue };
            let name = &p.target.name;
            let spent = self.placing.ledger.spent(name, now, &today);
            board.push(TargetSpend { target: name.clone(), spent, cap: spend.daily, capped: spent >= spend.daily });
            if spent >= spend.daily {
                if logged.insert(name.clone()) {
                    tracing::warn!(target = %name, %spent, cap = %spend.daily, "the daily spend cap is reached; taking no new runs on this target, and running ones finish");
                }
                capped.insert(name.clone());
            } else if logged.remove(name) {
                tracing::info!(target = %name, %spent, cap = %spend.daily, "under the daily spend cap again; placing runs on this target");
            }
        }
        self.status.targets(board);
        capped
    }

    /// How full each target is now.
    pub(crate) async fn survey(&self) -> Survey {
        let placed = self.placing.placed.lock().unwrap().clone();
        let mut loads: HashMap<String, usize> = HashMap::new();
        let mut down = HashSet::new();
        let mut holders = HashMap::new();
        for p in &self.targets {
            match self.held_on(p).await {
                Ok(held) => {
                    for s in held.iter().filter(|s| role_of(s) == Role::Worker) {
                        if let Some(task) = s.labels.get(label::TASK) {
                            holders.insert(TaskKey(task.clone()), p.target.name.clone());
                        }
                    }
                    // A placement stands in only for sandboxes on its own target: an old one left on another
                    // target still counts there.
                    let counted = held
                        .iter()
                        .filter(|s| {
                            let task = s.labels.get(label::TASK).cloned().unwrap_or_default();
                            placed.get(&(TaskKey(task), role_of(s))) != Some(&p.target.name)
                        })
                        .count();
                    loads.insert(p.target.name.clone(), counted);
                }
                Err(e) => {
                    tracing::warn!(target = %p.target.name, error = %e, "can't reach the target; placing nothing on it");
                    down.insert(p.target.name.clone());
                }
            }
        }
        for target in placed.into_values() {
            *loads.entry(target).or_default() += 1;
        }
        let capped = self.capped();
        Survey { loads, down, capped, holders }
    }

    /// The first target that is reachable and under its `max_sandboxes`, now one fuller. `None` when none has room.
    pub(crate) fn place<'a>(&'a self, survey: &mut Survey) -> Option<&'a Placed> {
        let chosen = self.targets.iter().find(|p| {
            let name = &p.target.name;
            !survey.down.contains(name)
                && !survey.capped.contains(name)
                && p.target.max_sandboxes.is_none_or(|max| survey.loads.get(name).copied().unwrap_or(0) < max)
        })?;
        *survey.loads.entry(chosen.target.name.clone()).or_default() += 1;
        Some(chosen)
    }

    /// Where `task` goes: the target its recorded run is on, when its worker sandbox there still holds its state (a
    /// parked or reworked task, `docs/factory/targets.md`), else the first with room, as `place` chooses. The
    /// sandbox already counts in the target's load, so staying put takes no further room.
    pub(crate) fn place_task<'a>(&'a self, survey: &mut Survey, task: &Task) -> Option<&'a Placed> {
        match self.kept_target(survey, task) {
            Some(p) => Some(p),
            None => self.place(survey),
        }
    }

    /// Whether `task` stays in the sandbox it holds, so starting it takes no room: its recorded target still holds
    /// the sandbox and is neither down nor capped. A holder that doesn't stay gets a new sandbox like any other task.
    pub(crate) fn stays_put(&self, survey: &Survey, task: &Task) -> bool {
        self.kept_target(survey, task).is_some()
    }

    fn kept_target(&self, survey: &Survey, task: &Task) -> Option<&Placed> {
        let stays = task.run.target.as_ref().filter(|t| survey.holders.get(&task.key) == Some(*t))?;
        self.targets
            .iter()
            .find(|p| p.target.name == *stays)
            .filter(|p| !survey.down.contains(&p.target.name) && !survey.capped.contains(&p.target.name))
    }

    /// Holds `task`'s place as `role` on `target` until the guard drops.
    pub(crate) fn hold(&self, task: &TaskKey, role: Role, target: &Placed) -> Placement {
        let key = (task.clone(), role);
        self.placing.placed.lock().unwrap().insert(key.clone(), target.target.name.clone());
        if let Some(spend) = &target.target.spend {
            self.placing.ledger.begin(key.clone(), &target.target.name, spend.hourly(), Instant::now());
        }
        Placement { placed: self.placing.placed.clone(), key, ledger: self.placing.ledger.clone() }
    }

    /// The target's name as a pooled sandbox's name carries it: nothing for the default target, whose sandboxes keep
    /// the names they had before targets.
    pub(crate) fn pool_prefix(&self, p: &Placed) -> String {
        let repo = self.config.repo.name.to_ascii_lowercase();
        if p.target.name == trailhead_factory::DEFAULT_TARGET { repo } else { format!("{repo}-{}", p.target.name) }
    }
}

/// The role a sandbox's label gives it; a sandbox with none is taken for a worker's.
fn role_of(sandbox: &Sandbox) -> Role {
    if sandbox.labels.get(label::ROLE).map(String::as_str) == Some(Role::Reviewer.as_str()) {
        Role::Reviewer
    } else {
        Role::Worker
    }
}

#[cfg(test)]
mod tests {
    use trailhead_factory::{Compute, FactoryConfig, HostConfig, SandboxSpec, Target, Tracker};
    use trailhead_testkit::{FakeCodeHost, FakeCompute, FakeProvider, FakeTracker, LocalDaemon, ScriptedHarness};

    use super::*;
    use crate::Host;

    const CONFIG: &str = r#"
[repo]
owner = "lanesawyer"
name = "trailhead"

[checks]
test = "cargo test"

[profiles.worker]
role = "worker"
adapter = "claude-code"
provider = "anthropic"
model = "sonnet"
budget = { usd = 5, minutes = 60, steps = 40 }

[profiles.reviewer]
role = "reviewer"
adapter = "claude-code"
provider = "anthropic"
model = "opus"
budget = { usd = 2, minutes = 20, steps = 20 }

[targets.local]
kind = "docker"
priority = 1

[targets.cloud]
kind = "sprites"
priority = 2
"#;

    #[tokio::test]
    async fn a_placement_on_another_target_leaves_the_old_sandbox_counted() {
        let config = FactoryConfig::parse_table(CONFIG, "TRAIL", None).unwrap();
        let tracker = FakeTracker::new("factory");
        let codehost = FakeCodeHost::new();
        let local = FakeCompute::new();
        let mut placed = Vec::new();
        for spec in config.target_specs() {
            let compute = if spec.name == "local" { local.clone() } else { FakeCompute::new() };
            placed.push(Placed {
                target: Target::default().listed(&spec),
                compute: Arc::new(compute),
                daemon: Arc::new(LocalDaemon::new(
                    Arc::new(ScriptedHarness::new()),
                    Arc::new(codehost.clone()),
                    tracker.clone(),
                )),
                provider: Arc::new(FakeProvider::new(1)),
            });
        }
        let factory = Factory::with_targets(config, placed, Arc::new(tracker), Host::new(&HostConfig::default()));

        // A busy worker sandbox left on local by a parked task, which is then answered and placed on the cloud.
        let key = TaskKey("TRAIL-1".into());
        let labels = Labels::from([
            (label::FACTORY.into(), "trailhead".into()),
            (label::REPO.into(), "lanesawyer/trailhead".into()),
            (label::POOL.into(), "worker".into()),
            (label::STATE.into(), label::BUSY.into()),
            (label::TASK.into(), key.0.clone()),
            (label::ROLE.into(), "worker".into()),
            (label::TARGET.into(), "local".into()),
        ]);
        local.create(&SandboxSpec { name: "trailhead-w1".into(), labels, egress: Default::default() }).await.unwrap();
        let cloud = &factory.targets()[1];
        let _held = factory.hold(&key, Role::Worker, cloud);

        let survey = factory.survey().await;
        assert_eq!(survey.loads.get("local"), Some(&1), "the old sandbox still fills a place on local");
        assert_eq!(survey.loads.get("cloud"), Some(&1), "and the placement fills one on the cloud");
    }

    #[tokio::test]
    async fn a_holder_stays_put_only_while_its_target_is_up_and_uncapped() {
        let config = FactoryConfig::parse_table(CONFIG, "TRAIL", None).unwrap();
        let tracker = FakeTracker::new("factory");
        let codehost = FakeCodeHost::new();
        let placed: Vec<Placed> = config
            .target_specs()
            .iter()
            .map(|spec| Placed {
                target: Target::default().listed(spec),
                compute: Arc::new(FakeCompute::new()),
                daemon: Arc::new(LocalDaemon::new(
                    Arc::new(ScriptedHarness::new()),
                    Arc::new(codehost.clone()),
                    tracker.clone(),
                )),
                provider: Arc::new(FakeProvider::new(1)),
            })
            .collect();
        let factory =
            Factory::with_targets(config, placed, Arc::new(tracker.clone()), Host::new(&HostConfig::default()));

        let key = tracker.add_task("TRAIL", "held", "");
        tracker.set_run(&key, trailhead_factory::RunRecord { target: Some("local".into()), ..Default::default() });
        let task = tracker.show(&key).await.unwrap();
        let mut survey = factory.survey().await;
        survey.holders.insert(key.clone(), "local".into());
        assert!(factory.stays_put(&survey, &task));

        survey.down.insert("local".into());
        assert!(!factory.stays_put(&survey, &task), "a down target can't take it back");
        survey.down.clear();
        survey.capped.insert("local".into());
        assert!(!factory.stays_put(&survey, &task), "nor can a capped one");
        survey.capped.clear();
        survey.holders.insert(key, "cloud".into());
        assert!(!factory.stays_put(&survey, &task), "nor one that isn't where its sandbox is");
    }
}
