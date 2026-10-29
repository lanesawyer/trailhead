use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{Notify, mpsc};
use trailhead_factory::{
    Check, CheckResult, CodeHost, Credential, Error, EventSink, Harness, NewPr, PrRef, PrepareRequest, Result, Role,
    RunContext, RunEvent, RunOutcome, RunReport, RunRequest, RunResult, RunUpdate, Tracker, Usage, Verdict,
};

/// Turns a run's credential into a tracker handle acting as the sandbox's sub-agent.
pub type Connect =
    Arc<dyn Fn(Credential) -> Pin<Box<dyn Future<Output = Result<Arc<dyn Tracker>>> + Send>> + Send + Sync>;

/// The repo checkout inside a sandbox.
#[async_trait]
pub trait Workspace: Send + Sync {
    fn dir(&self) -> PathBuf;
    async fn prepare(&self, req: &PrepareRequest) -> Result<()>;
    async fn check(&self, check: &Check) -> CheckResult;
    /// Returns once none of `hosts` resolve, or fails if they still do after a while.
    async fn confirm_blocked(&self, hosts: &[String]) -> Result<()>;
    /// The commit sha the checkout is at.
    async fn head(&self) -> Result<String>;
}

/// How often a worker renews its claim while the agent runs. Leases default to 30 minutes.
pub const RENEW_EVERY: Duration = Duration::from_secs(10 * 60);

/// Everything the factory posts goes out under the connector's account (Lane's, until TRAIL-95), so posts say which
/// agent and model wrote them.
fn pr_footer(req: &RunRequest, agent: &str) -> String {
    format!("\n\n---\n🤖 Opened by the Trailhead factory · worker `{agent}` · `{}` · {}", req.model.0, req.task.key.0)
}

fn review_comment(v: &Verdict) -> String {
    let verdict = match v.kind {
        trailhead_factory::VerdictKind::Approve => "Approve",
        trailhead_factory::VerdictKind::RequestChanges => "Request changes",
    };
    let model = v.model.as_ref().map(|m| format!(" (`{}`)", m.0)).unwrap_or_default();
    format!("🔍 **Review by `{}`{model} · Verdict: {verdict}**\n\n{}", v.reviewer, v.body)
}

/// How much of a failed check's output goes on the task.
const NOTE_TAIL: usize = 4000;

/// Why an attempt's checks failed, for the task, so a person or the next attempt can see it.
fn failure_note(checks: &[CheckResult]) -> String {
    let mut note = String::from("The factory's checks failed, so nothing was published.\n");
    for c in checks.iter().filter(|c| !c.passed) {
        let out = c.output.trim_end();
        let mut start = out.len().saturating_sub(NOTE_TAIL);
        while !out.is_char_boundary(start) {
            start += 1;
        }
        note.push_str(&format!("\n`{}` failed; the end of its output:\n```\n{}\n```\n", c.name, &out[start..]));
    }
    note
}

/// The daemon's duties for one run: claim, run the harness, run checks, publish and open the PR, or review it,
/// and record the result in the tracker as the sandbox's sub-agent.
pub struct Runner {
    pub harness: Arc<dyn Harness>,
    pub codehost: Arc<dyn CodeHost>,
    pub workspace: Arc<dyn Workspace>,
    pub connect: Connect,
    pub renew_every: Duration,
}

impl Runner {
    pub async fn prepare(&self, req: &PrepareRequest) -> Result<()> {
        self.workspace.prepare(req).await
    }

    /// Runs one step until it finishes or `cancel` fires, which ends it with `Error::Cancelled`. A cancel during the
    /// checks stops them before anything is published; once publishing starts, the step runs to the end.
    pub async fn run(&self, req: RunRequest, events: EventSink, cancel: Arc<Notify>) -> Result<RunReport> {
        let key = &req.task.key.0;
        self.workspace.confirm_blocked(&req.blocked_hosts).await?;
        tracing::info!(task = %key, role = req.role.as_str(), "egress confirmed; starting");
        let tracker = (self.connect)(req.credential.clone()).await?;
        if req.role == Role::Worker {
            tracker.claim(&req.task.key).await?;
            tracing::info!(task = %key, "claimed");
        }
        let diff = match &req.pr {
            Some(pr) if req.role == Role::Reviewer => Some(self.codehost.diff(pr).await?),
            _ => None,
        };
        let ctx = RunContext {
            role: req.role,
            task: req.task.clone(),
            workdir: self.workspace.dir(),
            model: req.model.clone(),
            endpoint: req.endpoint.clone(),
            max_steps: req.max_steps,
            diff,
            checks: req.worker_checks.clone(),
        };
        tracing::info!(task = %key, "agent starting");
        // Where the agent's work starts, which is what it's published on top of.
        let start = self.workspace.head().await?;
        let (outcome, usage) = self.drive(&req, ctx, tracker.as_ref(), &cancel, &events).await?;
        tracing::info!(task = %key, "agent finished");
        let (result, checks) = self.finish(&req, tracker.as_ref(), outcome, &cancel, &start).await?;
        Ok(RunReport { result, checks, usage })
    }

    /// Runs the harness, forwarding its events and totting up usage, renewing a worker's claim as it goes.
    async fn drive(
        &self,
        req: &RunRequest,
        ctx: RunContext,
        tracker: &dyn Tracker,
        cancel: &Notify,
        events: &EventSink,
    ) -> Result<(RunOutcome, Usage)> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let run = match (&req.feedback, req.role) {
            (Some(feedback), Role::Worker) => self.harness.resume(ctx, feedback.clone(), tx),
            _ => self.harness.run(ctx, tx),
        };
        tokio::pin!(run);
        let mut renew = tokio::time::interval_at(tokio::time::Instant::now() + self.renew_every, self.renew_every);
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
                _ = renew.tick(), if req.role == Role::Worker => {
                    if let Err(e) = tracker.renew(&req.task.key).await {
                        tracing::warn!(task = %req.task.key.0, error = %e, "renewing the claim failed");
                    }
                }
                out = &mut run => break out?,
            }
        };
        while let Ok(ev) = rx.try_recv() {
            forward(ev);
        }
        Ok((outcome, usage))
    }

    async fn publish(
        &self,
        req: &RunRequest,
        agent: &str,
        start: &str,
        commits: Vec<trailhead_factory::Commit>,
        summary: String,
        retried: &[String],
    ) -> Result<PrRef> {
        self.codehost.publish(&req.repo, start, &req.branch, &commits).await?;
        let flaky: String =
            retried.iter().map(|name| format!("\n\n⚠️ Check `{name}` failed, then passed when rerun.")).collect();
        match &req.pr {
            Some(pr) => {
                // The PR is already open, so a note on a flaky check goes on it as a comment.
                if !flaky.is_empty() {
                    self.codehost.comment(pr, &format!("{}{}", flaky.trim_start(), pr_footer(req, agent))).await?;
                }
                Ok(pr.clone())
            }
            None => {
                let new = NewPr {
                    branch: req.branch.clone(),
                    base: req.base_branch.clone(),
                    title: format!("{}: {}", req.task.key.0, req.task.title),
                    body: format!("{summary}{flaky}{}", pr_footer(req, agent)),
                };
                self.codehost.open_pr(&req.repo, new).await
            }
        }
    }

    /// Runs each check, rerunning one that fails once in case it was a flake. Returns the final results and the
    /// names of checks that passed on the rerun.
    async fn checks(&self, checks: &[Check]) -> (Vec<CheckResult>, Vec<String>) {
        let mut out = Vec::with_capacity(checks.len());
        let mut retried = Vec::new();
        for check in checks {
            let mut result = self.workspace.check(check).await;
            if !result.passed {
                tracing::warn!(check = %check.name, "check failed; rerunning once");
                result = self.workspace.check(check).await;
                if result.passed {
                    retried.push(check.name.clone());
                }
            }
            out.push(result);
        }
        (out, retried)
    }

    async fn finish(
        &self,
        req: &RunRequest,
        tracker: &dyn Tracker,
        outcome: RunOutcome,
        cancel: &Notify,
        start: &str,
    ) -> Result<(RunResult, Vec<CheckResult>)> {
        let key = &req.task.key;
        match outcome {
            RunOutcome::Changes { commits, summary } => {
                let (checks, retried) = tokio::select! {
                    biased;
                    _ = cancel.notified() => return Err(Error::Cancelled),
                    done = self.checks(&req.required_checks) => done,
                };
                if let Some(failed) = checks.iter().find(|c| !c.passed) {
                    let reason = format!("check {} failed", failed.name);
                    let note = failure_note(&checks);
                    tracker.note(key, &note).await?;
                    tracker.report(key, RunUpdate { failure: Some(note), ..Default::default() }).await?;
                    tracker.release(key).await?;
                    return Ok((RunResult::Failed(reason), checks));
                }
                let pr = match self.publish(req, tracker.agent(), start, commits, summary, &retried).await {
                    Ok(pr) => pr,
                    // The agent's work is done and checked; retrying the whole run would only hit the same wall, so
                    // park the task with the reason instead, keeping the sandbox and its checkout.
                    Err(e) => {
                        let question = format!(
                            "The agent finished and the checks passed, but publishing to GitHub failed: {e}. Fix the \
                             cause, then reopen the task."
                        );
                        tracker.ask(key, &question).await?;
                        return Ok((RunResult::NeedsInput(question), checks));
                    }
                };
                tracker.report(key, RunUpdate { pr: Some(pr.clone()), ..Default::default() }).await?;
                tracker.request_review(key).await?;
                Ok((RunResult::PrOpened(pr), checks))
            }
            RunOutcome::Verdict { kind, body } => {
                let verdict =
                    Verdict { kind, body, reviewer: tracker.agent().to_string(), model: Some(req.model.clone()) };
                // Recorded first: if Trailhead refuses it, a retried review mustn't have already posted to GitHub.
                tracker.record_verdict(key, &verdict).await?;
                if let Some(pr) = &req.pr {
                    self.codehost.comment(pr, &review_comment(&verdict)).await?;
                }
                Ok((RunResult::Reviewed(verdict), Vec::new()))
            }
            RunOutcome::NeedsInput { question } => {
                tracker.ask(key, &question).await?;
                Ok((RunResult::NeedsInput(question), Vec::new()))
            }
            RunOutcome::Failed { reason } => {
                if req.role == Role::Worker {
                    let failure = Some(format!("The agent gave up: {reason}"));
                    tracker.report(key, RunUpdate { failure, ..Default::default() }).await?;
                    tracker.release(key).await?;
                }
                Ok((RunResult::Failed(reason), Vec::new()))
            }
        }
    }
}
