use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;
use trailhead_factory::{
    CheckpointId, Egress, Error, Labels, PrepareRequest, Result, Role, RunPhase, Sandbox, SandboxId, SandboxSpec, Task,
    TaskKey, install_host_allowed, label,
};

use crate::timing::backoff;
use crate::{Factory, Placed, RELEASE_ATTEMPTS, RELEASE_PATIENCE, Reconciled};

/// What [`Factory::refresh_pool`] did with each pooled sandbox, by name.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Refreshed {
    pub destroyed: Vec<String>,
    /// Busy now: destroyed when its run releases it.
    pub on_release: Vec<String>,
    /// Couldn't be destroyed, with why; left for reconcile.
    pub failed: Vec<(String, String)>,
    /// Targets that didn't answer, whose sandboxes weren't touched.
    pub unreachable: Vec<String>,
}

/// How many prepares in a row may fail on a pooled sandbox before it is destroyed rather than released.
const PREPARE_FAILURES_BEFORE_DESTROY: u32 = 2;

/// How many runs in a row may find no room on a warm pooled sandbox's host before the sandbox is destroyed. A wait
/// or two is another sandbox's checkpoint passing through; more is the warm checkpoints themselves filling the disk,
/// which only grow, and no wait frees (TRAIL-1633). Well under the waits a task is given before it parks.
const SHORT_PREFLIGHTS_BEFORE_DESTROY: u32 = 3;

impl Factory {
    /// Prepares `sandbox`'s checkout under install egress. The control plane can't see the checkout, so the daemon
    /// first says which hosts the `[stack]` tools it will install need; exactly
    /// those are opened on top of the install set for the prepare, each logged with the tool that asked, and
    /// confirmed closed again before any agent runs.
    async fn prepare_sandbox(&self, p: &Placed, sandbox: &Sandbox, req: PrepareRequest, cold: bool) -> Result<()> {
        let began = Instant::now();
        tracing::info!(sandbox = %sandbox.name, cold, setup = req.setup.as_ref().map(|c| c.command.as_str()), "preparing the sandbox");
        let result = self.prepare_checkout(p, sandbox, req).await;
        let took_ms = u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX);
        match &result {
            Ok(()) => tracing::info!(sandbox = %sandbox.name, cold, took_ms, "sandbox prepared"),
            Err(e) => tracing::info!(sandbox = %sandbox.name, cold, took_ms, error = %e, "sandbox prepare failed"),
        }
        result
    }

    async fn prepare_checkout(&self, p: &Placed, sandbox: &Sandbox, req: PrepareRequest) -> Result<()> {
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
            self.pool.opened_install(&sandbox.id, opened);
        }
        p.daemon.prepare(&sandbox.id, req).await
    }

    fn forget_install_opened(&self, sandbox: &Sandbox) {
        self.pool.take_install(&sandbox.id);
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
            self.prepare_sandbox(p, &sandbox, self.config.prepare_request(role, branch), true).await?;
            p.compute.set_egress(&sandbox.id, &self.agent_egress()).await
        };
        if let Err(e) = ready.await {
            self.forget_install_opened(&sandbox);
            // Not handed out, so nothing else would clean it up before reconcile.
            self.status.sandbox_gone(&sandbox.name);
            p.daemon.forget(&sandbox.id).await;
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

    /// A worker sandbox for `task` from the repo's pool (`docs/factory/sandbox.md`): the one it already holds, else an idle
    /// one, restored to its warm checkpoint, or a new one when none is idle. The tick's room check keeps the pool within `max_sandboxes`.
    /// The prepared checkout becomes the new warm checkpoint when the old one has gone stale (TRAIL-707).
    /// A rework whose `setup` fails still gets its sandbox, with the failure output returned beside it.
    pub(crate) async fn acquire_worker(
        &self,
        p: &Placed,
        task: &Task,
        branch: Option<String>,
        branch_if_present: bool,
        merge_base: bool,
    ) -> Result<(Sandbox, Option<String>)> {
        let pick = self.pool.pick.lock().await;
        // The task's own sandbox first, when it holds one (a parked rework resumes in it, TRAIL-1121); then idle ones.
        let mut idle = p.list(&self.pooled(None, label::BUSY, None, Some(task))).await?;
        // The one the tracker says it ran on comes first; any other labelled for it is left over from an earlier attempt.
        let recorded = task.run.host.as_deref();
        idle.sort_by_key(|s| Some(s.name.as_str()) != recorded);
        let held = idle.clone();
        idle.extend(p.list(&self.pooled(None, label::IDLE, None, None)).await?);
        let stack = self.config.stack.tag();
        let mut current = Vec::new();
        // Sandboxes the stack check destroyed or labelled broken: the release of the task's held ones below mustn't relabel them.
        let mut dealt_with = HashSet::new();
        for s in idle {
            if s.labels.get(label::STACK).map(String::as_str).unwrap_or("") == stack {
                current.push(s);
                continue;
            }
            // Built for another stack: reinstalling on top would keep the old tools, so start over.
            tracing::info!(sandbox = %s.name, "[stack] changed; destroying the pooled sandbox to reinstall");
            dealt_with.insert(s.name.clone());
            match p.compute.destroy(&s.id).await {
                Ok(()) => self.forget_sandbox(&s.id),
                Err(e) => {
                    // An idle one doesn't count toward `max_sandboxes` (only a role or a busy state does), but left as it
                    // is it would be retried on every acquire. Broken, it isn't retried and reconcile destroys it. If it
                    // is the task's own busy sandbox it did count, so this also frees that place.
                    tracing::warn!(sandbox = %s.name, error = %e, "couldn't destroy a sandbox built for another stack; labelling it broken");
                    self.mark_broken(&s).await;
                }
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
        // A task holds one pooled sandbox: another still labelled for it would fill the cap for nothing (TRAIL-1185).
        for stale in held.iter().filter(|s| s.name != sandbox.name && !dealt_with.contains(&s.name)) {
            if let Err(e) = self.release_worker(stale).await {
                tracing::warn!(sandbox = %stale.name, task = %task.key.0, error = %e, "couldn't release a stale sandbox held for the task; reconcile will");
            }
        }
        self.status.phase(&task.key, RunPhase::Preparing { cold: warm.is_none() }, Some(&sandbox.name));
        self.status.sandbox(&Sandbox { labels: busy.clone(), ..sandbox.clone() });
        drop(pick);
        let mine = held.iter().any(|s| s.name == sandbox.name);
        let began = Instant::now();
        let restored = match self.start_worker(p, &sandbox, warm.as_deref(), began).await {
            Ok(restored) => restored,
            Err((e, short)) => {
                self.not_started(p, &sandbox, &busy, short && warm.is_some() && !mine, &e).await;
                return Err(e);
            }
        };
        let preparing = Instant::now();
        let rework = branch.is_some() && !branch_if_present;
        let prepare =
            PrepareRequest { merge_base, branch_if_present, ..self.config.prepare_request(Role::Worker, branch) };
        let mut prepared = self.prepare_sandbox(p, &sandbox, prepare.clone(), warm.is_none()).await;
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
            self.prepare_failed(p, &sandbox, &busy, &e).await;
            return Err(e);
        }
        self.pool.prepare_passed(&sandbox.id);
        let prepare_took = preparing.elapsed();
        // A checkpoint pauses the sandbox while its whole filesystem is copied (minutes on Docker), and what it saves the
        // next prepare is mostly dependencies, which rarely change. So the warm point is kept while a prepare from it
        // takes no longer than taking a new one did; a slower prepare means it has gone stale.
        let cost = self.pool.warm_cost(&sandbox.id);
        let rewarm = warm.is_none() || cost.is_none_or(|cost| prepare_took > cost);
        let mut taken = None;
        let warmed = async {
            let mut labels = busy.clone();
            let checkpointing = Instant::now();
            if rewarm {
                let cp = p.compute.checkpoint(&sandbox.id, "warm").await?;
                taken = Some(cp.clone());
                self.pool.set_warm_cost(&sandbox.id, Some(checkpointing.elapsed()));
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
                // A new checkpoint nothing points at would stay on providers that keep them. A new sandbox is destroyed
                // with its checkpoints, so only one going back to the pool needs it deleted.
                if let (Some(cp), Some(_)) = (&taken, &warm)
                    && let Err(d) = p.compute.delete_checkpoint(&sandbox.id, cp).await
                {
                    tracing::warn!(sandbox = %sandbox.name, checkpoint = %cp.0, error = %d, "couldn't delete the unused warm checkpoint");
                }
                // The cost recorded for the new checkpoint goes with it: release restores the old one, which that
                // cost doesn't describe, and the next stale check would compare against the wrong figure.
                if taken.is_some() {
                    self.pool.set_warm_cost(&sandbox.id, cost);
                }
                // The old warm checkpoint (if any) is what release restores; a new sandbox has none and is destroyed.
                self.give_back(&sandbox, &busy).await;
                Err(e)
            }
        }
    }

    /// A planner's sandbox: the one the task held for its last turn, as that turn left it, when the task still holds
    /// it, so the agent's session on its disk carries on. It is neither restored nor prepared, which would wipe the
    /// session. Otherwise a worker's sandbox on the base branch, from the pool, as [`Factory::acquire_worker`] gives.
    pub(crate) async fn acquire_planner(&self, p: &Placed, task: &Task) -> Result<Sandbox> {
        if let Some(host) = task.run.host.as_deref().filter(|_| task.keeps_planner_sandbox()) {
            let stack = self.config.stack.tag();
            // Listed by role too, so a sandbox whose kill switch took its role away is never handed out again.
            let held = p.list(&self.pooled(None, label::BUSY, None, Some(task))).await?.into_iter().find(|s| {
                s.name == host
                    && s.labels.get(label::STACK).map(String::as_str).unwrap_or("") == stack
                    && !self.abandoned(&s.name)
            });
            if let Some(sandbox) = held {
                p.compute.set_egress(&sandbox.id, &self.agent_egress()).await?;
                tracing::info!(task = %task.key, sandbox = %sandbox.name, "the planner carries on in the sandbox that holds its session");
                self.status.phase(&task.key, RunPhase::Preparing { cold: false }, Some(&sandbox.name));
                self.status.sandbox(&sandbox);
                return Ok(sandbox);
            }
        }
        Ok(self.acquire_worker(p, task, None, false, false).await?.0)
    }

    /// Opens install egress on a worker sandbox picked for a run, restores its warm checkpoint and checks its host has
    /// room. How long the restore took, or the error with whether it was the host having no room.
    async fn start_worker(
        &self,
        p: &Placed,
        sandbox: &Sandbox,
        warm: Option<&str>,
        began: Instant,
    ) -> std::result::Result<Duration, (Error, bool)> {
        let restore = async {
            p.compute.set_egress(&sandbox.id, &self.install_egress()).await?;
            if let Some(cp) = warm {
                p.compute.restore(&sandbox.id, &CheckpointId(cp.to_string())).await?;
                p.daemon.restored(&sandbox.id).await;
            }
            Ok::<_, Error>(began.elapsed())
        };
        let restored = restore.await.map_err(|e| (e, false))?;
        if let Err(e) = p.compute.preflight(&sandbox.id).await {
            let short = matches!(e, Error::Unavailable(_));
            return Err((e, short));
        }
        self.pool.had_room(&sandbox.id);
        Ok(restored)
    }

    /// A worker sandbox whose prepare failed, put where it belongs: back in the pool, or destroyed once it has failed
    /// too often to be the network's or the code's doing.
    async fn prepare_failed(&self, p: &Placed, sandbox: &Sandbox, busy: &Labels, e: &Error) {
        self.forget_install_opened(sandbox);
        p.daemon.forget(&sandbox.id).await;
        if matches!(e, Error::SetupFailed(_)) {
            // The code failed, not the sandbox, and the same error would come from any other: it is released
            // rather than destroyed as broken. Release restores a warm checkpoint, so a sandbox that was idle
            // returns to the pool; a new one has none yet (the checkpoint comes after a good prepare) and is
            // destroyed, costing nothing a retry wouldn't pay again, as `setup` would fail on it too.
            tracing::warn!(sandbox = %sandbox.name, error = %e, "setup failed on a fresh run; releasing the sandbox");
            self.give_back(sandbox, busy).await;
            return;
        }
        // One failure is as likely a network blip or a briefly broken `main` as a bad sandbox, and releasing
        // restores the warm checkpoint, wiping what the prepare left behind, so the cache survives. Failing
        // again and again means the sandbox itself is the problem, so it leaves the pool.
        let failures = self.pool.prepare_failed(&sandbox.id);
        if failures < PREPARE_FAILURES_BEFORE_DESTROY {
            tracing::warn!(sandbox = %sandbox.name, error = %e, failures, "prepare failed; releasing the sandbox");
            self.give_back(sandbox, busy).await;
            return;
        }
        tracing::warn!(sandbox = %sandbox.name, error = %e, failures, "prepare failed repeatedly; destroying the sandbox");
        self.pool.prepare_passed(&sandbox.id);
        match p.compute.destroy(&sandbox.id).await {
            Ok(()) => self.forget_sandbox(&sandbox.id),
            Err(d) => {
                tracing::warn!(sandbox = %sandbox.name, error = %d, "couldn't destroy a sandbox that failed to prepare; reconcile will");
            }
        }
    }

    /// Deals with a worker sandbox that was labelled busy but couldn't be started. `no_room` is a warm sandbox, not
    /// one the task was parked in, whose host had no room for a run: a warm sandbox is a cache, and its checkpoints
    /// are what fills its host, so after enough of those in a row it is destroyed. One a task was parked in may hold
    /// work nobody has pushed, so that one always waits. Anything else goes back to the pool.
    async fn not_started(&self, p: &Placed, sandbox: &Sandbox, busy: &Labels, no_room: bool, e: &Error) {
        if no_room && self.short_too_often(&sandbox.id) {
            tracing::warn!(sandbox = %sandbox.name, error = %e, "no room for a run after repeated waits; destroying the warm sandbox so the next run builds a fresh one");
            p.daemon.forget(&sandbox.id).await;
            match self.destroy_pooled(p, sandbox).await {
                Ok(()) => return,
                Err(d) => {
                    tracing::warn!(sandbox = %sandbox.name, error = %d, "couldn't destroy it; returning it to the pool");
                }
            }
        }
        // Labelled busy but never handed out: without this it holds its disk until reconcile.
        self.give_back(sandbox, busy).await;
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
        if self.pool.wants_refresh(&sandbox.name) {
            tracing::info!(sandbox = %sandbox.name, "refresh: destroying the pooled sandbox on release");
            p.daemon.forget(&sandbox.id).await;
            return self.destroy_pooled(p, sandbox).await;
        }
        if let Err(e) = p.compute.restore(&sandbox.id, &CheckpointId(warm.clone())).await {
            tracing::warn!(sandbox = %sandbox.name, error = %e, "couldn't restore the warm checkpoint; destroying");
            p.daemon.forget(&sandbox.id).await;
            return self.destroy_pooled(p, sandbox).await;
        }
        p.daemon.restored(&sandbox.id).await;
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

    /// Empties the worker pool so its sandboxes are made again from the compute's image, for a new image or warm
    /// checkpoints grown too big: idle ones are destroyed now, with their checkpoints, and busy ones when they are
    /// released, instead of being restored. The next run on each pays a cold prepare.
    pub async fn refresh_pool(&self) -> Result<Refreshed> {
        let _pick = self.pool.pick.lock().await;
        let (pooled, down) = self.list_all(&self.pool_labels()).await?;
        let mut done = Refreshed { unreachable: down.into_iter().collect(), ..Refreshed::default() };
        done.unreachable.sort();
        for s in pooled {
            if s.labels.get(label::STATE).map(String::as_str) != Some(label::IDLE) {
                self.pool.ask_refresh(&s.name);
                done.on_release.push(s.name);
                continue;
            }
            let p = self.target_of(&s);
            p.daemon.forget(&s.id).await;
            match self.destroy_pooled(p, &s).await {
                Ok(()) => {
                    tracing::info!(sandbox = %s.name, "refresh: destroyed an idle pooled sandbox");
                    done.destroyed.push(s.name);
                }
                Err(e) => {
                    tracing::warn!(sandbox = %s.name, error = %e, "refresh: couldn't destroy a pooled sandbox; labelling it broken for reconcile");
                    self.mark_broken(&s).await;
                    done.failed.push((s.name, e.to_string()));
                }
            }
        }
        done.destroyed.sort();
        done.on_release.sort();
        Ok(done)
    }

    /// Drops what the control plane keeps per sandbox, for one that no longer exists, so the maps don't grow
    /// with every sandbox a long-running server has destroyed (TRAIL-1251).
    pub(crate) fn forget_sandbox(&self, id: &SandboxId) {
        self.pool.forget(id);
    }

    /// Counts a preflight that found no room on `id`'s host, and says whether that makes too many in a row.
    fn short_too_often(&self, id: &SandboxId) -> bool {
        self.pool.short_again(id) >= SHORT_PREFLIGHTS_BEFORE_DESTROY
    }

    async fn destroy_pooled(&self, p: &Placed, sandbox: &Sandbox) -> Result<()> {
        p.compute.destroy(&sandbox.id).await?;
        self.forget_sandbox(&sandbox.id);
        self.pool.refreshed(&sandbox.name);
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
            match self.destroy_pooled(p, sandbox).await {
                Ok(()) => {
                    tracing::info!(sandbox = %name, "destroyed a broken pooled sandbox");
                    self.pool.untroubled(name);
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
                self.pool.untroubled(name);
                done.released.push(name.clone());
            }
            Err(e) if self.failed(name) => {
                self.abandon(sandbox, &e, started.elapsed()).await;
                done.abandoned.push(name.clone());
            }
            Err(e) => {
                tracing::warn!(sandbox = %name, error = %e, took = ?started.elapsed(), "couldn't release a pooled sandbox; will retry");
            }
        }
    }

    /// Takes a sandbox out of the pool for good: destroyed if the compute host allows it, otherwise labelled
    /// [`label::BROKEN`] without a role (so it loses connector access) and destroyed by a later reconcile.
    async fn abandon(&self, sandbox: &Sandbox, error: &Error, took: Duration) {
        let p = self.target_of(sandbox);
        let name = &sandbox.name;
        tracing::error!(
            sandbox = %name,
            %error,
            ?took,
            "giving up on a pooled sandbox that won't release; leaving it out of the pool and starting a new one"
        );
        self.pool.abandon(name);
        if p.compute.destroy(&sandbox.id).await.is_ok() {
            self.forget_sandbox(&sandbox.id);
            self.status.sandbox_gone(name);
            self.pool.untroubled(name);
            return;
        }
        self.mark_broken(sandbox).await;
    }

    /// Labels a sandbox [`label::BROKEN`], without a role, for a later reconcile to destroy.
    async fn mark_broken(&self, sandbox: &Sandbox) {
        let p = self.target_of(sandbox);
        let mut labels = self.pool_labels();
        labels.insert(label::STATE.into(), label::BROKEN.into());
        labels.insert(label::TARGET.into(), p.target.name.clone());
        match p.compute.set_labels(&sandbox.id, &labels).await {
            Ok(()) => self.status.sandbox(&Sandbox { labels, ..sandbox.clone() }),
            Err(e) => tracing::error!(
                sandbox = %sandbox.name,
                error = %e,
                "couldn't label it broken either; it's out of the pool until this control plane restarts"
            ),
        }
    }

    pub(crate) fn abandoned(&self, name: &str) -> bool {
        self.pool.abandoned(name)
    }

    /// Whether a troubled sandbox's backoff has run out, or it isn't troubled.
    pub(crate) fn due(&self, name: &str) -> bool {
        self.pool.due(name)
    }

    /// Records a failure and backs off. True once the sandbox has used up its tries or its patience.
    fn failed(&self, name: &str) -> bool {
        self.pool.failed(name)
    }
}

/// The first wait between tries at a troubled sandbox, doubling up to [`MAX_BACKOFF`].
const FIRST_BACKOFF: Duration = Duration::from_secs(30);

const MAX_BACKOFF: Duration = Duration::from_secs(5 * 60);

/// A pooled sandbox that keeps failing to release. Kept in memory only, so a restart counts again from zero.
struct Trouble {
    failures: u32,
    since: Instant,
    next_try: Instant,
    /// Given up on: out of the pool and the sandbox cap, even if labelling it broken failed too.
    abandoned: bool,
}

/// What the factory remembers of its pooled sandboxes. In memory: a restart counts again from nothing.
#[derive(Default)]
pub(crate) struct PoolState {
    troubled: Mutex<HashMap<String, Trouble>>,
    /// When each task holding a pooled sandbox was first seen parked, so `release_parked_after` counts from there. In
    /// memory: a restart starts the wait again (TRAIL-601).
    parked_since: Mutex<HashMap<TaskKey, Instant>>,
    /// Held while a worker picks an idle pooled sandbox (or a name for a new one) and labels it busy, so runs
    /// starting together never take the same sandbox.
    pub(crate) pick: tokio::sync::Mutex<()>,
    /// Hosts opened for a sandbox's install on top of the install set, until its run asks the daemon to confirm
    /// they're blocked again.
    install_opened: Mutex<HashMap<SandboxId, Vec<String>>>,
    /// How long each pooled sandbox's last warm checkpoint took, to weigh against its next prepare (TRAIL-707).
    warm_cost: Mutex<HashMap<SandboxId, Duration>>,
    /// Consecutive failed prepares of each pooled sandbox, cleared by a good one (TRAIL-561).
    prepare_failures: Mutex<HashMap<SandboxId, u32>>,
    /// Consecutive runs that found no room on each warm pooled sandbox's host, cleared by one that did (TRAIL-1633).
    short_preflights: Mutex<HashMap<SandboxId, u32>>,
    /// Busy pooled sandboxes a refresh asked for, by name: destroyed on release rather than restored. In memory: a
    /// restart forgets them, and the refresh is asked again.
    refresh: Mutex<HashSet<String>>,
}

/// One more in a row for `id`, and how many that makes.
fn count_again(counts: &Mutex<HashMap<SandboxId, u32>>, id: &SandboxId) -> u32 {
    let mut counts = counts.lock().unwrap();
    let n = counts.entry(id.clone()).or_default();
    *n += 1;
    *n
}

impl PoolState {
    /// Drops what is kept per sandbox, for one that no longer exists. A new per-sandbox map is cleared here too.
    fn forget(&self, id: &SandboxId) {
        self.prepare_failures.lock().unwrap().remove(id);
        self.warm_cost.lock().unwrap().remove(id);
        self.short_preflights.lock().unwrap().remove(id);
    }

    /// How many entries the per-sandbox maps hold between them.
    pub(crate) fn entries(&self) -> usize {
        self.prepare_failures.lock().unwrap().len()
            + self.warm_cost.lock().unwrap().len()
            + self.short_preflights.lock().unwrap().len()
    }

    /// Counts a failed prepare of a sandbox, and says how many it has had in a row.
    fn prepare_failed(&self, id: &SandboxId) -> u32 {
        count_again(&self.prepare_failures, id)
    }

    /// A prepare went through, or the sandbox is gone: its failed prepares in a row are over.
    fn prepare_passed(&self, id: &SandboxId) {
        self.prepare_failures.lock().unwrap().remove(id);
    }

    /// Counts a run that found no room on a sandbox's host, and says how many have in a row.
    fn short_again(&self, id: &SandboxId) -> u32 {
        count_again(&self.short_preflights, id)
    }

    /// A run found room on the sandbox's host: its runs in a row without are over.
    fn had_room(&self, id: &SandboxId) {
        self.short_preflights.lock().unwrap().remove(id);
    }

    /// How long the sandbox's last warm checkpoint took, when one was timed.
    fn warm_cost(&self, id: &SandboxId) -> Option<Duration> {
        self.warm_cost.lock().unwrap().get(id).copied()
    }

    /// Keeps how long the sandbox's warm checkpoint took, or with `None` forgets it.
    fn set_warm_cost(&self, id: &SandboxId, cost: Option<Duration>) {
        let mut costs = self.warm_cost.lock().unwrap();
        match cost {
            Some(cost) => costs.insert(id.clone(), cost),
            None => costs.remove(id),
        };
    }

    /// Keeps the hosts opened for a sandbox's install on top of the install set.
    fn opened_install(&self, id: &SandboxId, hosts: Vec<String>) {
        self.install_opened.lock().unwrap().insert(id.clone(), hosts);
    }

    /// The hosts opened for a sandbox's install, which are then no longer kept.
    pub(crate) fn take_install(&self, id: &SandboxId) -> Vec<String> {
        self.install_opened.lock().unwrap().remove(id).unwrap_or_default()
    }

    /// Asks for a busy sandbox to be destroyed on release rather than restored.
    fn ask_refresh(&self, name: &str) {
        self.refresh.lock().unwrap().insert(name.to_string());
    }

    fn wants_refresh(&self, name: &str) -> bool {
        self.refresh.lock().unwrap().contains(name)
    }

    /// The sandbox a refresh was asked for is gone.
    fn refreshed(&self, name: &str) {
        self.refresh.lock().unwrap().remove(name);
    }

    /// Notes a task seen parked while it holds a pooled sandbox: the wait before the sandbox is given up counts
    /// from the first time.
    pub(crate) fn seen_parked(&self, key: &TaskKey) {
        self.parked_since.lock().unwrap().entry(key.clone()).or_insert_with(Instant::now);
    }

    /// How long a task has been seen parked, if it has been.
    pub(crate) fn parked_for(&self, key: &TaskKey) -> Option<Duration> {
        self.parked_since.lock().unwrap().get(key).map(|since| since.elapsed())
    }

    /// Every task seen parked, and for how long.
    pub(crate) fn parked(&self) -> Vec<(TaskKey, Duration)> {
        self.parked_since.lock().unwrap().iter().map(|(key, since)| (key.clone(), since.elapsed())).collect()
    }

    pub(crate) fn unparked(&self, key: &TaskKey) {
        self.parked_since.lock().unwrap().remove(key);
    }

    pub(crate) fn forget_parked(&self) {
        self.parked_since.lock().unwrap().clear();
    }

    /// Forgets every parked task `still` doesn't keep.
    pub(crate) fn keep_parked(&self, still: impl Fn(&TaskKey) -> bool) {
        self.parked_since.lock().unwrap().retain(|key, _| still(key));
    }

    /// The sandbox released, or is gone: it is no longer in trouble.
    fn untroubled(&self, name: &str) {
        self.troubled.lock().unwrap().remove(name);
    }

    /// Gives up on a troubled sandbox: out of the pool and the cap.
    fn abandon(&self, name: &str) {
        if let Some(t) = self.troubled.lock().unwrap().get_mut(name) {
            t.abandoned = true;
        }
    }

    fn abandoned(&self, name: &str) -> bool {
        self.troubled.lock().unwrap().get(name).is_some_and(|t| t.abandoned)
    }

    /// Whether a troubled sandbox's backoff has run out, or it isn't troubled.
    fn due(&self, name: &str) -> bool {
        self.troubled.lock().unwrap().get(name).is_none_or(|t| Instant::now() >= t.next_try)
    }

    /// Records a failure to release and backs off. True once the sandbox has used up its tries or its patience.
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
