use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::Notify;
use trailhead_daemon::{RENEW_EVERY, Runner, Workspace};
use trailhead_factory::{
    Check, CheckResult, CodeHost, Daemon, EventSink, Harness, PrepareRequest, Repo, Result, RunReport, RunRequest,
    SandboxId, Tracker,
};

pub use trailhead_daemon::Connect;

use crate::FakeTracker;

/// The commit every fake checkout is at.
pub const FAKE_BASE: &str = "base0000";

/// Where a branch's head is on the code host, for a fake checkout of that branch to start from.
pub type Heads = Arc<dyn Fn(&Repo, &str) -> Option<String> + Send + Sync>;

/// A checkout that doesn't exist: prepares are recorded and checks pass unless told to fail.
#[derive(Default)]
pub struct FakeWorkspace {
    /// Check name to its failing output and how many more runs fail (`None`: all of them).
    failing: Mutex<BTreeMap<String, (String, Option<usize>)>>,
    prepared: Mutex<Vec<PrepareRequest>>,
    /// The branch the checkout is on after the latest prepare, when it isn't the base.
    branch: Mutex<Option<(Repo, String)>>,
    heads: Mutex<Option<Heads>>,
    prepares_started: AtomicUsize,
    prepare_gate: Mutex<Option<Arc<Notify>>>,
}

#[async_trait]
impl Workspace for FakeWorkspace {
    fn dir(&self) -> PathBuf {
        PathBuf::from("/work")
    }

    async fn prepare(&self, req: &PrepareRequest) -> Result<()> {
        self.prepares_started.fetch_add(1, Ordering::SeqCst);
        let gate = self.prepare_gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            gate.notified().await;
        }
        *self.branch.lock().unwrap() = req.branch.clone().map(|b| (req.repo.clone(), b));
        self.prepared.lock().unwrap().push(req.clone());
        Ok(())
    }

    async fn confirm_blocked(&self, _hosts: &[String]) -> Result<()> {
        Ok(())
    }

    async fn head(&self) -> Result<String> {
        let on = self.branch.lock().unwrap().clone();
        let heads = self.heads.lock().unwrap().clone();
        match (on, heads) {
            (Some((repo, branch)), Some(heads)) => Ok(heads(&repo, &branch).unwrap_or_else(|| FAKE_BASE.to_string())),
            _ => Ok(FAKE_BASE.to_string()),
        }
    }

    async fn check(&self, check: &Check) -> CheckResult {
        let mut failing = self.failing.lock().unwrap();
        let output = match failing.get_mut(&check.name) {
            Some((output, None)) => Some(output.clone()),
            Some((output, Some(left))) => {
                *left -= 1;
                let output = output.clone();
                if *left == 0 {
                    failing.remove(&check.name);
                }
                Some(output)
            }
            None => None,
        };
        match output {
            Some(output) => CheckResult { name: check.name.clone(), passed: false, output },
            None => CheckResult { name: check.name.clone(), passed: true, output: String::new() },
        }
    }
}

/// The real daemon's [`Runner`] in-process, over a [`FakeWorkspace`], standing in for the iroh daemon.
pub struct LocalDaemon {
    runner: Arc<Runner>,
    workspace: Arc<FakeWorkspace>,
    ignore_cancel: AtomicBool,
    cancels: Mutex<HashMap<SandboxId, Arc<Notify>>>,
    prepared: Mutex<Vec<(SandboxId, PrepareRequest)>>,
    requests: Mutex<Vec<RunRequest>>,
}

impl LocalDaemon {
    /// A daemon whose sandboxes reach a [`FakeTracker`].
    pub fn new(harness: Arc<dyn Harness>, codehost: Arc<dyn CodeHost>, tracker: FakeTracker) -> LocalDaemon {
        let connect: Connect = Arc::new(move |cred| {
            let tracker = tracker.clone();
            Box::pin(async move { Ok(Arc::new(tracker.authenticate(&cred)?) as Arc<dyn Tracker>) })
        });
        LocalDaemon::connecting(harness, codehost, connect)
    }

    pub fn connecting(harness: Arc<dyn Harness>, codehost: Arc<dyn CodeHost>, connect: Connect) -> LocalDaemon {
        let workspace = Arc::new(FakeWorkspace::default());
        let runner = Runner { harness, codehost, workspace: workspace.clone(), connect, renew_every: RENEW_EVERY };
        LocalDaemon {
            runner: Arc::new(runner),
            workspace,
            ignore_cancel: AtomicBool::new(false),
            cancels: Mutex::default(),
            prepared: Mutex::default(),
            requests: Mutex::default(),
        }
    }

    /// Lets fake checkouts of a branch start at its head on the code host, as a rework's does.
    pub fn with_heads(self, heads: impl Fn(&Repo, &str) -> Option<String> + Send + Sync + 'static) -> LocalDaemon {
        *self.workspace.heads.lock().unwrap() = Some(Arc::new(heads));
        self
    }

    /// The runner, for serving it over another transport.
    pub fn runner(&self) -> Arc<Runner> {
        self.runner.clone()
    }

    /// Makes a named check fail with this output on every later run.
    pub fn fail_check(&self, name: &str, output: &str) {
        self.workspace.failing.lock().unwrap().insert(name.to_string(), (output.to_string(), None));
    }

    /// Makes a named check fail its next `times` runs, then pass. Zero times means it passes.
    pub fn fail_check_times(&self, name: &str, output: &str, times: usize) {
        let mut failing = self.workspace.failing.lock().unwrap();
        if times == 0 {
            failing.remove(name);
        } else {
            failing.insert(name.to_string(), (output.to_string(), Some(times)));
        }
    }

    pub fn pass_check(&self, name: &str) {
        self.workspace.failing.lock().unwrap().remove(name);
    }

    /// Makes every later prepare wait for `gate`, like a cold build.
    pub fn gate_prepare(&self, gate: Arc<Notify>) {
        *self.workspace.prepare_gate.lock().unwrap() = Some(gate);
    }

    /// Prepares the workspace has begun, including ones still running.
    pub fn prepares_started(&self) -> usize {
        self.workspace.prepares_started.load(Ordering::SeqCst)
    }

    /// Makes the daemon ignore cancels, like a wedged or hostile sandbox.
    pub fn ignore_cancel(&self) {
        self.ignore_cancel.store(true, Ordering::Relaxed);
    }

    /// Sandboxes prepared so far, oldest first.
    pub fn prepared(&self) -> Vec<(SandboxId, PrepareRequest)> {
        self.prepared.lock().unwrap().clone()
    }

    /// Run requests received so far, oldest first.
    pub fn requests(&self) -> Vec<RunRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn cancel_handle(&self, sandbox: &SandboxId) -> Arc<Notify> {
        self.cancels.lock().unwrap().entry(sandbox.clone()).or_default().clone()
    }
}

#[async_trait]
impl Daemon for LocalDaemon {
    async fn prepare(&self, sandbox: &SandboxId, req: PrepareRequest) -> Result<()> {
        self.runner.prepare(&req).await?;
        self.prepared.lock().unwrap().push((sandbox.clone(), req));
        Ok(())
    }

    async fn run(&self, sandbox: &SandboxId, req: RunRequest, events: EventSink) -> Result<RunReport> {
        self.requests.lock().unwrap().push(req.clone());
        self.runner.run(req, events, self.cancel_handle(sandbox)).await
    }

    async fn cancel(&self, sandbox: &SandboxId) -> Result<()> {
        if !self.ignore_cancel.load(Ordering::Relaxed) {
            self.cancel_handle(sandbox).notify_one();
        }
        Ok(())
    }

    async fn shutdown(&self) {
        let running: Vec<SandboxId> = self.cancels.lock().unwrap().keys().cloned().collect();
        for sandbox in running {
            let _ = self.cancel(&sandbox).await;
        }
    }
}
