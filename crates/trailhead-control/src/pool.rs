use std::collections::HashSet;
use std::time::Duration;

use tokio::time::Instant;
use trailhead_factory::{
    CheckpointId, Egress, Error, Labels, PrepareRequest, Result, Role, RunPhase, Sandbox, SandboxSpec, Task,
    install_host_allowed, label,
};

use crate::timing::backoff;
use crate::{Factory, Placed, RELEASE_ATTEMPTS, RELEASE_PATIENCE, Reconciled};

impl Factory {
    /// Prepares `sandbox`'s checkout under install egress. The control plane can't see the checkout, so the daemon
    /// first says which hosts the `[stack]` tools it will install need; exactly
    /// those are opened on top of the install set for the prepare, each logged with the tool that asked, and
    /// confirmed closed again before any agent runs.
    async fn prepare_sandbox(&self, p: &Placed, sandbox: &Sandbox, req: PrepareRequest) -> Result<()> {
        let needs = p.daemon.install_hosts(&sandbox.id, &req).await?;
        if !needs.is_empty() {
            let mut egress = self.install_egress();
            let mut opened = Vec::new();
            for need in &needs {
                if !install_host_allowed(&req, need) {
                    tracing::warn!(sandbox = %sandbox.name, host = %need.host, tool = %need.tool, "refusing install egress the daemon asked for outside the tool table and [factory.egress.tools]");
                    return Err(Error::Other(format!(
                        "the daemon asked to open `{}` for `{}`, which isn't in the install host table or [factory.egress.tools]",
                        need.host, need.tool
                    )));
                }
                tracing::info!(sandbox = %sandbox.name, host = %need.host, tool = %need.tool, "opening install egress");
                if !egress.domains.contains(&need.host) {
                    egress.domains.push(need.host.clone());
                }
                opened.push(need.host.clone());
            }
            p.compute.set_egress(&sandbox.id, &egress).await?;
            self.install_opened.lock().unwrap().insert(sandbox.id.clone(), opened);
        }
        p.daemon.prepare(&sandbox.id, req).await
    }

    fn forget_install_opened(&self, sandbox: &Sandbox) {
        self.install_opened.lock().unwrap().remove(&sandbox.id);
    }

    /// A sandbox for `task`, its checkout prepared on `branch` when given (a reviewer's is the PR's head).
    pub(crate) async fn create_sandbox(
        &self,
        p: &Placed,
        name: &str,
        task: &Task,
        role: Role,
        branch: Option<String>,
    ) -> Result<Sandbox> {
        let mut labels = self.base_labels();
        labels.insert(label::TASK.into(), task.key.0.clone());
        labels.insert(label::ROLE.into(), role.as_str().into());
        labels.insert(label::TARGET.into(), p.target.name.clone());
        // Install egress only while the checkout is prepared, then narrowed before any agent runs.
        let spec = SandboxSpec { name: name.to_string(), labels, egress: self.install_egress() };
        let sandbox = p.compute.create(&spec).await?;
        self.status.sandbox(&sandbox);
        let ready = async {
            self.prepare_sandbox(p, &sandbox, self.config.prepare_request(role, branch)).await?;
            p.compute.set_egress(&sandbox.id, &self.agent_egress()).await
        };
        if let Err(e) = ready.await {
            self.forget_install_opened(&sandbox);
            // Not handed out, so nothing else would clean it up before reconcile.
            self.status.sandbox_gone(&sandbox.name);
            if let Err(d) = p.compute.destroy(&sandbox.id).await {
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

    fn pooled(&self, target: Option<&Placed>, state: &str, warm: Option<&str>, task: Option<&Task>) -> Labels {
        let mut labels = self.pool_labels();
        labels.insert(label::STATE.into(), state.into());
        if let Some(p) = target {
            labels.insert(label::TARGET.into(), p.target.name.clone());
        }
        if let Some(warm) = warm {
            labels.insert(label::WARM.into(), warm.into());
            let stack = self.config.stack.tag();
            if !stack.is_empty() {
                labels.insert(label::STACK.into(), stack);
            }
        }
        if let Some(task) = task {
            labels.insert(label::TASK.into(), task.key.0.clone());
            labels.insert(label::ROLE.into(), Role::Worker.as_str().into());
        }
        labels
    }

    /// A worker sandbox for `task` from the repo's pool (`docs/factory/sandbox.md`): an idle one restored to its warm
    /// checkpoint, or a new one when none is idle. The tick's room check keeps the pool within `max_sandboxes`.
    /// The prepared checkout becomes the new warm checkpoint when the old one has gone stale (TRAIL-707).
    /// A rework whose `setup` fails still gets its sandbox, with the failure output returned beside it.
    pub(crate) async fn acquire_worker(
        &self,
        p: &Placed,
        task: &Task,
        branch: Option<String>,
        merge_base: bool,
    ) -> Result<(Sandbox, Option<String>)> {
        let pick = self.pool_pick.lock().await;
        let idle = p.list(&self.pooled(None, label::IDLE, None, None)).await?;
        let stack = self.config.stack.tag();
        let mut current = Vec::new();
        for s in idle {
            if s.labels.get(label::STACK).map(String::as_str).unwrap_or("") == stack {
                current.push(s);
                continue;
            }
            // Built for another stack: reinstalling on top would keep the old tools, so start over.
            tracing::info!(sandbox = %s.name, "[stack] changed; destroying the pooled sandbox to reinstall");
            if let Err(e) = p.compute.destroy(&s.id).await {
                tracing::warn!(sandbox = %s.name, error = %e, "couldn't destroy a sandbox built for another stack");
            }
        }
        let (sandbox, warm) = match current.into_iter().find(|s| !self.abandoned(&s.name)) {
            Some(s) => {
                let warm = s.labels.get(label::WARM).cloned();
                (s, warm)
            }
            None => {
                let name = self.next_pool_name(p).await?;
                let spec = SandboxSpec {
                    name,
                    labels: self.pooled(Some(p), label::BUSY, None, Some(task)),
                    egress: self.install_egress(),
                };
                (p.compute.create(&spec).await?, None)
            }
        };
        let busy = self.pooled(Some(p), label::BUSY, warm.as_deref(), Some(task));
        if let Err(e) = p.compute.set_labels(&sandbox.id, &busy).await {
            self.give_back(&sandbox, &busy).await;
            return Err(e);
        }
        self.status.phase(&task.key, RunPhase::Preparing { cold: warm.is_none() }, Some(&sandbox.name));
        self.status.sandbox(&Sandbox { labels: busy.clone(), ..sandbox.clone() });
        drop(pick);
        let began = Instant::now();
        let started = async {
            p.compute.set_egress(&sandbox.id, &self.install_egress()).await?;
            if let Some(cp) = &warm {
                p.compute.restore(&sandbox.id, &CheckpointId(cp.clone())).await?;
                p.daemon.forget(&sandbox.id).await;
            }
            let restored = began.elapsed();
            p.compute.preflight(&sandbox.id).await?;
            Ok::<_, Error>(restored)
        }
        .await;
        let restored = match started {
            Ok(restored) => restored,
            Err(e) => {
                // Labelled busy but never handed out: without this it holds its disk until reconcile.
                self.give_back(&sandbox, &busy).await;
                return Err(e);
            }
        };
        let preparing = Instant::now();
        let rework = branch.is_some();
        let prepare = PrepareRequest { merge_base, ..self.config.prepare_request(Role::Worker, branch) };
        let mut prepared = self.prepare_sandbox(p, &sandbox, prepare.clone()).await;
        let mut setup_failure = None;
        // On a rework the failed `setup` is the code's doing, probably what the worker is there to fix, so the agent
        // starts anyway: the checkout is otherwise ready, and prepares again without `setup` to finish it (TRAIL-649).
        if let Err(Error::SetupFailed(output)) = &prepared
            && rework
        {
            tracing::warn!(sandbox = %sandbox.name, "setup failed on a rework; starting the agent with the output");
            setup_failure = Some(output.clone());
            prepared = p.daemon.prepare(&sandbox.id, PrepareRequest { setup: None, ..prepare }).await;
        }
        if let Err(e) = prepared {
            self.forget_install_opened(&sandbox);
            p.daemon.forget(&sandbox.id).await;
            if matches!(e, Error::SetupFailed(_)) {
                // The code failed, not the sandbox, and the same error would come from any other: it goes back to
                // the pool rather than being destroyed for a cold build to replace.
                tracing::warn!(sandbox = %sandbox.name, error = %e, "setup failed on a fresh run; returning the sandbox");
                self.give_back(&sandbox, &busy).await;
                return Err(e);
            }
            // A sandbox whose checkout won't prepare would fail the next task placed on it the same way, so it
            // leaves the pool rather than going back with its warm checkpoint.
            tracing::warn!(sandbox = %sandbox.name, error = %e, "prepare failed; destroying the sandbox");
            if let Err(d) = p.compute.destroy(&sandbox.id).await {
                tracing::warn!(sandbox = %sandbox.name, error = %d, "couldn't destroy a sandbox that failed to prepare; reconcile will");
            }
            return Err(e);
        }
        let prepare_took = preparing.elapsed();
        // A checkpoint pauses the sandbox while its whole filesystem is copied (minutes on Docker), and what it saves the
        // next prepare is mostly dependencies, which rarely change. So the warm point is kept while a prepare from it
        // takes no longer than taking a new one did; a slower prepare means it has gone stale.
        let cost = self.warm_cost.lock().unwrap().get(&sandbox.id).copied();
        let rewarm = warm.is_none() || cost.is_none_or(|cost| prepare_took > cost);
        let warmed = async {
            let mut labels = busy.clone();
            let checkpointing = Instant::now();
            if rewarm {
                let cp = p.compute.checkpoint(&sandbox.id, "warm").await?;
                self.warm_cost.lock().unwrap().insert(sandbox.id.clone(), checkpointing.elapsed());
                labels = self.pooled(Some(p), label::BUSY, Some(&cp.0), Some(task));
                p.compute.set_labels(&sandbox.id, &labels).await?;
            }
            p.compute.set_egress(&sandbox.id, &self.agent_egress()).await?;
            tracing::info!(
                sandbox = %sandbox.name,
                restore_s = restored.as_secs(),
                prepare_s = prepare_took.as_secs(),
                checkpoint_s = rewarm.then(|| checkpointing.elapsed().as_secs()),
                total_s = began.elapsed().as_secs(),
                "worker sandbox ready"
            );
            Ok::<_, Error>(labels)
        }
        .await;
        match warmed {
            Ok(labels) => {
                let sandbox = Sandbox { labels, ..sandbox };
                self.status.sandbox(&sandbox);
                Ok((sandbox, setup_failure))
            }
            Err(e) => {
                self.forget_install_opened(&sandbox);
                p.daemon.forget(&sandbox.id).await;
                // The old warm checkpoint (if any) is what release restores; a new sandbox has none and is destroyed.
                self.give_back(&sandbox, &busy).await;
                Err(e)
            }
        }
    }

    /// Releases a worker sandbox that `acquire_worker` labelled busy but isn't handing out. A failure is logged and
    /// left to reconcile.
    async fn give_back(&self, sandbox: &Sandbox, labels: &Labels) {
        let held = Sandbox { labels: labels.clone(), ..sandbox.clone() };
        if let Err(r) = self.release_worker(&held).await {
            tracing::warn!(sandbox = %held.name, error = %r, "couldn't return the sandbox to the pool; reconcile will");
        }
    }

    /// Returns a worker sandbox to the pool: restored to its warm checkpoint, which wipes everything the task did,
    /// with no egress. One that never got warm, or can't be restored, is destroyed instead: a sandbox that may still
    /// hold the last task's leftovers is never handed to another task.
    pub(crate) async fn release_worker(&self, sandbox: &Sandbox) -> Result<()> {
        let p = self.target_of(sandbox);
        p.compute.set_egress(&sandbox.id, &Egress::none()).await?;
        let Some(warm) = sandbox.labels.get(label::WARM).cloned() else {
            return self.destroy_pooled(p, sandbox).await;
        };
        if let Err(e) = p.compute.restore(&sandbox.id, &CheckpointId(warm.clone())).await {
            tracing::warn!(sandbox = %sandbox.name, error = %e, "couldn't restore the warm checkpoint; destroying");
            p.daemon.forget(&sandbox.id).await;
            return self.destroy_pooled(p, sandbox).await;
        }
        p.daemon.forget(&sandbox.id).await;
        // The warm checkpoint was built for the stack the sandbox is labelled with, which isn't the current
        // `[stack]` if the config changed while it was busy: carry the label over rather than recompute it.
        let mut labels = self.pooled(Some(p), label::IDLE, Some(&warm), None);
        match sandbox.labels.get(label::STACK) {
            Some(stack) => labels.insert(label::STACK.into(), stack.clone()),
            None => labels.remove(label::STACK),
        };
        p.compute.set_labels(&sandbox.id, &labels).await?;
        self.status.sandbox(&Sandbox { labels, ..sandbox.clone() });
        Ok(())
    }

    async fn destroy_pooled(&self, p: &Placed, sandbox: &Sandbox) -> Result<()> {
        p.compute.destroy(&sandbox.id).await?;
        self.status.sandbox_gone(&sandbox.name);
        Ok(())
    }

    /// The first free `<repo>-w<n>` name.
    async fn next_pool_name(&self, p: &Placed) -> Result<String> {
        let taken: HashSet<String> = p.list(&self.pool_labels()).await?.into_iter().map(|s| s.name).collect();
        let prefix = self.pool_prefix(p);
        Ok((1..).map(|n| format!("{prefix}-w{n}")).find(|n| !taken.contains(n)).expect("a free name"))
    }

    /// The kill switch: without its role label a sandbox loses connector access, and without egress it reaches nothing.
    pub(crate) async fn cut_off(&self, sandbox: &Sandbox) -> Result<()> {
        let p = self.target_of(sandbox);
        // Both halves run, egress first, so one failing doesn't skip the other.
        let mut labels = sandbox.labels.clone();
        labels.remove(label::ROLE);
        let egress = p.compute.set_egress(&sandbox.id, &Egress::none()).await;
        let relabelled = p.compute.set_labels(&sandbox.id, &labels).await;
        egress.and(relabelled)
    }

    /// A pooled sandbox stays; one left busy by an interrupted run goes back to idle. One that keeps failing to
    /// release is given up on, so it can't hold the pool's only place while every retry fails (TRAIL-135).
    pub(crate) async fn reconcile_pooled(&self, sandbox: &Sandbox, done: &mut Reconciled) {
        let p = self.target_of(sandbox);
        let name = &sandbox.name;
        let state = sandbox.labels.get(label::STATE).map(String::as_str);
        if !self.due(name) {
            return;
        }
        if state == Some(label::BROKEN) || self.abandoned(name) {
            match p.compute.destroy(&sandbox.id).await {
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
        let p = self.target_of(sandbox);
        let name = &sandbox.name;
        tracing::error!(
            sandbox = %name,
            %error,
            "giving up on a pooled sandbox that won't release; leaving it out of the pool and starting a new one"
        );
        if let Some(t) = self.troubled.lock().unwrap().get_mut(name) {
            t.abandoned = true;
        }
        if p.compute.destroy(&sandbox.id).await.is_ok() {
            self.status.sandbox_gone(name);
            self.troubled.lock().unwrap().remove(name);
            return;
        }
        let mut labels = self.pool_labels();
        labels.insert(label::STATE.into(), label::BROKEN.into());
        labels.insert(label::TARGET.into(), p.target.name.clone());
        match p.compute.set_labels(&sandbox.id, &labels).await {
            Ok(()) => self.status.sandbox(&Sandbox { labels, ..sandbox.clone() }),
            Err(e) => tracing::error!(
                sandbox = %name,
                error = %e,
                "couldn't label it broken either; it's out of the pool until this control plane restarts"
            ),
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
