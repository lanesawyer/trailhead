use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{Notify, mpsc};
use trailhead_factory::messages::{self, Filed};
use trailhead_factory::{
    Check, CheckResult, CodeHost, Credential, Error, EventSink, FollowUp, Harness, InstallHost, NewPr, PrRef,
    PrepareRequest, Result, Role, RunContext, RunEvent, RunOutcome, RunReport, RunRequest, RunResult, RunUpdate,
    TaskKey, Tracker, Usage, Verdict, VerdictKind,
};

/// Turns a run's credential into a tracker handle acting as the sandbox's sub-agent.
pub type Connect =
    Arc<dyn Fn(Credential) -> Pin<Box<dyn Future<Output = Result<Arc<dyn Tracker>>> + Send>> + Send + Sync>;

/// The repo checkout inside a sandbox.
#[async_trait]
pub trait Workspace: Send + Sync {
    fn dir(&self) -> PathBuf;
    /// Which hosts installing `req`'s tools needs, worked out offline.
    async fn install_hosts(&self, _req: &PrepareRequest) -> Result<Vec<InstallHost>> {
        Ok(Vec::new())
    }
    async fn prepare(&self, req: &PrepareRequest) -> Result<()>;
    async fn check(&self, check: &Check) -> CheckResult;
    /// The `[factory.stack] env` that `check` and `agent_env` add on top of mise's environment, until it's set again.
    fn set_env(&self, _env: &BTreeMap<String, String>) {}
    /// Environment for the agent on top of the daemon's: the `[stack]` tools' mise environment, when there are any.
    async fn agent_env(&self) -> BTreeMap<String, String> {
        BTreeMap::new()
    }
    /// Returns once none of `hosts` resolve, or fails if they still do after a while.
    async fn confirm_blocked(&self, hosts: &[String]) -> Result<()>;
    /// The commit sha the checkout is at.
    async fn head(&self) -> Result<String>;
    /// The commit being merged into the checkout, while a merge is under way (`PrepareRequest::merge_base`).
    async fn merge_head(&self) -> Result<Option<String>>;
    /// The tip of `origin/<base>`, when a merge commit in `start..HEAD` has it as a parent: a merge of the base branch
    /// the agent started and committed itself, which leaves no `MERGE_HEAD` behind.
    async fn merged_base(&self, start: &str, base: &str) -> Result<Option<String>>;
    /// Whether `commit` is an ancestor of (or is) the checkout's HEAD.
    async fn contains(&self, commit: &str) -> Result<bool>;
    /// The files that have `<<<<<<<` or `>>>>>>>` lines added since `start`, committed or not.
    async fn conflict_markers(&self, start: &str) -> Result<Vec<String>>;
    /// The files the checkout changes against the base branch's remote head (`origin/<base>`), committed or not.
    async fn changed_files(&self, base: &str) -> Result<Vec<String>>;
    /// Whether the checkout has uncommitted changes or commits that are not on `origin/<remote_branch>`.
    async fn unpublished(&self, remote_branch: &str) -> Result<bool>;
}

/// What became of the merge of the base branch that a run's checkout started with, if any.
#[derive(PartialEq)]
enum Merge {
    None,
    /// Under way or committed: published as a merge commit.
    Merged,
    /// Abandoned by the agent; the commit that was being merged is not in its tree.
    Lost(String),
}

/// How often a worker renews its claim while the agent runs. Leases default to 30 minutes.
pub const RENEW_EVERY: Duration = Duration::from_secs(10 * 60);

/// How often the daemon tells the control plane how quiet the agent has been, while a run is going.
pub const HEARTBEAT_EVERY: Duration = Duration::from_secs(30);

fn pr_footer(req: &RunRequest, agent: &str) -> String {
    messages::pr_footer(agent, &req.model, &req.task.key)
}

/// How long an agent has been quiet.
struct Quiet {
    since: tokio::time::Instant,
    tools: u32,
}

impl Quiet {
    fn note(&mut self, ev: &RunEvent) {
        match ev {
            RunEvent::ToolCall { .. } => self.tools += 1,
            RunEvent::ToolResult => self.tools = self.tools.saturating_sub(1),
            _ => {}
        }
        self.since = tokio::time::Instant::now();
    }

    /// Whole seconds of quiet, not counting a tool call's running time.
    fn secs(&self) -> u64 {
        if self.tools > 0 { 0 } else { self.since.elapsed().as_secs() }
    }
}

/// The most follow-ups one review may file, so a runaway reviewer can't flood the project.
const MAX_FOLLOW_UPS: usize = 10;

/// The daemon's duties for one run: claim, run the harness, run checks, publish and open the PR, or review it,
/// and record the result in the tracker as the sandbox's sub-agent.
/// How long publishing keeps retrying a transient failure before the task is parked.
const PUBLISH_RETRY_FOR: Duration = Duration::from_secs(10 * 60);

pub struct Runner {
    pub harness: Arc<dyn Harness>,
    pub codehost: Arc<dyn CodeHost>,
    pub workspace: Arc<dyn Workspace>,
    pub connect: Connect,
    pub renew_every: Duration,
    pub heartbeat_every: Duration,
}

impl Runner {
    pub async fn install_hosts(&self, req: &PrepareRequest) -> Result<Vec<InstallHost>> {
        self.workspace.install_hosts(req).await
    }

    pub async fn prepare(&self, req: &PrepareRequest) -> Result<()> {
        self.workspace.prepare(req).await
    }

    pub async fn unpublished(&self, remote_branch: &str) -> Result<bool> {
        self.workspace.unpublished(remote_branch).await
    }

    /// Runs one step until it finishes or `cancel` fires, which ends it with `Error::Cancelled`. A cancel during the
    /// checks stops them before anything is published; once publishing starts, the step runs to the end.
    pub async fn run(&self, req: RunRequest, events: EventSink, cancel: Arc<Notify>) -> Result<RunReport> {
        let key = &req.task.key.0;
        self.workspace.set_env(&req.env);
        self.workspace.confirm_blocked(&req.blocked_hosts).await?;
        tracing::info!(task = %key, role = req.role.as_str(), "egress confirmed; starting");
        let tracker = (self.connect)(req.credential.clone()).await?;
        if req.role == Role::Worker {
            tracker.claim(&req.task.key).await?;
            tracing::info!(task = %key, "claimed");
        }
        let (diff, title) = match &req.pr {
            Some(pr) if req.role == Role::Reviewer => {
                // Only so the reviewer can check it; a review goes ahead without it.
                let title = match self.codehost.title(pr).await {
                    Ok(t) => Some(t),
                    Err(e) => {
                        tracing::warn!(task = %key, error = %e, "couldn't read the PR's title");
                        None
                    }
                };
                (Some(self.codehost.diff(pr).await?), title)
            }
            _ => (None, None),
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
            title,
            base_branch: (req.role == Role::Reviewer).then(|| req.base_branch.clone()),
            env: self.workspace.agent_env().await,
        };
        tracing::info!(task = %key, "agent starting");
        // Where the agent's work starts, which is what it's published on top of.
        let start = self.workspace.head().await?;
        let merging_before = self.workspace.merge_head().await?;
        let (mut outcome, usage) = self.drive(&req, ctx, tracker.as_ref(), &cancel, &events).await?;
        // A merge of the base branch is published as one, or the PR would still conflict. It may be one prepare
        // started or one the agent did itself, so read it again now, preferring that.
        let now = self.workspace.merge_head().await?;
        // A merge the agent abandoned (`git merge --abort`, a rebase) leaves a tree without the base's changes, so
        // naming the base as a parent would make merging the PR revert what landed there.
        let mut lost_merge = None;
        if let (None, Some(before)) = (&now, &merging_before)
            && matches!(outcome, RunOutcome::Changes { .. })
            && !self.workspace.contains(before).await?
        {
            lost_merge = Some(before.clone());
        }
        let mut merging = now.or(merging_before).filter(|_| lost_merge.is_none());
        // One the agent both started and committed leaves no MERGE_HEAD to see, only a merge commit with the base as a
        // parent.
        if merging.is_none() && lost_merge.is_none() && matches!(outcome, RunOutcome::Changes { .. }) {
            merging = self.workspace.merged_base(&start, &req.base_branch).await?;
        }
        let merge = match (&merging, lost_merge) {
            (_, Some(base)) => Merge::Lost(base),
            (Some(_), None) => Merge::Merged,
            (None, None) => Merge::None,
        };
        if let (Some(merging), RunOutcome::Changes { commits, .. }) = (merging, &mut outcome)
            && let Some(last) = commits.last_mut()
        {
            last.merges.push(merging);
        }
        tracing::info!(task = %key, "agent finished");
        // The checks can go quiet for minutes (a build, the tests), which isn't the agent stalling.
        let beating = async {
            loop {
                tokio::time::sleep(self.heartbeat_every).await;
                let _ = events.send(RunEvent::Heartbeat { quiet_secs: 0 });
            }
        };
        let finished = tokio::select! {
            done = self.finish(&req, tracker.as_ref(), outcome, &cancel, &start, merge, &events) => done,
            () = beating => unreachable!("heartbeats don't end"),
        };
        let (result, checks) = match finished {
            Ok(done) => done,
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            // The agent's run is over, so the control plane is told how it came out rather than an error, which it
            // takes for an infrastructure failure and hands the attempt back (TRAIL-214).
            Err(e) => {
                tracing::warn!(task = %key, error = %e, "finishing the run failed");
                if req.role == Role::Worker
                    && let Err(e) = tracker.release(&req.task.key).await
                {
                    tracing::warn!(task = %key, error = %e, "couldn't release the task");
                }
                (RunResult::Failed(format!("finishing the run failed: {e}")), vec![])
            }
        };
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
        let mut beat =
            tokio::time::interval_at(tokio::time::Instant::now() + self.heartbeat_every, self.heartbeat_every);
        let mut usage = Usage::default();
        // When the agent last did anything, and how many tool calls are running: time spent in a tool isn't quiet.
        let mut quiet = Quiet { since: tokio::time::Instant::now(), tools: 0 };
        let mut forward = |ev: RunEvent, quiet: &mut Quiet| {
            if let RunEvent::Usage { usage: u, .. } = &ev {
                usage += *u;
            }
            quiet.note(&ev);
            let _ = events.send(ev);
        };
        let outcome = loop {
            tokio::select! {
                biased;
                _ = cancel.notified() => return Err(Error::Cancelled),
                Some(ev) = rx.recv() => forward(ev, &mut quiet),
                _ = beat.tick() => {
                    let quiet_secs = quiet.secs();
                    let _ = events.send(RunEvent::Heartbeat { quiet_secs });
                }
                _ = renew.tick(), if req.role == Role::Worker => {
                    if let Err(e) = tracker.renew(&req.task.key).await {
                        tracing::warn!(task = %req.task.key.0, error = %e, "renewing the claim failed");
                    }
                }
                out = &mut run => break out?,
            }
        };
        while let Ok(ev) = rx.try_recv() {
            forward(ev, &mut quiet);
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
        notes: &str,
    ) -> Result<PrRef> {
        // Checked here as well as in the harness, so no harness can open a PR the changelog would drop.
        let title = commits
            .first()
            .and_then(|c| c.message.lines().next())
            .filter(|t| {
                t.strip_suffix(&format!(" [{}]", req.task.key.0)).is_some_and(trailhead_factory::is_conventional)
            })
            .map(str::to_string);
        self.publish_with_retries(req, start, &commits).await?;
        match &req.pr {
            Some(pr) => {
                // The PR is already open, so a note on a flaky or skipped check goes on it as a comment.
                if !notes.is_empty() {
                    self.codehost.comment(pr, &format!("{}{}", notes.trim_start(), pr_footer(req, agent))).await?;
                }
                Ok(pr.clone())
            }
            None => {
                let new = NewPr {
                    branch: req.branch.clone(),
                    base: req.base_branch.clone(),
                    title: title.unwrap_or_else(|| {
                        trailhead_factory::conventional_title(None, &req.task.key, &req.task.title).0
                    }),
                    body: format!("{summary}{notes}{}", pr_footer(req, agent)),
                };
                self.codehost.open_pr(&req.repo, new).await
            }
        }
    }

    /// Publishes, asking again after a transient failure (a 5xx, a timeout) with a growing wait, until the waits add up
    /// to `PUBLISH_RETRY_FOR`. Anything else the host answers (a 403, a 422, a moved branch) is returned at once.
    /// Repeating is safe: blobs and trees are content-addressed, an unreferenced commit is harmless, and the branch
    /// ref moves last, only from `start`.
    async fn publish_with_retries(
        &self,
        req: &RunRequest,
        start: &str,
        commits: &[trailhead_factory::Commit],
    ) -> Result<()> {
        let mut wait = Duration::from_secs(5);
        let mut waited = Duration::ZERO;
        loop {
            match self.codehost.publish(&req.repo, start, &req.branch, commits).await {
                Err(Error::Other(e)) if waited < PUBLISH_RETRY_FOR => {
                    tracing::warn!(task = %req.task.key.0, error = %e, ?wait, "publishing failed; retrying");
                    tokio::time::sleep(wait).await;
                    waited += wait;
                    wait = (wait * 2).min(Duration::from_secs(120));
                }
                done => return done,
            }
        }
    }

    /// Files each follow-up as a task in the reviewed task's project, unqueued, pointing back at the task and PR. A
    /// title already open is reused. One that can't be filed is logged and skipped: the review stands without it.
    async fn file_follow_ups(
        &self,
        tracker: &dyn Tracker,
        source: &TaskKey,
        pr: Option<&PrRef>,
        follow_ups: &[FollowUp],
    ) -> Vec<Filed> {
        let project = source.0.rsplit_once('-').map_or(source.0.as_str(), |(p, _)| p);
        let mut filed: Vec<Filed> = Vec::new();
        for f in follow_ups.iter().take(MAX_FOLLOW_UPS) {
            let title = f.title.trim().to_string();
            if title.is_empty() || filed.iter().any(|x| x.title.eq_ignore_ascii_case(&title)) {
                continue;
            }
            let result = match tracker.find_open_by_title(project, &title).await {
                Ok(Some(key)) => Ok(Filed { key, title: title.clone(), existing: true }),
                Ok(None) => {
                    let description =
                        messages::follow_up_description(&f.description, source, pr.map(|p| p.url.as_str()));
                    tracker.create_task(project, &title, &description, f.priority).await.map(|key| Filed {
                        key,
                        title: title.clone(),
                        existing: false,
                    })
                }
                Err(e) => Err(e),
            };
            match result {
                Ok(f) => filed.push(f),
                Err(e) => tracing::warn!(task = %source.0, %title, error = %e, "couldn't file a review follow-up"),
            }
        }
        filed
    }

    /// Runs each check that applies to the change, rerunning one that fails once in case it was a flake. A check
    /// scoped to paths the change didn't touch is reported as skipped. Returns the final results and the names of
    /// checks that passed on the rerun.
    async fn checks(&self, base: &str, checks: &[Check]) -> (Vec<CheckResult>, Vec<String>) {
        let mut out = Vec::with_capacity(checks.len());
        let mut retried = Vec::new();
        // Only worked out when a check is scoped. If it can't be, every check runs: a skip must not hide a failure.
        let changed = if checks.iter().any(|c| !c.paths.is_empty()) {
            match self.workspace.changed_files(base).await {
                Ok(files) => Some(files),
                Err(e) => {
                    tracing::warn!(error = %e, "couldn't list the changed files; running every check");
                    None
                }
            }
        } else {
            None
        };
        for check in checks {
            if changed.as_ref().is_some_and(|files| !check.applies_to(files)) {
                out.push(CheckResult::skipped(&check.name));
                continue;
            }
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

    #[allow(clippy::too_many_arguments)]
    async fn finish(
        &self,
        req: &RunRequest,
        tracker: &dyn Tracker,
        outcome: RunOutcome,
        cancel: &Notify,
        start: &str,
        merge: Merge,
        events: &EventSink,
    ) -> Result<(RunResult, Vec<CheckResult>)> {
        let key = &req.task.key;
        match outcome {
            RunOutcome::Changes { commits, summary } => {
                if let Merge::Lost(base) = &merge {
                    let (reason, note) = messages::merge_abandoned(base);
                    tracker.note(key, &note).await?;
                    tracker.report(key, RunUpdate { failure: Some(note), ..Default::default() }).await?;
                    tracker.release(key).await?;
                    return Ok((RunResult::Failed(reason), vec![]));
                }
                // A rebase rewrites the agent's commits, so the checkout no longer builds on `start`, which is what
                // the commits are published on top of. Publishing would drop or duplicate what the branch had.
                if !self.workspace.contains(start).await? {
                    let (reason, note) = messages::branch_rewritten(start);
                    tracker.note(key, &note).await?;
                    tracker.report(key, RunUpdate { failure: Some(note), ..Default::default() }).await?;
                    tracker.release(key).await?;
                    return Ok((RunResult::Failed(reason), vec![]));
                }
                // The checks only catch markers in files they compile, so look for them in everything changed.
                if merge == Merge::Merged {
                    let files = self.workspace.conflict_markers(start).await?;
                    if !files.is_empty() {
                        let (reason, note) = messages::conflict_markers_left(&files);
                        tracker.note(key, &note).await?;
                        tracker.report(key, RunUpdate { failure: Some(note), ..Default::default() }).await?;
                        tracker.release(key).await?;
                        return Ok((RunResult::Failed(reason), vec![]));
                    }
                }
                let _ = events.send(RunEvent::Checking);
                let (checks, retried) = tokio::select! {
                    biased;
                    _ = cancel.notified() => return Err(Error::Cancelled),
                    done = self.checks(&req.base_branch, &req.required_checks) => done,
                };
                if let Some(failed) = checks.iter().find(|c| !c.passed) {
                    let reason = messages::check_failed_reason(&failed.name);
                    let note = messages::checks_failed_note(&checks);
                    tracker.note(key, &note).await?;
                    tracker.report(key, RunUpdate { failure: Some(note), ..Default::default() }).await?;
                    tracker.release(key).await?;
                    return Ok((RunResult::Failed(reason), checks));
                }
                let _ = events.send(RunEvent::Publishing);
                let notes = format!("{}{}", messages::flaky_checks(&retried), messages::skipped_checks(&checks));
                let pr = match self.publish(req, tracker.agent(), start, commits, summary, &notes).await {
                    Ok(pr) => pr,
                    // The agent's work is done and checked; retrying the whole run would only hit the same wall, so
                    // park the task with the reason instead, keeping the sandbox and its checkout.
                    Err(e) => {
                        let question = messages::publish_failed_question(&e);
                        tracker.ask(key, &question).await?;
                        return Ok((RunResult::NeedsInput(question), checks));
                    }
                };
                // The PR is open and the work checked, so a tracker that fails now doesn't fail the run: the
                // control plane records the PR and the review request itself when it sees `PrOpened`.
                let mut written = false;
                for attempt in 0..3u64 {
                    if attempt > 0 {
                        tokio::time::sleep(Duration::from_millis(250 * attempt)).await;
                    }
                    let result =
                        match tracker.report(key, RunUpdate { pr: Some(pr.clone()), ..Default::default() }).await {
                            Ok(()) => tracker.request_review(key).await,
                            Err(e) => Err(e),
                        };
                    match result {
                        Ok(()) => {
                            written = true;
                            break;
                        }
                        Err(e) => tracing::warn!(task = %key.0, attempt, error = %e, "couldn't record the opened PR"),
                    }
                }
                if !written {
                    tracing::warn!(task = %key.0, "the PR is open but the tracker wouldn't take it; leaving it to the control plane");
                }
                Ok((RunResult::PrOpened(pr), checks))
            }
            RunOutcome::Verdict { kind, body, follow_ups, raise_grade } => {
                let verdict = Verdict {
                    kind,
                    body,
                    reviewer: tracker.agent().to_string(),
                    model: Some(req.model.clone()),
                    raise_grade,
                };
                // Recorded first: if Trailhead refuses it, a retried review mustn't have already posted to GitHub.
                tracker.record_verdict(key, &verdict).await?;
                // Only an approval files them: a rework may well fix what a request for changes mentions.
                let filed = if kind == VerdictKind::Approve {
                    self.file_follow_ups(tracker, key, req.pr.as_ref(), &follow_ups).await
                } else {
                    Vec::new()
                };
                if let Some(pr) = &req.pr {
                    // The grade as the control plane will settle it, with the reviewer's raise.
                    let mut grade = req.grade.clone();
                    if let (Some(g), Some(raise)) = (&mut grade, &verdict.raise_grade) {
                        g.raise(raise.grade, &raise.reason);
                    }
                    self.codehost.comment(pr, &messages::review_comment(&verdict, grade.as_ref(), &filed)).await?;
                }
                Ok((RunResult::Reviewed(verdict), Vec::new()))
            }
            RunOutcome::NeedsInput { question } => {
                tracker.ask(key, &question).await?;
                Ok((RunResult::NeedsInput(question), Vec::new()))
            }
            RunOutcome::Failed { reason } => {
                if req.role == Role::Worker {
                    let failure = Some(messages::agent_gave_up(&reason));
                    tracker.report(key, RunUpdate { failure, ..Default::default() }).await?;
                    tracker.release(key).await?;
                }
                Ok((RunResult::Failed(reason), Vec::new()))
            }
        }
    }
}
