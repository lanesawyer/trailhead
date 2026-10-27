use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::{Notify, mpsc};
use trailhead_factory::{
    Check, CheckResult, CodeHost, Credential, Daemon, Error, EventSink, Harness, NewPr, PrepareRequest, Result, Role,
    RunContext, RunEvent, RunOutcome, RunReport, RunRequest, RunResult, SandboxId, Tracker, Usage, Verdict,
};

use crate::FakeTracker;

/// Turns a sandbox's credential into a tracker handle, as the daemon's REST client would.
pub type Connect =
    Arc<dyn Fn(Credential) -> Pin<Box<dyn Future<Output = Result<Arc<dyn Tracker>>> + Send>> + Send + Sync>;

/// The daemon's duties, run in-process: claim, run the harness, run checks,
/// publish and open the PR, or review it. Stands in for the iroh daemon until M3.
pub struct LocalDaemon {
    harness: Arc<dyn Harness>,
    codehost: Arc<dyn CodeHost>,
    connect: Connect,
    failing_checks: Mutex<BTreeMap<String, String>>,
    ignore_cancel: std::sync::atomic::AtomicBool,
    cancels: Mutex<HashMap<SandboxId, Arc<Notify>>>,
    prepared: Mutex<Vec<(SandboxId, PrepareRequest)>>,
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
        LocalDaemon {
            harness,
            codehost,
            connect,
            failing_checks: Mutex::default(),
            ignore_cancel: Default::default(),
            cancels: Mutex::default(),
            prepared: Mutex::default(),
        }
    }

    /// Makes a named check fail with this output on every later run.
    pub fn fail_check(&self, name: &str, output: &str) {
        self.failing_checks.lock().unwrap().insert(name.to_string(), output.to_string());
    }

    /// Makes the daemon ignore cancels, like a wedged or hostile sandbox.
    pub fn ignore_cancel(&self) {
        self.ignore_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Sandboxes prepared so far, oldest first.
    pub fn prepared(&self) -> Vec<(SandboxId, PrepareRequest)> {
        self.prepared.lock().unwrap().clone()
    }

    pub fn pass_check(&self, name: &str) {
        self.failing_checks.lock().unwrap().remove(name);
    }

    fn cancel_handle(&self, sandbox: &SandboxId) -> Arc<Notify> {
        self.cancels.lock().unwrap().entry(sandbox.clone()).or_default().clone()
    }

    fn checks(&self, checks: &[Check]) -> Vec<CheckResult> {
        let failing = self.failing_checks.lock().unwrap();
        checks
            .iter()
            .map(|c| match failing.get(&c.name) {
                Some(output) => CheckResult { name: c.name.clone(), passed: false, output: output.clone() },
                None => CheckResult { name: c.name.clone(), passed: true, output: String::new() },
            })
            .collect()
    }

    /// Runs the harness, forwarding its events and totting up usage, until it ends or is cancelled.
    async fn drive(&self, ctx: RunContext, cancel: Arc<Notify>, events: &EventSink) -> Result<(RunOutcome, Usage)> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let run = self.harness.run(ctx, tx);
        tokio::pin!(run);
        let mut usage = Usage::default();
        let mut forward = |ev: RunEvent| {
            if let RunEvent::Usage { usage: u, .. } = &ev {
                usage += *u;
            }
            let _ = events.send(ev);
        };
        let outcome = loop {
            tokio::select! {
                biased;
                _ = cancel.notified() => return Err(Error::Cancelled),
                Some(ev) = rx.recv() => forward(ev),
                out = &mut run => break out?,
            }
        };
        while let Ok(ev) = rx.try_recv() {
            forward(ev);
        }
        Ok((outcome, usage))
    }

    async fn work(
        &self,
        req: &RunRequest,
        tracker: &dyn Tracker,
        outcome: RunOutcome,
    ) -> Result<(RunResult, Vec<CheckResult>)> {
        let key = &req.task.key;
        match outcome {
            RunOutcome::Changes { commits, summary } => {
                let checks = self.checks(&req.required_checks);
                if let Some(failed) = checks.iter().find(|c| !c.passed) {
                    let reason = format!("check {} failed", failed.name);
                    tracker.release(key).await?;
                    return Ok((RunResult::Failed(reason), checks));
                }
                self.codehost.publish(&req.repo, &req.branch, &commits).await?;
                let pr = match &req.pr {
                    Some(pr) => pr.clone(),
                    None => {
                        let new = NewPr {
                            branch: req.branch.clone(),
                            base: req.base_branch.clone(),
                            title: format!("{}: {}", key.0, req.task.title),
                            body: summary,
                        };
                        self.codehost.open_pr(&req.repo, new).await?
                    }
                };
                tracker
                    .report(key, trailhead_factory::RunUpdate { pr: Some(pr.clone()), ..Default::default() })
                    .await?;
                tracker.request_review(key).await?;
                Ok((RunResult::PrOpened(pr), checks))
            }
            RunOutcome::Verdict { kind, body } => {
                let verdict =
                    Verdict { kind, body, reviewer: tracker.agent().to_string(), model: Some(req.model.clone()) };
                if let Some(pr) = &req.pr {
                    self.codehost.comment(pr, &verdict.body).await?;
                }
                tracker.record_verdict(key, &verdict).await?;
                Ok((RunResult::Reviewed(verdict), Vec::new()))
            }
            RunOutcome::NeedsInput { question } => {
                tracker.ask(key, &question).await?;
                Ok((RunResult::NeedsInput(question), Vec::new()))
            }
            RunOutcome::Failed { reason } => {
                if req.role == Role::Worker {
                    tracker.release(key).await?;
                }
                Ok((RunResult::Failed(reason), Vec::new()))
            }
        }
    }
}

#[async_trait]
impl Daemon for LocalDaemon {
    async fn prepare(&self, sandbox: &SandboxId, req: PrepareRequest) -> Result<()> {
        self.prepared.lock().unwrap().push((sandbox.clone(), req));
        Ok(())
    }

    async fn run(&self, sandbox: &SandboxId, req: RunRequest, events: EventSink) -> Result<RunReport> {
        let tracker = (self.connect)(req.credential.clone()).await?;
        let cancel = self.cancel_handle(sandbox);
        if req.role == Role::Worker {
            tracker.claim(&req.task.key).await?;
        }
        let diff = match &req.pr {
            Some(pr) if req.role == Role::Reviewer => Some(self.codehost.diff(pr).await?),
            _ => None,
        };
        let ctx = RunContext {
            role: req.role,
            task: req.task.clone(),
            workdir: PathBuf::from("/work"),
            model: req.model.clone(),
            endpoint: req.endpoint.clone(),
            max_steps: req.max_steps,
            diff,
        };
        let (outcome, usage) = self.drive(ctx, cancel, &events).await?;
        let (result, checks) = self.work(&req, tracker.as_ref(), outcome).await?;
        Ok(RunReport { result, checks, usage })
    }

    async fn cancel(&self, sandbox: &SandboxId) -> Result<()> {
        if self.ignore_cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(());
        }
        self.cancel_handle(sandbox).notify_one();
        Ok(())
    }
}
