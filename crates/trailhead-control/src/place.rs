//! Where a run goes (`docs/factory/targets.md`): the first target, by priority, that is reachable and under its
//! limit. Workers and reviewers are placed one at a time and on their own, so a review needn't share a target with the
//! worker whose PR it checks.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use trailhead_factory::{Daemon, Labels, ModelProvider, Role, Sandbox, TaskKey, label};

use crate::{Factory, Placed};

/// How full each target is, read once and then added to as tasks are placed.
pub(crate) struct Survey {
    loads: HashMap<String, usize>,
    /// Targets whose compute didn't answer: nothing is placed there, and nothing is assumed about what is.
    pub(crate) down: HashSet<String>,
}

/// A worker's or review's place on a target, held from the moment it is chosen until it ends, because its sandbox
/// isn't there to count until the run gets that far.
pub(crate) struct Placement {
    placed: Arc<Mutex<HashMap<(TaskKey, Role), String>>>,
    key: (TaskKey, Role),
}

impl Drop for Placement {
    fn drop(&mut self) {
        self.placed.lock().unwrap().remove(&self.key);
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

    /// How full each target is now.
    pub(crate) async fn survey(&self) -> Survey {
        let placed = self.placed.lock().unwrap().clone();
        let mut loads: HashMap<String, usize> = HashMap::new();
        let mut down = HashSet::new();
        for p in &self.targets {
            match self.held_on(p).await {
                Ok(held) => {
                    let counted = held
                        .iter()
                        .filter(|s| {
                            let task = s.labels.get(label::TASK).cloned().unwrap_or_default();
                            !placed.contains_key(&(TaskKey(task), role_of(s)))
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
        Survey { loads, down }
    }

    /// The first target that is reachable and under its `max_sandboxes`, now one fuller. `None` when none has room.
    pub(crate) fn place<'a>(&'a self, survey: &mut Survey) -> Option<&'a Placed> {
        let chosen = self.targets.iter().find(|p| {
            let name = &p.target.name;
            !survey.down.contains(name)
                && p.target.max_sandboxes.is_none_or(|max| survey.loads.get(name).copied().unwrap_or(0) < max)
        })?;
        *survey.loads.entry(chosen.target.name.clone()).or_default() += 1;
        Some(chosen)
    }

    /// Holds `task`'s place as `role` on `target` until the guard drops.
    pub(crate) fn hold(&self, task: &TaskKey, role: Role, target: &Placed) -> Placement {
        let key = (task.clone(), role);
        self.placed.lock().unwrap().insert(key.clone(), target.target.name.clone());
        Placement { placed: self.placed.clone(), key }
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
