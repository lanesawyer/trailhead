//! The factory's control plane: starts sandboxes for ready tasks, enforces
//! budgets, dispatches reviews and reconciles on boot. It keeps no state of its
//! own; everything it needs is in the tracker and the compute host
//! (`docs/factory/state.md`).

mod core_tracker;

pub use core_tracker::{CoreTracker, METADATA_KEY};

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use trailhead_factory::{
    Compute, Daemon, Egress, Error, FactoryConfig, Labels, ModelProvider, PrRef, Profile, Result, Role, RunEvent,
    RunReport, RunRequest, RunResult, RunUpdate, Sandbox, SandboxSpec, Status, Task, TaskSpec, Tracker, Usd, Verdict,
    label, review_name, run_name,
};

/// How long a daemon has to stop after a cancel before the control plane cuts its sandbox off regardless.
pub const CANCEL_GRACE: Duration = Duration::from_secs(60);

/// The `factory` label value on every sandbox this control plane starts.
pub const FACTORY_LABEL: &str = "trailhead";

pub struct Factory {
    pub config: FactoryConfig,
    pub compute: Arc<dyn Compute>,
    pub tracker: Arc<dyn Tracker>,
    pub provider: Arc<dyn ModelProvider>,
    pub daemon: Arc<dyn Daemon>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The PR is open and the agent review is recorded; a human merges.
    Reviewed { pr: PrRef, verdict: Verdict },
    /// The PR is open but the review didn't finish; the reconciler retries it.
    AwaitingReview { pr: PrRef },
    /// The task is parked for a human; its sandbox is kept for resuming.
    Parked { reason: String },
    /// The attempt failed and the task went back to the queue.
    Failed { reason: String },
}

/// What one sandbox run came to, before the lifecycle acts on it.
enum Run {
    Report(RunReport, Usd),
    Breach(String, Usd),
}

#[derive(Debug, Default, PartialEq)]
pub struct Reconciled {
    pub destroyed: Vec<String>,
    pub requeued: Vec<String>,
    pub reviewed: Vec<String>,
}

impl Factory {
    fn project(&self) -> &str {
        &self.config.repo.project
    }

    fn base_labels(&self) -> Labels {
        Labels::from([
            (label::FACTORY.into(), FACTORY_LABEL.into()),
            (label::REPO.into(), self.config.repo.repo().slug()),
        ])
    }

    fn agent_egress(&self) -> Egress {
        Egress { domains: self.config.egress.always.clone() }
    }

    fn branch(task: &Task) -> String {
        format!("factory/{}", task.key.0.to_ascii_lowercase())
    }

    /// Starts runs for ready tasks, up to the repo's sandbox cap.
    pub async fn tick(&self) -> Result<Vec<(Task, Outcome)>> {
        let mut workers = self.base_labels();
        workers.insert(label::ROLE.into(), Role::Worker.as_str().into());
        let live = self.compute.list(&workers).await?.len();
        let room = self.config.repo.max_sandboxes.saturating_sub(live);
        if room == 0 {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for task in self.tracker.ready(self.project(), room).await? {
            let outcome = self.run_task(&task).await?;
            out.push((task, outcome));
        }
        Ok(out)
    }

    /// One attempt at a task: a fresh sandbox, the worker, then the reviewer.
    pub async fn run_task(&self, task: &Task) -> Result<Outcome> {
        let attempt = task.run.attempt.unwrap_or(0) + 1;
        let name = run_name(&task.key, attempt);
        let profile = self.config.worker().clone();
        let credential = self.tracker.create_sub_agent(&name, self.project(), token_ttl(&profile)).await?;
        let sub = self.tracker.sub_agent(&name);
        // Claiming as the sub-agent before creating the sandbox keeps two control planes from racing for one task.
        sub.claim(&task.key).await?;
        sub.report(
            &task.key,
            RunUpdate {
                attempt: Some(attempt),
                sandbox: Some(name.clone()),
                review_rounds: Some(0),
                profile: Some(self.config.repo.worker.clone()),
                ..RunUpdate::default()
            },
        )
        .await?;
        tracing::info!(task = %task.key.0, sandbox = %name, "starting worker");

        let sandbox = self.create_sandbox(&name, task, Role::Worker).await?;
        self.compute.checkpoint(&sandbox.id, &format!("before attempt {attempt}")).await?;
        let req = RunRequest {
            role: Role::Worker,
            task: spec(task),
            repo: self.config.repo.repo(),
            base_branch: self.config.repo.base_branch.clone(),
            branch: Self::branch(task),
            pr: task.run.pr.clone(),
            model: self.provider.resolve(profile.model),
            endpoint: self.provider.endpoint(),
            max_steps: profile.budget.steps,
            required_checks: self.config.required_checks(),
            credential,
        };
        let prior = task.run.spend.unwrap_or_default();
        let run = self.run_metered(&sandbox, req, &profile, prior).await?;

        let (report, spend) = match run {
            Run::Breach(reason, spend) => {
                tracing::warn!(task = %task.key.0, %reason, "budget breached; parking");
                self.compute.checkpoint(&sandbox.id, &format!("parked: {reason}")).await?;
                self.cut_off(&sandbox).await?;
                sub.report(&task.key, RunUpdate { spend: Some(spend), ..RunUpdate::default() }).await?;
                sub.ask(
                    &task.key,
                    &format!("Budget breached: {reason}. The sandbox {name} is suspended; raise the budget or cancel."),
                )
                .await?;
                return Ok(Outcome::Parked { reason });
            }
            Run::Report(report, spend) => (report, spend),
        };
        sub.report(&task.key, RunUpdate { spend: Some(spend), ..RunUpdate::default() }).await?;

        match report.result {
            RunResult::PrOpened(pr) => {
                // MVP: no suspend during review; requested changes start a fresh sandbox from the PR branch.
                self.compute.destroy(&sandbox.id).await?;
                self.tracker.revoke_sub_agent(&name).await?;
                self.review(task, &pr, attempt).await
            }
            RunResult::NeedsInput(question) => Ok(Outcome::Parked { reason: question }),
            RunResult::Failed(reason) => {
                tracing::warn!(task = %task.key.0, %reason, "attempt failed");
                self.compute.destroy(&sandbox.id).await?;
                self.tracker.revoke_sub_agent(&name).await?;
                Ok(Outcome::Failed { reason })
            }
            RunResult::Reviewed(_) => Err(Error::Other("a worker returned a review".into())),
        }
    }

    /// Runs the reviewer on an open PR in its own fresh sandbox.
    pub async fn review(&self, task: &Task, pr: &PrRef, attempt: u32) -> Result<Outcome> {
        let round = self.tracker.show(&task.key).await?.run.review_rounds.unwrap_or(0) + 1;
        let name = review_name(&task.key, attempt, round);
        self.tracker.report(&task.key, RunUpdate { review_rounds: Some(round), ..RunUpdate::default() }).await?;
        let profile = self.config.reviewer().clone();
        let credential = self.tracker.create_sub_agent(&name, self.project(), token_ttl(&profile)).await?;
        let sub = self.tracker.sub_agent(&name);
        let sandbox = self.create_sandbox(&name, task, Role::Reviewer).await?;
        tracing::info!(task = %task.key.0, sandbox = %name, "starting reviewer");
        let req = RunRequest {
            role: Role::Reviewer,
            task: spec(task),
            repo: self.config.repo.repo(),
            base_branch: self.config.repo.base_branch.clone(),
            branch: Self::branch(task),
            pr: Some(pr.clone()),
            model: self.provider.resolve(profile.model),
            endpoint: self.provider.endpoint(),
            max_steps: profile.budget.steps,
            required_checks: Vec::new(),
            credential,
        };
        let prior = self.tracker.show(&task.key).await?.run.spend.unwrap_or_default();
        let run = self.run_metered(&sandbox, req, &profile, prior).await;
        self.compute.destroy(&sandbox.id).await?;
        let run = run?;
        let (spend, reviewed) = match &run {
            Run::Report(RunReport { result: RunResult::Reviewed(_), .. }, s) => (*s, Some(attempt)),
            Run::Report(_, s) | Run::Breach(_, s) => (*s, None),
        };
        sub.report(&task.key, RunUpdate { spend: Some(spend), reviewed, ..RunUpdate::default() }).await?;
        self.tracker.revoke_sub_agent(&name).await?;
        match run {
            Run::Report(RunReport { result: RunResult::Reviewed(verdict), .. }, _) => {
                Ok(Outcome::Reviewed { pr: pr.clone(), verdict })
            }
            Run::Report(report, _) => {
                tracing::warn!(task = %task.key.0, result = ?report.result, "review didn't produce a verdict");
                Ok(Outcome::AwaitingReview { pr: pr.clone() })
            }
            Run::Breach(reason, _) => {
                tracing::warn!(task = %task.key.0, %reason, "reviewer breached its budget");
                Ok(Outcome::AwaitingReview { pr: pr.clone() })
            }
        }
    }

    async fn create_sandbox(&self, name: &str, task: &Task, role: Role) -> Result<Sandbox> {
        let mut labels = self.base_labels();
        labels.insert(label::TASK.into(), task.key.0.clone());
        labels.insert(label::ROLE.into(), role.as_str().into());
        let spec = SandboxSpec { name: name.to_string(), labels, egress: self.agent_egress(), base: None };
        self.compute.create(&spec).await
    }

    /// The kill switch: without its role label a sandbox loses connector access, and without egress it reaches nothing.
    async fn cut_off(&self, sandbox: &Sandbox) -> Result<()> {
        let mut labels = sandbox.labels.clone();
        labels.remove(label::ROLE);
        self.compute.set_labels(&sandbox.id, &labels).await?;
        self.compute.set_egress(&sandbox.id, &Egress::none()).await
    }

    /// Runs one step in a sandbox, pricing usage as it streams in and cancelling on a breach.
    async fn run_metered(&self, sandbox: &Sandbox, req: RunRequest, profile: &Profile, prior: Usd) -> Result<Run> {
        let limit = profile.budget.usd();
        let max_steps = profile.budget.steps;
        let deadline = tokio::time::sleep(Duration::from_secs(u64::from(profile.budget.minutes) * 60));
        tokio::pin!(deadline);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let run = self.daemon.run(&sandbox.id, req, tx);
        tokio::pin!(run);

        let mut spent = Usd::ZERO;
        let mut steps = 0;
        let mut breach: Option<String> = None;
        let mut on_event = |ev: RunEvent, spent: &mut Usd| -> Option<String> {
            match ev {
                RunEvent::Usage { model, usage } => *spent += self.provider.price(&model, &usage),
                RunEvent::StepStarted { .. } => steps += 1,
                _ => {}
            }
            if *spent > limit {
                Some(format!("spent {spent} of a {limit} budget"))
            } else if steps > max_steps {
                Some(format!("took more than {max_steps} steps"))
            } else {
                None
            }
        };
        // Armed on a breach: a daemon that doesn't stop by then is cut off without waiting for it.
        let grace = tokio::time::sleep(Duration::MAX / 4);
        tokio::pin!(grace);
        let result = loop {
            let new_breach = tokio::select! {
                biased;
                Some(ev) = rx.recv() => if breach.is_none() { on_event(ev, &mut spent) } else { None },
                r = &mut run => break r,
                _ = &mut deadline, if breach.is_none() => Some(format!("ran past {} minutes", profile.budget.minutes)),
                _ = &mut grace, if breach.is_some() => {
                    tracing::warn!(sandbox = %sandbox.name, "daemon ignored cancel; cutting it off");
                    break Err(Error::Cancelled);
                }
            };
            if let Some(reason) = new_breach {
                if let Err(e) = self.daemon.cancel(&sandbox.id).await {
                    tracing::warn!(sandbox = %sandbox.name, error = %e, "couldn't cancel; relying on the kill switch");
                }
                grace.as_mut().reset(tokio::time::Instant::now() + CANCEL_GRACE);
                breach = Some(reason);
            }
        };
        while let Ok(ev) = rx.try_recv() {
            on_event(ev, &mut spent);
        }
        let total = prior + spent;
        match (result, breach) {
            (Err(Error::Cancelled), Some(reason)) => Ok(Run::Breach(reason, total)),
            // The step finished before the cancel landed; its result stands.
            (Ok(report), _) => Ok(Run::Report(report, total)),
            (Err(e), _) => Err(e),
        }
    }

    /// Rebuilds the control plane's view after a restart: destroys sandboxes no
    /// task accounts for, requeues tasks whose run was interrupted, and retries
    /// reviews that didn't finish.
    pub async fn reconcile(&self) -> Result<Reconciled> {
        let mut done = Reconciled::default();
        let live = self.compute.list(&self.base_labels()).await?;
        let tasks = self.tracker.in_flight(self.project()).await?;

        for sandbox in &live {
            let parked = tasks
                .iter()
                .any(|t| t.status == Status::NeedsInput && t.run.sandbox.as_deref() == Some(sandbox.name.as_str()));
            if parked {
                continue;
            }
            self.compute.destroy(&sandbox.id).await?;
            done.destroyed.push(sandbox.name.clone());
        }

        for task in &tasks {
            let Some(name) = &task.run.sandbox else { continue };
            match task.status {
                Status::InProgress => {
                    let sub = self.tracker.sub_agent(name);
                    if task.holder.as_deref() == Some(sub.agent()) {
                        sub.release(&task.key).await?;
                    }
                    self.tracker.revoke_sub_agent(name).await?;
                    done.requeued.push(task.key.0.clone());
                }
                Status::Open => self.tracker.revoke_sub_agent(name).await?,
                Status::InReview => {
                    let attempt = task.run.attempt.unwrap_or(1);
                    if let Some(pr) = &task.run.pr
                        && task.run.reviewed != Some(attempt)
                    {
                        if let Some(round) = task.run.review_rounds.filter(|r| *r > 0) {
                            self.tracker.revoke_sub_agent(&review_name(&task.key, attempt, round)).await?;
                        }
                        self.review(task, pr, attempt).await?;
                        done.reviewed.push(task.key.0.clone());
                    }
                }
                _ => {}
            }
        }
        Ok(done)
    }
}

/// A sandbox's token outlives its wall-clock budget by a margin, so a missed revoke cleans itself up.
fn token_ttl(profile: &Profile) -> Duration {
    Duration::from_secs((u64::from(profile.budget.minutes) + 30) * 60)
}

fn spec(task: &Task) -> TaskSpec {
    TaskSpec { key: task.key.clone(), title: task.title.clone(), description: task.description.clone() }
}
