//! The factory's control plane: starts sandboxes for ready tasks, enforces
//! budgets, dispatches reviews and reconciles on boot. It keeps no state of its
//! own; everything it needs is in the tracker and the compute host
//! (`docs/factory/state.md`).

mod core_tracker;
pub mod daemon_binary;
mod decide;
mod lifecycle;
mod merger;
mod messages;
mod pool;
mod reconcile;
mod review;
mod run_loop;
mod timing;

pub use core_tracker::{CoreTracker, METADATA_KEY};

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::Instant;
use trailhead_factory::{
    ChangedFile, CodeHost, Compute, Daemon, Egress, FactoryConfig, IROH_DOMAINS, Labels, MergeMode, ModelProvider,
    PrRef, Task, TaskKey, Tracker, Verdict, label,
};

use crate::pool::Trouble;

/// How long a daemon has to stop after a cancel before the control plane cuts its sandbox off regardless.
pub const CANCEL_GRACE: Duration = Duration::from_secs(60);

/// How long a stopping control plane waits for its cancelled runs to end before abandoning them.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(15);

/// The `factory` label value on every sandbox this control plane starts.
pub const FACTORY_LABEL: &str = "trailhead";

/// How many times reconcile tries to release a pooled sandbox before giving up on it and starting a new one.
pub const RELEASE_ATTEMPTS: u32 = 5;

/// How long a pooled sandbox may go on failing to release before the factory gives up on it, however few tries
/// that took.
pub const RELEASE_PATIENCE: Duration = Duration::from_secs(10 * 60);

/// Where a sandbox clones a public GitHub repo from during prepare (`docs/factory/sandbox.md`).
const CLONE_DOMAINS: &[&str] = &["github.com", "codeload.github.com"];

/// Reads the current `[merge] mode`, or says why it can't.
pub type MergeModeSource = Box<dyn Fn() -> std::result::Result<MergeMode, String> + Send + Sync>;

/// Checks that the Trailhead server the sandboxes' daemons claim through is reachable, or says why not.
pub type ServerCheck =
    Box<dyn Fn() -> futures_util::future::BoxFuture<'static, std::result::Result<(), String>> + Send + Sync>;

/// State shared by every [`Factory`] on one host (one per repo): a usage reading from any repo's run pauses them all,
/// and the sandbox cap can span repos.
#[derive(Default)]
pub struct Host {
    /// The most sandboxes held for workers across all repos; `None` is no cross-repo cap.
    max_sandboxes: Option<usize>,
    /// The latest reading of each subscription usage window: percent used, and when it resets (unix seconds).
    quota: Mutex<HashMap<String, (u32, i64)>>,
    /// The reset a pause was last logged for, so a pause is logged once per host rather than on every poll.
    pause_logged: Mutex<Option<i64>>,
}

impl Host {
    pub fn new(max_sandboxes: Option<usize>) -> Arc<Host> {
        Arc::new(Host { max_sandboxes, ..Host::default() })
    }
}

pub struct Factory {
    pub config: FactoryConfig,
    pub compute: Arc<dyn Compute>,
    pub tracker: Arc<dyn Tracker>,
    pub provider: Arc<dyn ModelProvider>,
    pub daemon: Arc<dyn Daemon>,
    /// Reads PRs from the control plane, which sandboxes' own code host access can't serve. Without one, reconcile
    /// doesn't notice merges or closed PRs.
    pub codehost: Option<Arc<dyn CodeHost>>,
    /// Where the merge mode is re-read from before each merge decision, so it can be switched without a restart.
    merge_mode_source: Option<MergeModeSource>,
    /// The merge mode last read from `merge_mode_source`, kept when a later read fails.
    merge_mode: Mutex<MergeMode>,
    /// Run before starting runs: the control plane reads the database directly, but each sandbox's daemon needs the
    /// server, so a run started while it is down fails on its first claim.
    server_check: Option<ServerCheck>,
    /// Whether the server was last found down, so an outage is reported loudly once and its end once.
    server_down: Mutex<bool>,
    troubled: Mutex<HashMap<String, Trouble>>,
    /// Tasks whose last runs failed on the infrastructure: how many in a row, and when to try again.
    setbacks: Mutex<HashMap<TaskKey, (u32, u32, Instant)>>,
    /// Tasks whose PR a run just pushed to: the host's next `mergeable = false` for each may be a verdict on the head
    /// before the push, so it is read as not yet worked out, once (TRAIL-241).
    fresh_pushes: Mutex<HashSet<TaskKey>>,
    /// The files last listed for each task's PR and the head they were listed at, so a pass that finds the head
    /// unchanged grades from them instead of asking the host again (TRAIL-266).
    listed: Mutex<HashMap<TaskKey, (String, Vec<ChangedFile>)>>,
    /// Tasks whose PR the merger asked the host to bring up to date, by the head it asked on. The host queues the merge
    /// (a 202), so until the head moves the PR is still behind and asking again would only queue another.
    updating: Mutex<HashMap<TaskKey, String>>,
    /// What every repo's factory on this host shares: usage readings, the pause log and the cross-repo sandbox cap.
    host: Arc<Host>,
    /// The ready tasks last reported as starved by parked sandboxes, so a poll says it once per change.
    starved: Mutex<Option<Vec<TaskKey>>>,
    /// Tasks already reported as ungraded, so a poll doesn't say it again.
    ungraded: Mutex<HashSet<TaskKey>>,
    /// Held while a worker picks an idle pooled sandbox (or a name for a new one) and labels it busy, so runs
    /// starting together never take the same sandbox.
    pool_pick: tokio::sync::Mutex<()>,
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
    /// The task's PR had already merged or closed, so no run started: a merge finished the task, a close parked it.
    Settled { reason: String },
}

#[derive(Debug, Default, PartialEq)]
pub struct Reconciled {
    pub destroyed: Vec<String>,
    /// Pooled sandboxes an interrupted run left busy, returned to idle.
    pub released: Vec<String>,
    /// Tasks whose run was still going in its sandbox, followed again instead of requeued (TRAIL-137).
    pub reattached: Vec<String>,
    pub requeued: Vec<String>,
    pub reviewed: Vec<String>,
    /// Tasks whose review asked for changes but stayed in review, now reopened for a rework or parked.
    pub reworked: Vec<String>,
    /// In-review tasks whose PR no longer merges cleanly, now reopened to merge the base branch in or parked.
    pub conflicted: Vec<String>,
    /// In-review tasks whose PR was merged, now done.
    pub merged: Vec<String>,
    /// In-review tasks whose PR was closed without merging, now parked for a human.
    pub closed: Vec<String>,
    /// Pooled sandboxes that kept failing to release, now out of the pool.
    pub abandoned: Vec<String>,
}

impl Factory {
    pub fn new(
        config: FactoryConfig,
        compute: Arc<dyn Compute>,
        tracker: Arc<dyn Tracker>,
        provider: Arc<dyn ModelProvider>,
        daemon: Arc<dyn Daemon>,
        host: Arc<Host>,
    ) -> Factory {
        let merge_mode = Mutex::new(config.merge.mode);
        Factory {
            config,
            compute,
            tracker,
            provider,
            daemon,
            codehost: None,
            merge_mode_source: None,
            merge_mode,
            server_check: None,
            server_down: Mutex::new(false),
            troubled: Mutex::default(),
            setbacks: Mutex::default(),
            fresh_pushes: Mutex::default(),
            listed: Mutex::default(),
            updating: Mutex::default(),
            host,
            starved: Mutex::default(),
            ungraded: Mutex::default(),
            pool_pick: tokio::sync::Mutex::default(),
        }
    }

    /// Re-reads the merge mode from `source` before each merge decision, so switching it (to `off` as the quick stop)
    /// takes effect on the next pass. A failed read keeps the last mode read and is logged.
    pub fn with_merge_mode_source(mut self, source: MergeModeSource) -> Factory {
        self.merge_mode_source = Some(source);
        self
    }

    /// Checks the server is reachable before starting runs, and starts none while it isn't.
    pub fn with_server_check(mut self, check: ServerCheck) -> Factory {
        self.server_check = Some(check);
        self
    }

    /// Whether the server the daemons need is up. While it isn't, the outage is logged as an error when it starts
    /// and again when it ends, and no new runs start (running ones carry on).
    async fn server_up(&self) -> bool {
        let Some(check) = &self.server_check else { return true };
        let result = check().await;
        let mut down = self.server_down.lock().unwrap();
        match result {
            Ok(()) => {
                if std::mem::take(&mut *down) {
                    tracing::info!("the Trailhead server is reachable again; starting runs again");
                }
                true
            }
            Err(why) => {
                if !std::mem::replace(&mut *down, true) {
                    tracing::error!(%why, "the Trailhead server is down; starting no new runs until it is back (start it with `trailhead server start`)");
                }
                false
            }
        }
    }

    /// The merge mode now in force.
    fn merge_mode(&self) -> MergeMode {
        let mut mode = self.merge_mode.lock().unwrap();
        if let Some(source) = &self.merge_mode_source {
            match source() {
                Ok(read) => {
                    if read != *mode {
                        tracing::info!(from = ?*mode, to = ?read, "merge mode changed");
                    }
                    *mode = read;
                }
                Err(why) => tracing::warn!(%why, mode = ?*mode, "can't re-read the merge mode; keeping the last one"),
            }
        }
        *mode
    }

    /// Lets reconcile check each in-review task's PR (`docs/factory/operating.md`).
    pub fn with_codehost(mut self, codehost: Arc<dyn CodeHost>) -> Factory {
        self.codehost = Some(codehost);
        self
    }

    fn project(&self) -> &str {
        &self.config.repo.project
    }

    fn queue(&self) -> Vec<TaskKey> {
        self.config.repo.queue.iter().cloned().map(TaskKey).collect()
    }

    fn base_labels(&self) -> Labels {
        Labels::from([
            (label::FACTORY.into(), FACTORY_LABEL.into()),
            (label::REPO.into(), self.config.repo.repo().slug()),
        ])
    }

    fn agent_egress(&self) -> Egress {
        let iroh = IROH_DOMAINS.iter().map(|d| d.to_string());
        Egress { domains: iroh.chain(self.config.egress.always.iter().cloned()).collect() }
    }

    /// Install-phase hosts the agent phase drops, which the daemon confirms are unreachable before the agent starts.
    /// Wildcards can't be looked up, so they're left out.
    fn blocked_hosts(&self) -> Vec<String> {
        let agent = self.agent_egress().domains;
        let install = self.install_egress().domains;
        install.into_iter().filter(|d| !d.contains('*') && !agent.contains(d)).collect()
    }

    /// The agent phase's named hosts (wildcards can't be looked up), which the daemon keeps resolved.
    fn allowed_hosts(&self) -> Vec<String> {
        self.agent_egress().domains.into_iter().filter(|d| !d.contains('*')).collect()
    }

    fn install_egress(&self) -> Egress {
        let mut egress = self.agent_egress();
        let extra = self.config.egress.install.iter().cloned().chain(CLONE_DOMAINS.iter().map(|d| d.to_string()));
        egress.domains.extend(extra);
        egress
    }

    fn branch(task: &Task) -> String {
        format!("factory/{}", task.key.0.to_ascii_lowercase())
    }
}
