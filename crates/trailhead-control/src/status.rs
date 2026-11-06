//! The control plane's live view of itself (`docs/factory/ui.md#factory-status`): the snapshot and the stream of events
//! that change it. Kept in memory only. A boot rebuilds the sandboxes from the compute labels in its first reconcile,
//! and the runs start empty; the stream numbers from 1.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;
use trailhead_factory::{
    ControlState, FactoryEvent, FactorySnapshot, Grade, RunPhase, RunStatus, Sandbox, SandboxState, SandboxStatus,
    Sequenced, TargetSpend, TaskKey, Usd, label,
};

use crate::timing::unix_now;

/// How many events a slow subscriber may fall behind by before it misses some.
const BACKLOG: usize = 1024;

struct Inner {
    control: ControlState,
    last_error: Option<(i64, String)>,
    sandboxes: Vec<SandboxStatus>,
    runs: Vec<RunStatus>,
    holding: Option<String>,
    targets: Vec<TargetSpend>,
    human_grades: Vec<Grade>,
    seq: u64,
}

pub struct StatusBoard {
    inner: Mutex<Inner>,
    tx: broadcast::Sender<Sequenced>,
}

impl Default for StatusBoard {
    fn default() -> Self {
        StatusBoard {
            inner: Mutex::new(Inner {
                control: ControlState::Reconciling,
                last_error: None,
                sandboxes: Vec::new(),
                runs: Vec::new(),
                holding: None,
                targets: Vec::new(),
                human_grades: Vec::new(),
                seq: 0,
            }),
            tx: broadcast::channel(BACKLOG).0,
        }
    }
}

/// A sandbox as the board shows it, from its labels.
pub fn sandbox_status(s: &Sandbox, paused: bool) -> SandboxStatus {
    let get = |k: &str| s.labels.get(k).cloned();
    let state = match s.labels.get(label::STATE).map(String::as_str) {
        Some(label::IDLE) => SandboxState::Idle,
        Some(label::BROKEN) => SandboxState::Broken,
        _ if paused => SandboxState::Paused,
        _ => SandboxState::Busy,
    };
    SandboxStatus {
        name: s.name.clone(),
        target: get(label::TARGET),
        state,
        task: get(label::TASK).map(TaskKey),
        role: get(label::ROLE),
        warm: s.labels.contains_key(label::WARM),
        own: !s.labels.contains_key(label::POOL),
    }
}

fn snapshot_locked(inner: &Inner) -> FactorySnapshot {
    let mut sandboxes = inner.sandboxes.clone();
    sandboxes.sort_by(|a, b| a.name.cmp(&b.name));
    let mut runs = inner.runs.clone();
    runs.sort_by(|a, b| a.task.cmp(&b.task));
    FactorySnapshot {
        control: inner.control.clone(),
        last_error: inner.last_error.clone(),
        sandboxes,
        runs,
        holding: inner.holding.clone(),
        targets: inner.targets.clone(),
        human_grades: inner.human_grades.clone(),
        seq: inner.seq,
    }
}

impl StatusBoard {
    pub fn new() -> Arc<StatusBoard> {
        Arc::default()
    }

    /// Applies `change` and publishes the event it returns, under one lock so the stream's order is the snapshot's.
    fn apply(&self, change: impl FnOnce(&mut Inner) -> Option<FactoryEvent>) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(event) = change(&mut inner) {
            inner.seq += 1;
            // No subscriber is the usual case.
            let _ = self.tx.send(Sequenced { seq: inner.seq, event });
        }
    }

    pub fn snapshot(&self) -> FactorySnapshot {
        snapshot_locked(&self.inner.lock().unwrap())
    }

    /// The snapshot, and a receiver for every event after it.
    pub fn subscribe(&self) -> (FactorySnapshot, broadcast::Receiver<Sequenced>) {
        // Both under the lock `apply` publishes under, so the snapshot's `seq` is exactly where the stream continues.
        let inner = self.inner.lock().unwrap();
        let rx = self.tx.subscribe();
        (snapshot_locked(&inner), rx)
    }

    /// Sets why merges are held (`None`: they aren't). Whether it changed.
    pub fn holding(&self, holding: Option<String>) -> bool {
        let mut changed = false;
        self.apply(|i| {
            if i.holding == holding {
                return None;
            }
            changed = true;
            i.holding = holding.clone();
            Some(FactoryEvent::Holding { holding })
        });
        changed
    }

    /// Sets the spend of each capped target. It goes in the snapshot and not the stream, because it changes with the
    /// clock and not with an event.
    pub fn targets(&self, targets: Vec<TargetSpend>) {
        self.inner.lock().unwrap().targets = targets;
    }

    /// Sets the grades whose gate wants a person. It goes in the snapshot and not the stream, because only the config
    /// changes it.
    pub fn human_grades(&self, grades: Vec<Grade>) {
        self.inner.lock().unwrap().human_grades = grades;
    }

    pub fn control(&self, control: ControlState) {
        self.apply(|i| {
            if i.control == control {
                return None;
            }
            i.control = control.clone();
            Some(FactoryEvent::Control { control })
        });
    }

    pub fn error(&self, message: String) {
        self.apply(|i| {
            i.last_error = Some((unix_now(), message.clone()));
            Some(FactoryEvent::Error { message })
        });
    }

    /// Sets one sandbox, from its labels.
    pub fn sandbox(&self, sandbox: &Sandbox) {
        self.apply(|i| {
            let paused = i.sandboxes.iter().any(|s| s.name == sandbox.name && s.state == SandboxState::Paused);
            let status = sandbox_status(sandbox, paused);
            set_sandbox(i, status)
        });
    }

    pub fn sandbox_gone(&self, name: &str) {
        self.apply(|i| {
            let before = i.sandboxes.len();
            i.sandboxes.retain(|s| s.name != name);
            (i.sandboxes.len() != before).then(|| FactoryEvent::SandboxGone { name: name.to_string() })
        });
    }

    /// A parked task keeps its sandbox suspended.
    pub fn paused(&self, name: &str) {
        self.apply(|i| {
            let s = i.sandboxes.iter_mut().find(|s| s.name == name)?;
            if s.state != SandboxState::Busy {
                return None;
            }
            s.state = SandboxState::Paused;
            Some(FactoryEvent::SandboxChanged { sandbox: s.clone() })
        });
    }

    /// Makes the board's sandboxes those in `live`, and `kept` (on targets that didn't answer): what a reconcile
    /// listed is the truth, so this is also how a boot rebuilds them. A sandbox with no state label that no run is on
    /// is a parked task's, suspended, and so is a pooled one named in `parked` (the sandboxes of the tracker's NeedsInput
    /// tasks), which a board rebuilt at boot can't know.
    pub fn sync_sandboxes(&self, live: &[Sandbox], kept_targets: &HashSet<String>, parked: &HashSet<String>) {
        let mut events = Vec::new();
        self.apply(|i| {
            let mut next = Vec::new();
            for s in live {
                let running = s.labels.get(label::TASK).is_some_and(|t| i.runs.iter().any(|r| r.task.0 == *t));
                let known = parked.contains(&s.name)
                    || i.sandboxes.iter().any(|k| k.name == s.name && k.state == SandboxState::Paused);
                let own_idle = !s.labels.contains_key(label::POOL) && !running;
                next.push(sandbox_status(s, known || own_idle));
            }
            for old in &i.sandboxes {
                if old.target.as_ref().is_some_and(|t| kept_targets.contains(t))
                    && !next.iter().any(|n| n.name == old.name)
                {
                    next.push(old.clone());
                }
            }
            for old in &i.sandboxes {
                if !next.iter().any(|n| n.name == old.name) {
                    events.push(FactoryEvent::SandboxGone { name: old.name.clone() });
                }
            }
            for n in &next {
                if i.sandboxes.iter().find(|o| o.name == n.name) != Some(n) {
                    events.push(FactoryEvent::SandboxChanged { sandbox: n.clone() });
                }
            }
            i.sandboxes = next;
            None
        });
        // Published one by one so each gets its own number.
        for event in events {
            self.apply(|_| Some(event));
        }
    }

    /// A run entered `phase`, starting if it hadn't, on `sandbox` once it has one.
    pub fn phase(&self, task: &TaskKey, phase: RunPhase, sandbox: Option<&str>) {
        self.apply(|i| {
            match i.runs.iter_mut().find(|r| r.task == *task) {
                Some(r) if r.phase == phase && (sandbox.is_none() || r.sandbox.as_deref() == sandbox) => return None,
                Some(r) => {
                    r.phase = phase;
                    if sandbox.is_some() {
                        r.sandbox = sandbox.map(String::from);
                    }
                }
                None => i.runs.push(RunStatus {
                    task: task.clone(),
                    phase,
                    sandbox: sandbox.map(String::from),
                    steps: 0,
                    tool_calls: 0,
                    spend: Usd::ZERO,
                    last_tool: None,
                    started: unix_now(),
                }),
            }
            Some(FactoryEvent::RunPhase { task: task.clone(), phase })
        });
    }

    /// Starts a run at `phase`. The guard ends it when dropped, however the run ended.
    pub fn begin(self: &Arc<Self>, task: &TaskKey, phase: RunPhase) -> RunGuard {
        self.phase(task, phase, None);
        RunGuard { board: self.clone(), task: task.clone() }
    }

    pub fn run_ended(&self, task: &TaskKey) {
        self.apply(|i| {
            let before = i.runs.len();
            i.runs.retain(|r| r.task != *task);
            (i.runs.len() != before).then(|| FactoryEvent::RunEnded { task: task.clone() })
        });
    }

    pub fn step(&self, task: &TaskKey, step: u32, spend: Usd) {
        self.apply(|i| {
            let r = i.runs.iter_mut().find(|r| r.task == *task)?;
            r.steps = step;
            r.spend = spend;
            Some(FactoryEvent::Step { task: task.clone(), step, spend })
        });
    }

    pub fn spend(&self, task: &TaskKey, spend: Usd) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(r) = inner.runs.iter_mut().find(|r| r.task == *task) {
            r.spend = spend;
        }
    }

    pub fn tool_call(&self, task: &TaskKey, name: &str, detail: Option<&str>) {
        self.apply(|i| {
            let r = i.runs.iter_mut().find(|r| r.task == *task)?;
            r.tool_calls += 1;
            r.last_tool = Some(name.to_string());
            Some(FactoryEvent::ToolCall {
                task: task.clone(),
                name: name.to_string(),
                detail: detail.map(String::from),
            })
        });
    }
}

fn set_sandbox(i: &mut Inner, status: SandboxStatus) -> Option<FactoryEvent> {
    match i.sandboxes.iter_mut().find(|s| s.name == status.name) {
        Some(s) if *s == status => return None,
        Some(s) => *s = status.clone(),
        None => i.sandboxes.push(status.clone()),
    }
    Some(FactoryEvent::SandboxChanged { sandbox: status })
}

/// Ends a run on the board when dropped, however the run ended.
pub struct RunGuard {
    board: Arc<StatusBoard>,
    task: TaskKey,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        self.board.run_ended(&self.task);
    }
}

#[cfg(test)]
mod tests {
    use trailhead_factory::{Labels, SandboxId};

    use super::*;

    fn sandbox(name: &str, labels: &[(&str, &str)]) -> Sandbox {
        let labels: Labels = labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Sandbox { id: SandboxId(name.into()), name: name.into(), labels }
    }

    #[test]
    fn the_snapshot_carries_the_grades_that_want_a_person() {
        let board = StatusBoard::new();
        assert!(board.snapshot().human_grades.is_empty());
        board.human_grades(vec![Grade::ALL[1]]);
        assert_eq!(board.subscribe().0.human_grades, [Grade::ALL[1]]);
    }

    #[test]
    fn events_are_numbered_and_the_snapshot_agrees() {
        let board = StatusBoard::new();
        let (first, mut rx) = board.subscribe();
        assert_eq!(first.seq, 0);
        board.control(ControlState::Up);
        board.control(ControlState::Up); // unchanged: no event
        let key = TaskKey("TRAIL-1".into());
        let guard = board.begin(&key, RunPhase::Agent);
        board.step(&key, 1, Usd::dollars(0.5));
        board.tool_call(&key, "Bash", Some("ls"));
        let snap = board.snapshot();
        assert_eq!(snap.seq, 4);
        assert_eq!(snap.control, ControlState::Up);
        let run = &snap.runs[0];
        assert_eq!((run.steps, run.tool_calls, run.spend), (1, 1, Usd::dollars(0.5)));
        assert_eq!(run.last_tool.as_deref(), Some("Bash"));
        drop(guard);
        assert!(board.snapshot().runs.is_empty());
        let seqs: Vec<u64> = std::iter::from_fn(|| rx.try_recv().ok()).map(|e| e.seq).collect();
        assert_eq!(seqs, [1, 2, 3, 4, 5]);
    }

    #[test]
    fn a_subscription_continues_exactly_after_its_snapshot() {
        let board = StatusBoard::new();
        let publisher = {
            let board = board.clone();
            std::thread::spawn(move || {
                for i in 0..2000 {
                    board.error(format!("e{i}"));
                }
            })
        };
        for _ in 0..200 {
            let (snap, mut rx) = board.subscribe();
            if let Ok(next) = rx.try_recv() {
                assert_eq!(next.seq, snap.seq + 1);
            }
        }
        publisher.join().unwrap();
    }

    #[test]
    fn a_sync_rebuilds_sandboxes_from_labels_and_keeps_unreachable_targets() {
        let board = StatusBoard::new();
        let pooled = |n: &str, state: &str| {
            sandbox(n, &[("pool", "worker"), ("state", state), ("task", "TRAIL-1"), ("target", "a"), ("warm", "cp")])
        };
        board.sync_sandboxes(&[pooled("w1", "idle"), pooled("w2", "busy")], &HashSet::new(), &HashSet::new());
        let snap = board.snapshot();
        assert_eq!(snap.sandboxes.len(), 2);
        assert_eq!(snap.sandboxes[0].state, SandboxState::Idle);
        assert!(snap.sandboxes[1].warm);
        // A run's own sandbox that nothing is running on is a parked task's.
        board.sync_sandboxes(
            &[sandbox("trail-2-a1", &[("task", "TRAIL-2")])],
            &HashSet::from(["a".to_string()]),
            &HashSet::new(),
        );
        let snap = board.snapshot();
        assert_eq!(snap.sandboxes.len(), 3);
        let own = snap.sandboxes.iter().find(|s| s.name == "trail-2-a1").unwrap();
        assert_eq!(own.state, SandboxState::Paused);
        board.sync_sandboxes(&[], &HashSet::new(), &HashSet::new());
        assert!(board.snapshot().sandboxes.is_empty());
    }

    #[test]
    fn a_sync_shows_a_parked_tasks_pooled_sandbox_as_paused_after_a_boot() {
        let board = StatusBoard::new();
        let pooled = |n: &str| sandbox(n, &[("pool", "worker"), ("state", "busy"), ("task", "TRAIL-1")]);
        board.sync_sandboxes(&[pooled("w1"), pooled("w2")], &HashSet::new(), &HashSet::from(["w1".to_string()]));
        let snap = board.snapshot();
        assert_eq!(snap.sandboxes[0].state, SandboxState::Paused);
        assert_eq!(snap.sandboxes[1].state, SandboxState::Busy);
    }
}
