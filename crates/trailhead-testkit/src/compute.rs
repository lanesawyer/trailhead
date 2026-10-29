use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use trailhead_factory::{
    CheckpointId, Compute, Egress, Error, Labels, Program, Result, Sandbox, SandboxId, SandboxSpec,
};

#[derive(Debug, Clone)]
struct Live {
    sandbox: Sandbox,
    egress: Egress,
    checkpoints: Vec<(CheckpointId, String)>,
    restores: Vec<CheckpointId>,
    programs: Vec<Program>,
    wakes: usize,
}

type StartHook = Arc<dyn Fn(SandboxId) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

#[derive(Clone, Default)]
pub struct FakeCompute {
    on_start: Arc<Mutex<Option<StartHook>>>,
    live: Arc<Mutex<BTreeMap<SandboxId, Live>>>,
    failing: Arc<Mutex<BTreeSet<(String, &'static str)>>>,
    wake_failures: Arc<Mutex<BTreeMap<String, usize>>>,
    hung_wakes: Arc<Mutex<BTreeSet<String>>>,
    rejected_wakes: Arc<Mutex<BTreeSet<String>>>,
    wake_attempts: Arc<Mutex<BTreeMap<String, usize>>>,
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

    /// Programs started in a sandbox, oldest first.
    pub fn programs(&self, name: &str) -> Vec<Program> {
        let live = self.live.lock().unwrap();
        live.get(&SandboxId(name.into())).map(|l| l.programs.clone()).unwrap_or_default()
    }

    /// Runs `f` after every `start`, as the supervisor would replace the process. A test uses it to swap the
    /// daemon a sandbox runs.
    pub fn on_start<F, Fut>(&self, f: F)
    where
        F: Fn(SandboxId) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        *self.on_start.lock().unwrap() = Some(Arc::new(move |id| Box::pin(f(id))));
    }

    /// Makes the next `n` wakes of a sandbox fail with a transient 5xx before they start succeeding.
    pub fn fail_wakes(&self, name: &str, n: usize) {
        self.wake_failures.lock().unwrap().insert(name.to_string(), n);
    }

    /// Makes every wake of a sandbox hang, as a stalled compute API would.
    pub fn hang_wakes(&self, name: &str) {
        self.hung_wakes.lock().unwrap().insert(name.to_string());
    }

    /// Makes every wake of a sandbox fail with a permanent 4xx, as a request the API will never accept.
    pub fn reject_wakes(&self, name: &str) {
        self.rejected_wakes.lock().unwrap().insert(name.to_string());
    }

    /// Wakes tried on a sandbox, failed ones included.
    pub fn wake_attempts(&self, name: &str) -> usize {
        self.wake_attempts.lock().unwrap().get(name).copied().unwrap_or(0)
    }

    pub fn wakes(&self, name: &str) -> usize {
        self.live.lock().unwrap().get(&SandboxId(name.into())).map_or(0, |l| l.wakes)
    }

    /// Makes one operation (`restore`, `set_egress`, `set_labels` or `destroy`) fail on a sandbox until
    /// [`FakeCompute::heal`], like a Sprite whose storage has gone bad.
    pub fn fail(&self, name: &str, op: &'static str) {
        self.failing.lock().unwrap().insert((name.to_string(), op));
    }

    pub fn heal(&self, name: &str) {
        self.failing.lock().unwrap().retain(|(n, _)| n != name);
    }

    fn check(&self, id: &SandboxId, op: &'static str) -> Result<()> {
        if self.failing.lock().unwrap().contains(&(id.0.clone(), op)) {
            return Err(Error::Other(format!("sprites api 503 Service Unavailable: Storage not ready ({op})")));
        }
        Ok(())
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
            programs: Vec::new(),
            wakes: 0,
        });
        Ok(entry.sandbox.clone())
    }

    async fn list(&self, labels: &Labels) -> Result<Vec<Sandbox>> {
        let found: Vec<Sandbox> = self
            .live
            .lock()
            .unwrap()
            .values()
            .filter(|l| labels.iter().all(|(k, v)| l.sandbox.labels.get(k) == Some(v)))
            .map(|l| l.sandbox.clone())
            .collect();
        // A real list is a network call: other tasks run before the answer is used.
        tokio::task::yield_now().await;
        Ok(found)
    }

    async fn checkpoint(&self, id: &SandboxId, note: &str) -> Result<CheckpointId> {
        self.with(id, |l| {
            let cp = CheckpointId(format!("{}-cp{}", id.0, l.checkpoints.len() + 1));
            l.checkpoints.push((cp.clone(), note.to_string()));
            Ok(cp)
        })
    }

    async fn restore(&self, id: &SandboxId, cp: &CheckpointId) -> Result<()> {
        self.check(id, "restore")?;
        self.with(id, |l| {
            if !l.checkpoints.iter().any(|(c, _)| c == cp) {
                return Err(Error::NotFound(format!("checkpoint {}", cp.0)));
            }
            l.restores.push(cp.clone());
            Ok(())
        })
    }

    async fn start(&self, id: &SandboxId, program: &Program) -> Result<()> {
        self.with(id, |l| {
            l.programs.retain(|p| p.name != program.name);
            l.programs.push(program.clone());
            Ok(())
        })?;
        let hook = self.on_start.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(id.clone()).await;
        }
        Ok(())
    }

    async fn wake(&self, id: &SandboxId) -> Result<()> {
        *self.wake_attempts.lock().unwrap().entry(id.0.clone()).or_default() += 1;
        if self.rejected_wakes.lock().unwrap().contains(&id.0) {
            return Err(Error::Rejected("sprites api 400 Bad Request: ".into()));
        }
        if self.hung_wakes.lock().unwrap().contains(&id.0) {
            std::future::pending::<()>().await;
        }
        if let Some(left) = self.wake_failures.lock().unwrap().get_mut(&id.0).filter(|n| **n > 0) {
            *left -= 1;
            return Err(Error::Other("sprites api 502 Bad Gateway: ".into()));
        }
        self.with(id, |l| {
            l.wakes += 1;
            Ok(())
        })
    }

    async fn set_egress(&self, id: &SandboxId, egress: &Egress) -> Result<()> {
        self.check(id, "set_egress")?;
        self.with(id, |l| {
            l.egress = egress.clone();
            Ok(())
        })
    }

    async fn set_labels(&self, id: &SandboxId, labels: &Labels) -> Result<()> {
        self.check(id, "set_labels")?;
        self.with(id, |l| {
            l.sandbox.labels = labels.clone();
            Ok(())
        })
    }

    async fn destroy(&self, id: &SandboxId) -> Result<()> {
        self.check(id, "destroy")?;
        self.live.lock().unwrap().remove(id);
        Ok(())
    }
}
