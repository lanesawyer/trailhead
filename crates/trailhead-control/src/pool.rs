use std::collections::HashSet;
use std::time::Duration;

use tokio::time::Instant;
use trailhead_factory::{
    CheckpointId, Egress, Error, Labels, PrepareRequest, Result, Role, Sandbox, SandboxSpec, Task, label,
};

use crate::timing::backoff;
use crate::{Factory, RELEASE_ATTEMPTS, RELEASE_PATIENCE, Reconciled};

impl Factory {
    /// A sandbox for `task`, its checkout prepared on `branch` when given (a reviewer's is the PR's head).
    pub(crate) async fn create_sandbox(
        &self,
        name: &str,
        task: &Task,
        role: Role,
        branch: Option<String>,
    ) -> Result<Sandbox> {
        let mut labels = self.base_labels();
        labels.insert(label::TASK.into(), task.key.0.clone());
        labels.insert(label::ROLE.into(), role.as_str().into());
        // Install egress only while the checkout is prepared, then narrowed before any agent runs.
        let spec = SandboxSpec { name: name.to_string(), labels, egress: self.install_egress() };
        let sandbox = self.compute.create(&spec).await?;
        let ready = async {
            self.daemon.prepare(&sandbox.id, self.config.prepare_request(role, branch)).await?;
            self.compute.set_egress(&sandbox.id, &self.agent_egress()).await
        };
        if let Err(e) = ready.await {
            // Not handed out, so nothing else would clean it up before reconcile.
            if let Err(d) = self.compute.destroy(&sandbox.id).await {
                tracing::warn!(sandbox = %sandbox.name, error = %d, "couldn't destroy a sandbox that failed to prepare; reconcile will");
            }
            return Err(e);
        }
        Ok(sandbox)
    }

    pub(crate) fn pool_labels(&self) -> Labels {
        let mut labels = self.base_labels();
        labels.insert(label::POOL.into(), Role::Worker.as_str().into());
        labels
    }

    fn pooled(&self, state: &str, warm: Option<&str>, task: Option<&Task>) -> Labels {
        let mut labels = self.pool_labels();
        labels.insert(label::STATE.into(), state.into());
        if let Some(warm) = warm {
            labels.insert(label::WARM.into(), warm.into());
        }
        if let Some(task) = task {
            labels.insert(label::TASK.into(), task.key.0.clone());
            labels.insert(label::ROLE.into(), Role::Worker.as_str().into());
        }
        labels
    }

    /// A worker sandbox for `task` from the repo's pool (`docs/factory/sandbox.md`): an idle one restored to its warm
    /// checkpoint, or a new one when none is idle. The tick's room check keeps the pool within `max_sandboxes`.
    /// The prepared checkout becomes the new warm checkpoint, so the next task starts from it.
    pub(crate) async fn acquire_worker(
        &self,
        task: &Task,
        branch: Option<String>,
        merge_base: bool,
    ) -> Result<Sandbox> {
        let pick = self.pool_pick.lock().await;
        let idle = self.compute.list(&self.pooled(label::IDLE, None, None)).await?;
        let (sandbox, warm) = match idle.into_iter().find(|s| !self.abandoned(&s.name)) {
            Some(s) => {
                let warm = s.labels.get(label::WARM).cloned();
                (s, warm)
            }
            None => {
                let name = self.next_pool_name().await?;
                let spec = SandboxSpec {
                    name,
                    labels: self.pooled(label::BUSY, None, Some(task)),
                    egress: self.install_egress(),
                };
                (self.compute.create(&spec).await?, None)
            }
        };
        self.compute.set_labels(&sandbox.id, &self.pooled(label::BUSY, warm.as_deref(), Some(task))).await?;
        drop(pick);
        self.compute.set_egress(&sandbox.id, &self.install_egress()).await?;
        if let Some(cp) = &warm {
            self.compute.restore(&sandbox.id, &CheckpointId(cp.clone())).await?;
            self.daemon.forget(&sandbox.id).await;
        }
        self.compute.preflight(&sandbox.id).await?;
        let prepare = PrepareRequest { merge_base, ..self.config.prepare_request(Role::Worker, branch) };
        if let Err(e) = self.daemon.prepare(&sandbox.id, prepare).await {
            // A sandbox whose checkout won't prepare would fail the next task placed on it the same way, so it
            // leaves the pool rather than going back with its warm checkpoint.
            tracing::warn!(sandbox = %sandbox.name, error = %e, "prepare failed; destroying the sandbox");
            self.daemon.forget(&sandbox.id).await;
            if let Err(d) = self.compute.destroy(&sandbox.id).await {
                tracing::warn!(sandbox = %sandbox.name, error = %d, "couldn't destroy a sandbox that failed to prepare; reconcile will");
            }
            return Err(e);
        }
        let cp = self.compute.checkpoint(&sandbox.id, "warm").await?;
        let labels = self.pooled(label::BUSY, Some(&cp.0), Some(task));
        self.compute.set_labels(&sandbox.id, &labels).await?;
        self.compute.set_egress(&sandbox.id, &self.agent_egress()).await?;
        Ok(Sandbox { labels, ..sandbox })
    }

    /// Returns a worker sandbox to the pool: restored to its warm checkpoint, which wipes everything the task did,
    /// with no egress. One that never got warm, or can't be restored, is destroyed instead: a sandbox that may still
    /// hold the last task's leftovers is never handed to another task.
    pub(crate) async fn release_worker(&self, sandbox: &Sandbox) -> Result<()> {
        self.compute.set_egress(&sandbox.id, &Egress::none()).await?;
        let Some(warm) = sandbox.labels.get(label::WARM).cloned() else {
            return self.compute.destroy(&sandbox.id).await;
        };
        if let Err(e) = self.compute.restore(&sandbox.id, &CheckpointId(warm.clone())).await {
            tracing::warn!(sandbox = %sandbox.name, error = %e, "couldn't restore the warm checkpoint; destroying");
            self.daemon.forget(&sandbox.id).await;
            return self.compute.destroy(&sandbox.id).await;
        }
        self.daemon.forget(&sandbox.id).await;
        self.compute.set_labels(&sandbox.id, &self.pooled(label::IDLE, Some(&warm), None)).await
    }

    /// The first free `<repo>-w<n>` name.
    async fn next_pool_name(&self) -> Result<String> {
        let taken: HashSet<String> =
            self.compute.list(&self.pool_labels()).await?.into_iter().map(|s| s.name).collect();
        let repo = self.config.repo.name.to_ascii_lowercase();
        Ok((1..).map(|n| format!("{repo}-w{n}")).find(|n| !taken.contains(n)).expect("a free name"))
    }

    /// The kill switch: without its role label a sandbox loses connector access, and without egress it reaches nothing.
    pub(crate) async fn cut_off(&self, sandbox: &Sandbox) -> Result<()> {
        // Both halves run, egress first, so one failing doesn't skip the other.
        let mut labels = sandbox.labels.clone();
        labels.remove(label::ROLE);
        let egress = self.compute.set_egress(&sandbox.id, &Egress::none()).await;
        let relabelled = self.compute.set_labels(&sandbox.id, &labels).await;
        egress.and(relabelled)
    }

    /// A pooled sandbox stays; one left busy by an interrupted run goes back to idle. One that keeps failing to
    /// release is given up on, so it can't hold the pool's only place while every retry fails (TRAIL-135).
    pub(crate) async fn reconcile_pooled(&self, sandbox: &Sandbox, done: &mut Reconciled) {
        let name = &sandbox.name;
        let state = sandbox.labels.get(label::STATE).map(String::as_str);
        if !self.due(name) {
            return;
        }
        if state == Some(label::BROKEN) || self.abandoned(name) {
            match self.compute.destroy(&sandbox.id).await {
                Ok(()) => {
                    tracing::info!(sandbox = %name, "destroyed a broken pooled sandbox");
                    self.troubled.lock().unwrap().remove(name);
                    done.destroyed.push(name.clone());
                }
                Err(e) => {
                    tracing::debug!(sandbox = %name, error = %e, "a broken pooled sandbox still can't be destroyed");
                    self.failed(name);
                }
            }
            return;
        }
        if state == Some(label::IDLE) {
            return;
        }
        tracing::info!(sandbox = %name, "releasing a pooled sandbox back to idle");
        let started = Instant::now();
        match self.release_worker(sandbox).await {
            Ok(()) => {
                tracing::info!(sandbox = %name, took = ?started.elapsed(), "released a pooled sandbox");
                self.troubled.lock().unwrap().remove(name);
                done.released.push(name.clone());
            }
            Err(e) if self.failed(name) => {
                self.abandon(sandbox, &e).await;
                done.abandoned.push(name.clone());
            }
            Err(e) => tracing::warn!(sandbox = %name, error = %e, "couldn't release a pooled sandbox; will retry"),
        }
    }

    /// Takes a sandbox out of the pool for good: destroyed if the compute host allows it, otherwise labelled
    /// [`label::BROKEN`] without a role (so it loses connector access) and destroyed by a later reconcile.
    async fn abandon(&self, sandbox: &Sandbox, error: &Error) {
        let name = &sandbox.name;
        tracing::error!(
            sandbox = %name,
            %error,
            "giving up on a pooled sandbox that won't release; leaving it out of the pool and starting a new one"
        );
        if let Some(t) = self.troubled.lock().unwrap().get_mut(name) {
            t.abandoned = true;
        }
        if self.compute.destroy(&sandbox.id).await.is_ok() {
            self.troubled.lock().unwrap().remove(name);
            return;
        }
        let mut labels = self.pool_labels();
        labels.insert(label::STATE.into(), label::BROKEN.into());
        if let Err(e) = self.compute.set_labels(&sandbox.id, &labels).await {
            tracing::error!(
                sandbox = %name,
                error = %e,
                "couldn't label it broken either; it's out of the pool until this control plane restarts"
            );
        }
    }

    pub(crate) fn abandoned(&self, name: &str) -> bool {
        self.troubled.lock().unwrap().get(name).is_some_and(|t| t.abandoned)
    }

    /// Whether a troubled sandbox's backoff has run out, or it isn't troubled.
    pub(crate) fn due(&self, name: &str) -> bool {
        self.troubled.lock().unwrap().get(name).is_none_or(|t| Instant::now() >= t.next_try)
    }

    /// Records a failure and backs off. True once the sandbox has used up its tries or its patience.
    fn failed(&self, name: &str) -> bool {
        let now = Instant::now();
        let mut troubled = self.troubled.lock().unwrap();
        let t = troubled.entry(name.to_string()).or_insert(Trouble {
            failures: 0,
            since: now,
            next_try: now,
            abandoned: false,
        });
        t.failures += 1;
        t.next_try = now + backoff(t.failures, FIRST_BACKOFF, MAX_BACKOFF);
        !t.abandoned && (t.failures >= RELEASE_ATTEMPTS || now - t.since >= RELEASE_PATIENCE)
    }
}

/// The first wait between tries at a troubled sandbox, doubling up to [`MAX_BACKOFF`].
const FIRST_BACKOFF: Duration = Duration::from_secs(30);

const MAX_BACKOFF: Duration = Duration::from_secs(5 * 60);

/// A pooled sandbox that keeps failing to release. Kept in memory only, so a restart counts again from zero.
pub(crate) struct Trouble {
    failures: u32,
    since: Instant,
    next_try: Instant,
    /// Given up on: out of the pool and the sandbox cap, even if labelling it broken failed too.
    abandoned: bool,
}
