use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::Notify;
use trailhead_daemon::{RENEW_EVERY, Runner, Workspace};
use trailhead_factory::{
    Check, CheckResult, CodeHost, Daemon, Error, EventSink, Harness, InstallHost, PrepareRequest, Repo, Result,
    RunEvent, RunReport, RunRequest, SandboxId, Tracker,
};

pub use trailhead_daemon::Connect;

use crate::FakeTracker;

/// The commit every fake checkout is at.
pub const FAKE_BASE: &str = "base0000";
/// The base branch's head, as a fake checkout sees it while merging it into a PR branch.
pub const FAKE_BASE_MERGE: &str = "main0000";

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
    /// Whether the latest prepare left a merge of the base branch under way.
    merging: Mutex<bool>,
    /// Whether the agent starts a merge itself, which prepare then doesn't: `merge_head` sees one from its second call on.
    agent_merges: AtomicBool,
    merge_head_calls: AtomicUsize,
    /// Whether the agent aborts the merge: `merge_head` sees none after prepare, and HEAD lacks the base.
    abandons_merge: AtomicBool,
    /// Files the agent leaves conflict markers in.
    markers: Mutex<Vec<String>>,
    /// Tools in the fake checkout's own mise config.
    repo_tools: Mutex<Vec<String>>,
    prepares_started: AtomicUsize,
    prepare_gate: Mutex<Option<Arc<Notify>>>,
}

#[async_trait]
impl Workspace for FakeWorkspace {
    fn dir(&self) -> PathBuf {
        PathBuf::from("/work")
    }

    async fn install_hosts(&self, req: &PrepareRequest) -> Result<Vec<InstallHost>> {
        let stack: Vec<String> = req.tools.keys().cloned().collect();
        trailhead_daemon::install_hosts_for(&stack, &self.repo_tools.lock().unwrap(), &req.tool_hosts, false, false)
    }

    async fn prepare(&self, req: &PrepareRequest) -> Result<()> {
        self.prepares_started.fetch_add(1, Ordering::SeqCst);
        let gate = self.prepare_gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            gate.notified().await;
        }
        *self.branch.lock().unwrap() = req.branch.clone().map(|b| (req.repo.clone(), b));
        *self.merging.lock().unwrap() = req.merge_base && !self.agent_merges.load(Ordering::SeqCst);
        self.merge_head_calls.store(0, Ordering::SeqCst);
        self.prepared.lock().unwrap().push(req.clone());
        Ok(())
    }

    async fn confirm_blocked(&self, _hosts: &[String]) -> Result<()> {
        Ok(())
    }

    async fn merge_head(&self) -> Result<Option<String>> {
        let call = self.merge_head_calls.fetch_add(1, Ordering::SeqCst);
        if self.abandons_merge.load(Ordering::SeqCst) && call > 0 {
            return Ok(None);
        }
        let merging = *self.merging.lock().unwrap() || (self.agent_merges.load(Ordering::SeqCst) && call > 0);
        Ok(merging.then(|| FAKE_BASE_MERGE.to_string()))
    }

    async fn contains(&self, _commit: &str) -> Result<bool> {
        Ok(!self.abandons_merge.load(Ordering::SeqCst))
    }

    async fn conflict_markers(&self, _start: &str) -> Result<Vec<String>> {
        Ok(self.markers.lock().unwrap().clone())
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
    /// Errors the next runs fail with before the agent does anything, like a lost daemon.
    run_errors: Mutex<Vec<trailhead_factory::Error>>,
    /// Runs that are going in a sandbox from before this control plane: their events so far, and how they end.
    ongoing: Mutex<HashMap<String, Ongoing>>,
    attached: Mutex<Vec<String>>,
}

/// A run from before this control plane: its events so far, and its report, or `None` if it never ends by itself.
type Ongoing = (Vec<RunEvent>, Option<RunReport>);

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
            run_errors: Mutex::default(),
            ongoing: Mutex::default(),
            attached: Mutex::default(),
        }
    }

    /// Has the checkout's own mise config ask for these tools, as a `mise.toml` in the repo would.
    pub fn repo_mise_tools(&self, tools: &[&str]) {
        *self.workspace.repo_tools.lock().unwrap() = tools.iter().map(|t| t.to_string()).collect();
    }

    /// Has the agent leave conflict markers in these files.
    pub fn leave_conflict_markers(&self, files: &[&str]) {
        *self.workspace.markers.lock().unwrap() = files.iter().map(|f| f.to_string()).collect();
    }

    /// Has the agent start the merge of the base branch itself, as when prepare didn't.
    pub fn agent_merges(&self) {
        self.workspace.agent_merges.store(true, Ordering::SeqCst);
    }

    /// Has the agent abort the merge of the base branch that prepare started.
    pub fn agent_aborts_merge(&self) {
        self.workspace.abandons_merge.store(true, Ordering::SeqCst);
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

    /// Makes the next `times` runs fail with this error, as a daemon the control plane lost would.
    pub fn fail_runs(&self, times: usize, error: &str) {
        let mut errors = self.run_errors.lock().unwrap();
        for _ in 0..times {
            errors.push(trailhead_factory::Error::Other(error.to_string()));
        }
    }

    /// Makes the next `times` runs end cancelled, as when the control plane is stopped on purpose.
    pub fn cancel_runs(&self, times: usize) {
        let mut errors = self.run_errors.lock().unwrap();
        for _ in 0..times {
            errors.push(trailhead_factory::Error::Cancelled);
        }
    }

    /// A run already going in a sandbox, as after a control plane restart: an attach to it replays `events`, then
    /// ends with `report`. Any other run is unknown to the daemon.
    pub fn ongoing(&self, run: &str, events: Vec<RunEvent>, report: RunReport) {
        self.ongoing.lock().unwrap().insert(run.to_string(), (events, Some(report)));
    }

    /// [`LocalDaemon::ongoing`] for a run that never ends by itself: after the replay it waits to be cancelled.
    pub fn ongoing_hung(&self, run: &str, events: Vec<RunEvent>) {
        self.ongoing.lock().unwrap().insert(run.to_string(), (events, None));
    }

    /// The runs attached to so far, oldest first.
    pub fn attached(&self) -> Vec<String> {
        self.attached.lock().unwrap().clone()
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
    async fn install_hosts(&self, _sandbox: &SandboxId, req: &PrepareRequest) -> Result<Vec<InstallHost>> {
        self.runner.install_hosts(req).await
    }

    async fn prepare(&self, sandbox: &SandboxId, req: PrepareRequest) -> Result<()> {
        self.runner.prepare(&req).await?;
        self.prepared.lock().unwrap().push((sandbox.clone(), req));
        Ok(())
    }

    async fn run(&self, sandbox: &SandboxId, req: RunRequest, events: EventSink) -> Result<RunReport> {
        self.requests.lock().unwrap().push(req.clone());
        if let Some(error) = self.run_errors.lock().unwrap().pop() {
            return Err(error);
        }
        self.runner.run(req, events, self.cancel_handle(sandbox)).await
    }

    async fn attach(&self, sandbox: &SandboxId, run: &str, events: EventSink) -> Result<RunReport> {
        self.attached.lock().unwrap().push(run.to_string());
        let Some((replay, report)) = self.ongoing.lock().unwrap().remove(run) else {
            return Err(Error::NotFound(format!("run {run} on this daemon")));
        };
        for ev in replay {
            let _ = events.send(ev);
        }
        match report {
            Some(report) => Ok(report),
            None => {
                self.cancel_handle(sandbox).notified().await;
                Err(Error::Cancelled)
            }
        }
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
