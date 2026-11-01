use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use futures_util::FutureExt;
use tokio::sync::Notify;
use tokio::task::{self, JoinSet};
use tracing::Instrument;
use trailhead_factory::{Labels, Result, Role, RunUpdate, Sandbox, Status, Task, TaskKey, label};

use crate::timing::{unix_now, until};
use crate::{FACTORY_LABEL, Factory, Outcome, Reconciled, SHUTDOWN_GRACE};

impl Factory {
    /// The control plane's main loop: reconcile, then keep up to the repo's cap of tasks running, checking for
    /// ready work when `wake` fires (on tracker events) and every `poll`. On `shutdown` it stops starting runs
    /// and abandons the ones in flight; the next boot's reconcile requeues them.
    pub async fn run(
        self: Arc<Self>,
        wake: Arc<Notify>,
        poll: Duration,
        shutdown: impl Future<Output = ()>,
    ) -> Result<()> {
        self.run_draining(wake, poll, std::future::pending(), shutdown).await
    }

    /// [`Factory::run`] with a first, gentler stop: once `drain` fires no run or review is started (reconcile keeps
    /// putting things right), the runs in flight finish as usual, and the loop returns when none are left. `shutdown`
    /// still cancels them, drain or not.
    pub async fn run_draining(
        self: Arc<Self>,
        wake: Arc<Notify>,
        poll: Duration,
        drain: impl Future<Output = ()>,
        shutdown: impl Future<Output = ()>,
    ) -> Result<()> {
        tokio::pin!(shutdown);
        tokio::pin!(drain);
        let mut draining = false;
        let mut running: JoinSet<Result<Outcome>> = JoinSet::new();
        let mut keys: HashMap<task::Id, TaskKey> = HashMap::new();
        // Reviews reconcile found due, running beside the worker runs without taking a worker's place in the cap.
        let mut reviews: HashSet<task::Id> = HashSet::new();
        // A failed boot reconcile (the compute API down, say) is retried in the loop rather than ending the process.
        let mut dirty = true;
        loop {
            // Shutdown is otherwise only noticed while idle; a stop during a long reconcile mustn't start new work.
            if (&mut shutdown).now_or_never().is_some() {
                break;
            }
            // A run that just errored left `dirty` set: its daemon may still have it, so reconcile before concluding.
            if draining && running.is_empty() && !dirty {
                tracing::info!("drained; no runs left");
                break;
            }
            if dirty {
                dirty = false;
                match self.reconcile_deferring(&keys.values().cloned().collect()).await {
                    Ok((done, due)) => {
                        if done != Reconciled::default() || !due.reviews.is_empty() {
                            let reviewing: Vec<&str> = due.reviews.iter().map(|t| t.key.0.as_str()).collect();
                            tracing::info!(?done, ?reviewing, "reconciled");
                        }
                        // Due reviews are new work, picked up again by the next start. Due runs are runs the daemon is
                        // still working on, so a drain keeps following them and waits for them too.
                        let due = if draining { Due { reviews: Vec::new(), ..due } } else { due };
                        for (task, sandbox) in due.runs {
                            let key = task.key.clone();
                            let factory = self.clone();
                            let handle = running
                                .spawn(async move { factory.resume_run(&task, &sandbox).await }.in_current_span());
                            keys.insert(handle.id(), key);
                        }
                        // A review is a session on the same subscription window, so a pause holds them back too; the
                        // next reconcile finds them due again (the resume below triggers one).
                        let reviews_due = if self.usage_paused() { Vec::new() } else { due.reviews };
                        for task in reviews_due {
                            let key = task.key.clone();
                            let factory = self.clone();
                            let handle =
                                running.spawn(async move { factory.resume_review(&task).await }.in_current_span());
                            keys.insert(handle.id(), key);
                            reviews.insert(handle.id());
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "reconcile failed"),
                }
                if (&mut shutdown).now_or_never().is_some() {
                    break;
                }
            }
            if !draining && let Err(e) = self.start_ready(&mut running, &mut keys, reviews.len()).await {
                tracing::warn!(error = %e, "couldn't start ready tasks");
            }
            let resume = self.paused_until().map(|(_, _, resets_at)| until(resets_at));
            tokio::select! {
                _ = &mut shutdown => break,
                _ = &mut drain, if !draining => {
                    draining = true;
                    let mut flying: Vec<&str> = keys.values().map(|k| k.0.as_str()).collect();
                    flying.sort();
                    tracing::info!(
                        "draining: {} runs in flight ({}); Ctrl-C again to cancel them",
                        flying.len(),
                        flying.join(", ")
                    );
                }
                _ = wake.notified() => {}
                _ = tokio::time::sleep(resume.unwrap_or_default()), if resume.is_some() => dirty = true,
                _ = tokio::time::sleep(poll) => dirty = true,
                Some(joined) = running.join_next_with_id() => match joined {
                    Ok((id, Ok(outcome))) => {
                        reviews.remove(&id);
                        let key = keys.remove(&id);
                        tracing::info!(task = ?key, ?outcome, "run finished");
                    }
                    Ok((id, Err(e))) => {
                        reviews.remove(&id);
                        let key = keys.remove(&id);
                        tracing::warn!(task = ?key, error = %e, "run failed; reconciling");
                        dirty = true;
                    }
                    Err(e) => {
                        reviews.remove(&e.id());
                        let key = keys.remove(&e.id());
                        tracing::error!(task = ?key, error = %e, "run panicked; reconciling");
                        dirty = true;
                    }
                },
            }
        }
        // Cancel rather than just drop the runs: a daemon that loses its stream keeps the run going for a while in
        // case the control plane reattaches, and a stop on purpose mustn't leave it working unobserved (TRAIL-142).
        if !running.is_empty() {
            tracing::info!(runs = running.len(), "cancelling runs in flight");
            self.daemon.shutdown().await;
            let drained = tokio::time::timeout(SHUTDOWN_GRACE, async { while running.join_next().await.is_some() {} });
            if drained.await.is_err() {
                tracing::warn!(runs = running.len(), "runs didn't stop in time; abandoning them");
            }
        }
        running.shutdown().await;
        Ok(())
    }

    async fn start_ready(
        self: &Arc<Self>,
        running: &mut JoinSet<Result<Outcome>>,
        keys: &mut HashMap<task::Id, TaskKey>,
        reviewing: usize,
    ) -> Result<()> {
        if self.usage_paused() || !self.server_up().await {
            return Ok(());
        }
        let busy: HashSet<TaskKey> = keys.values().cloned().collect();
        let others = self
            .occupied_after_release(&busy)
            .await?
            .into_iter()
            .filter(|s| s.labels.get(label::TASK).is_none_or(|t| !busy.contains(&TaskKey(t.clone()))))
            .count();
        let room = self.room(keys.len() - reviewing + others).await?;
        if room == 0 {
            if keys.len() > reviewing {
                *self.starved.lock().unwrap() = None;
            } else if others > 0 {
                self.warn_if_starved().await;
            }
            return Ok(());
        }
        *self.starved.lock().unwrap() = None;
        let ready = self.ready_graded(room).await?;
        for task in ready.into_iter().filter(|t| !busy.contains(&t.key) && self.setback_over(&t.key)) {
            let key = task.key.clone();
            let factory = self.clone();
            let handle = running.spawn(async move { factory.run_task(&task).await }.in_current_span());
            keys.insert(handle.id(), key);
        }
        Ok(())
    }

    /// With no place left and nothing running, parked tasks are holding the cap. If ready tasks are waiting, say so
    /// loudly, once per change in who waits: the queue stalls until a person answers (TRAIL-538).
    async fn warn_if_starved(&self) {
        let mut waiting: Vec<TaskKey> = match self.ready_graded(usize::MAX).await {
            Ok(ready) => ready.into_iter().map(|t| t.key).collect(),
            Err(e) => {
                tracing::warn!(error = %e, "couldn't list the ready tasks to see whether parked ones starve them");
                return;
            }
        };
        waiting.sort();
        let mut last = self.starved.lock().unwrap();
        if waiting.is_empty() {
            *last = None;
        } else if last.as_ref() != Some(&waiting) {
            let names: Vec<&str> = waiting.iter().map(|k| k.0.as_str()).collect();
            tracing::warn!(
                tasks = names.join(", "),
                max_sandboxes = self.config.repo.max_sandboxes,
                "{} ready tasks waiting; all sandboxes held by parked tasks. Answer or cancel them to free a place",
                waiting.len()
            );
            *last = Some(waiting);
        }
    }

    /// Up to `room` ready tasks that are the factory's work. Under a queue that means graded ones: grading a task is
    /// what queues it. An ungraded one is skipped and reported once, in the log and on the task, rather than on every
    /// poll (`docs/factory/review.md#grades`).
    async fn ready_graded(&self, room: usize) -> Result<Vec<Task>> {
        let queue = self.queue();
        // With no queue the whole project is the factory's by configuration, so a grade is not what releases a task.
        if queue.is_empty() {
            return self.tracker.ready(self.project(), &[], room).await;
        }
        // Ungraded tasks mustn't crowd graded ones out of the limit.
        let ready = self.tracker.ready(self.project(), &queue, usize::MAX).await?;
        let (graded, ungraded): (Vec<Task>, Vec<Task>) = ready.into_iter().partition(|t| t.grade.is_some());
        for task in ungraded {
            self.report_ungraded(&task).await;
        }
        for task in &graded {
            self.clear_ungraded(task).await;
        }
        Ok(graded.into_iter().take(room).collect())
    }

    /// A task that has been graded is no longer ungraded, so it's told again if its grade is ever cleared.
    async fn clear_ungraded(&self, task: &Task) {
        let was_reported = self.ungraded.lock().unwrap().remove(&task.key);
        if task.run.ungraded != Some(true) {
            return;
        }
        let update = RunUpdate { ungraded: Some(false), ..RunUpdate::default() };
        if let Err(e) = self.tracker.report(&task.key, update).await {
            // Keep the key out of the set only if nothing was cleared, so a failed write is retried next poll.
            if was_reported {
                self.ungraded.lock().unwrap().insert(task.key.clone());
            }
            tracing::warn!(task = %task.key.0, error = %e, "couldn't clear the task's ungraded flag");
        }
    }

    async fn report_ungraded(&self, task: &Task) {
        if task.run.ungraded == Some(true) || !self.ungraded.lock().unwrap().insert(task.key.clone()) {
            return;
        }
        tracing::info!(task = %task.key.0, "no grade; skipping it until a human sets one");
        let told = async {
            self.tracker.note(&task.key, "no grade; set one to queue it").await?;
            self.tracker.report(&task.key, RunUpdate { ungraded: Some(true), ..RunUpdate::default() }).await
        };
        if let Err(e) = told.await {
            tracing::warn!(task = %task.key.0, error = %e, "couldn't note that the task has no grade");
        }
    }

    /// The worker sandboxes that take a place in the cap: any with a worker role, and busy pooled ones, which
    /// include a parked task's whose kill switch took its role away. Abandoned ones don't count (TRAIL-135).
    async fn occupied(&self) -> Result<Vec<Sandbox>> {
        let mut workers = self.base_labels();
        workers.insert(label::ROLE.into(), Role::Worker.as_str().into());
        let mut busy = self.pool_labels();
        busy.insert(label::STATE.into(), label::BUSY.into());
        let mut out: Vec<Sandbox> = Vec::new();
        for s in self.compute.list(&workers).await?.into_iter().chain(self.compute.list(&busy).await?) {
            if !self.abandoned(&s.name) && !out.iter().any(|o| o.name == s.name) {
                out.push(s);
            }
        }
        Ok(out)
    }

    /// Places left to start runs in, given how many this repo holds: the repo's cap, and the host's when it has one,
    /// counted from the compute host's sandboxes in every repo so a restart sees the truth.
    async fn room(&self, own: usize) -> Result<usize> {
        let repo_room = self.config.repo.max_sandboxes.saturating_sub(own);
        let Some(cap) = self.host.max_sandboxes else { return Ok(repo_room) };
        let host_room = cap.saturating_sub(own + self.occupied_elsewhere().await?);
        Ok(repo_room.min(host_room))
    }

    /// Worker sandboxes held by other repos' factories, which also take places under the host's cap.
    async fn occupied_elsewhere(&self) -> Result<usize> {
        let mine = self.config.repo.repo().slug();
        let factory = Labels::from([(label::FACTORY.into(), FACTORY_LABEL.into())]);
        let mut workers = factory.clone();
        workers.insert(label::ROLE.into(), Role::Worker.as_str().into());
        let mut busy = factory;
        busy.insert(label::POOL.into(), Role::Worker.as_str().into());
        busy.insert(label::STATE.into(), label::BUSY.into());
        let mut names = HashSet::new();
        for s in self.compute.list(&workers).await?.into_iter().chain(self.compute.list(&busy).await?) {
            if s.labels.get(label::REPO).is_some_and(|r| *r != mine) {
                names.insert(s.name);
            }
        }
        Ok(names.len())
    }

    /// [`Factory::occupied`] once the sandboxes held for tasks a human has reopened, cancelled or finished are
    /// released. A parked task keeps its sandbox, so without this the place stays taken until the next reconcile
    /// (TRAIL-108, TRAIL-257). A reopened task's next attempt starts from a fresh checkout with the answer in its prompt.
    async fn occupied_after_release(&self, running: &HashSet<TaskKey>) -> Result<Vec<Sandbox>> {
        let held = self.occupied().await?;
        let candidates: Vec<&Sandbox> = held
            .iter()
            .filter(|s| s.labels.contains_key(label::POOL))
            .filter(|s| s.labels.get(label::TASK).is_some_and(|t| !running.contains(&TaskKey(t.clone()))))
            .collect();
        if candidates.is_empty() {
            return Ok(held);
        }
        let mut released = Vec::new();
        for sandbox in candidates {
            let Some(key) = sandbox.labels.get(label::TASK) else { continue };
            let task = match self.tracker.show(&TaskKey(key.clone())).await {
                Ok(task) if matches!(task.status, Status::Open | Status::Cancelled | Status::Done) => task,
                Ok(_) => continue,
                Err(e) => {
                    tracing::warn!(task = %key, error = %e, "couldn't look up a parked task's status");
                    continue;
                }
            };
            let mut done = Reconciled::default();
            self.reconcile_pooled(sandbox, &mut done).await;
            if !done.released.is_empty() || !done.abandoned.is_empty() || !done.destroyed.is_empty() {
                tracing::info!(task = %task.key.0, sandbox = %sandbox.name, "released the sandbox of a task no longer parked");
                if let Err(e) = self.reconcile_task(&task, &mut done).await {
                    tracing::warn!(task = %task.key.0, error = %e, "couldn't clean up a released task; reconcile will");
                }
                released.push(sandbox.name.clone());
            }
        }
        if released.is_empty() {
            return Ok(held);
        }
        Ok(held.into_iter().filter(|s| !released.contains(&s.name)).collect())
    }

    /// Starts runs for ready tasks, up to the repo's sandbox cap, one after another. For tests and one-shot use.
    pub async fn tick(&self) -> Result<Vec<(Task, Outcome)>> {
        if self.usage_paused() || !self.server_up().await {
            return Ok(Vec::new());
        }
        let room = self.room(self.occupied_after_release(&HashSet::new()).await?.len()).await?;
        if room == 0 {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let ready = self.ready_graded(room).await?;
        for task in ready.into_iter().filter(|t| self.setback_over(&t.key)) {
            let outcome = self.run_task(&task).await?;
            out.push((task, outcome));
        }
        Ok(out)
    }

    /// The fullest usage window at or past its `pause_at`, if any, as (window, percent used, resets at). A reading
    /// whose window has since reset no longer counts.
    fn paused_until(&self) -> Option<(String, u32, i64)> {
        let now = unix_now();
        let quota = self.host.quota.lock().unwrap();
        self.config
            .usage
            .pause_at
            .iter()
            .filter_map(|(window, limit)| {
                let (used, resets_at) = *quota.get(window)?;
                (used >= *limit && resets_at > now).then(|| (window.clone(), used, resets_at))
            })
            .max_by_key(|(_, _, resets_at)| *resets_at)
    }

    /// Whether to hold off starting runs because a usage window is nearly full. Running ones carry on.
    fn usage_paused(&self) -> bool {
        let paused = self.paused_until();
        let mut logged = self.host.pause_logged.lock().unwrap();
        match &paused {
            Some((window, used, resets_at)) if *logged != Some(*resets_at) => {
                tracing::warn!(
                    %window,
                    used_percent = used,
                    resumes_in = ?until(*resets_at),
                    "subscription usage window nearly full; starting no new runs until it resets"
                );
                *logged = Some(*resets_at);
            }
            None if logged.take().is_some() => tracing::info!("usage window reset; starting runs again"),
            _ => {}
        }
        paused.is_some()
    }
}

/// What reconcile found to carry on with, for the caller to run beside whatever else it does.
#[derive(Default)]
pub(crate) struct Due {
    /// Tasks whose PR is due a review.
    pub(crate) reviews: Vec<Task>,
    /// Worker runs still going in their sandboxes, to follow again.
    pub(crate) runs: Vec<(Task, Sandbox)>,
}
