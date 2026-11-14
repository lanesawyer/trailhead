use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Notify;
use trailhead_daemon::{HEARTBEAT_EVERY, RENEW_EVERY, Runner, Workspace};
use trailhead_factory::{
    Check, CheckResult, CodeHost, Daemon, Error, EventSink, Harness, HarnessKind, HarnessSetup, InstallHost,
    PrepareRequest, Registry, Repo, Result, RunEvent, RunReport, RunRequest, SandboxId, Tracker,
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
    /// Whether the agent commits the merge itself: `merge_head` sees none after prepare, but HEAD has the base.
    commits_merge: AtomicBool,
    /// Whether the agent starts the merge itself and commits it in the same session: `merge_head` never sees one, but
    /// `merged_base` finds the base's commits that HEAD gained.
    merges_and_commits: AtomicBool,
    /// Whether the agent rebases: the run's starting commit is no longer in HEAD's history.
    rebases: AtomicBool,
    /// Files the agent leaves conflict markers in.
    markers: Mutex<Vec<String>>,
    /// Files the agent changes, for path-scoped checks.
    changed: Mutex<Vec<String>>,
    prepares_started: AtomicUsize,
    /// Errors the next prepares fail with, before they change anything.
    prepare_errors: Mutex<VecDeque<String>>,
    /// Outputs the next prepares that run a `setup` check fail it with, after the checkout is otherwise ready.
    setup_errors: Mutex<VecDeque<String>>,
    prepare_gate: Mutex<Option<Arc<Notify>>>,
    /// How long each later prepare takes, on tokio's clock.
    prepare_takes: Mutex<Duration>,
    /// Whether the checkout holds work that exists nowhere else, and the remote branches asked about.
    unpublished: AtomicBool,
    unpublished_asked: Mutex<Vec<String>>,
    /// The remote branches that exist, when the test says; asking about any other is an error. None: all do.
    remote_branches: Mutex<Option<Vec<String>>>,
    /// Install hosts `install_hosts` names on top of the real ones, as a confused or compromised daemon would.
    extra_install_hosts: Mutex<Vec<InstallHost>>,
    /// The `[factory.stack] env` the latest prepare or run request set, and what each check ran with.
    env: Mutex<BTreeMap<String, String>>,
    check_envs: Mutex<Vec<(String, BTreeMap<String, String>)>>,
}

#[async_trait]
impl Workspace for FakeWorkspace {
    fn dir(&self) -> PathBuf {
        PathBuf::from("/work")
    }

    async fn install_hosts(&self, req: &PrepareRequest) -> Result<Vec<InstallHost>> {
        let stack: Vec<String> = req.tools.keys().cloned().collect();
        let mut hosts = trailhead_daemon::install_hosts_for(&stack, &req.tool_hosts, false)?;
        hosts.extend(self.extra_install_hosts.lock().unwrap().clone());
        Ok(hosts)
    }

    async fn prepare(&self, req: &PrepareRequest) -> Result<()> {
        self.set_env(&req.env);
        // The real workspace runs these with the env.
        for check in [&req.setup, &req.trim].into_iter().flatten() {
            self.check_envs.lock().unwrap().push((check.name.clone(), req.env.clone()));
        }
        self.prepares_started.fetch_add(1, Ordering::SeqCst);
        let gate = self.prepare_gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            gate.notified().await;
        }
        let takes = *self.prepare_takes.lock().unwrap();
        if !takes.is_zero() {
            tokio::time::sleep(takes).await;
        }
        if let Some(error) = self.prepare_errors.lock().unwrap().pop_front() {
            return Err(Error::Other(error));
        }
        if req.setup.is_some()
            && let Some(output) = self.setup_errors.lock().unwrap().pop_front()
        {
            *self.branch.lock().unwrap() = req.branch.clone().map(|b| (req.repo.clone(), b));
            return Err(Error::SetupFailed(output));
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
        if (self.abandons_merge.load(Ordering::SeqCst) || self.commits_merge.load(Ordering::SeqCst)) && call > 0 {
            return Ok(None);
        }
        let merging = *self.merging.lock().unwrap() || (self.agent_merges.load(Ordering::SeqCst) && call > 0);
        Ok(merging.then(|| FAKE_BASE_MERGE.to_string()))
    }

    async fn merged_base(&self, _start: &str, _base: &str) -> Result<Option<String>> {
        Ok(self.merges_and_commits.load(Ordering::SeqCst).then(|| FAKE_BASE_MERGE.to_string()))
    }

    async fn contains(&self, commit: &str) -> Result<bool> {
        if self.rebases.load(Ordering::SeqCst) && commit != FAKE_BASE_MERGE {
            return Ok(false);
        }
        Ok(!self.abandons_merge.load(Ordering::SeqCst))
    }

    async fn conflict_markers(&self, _start: &str, _base: &str) -> Result<Vec<String>> {
        Ok(self.markers.lock().unwrap().clone())
    }

    async fn changed_files(&self, _base: &str) -> Result<Vec<String>> {
        Ok(self.changed.lock().unwrap().clone())
    }

    async fn unpublished(&self, remote_branch: &str) -> Result<bool> {
        self.unpublished_asked.lock().unwrap().push(remote_branch.to_string());
        if self.remote_branches.lock().unwrap().as_ref().is_some_and(|b| !b.iter().any(|b| b == remote_branch)) {
            return Err(Error::Other(format!("no origin/{remote_branch}")));
        }
        Ok(self.unpublished.load(Ordering::SeqCst))
    }

    async fn head(&self) -> Result<String> {
        let on = self.branch.lock().unwrap().clone();
        let heads = self.heads.lock().unwrap().clone();
        match (on, heads) {
            (Some((repo, branch)), Some(heads)) => Ok(heads(&repo, &branch).unwrap_or_else(|| FAKE_BASE.to_string())),
            _ => Ok(FAKE_BASE.to_string()),
        }
    }

    fn set_env(&self, env: &BTreeMap<String, String>) {
        *self.env.lock().unwrap() = env.clone();
    }

    async fn check(&self, check: &Check) -> CheckResult {
        let env = self.env.lock().unwrap().clone();
        self.check_envs.lock().unwrap().push((check.name.clone(), env));
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
            Some(output) => CheckResult { name: check.name.clone(), passed: false, output, skipped: false },
            None => CheckResult { name: check.name.clone(), passed: true, output: String::new(), skipped: false },
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
    run_errors: Mutex<VecDeque<trailhead_factory::Error>>,
    /// Errors the next runs fail with once the agent has reported usage, like a daemon lost partway through.
    late_run_errors: Mutex<VecDeque<String>>,
    /// Runs that are going in a sandbox from before this control plane: their events so far, and how they end.
    ongoing: Mutex<HashMap<String, Ongoing>>,
    attached: Mutex<Vec<String>>,
    forgotten: Mutex<Vec<SandboxId>>,
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

    /// [`LocalDaemon::new`] with its harness built the way the real daemon builds its own: from a registry, by the
    /// `adapter` name the profile gives.
    pub fn from_registry(
        harnesses: &Registry<HarnessKind>,
        adapter: &str,
        codehost: Arc<dyn CodeHost>,
        tracker: FakeTracker,
    ) -> Result<LocalDaemon> {
        let setup = HarnessSetup { executable: "unused".into(), secrets: Default::default() };
        let harness = (harnesses.get(adapter)?.build)(&setup);
        Ok(LocalDaemon::new(harness, codehost, tracker))
    }

    pub fn connecting(harness: Arc<dyn Harness>, codehost: Arc<dyn CodeHost>, connect: Connect) -> LocalDaemon {
        let workspace = Arc::new(FakeWorkspace::default());
        let runner = Runner {
            harness,
            codehost,
            workspace: workspace.clone(),
            connect,
            renew_every: RENEW_EVERY,
            heartbeat_every: HEARTBEAT_EVERY,
        };
        LocalDaemon {
            runner: Arc::new(runner),
            workspace,
            ignore_cancel: AtomicBool::new(false),
            cancels: Mutex::default(),
            prepared: Mutex::default(),
            requests: Mutex::default(),
            run_errors: Mutex::default(),
            late_run_errors: Mutex::default(),
            ongoing: Mutex::default(),
            attached: Mutex::default(),
            forgotten: Mutex::default(),
        }
    }

    /// The sandboxes the control plane has told the daemon to forget, in order.
    pub fn forgotten(&self) -> Vec<SandboxId> {
        self.forgotten.lock().unwrap().clone()
    }

    /// Has the daemon send a heartbeat this often instead of every 30 seconds.
    pub fn heartbeat_every(mut self, every: Duration) -> LocalDaemon {
        Arc::get_mut(&mut self.runner).expect("the runner isn't shared yet").heartbeat_every = every;
        self
    }

    /// Has the agent change these files, which path-scoped checks match against.
    pub fn change_files(&self, files: &[&str]) {
        *self.workspace.changed.lock().unwrap() = files.iter().map(|f| f.to_string()).collect();
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

    /// Has the agent rebase its commits, so the commit the run started at is no longer an ancestor of HEAD.
    pub fn agent_rebases(&self) {
        self.workspace.rebases.store(true, Ordering::SeqCst);
    }

    /// Has the agent run `git commit` to finish the merge of the base branch that prepare started.
    pub fn agent_commits_merge(&self) {
        self.workspace.commits_merge.store(true, Ordering::SeqCst);
    }

    /// Has the agent start the merge of the base branch itself and commit it, so no merge is ever under way to see.
    pub fn agent_merges_and_commits(&self) {
        self.workspace.merges_and_commits.store(true, Ordering::SeqCst);
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

    /// Makes the next run fail with a permanent rejection, as a request the remote will never accept.
    pub fn reject_runs(&self, error: &str) {
        self.run_errors.lock().unwrap().push_back(trailhead_factory::Error::Rejected(error.to_string()));
    }

    /// Makes the next `times` runs fail with this error, as a daemon the control plane lost would.
    pub fn fail_runs(&self, times: usize, error: &str) {
        let mut errors = self.run_errors.lock().unwrap();
        for _ in 0..times {
            errors.push_back(trailhead_factory::Error::Other(error.to_string()));
        }
    }

    /// Makes the next `times` runs fail as a host short of a resource (its disk) would.
    pub fn unavailable_runs(&self, times: usize, error: &str) {
        let mut errors = self.run_errors.lock().unwrap();
        for _ in 0..times {
            errors.push_back(trailhead_factory::Error::Unavailable(error.to_string()));
        }
    }

    /// Makes the next `times` runs fail with this error right after the agent's first usage is reported, as a daemon
    /// lost partway through a run that has already spent money would.
    pub fn fail_runs_midway(&self, times: usize, error: &str) {
        let mut errors = self.late_run_errors.lock().unwrap();
        for _ in 0..times {
            errors.push_back(error.to_string());
        }
    }

    /// Makes the next `times` prepares fail with this error, as a checkout that won't clone or build would.
    pub fn fail_prepares(&self, times: usize, error: &str) {
        let mut errors = self.workspace.prepare_errors.lock().unwrap();
        for _ in 0..times {
            errors.push_back(error.to_string());
        }
    }

    /// Makes the next `times` prepares that run a `setup` check fail it with this output, as a build that no longer
    /// compiles would.
    pub fn fail_setups(&self, times: usize, output: &str) {
        let mut errors = self.workspace.setup_errors.lock().unwrap();
        for _ in 0..times {
            errors.push_back(output.to_string());
        }
    }

    /// Makes the next `times` runs end cancelled, as when the control plane is stopped on purpose.
    pub fn cancel_runs(&self, times: usize) {
        let mut errors = self.run_errors.lock().unwrap();
        for _ in 0..times {
            errors.push_back(trailhead_factory::Error::Cancelled);
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

    /// Makes every later prepare take `takes` on tokio's clock, for a test with paused time.
    pub fn prepare_takes(&self, takes: Duration) {
        *self.workspace.prepare_takes.lock().unwrap() = takes;
    }

    /// Makes the checkout hold work that exists nowhere else (an unpushed diff), or not.
    pub fn set_unpublished(&self, unpublished: bool) {
        self.workspace.unpublished.store(unpublished, Ordering::SeqCst);
    }

    /// The remote branches the control plane asked whether the checkout had unpublished work against, oldest first.
    pub fn unpublished_asked(&self) -> Vec<String> {
        self.workspace.unpublished_asked.lock().unwrap().clone()
    }

    /// Limits the remote branches the checkout can be compared with to `branches`; any other is an error, as in git.
    pub fn set_remote_branches(&self, branches: &[&str]) {
        *self.workspace.remote_branches.lock().unwrap() = Some(branches.iter().map(|b| b.to_string()).collect());
    }

    /// Makes `install_hosts` also ask for `host` on behalf of `tool`, whether or not the control plane allows it.
    pub fn add_install_host(&self, host: &str, tool: &str) {
        let need = InstallHost { host: host.into(), tool: tool.into() };
        self.workspace.extra_install_hosts.lock().unwrap().push(need);
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

    /// The environment each check ran with, in order, by check name.
    pub fn check_envs(&self) -> Vec<(String, BTreeMap<String, String>)> {
        self.workspace.check_envs.lock().unwrap().clone()
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

    async fn unpublished(&self, _sandbox: &SandboxId, remote_branch: &str) -> Result<bool> {
        self.runner.unpublished(remote_branch).await
    }

    async fn forget(&self, sandbox: &SandboxId) {
        self.forgotten.lock().unwrap().push(sandbox.clone());
    }

    async fn prepare(&self, sandbox: &SandboxId, req: PrepareRequest) -> Result<()> {
        self.runner.prepare(&req).await?;
        self.prepared.lock().unwrap().push((sandbox.clone(), req));
        Ok(())
    }

    async fn run(&self, sandbox: &SandboxId, req: RunRequest, events: EventSink) -> Result<RunReport> {
        self.requests.lock().unwrap().push(req.clone());
        if let Some(error) = self.run_errors.lock().unwrap().pop_front() {
            return Err(error);
        }
        let late = self.late_run_errors.lock().unwrap().pop_front();
        let Some(error) = late else {
            return self.runner.run(req, events, self.cancel_handle(sandbox)).await;
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let run = self.runner.run(req, tx, self.cancel_handle(sandbox));
        tokio::pin!(run);
        loop {
            tokio::select! {
                biased;
                event = rx.recv() => match event {
                    Some(event) => {
                        let spent = matches!(event, RunEvent::Usage { .. });
                        let _ = events.send(event);
                        if spent {
                            return Err(Error::Other(error));
                        }
                    }
                    None => return run.await,
                },
                report = &mut run => {
                    // The runner can send its events and finish within one poll; forward what it left behind.
                    while let Ok(event) = rx.try_recv() {
                        let spent = matches!(event, RunEvent::Usage { .. });
                        let _ = events.send(event);
                        if spent {
                            return Err(Error::Other(error));
                        }
                    }
                    return report;
                }
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn queued_prepare_errors_come_out_in_the_order_they_were_queued() {
        let ws = FakeWorkspace::default();
        {
            let mut errors = ws.prepare_errors.lock().unwrap();
            errors.push_back("first".into());
            errors.push_back("second".into());
        }
        let req = PrepareRequest {
            repo: Repo { owner: "o".into(), name: "r".into() },
            base_branch: "main".into(),
            branch: None,
            merge_base: false,
            branch_if_present: false,
            setup: None,
            tools: Default::default(),
            tool_hosts: Default::default(),
            env: Default::default(),
            trim: None,
            disk_warn_bytes: None,
            disk_limit_bytes: None,
        };
        for want in ["first", "second"] {
            let err = ws.prepare(&req).await.unwrap_err();
            assert!(err.to_string().contains(want), "{err}");
        }
        ws.prepare(&req).await.unwrap();
    }

    #[tokio::test]
    async fn queued_run_errors_of_mixed_kinds_come_out_in_the_order_they_were_queued() {
        use trailhead_factory::{Credential, Endpoint, ModelId, Role, TaskKey, TaskSpec};

        use crate::{FakeCodeHost, ScriptedHarness};

        let daemon = LocalDaemon::new(
            Arc::new(ScriptedHarness::new()),
            Arc::new(FakeCodeHost::new()),
            FakeTracker::new("factory"),
        );
        daemon.reject_runs("no");
        daemon.cancel_runs(1);
        let sandbox = SandboxId("s1".into());
        let request = || RunRequest {
            grade: None,
            run: "r1".into(),
            role: Role::Worker,
            task: TaskSpec {
                key: TaskKey("TRAIL-1".into()),
                title: "Add a greeting".into(),
                description: String::new(),
                previous_failure: None,
                context: Default::default(),
            },
            repo: Repo { owner: "o".into(), name: "r".into() },
            base_branch: "main".into(),
            branch: "factory/r1".into(),
            pr: None,
            feedback: None,
            model: ModelId("fake-sonnet".into()),
            endpoint: Endpoint { base_url: "http://gateway.invalid".into() },
            max_steps: 5,
            required_checks: Vec::new(),
            worker_checks: Vec::new(),
            blocked_hosts: Vec::new(),
            allowed_hosts: Vec::new(),
            env: Default::default(),
            credential: Credential { agent: "factory".into(), token: String::new() },
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let first = daemon.run(&sandbox, request(), tx.clone()).await.unwrap_err();
        assert!(matches!(first, Error::Rejected(ref m) if m == "no"), "{first:?}");
        let second = daemon.run(&sandbox, request(), tx).await.unwrap_err();
        assert!(matches!(second, Error::Cancelled), "{second:?}");
    }
}
