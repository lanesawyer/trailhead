use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use trailhead_factory::{CheckpointId, Compute, Egress, Error, Labels, Result, Sandbox, SandboxId, SandboxSpec};

#[derive(Debug, Clone)]
struct Live {
    sandbox: Sandbox,
    egress: Egress,
    checkpoints: Vec<(CheckpointId, String)>,
    restores: Vec<CheckpointId>,
}

#[derive(Clone, Default)]
pub struct FakeCompute {
    live: Arc<Mutex<BTreeMap<SandboxId, Live>>>,
}

impl FakeCompute {
    pub fn new() -> FakeCompute {
        FakeCompute::default()
    }

    pub fn sandboxes(&self) -> Vec<Sandbox> {
        self.live.lock().unwrap().values().map(|l| l.sandbox.clone()).collect()
    }

    pub fn get(&self, name: &str) -> Option<Sandbox> {
        self.live.lock().unwrap().get(&SandboxId(name.into())).map(|l| l.sandbox.clone())
    }

    pub fn egress(&self, name: &str) -> Option<Egress> {
        self.live.lock().unwrap().get(&SandboxId(name.into())).map(|l| l.egress.clone())
    }

    /// Checkpoint notes, oldest first.
    pub fn checkpoints(&self, name: &str) -> Vec<String> {
        let live = self.live.lock().unwrap();
        live.get(&SandboxId(name.into()))
            .map(|l| l.checkpoints.iter().map(|(_, n)| n.clone()).collect())
            .unwrap_or_default()
    }

    pub fn restores(&self, name: &str) -> Vec<CheckpointId> {
        let live = self.live.lock().unwrap();
        live.get(&SandboxId(name.into())).map(|l| l.restores.clone()).unwrap_or_default()
    }

    fn with<T>(&self, id: &SandboxId, f: impl FnOnce(&mut Live) -> Result<T>) -> Result<T> {
        let mut live = self.live.lock().unwrap();
        let l = live.get_mut(id).ok_or_else(|| Error::NotFound(format!("sandbox {}", id.0)))?;
        f(l)
    }
}

#[async_trait]
impl Compute for FakeCompute {
    async fn create(&self, spec: &SandboxSpec) -> Result<Sandbox> {
        let id = SandboxId(spec.name.clone());
        let mut live = self.live.lock().unwrap();
        let entry = live.entry(id.clone()).or_insert_with(|| Live {
            sandbox: Sandbox { id, name: spec.name.clone(), labels: spec.labels.clone() },
            egress: spec.egress.clone(),
            checkpoints: Vec::new(),
            restores: Vec::new(),
        });
        Ok(entry.sandbox.clone())
    }

    async fn list(&self, labels: &Labels) -> Result<Vec<Sandbox>> {
        let live = self.live.lock().unwrap();
        Ok(live
            .values()
            .filter(|l| labels.iter().all(|(k, v)| l.sandbox.labels.get(k) == Some(v)))
            .map(|l| l.sandbox.clone())
            .collect())
    }

    async fn checkpoint(&self, id: &SandboxId, note: &str) -> Result<CheckpointId> {
        self.with(id, |l| {
            let cp = CheckpointId(format!("{}-cp{}", id.0, l.checkpoints.len() + 1));
            l.checkpoints.push((cp.clone(), note.to_string()));
            Ok(cp)
        })
    }

    async fn restore(&self, id: &SandboxId, cp: &CheckpointId) -> Result<()> {
        self.with(id, |l| {
            if !l.checkpoints.iter().any(|(c, _)| c == cp) {
                return Err(Error::NotFound(format!("checkpoint {}", cp.0)));
            }
            l.restores.push(cp.clone());
            Ok(())
        })
    }

    async fn set_egress(&self, id: &SandboxId, egress: &Egress) -> Result<()> {
        self.with(id, |l| {
            l.egress = egress.clone();
            Ok(())
        })
    }

    async fn set_labels(&self, id: &SandboxId, labels: &Labels) -> Result<()> {
        self.with(id, |l| {
            l.sandbox.labels = labels.clone();
            Ok(())
        })
    }

    async fn destroy(&self, id: &SandboxId) -> Result<()> {
        self.live.lock().unwrap().remove(id);
        Ok(())
    }
}
