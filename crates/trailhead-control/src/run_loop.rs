use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use futures_util::FutureExt;
use tokio::sync::Notify;
use tokio::task::{self, JoinSet};
use tokio::time::Instant;
use tracing::Instrument;
use trailhead_factory::messages;
use trailhead_factory::{ControlState, Labels, Result, Role, RunUpdate, Sandbox, Status, Task, TaskKey, label};

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
            if dirty {
                dirty = false;
                self.status.control(ControlState::Reconciling);
                match self.reconcile_deferring(&keys.values().cloned().collect()).await {
                    Ok((done, due)) => {
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
                        // A review is a session on the same subscription window, so a pause holds them back too. The
                        // next reconcile finds them due again (the resume below triggers one, and so does the poll).
                        // Due runs aren't held: the daemon already has them.
                        let hold = !due.reviews.is_empty() && self.usage_paused();
                        let reviews_due = if hold { Vec::new() } else { due.reviews };
                        // Logged after the hold, so a pause doesn't list reviews that never start on every poll.
                        if done != Reconciled::default() || !reviews_due.is_empty() {
                            let reviewing: Vec<&str> = reviews_due.iter().map(|t| t.key.0.as_str()).collect();
                            tracing::info!(?done, ?reviewing, "reconciled");
                        }
                        for task in reviews_due {
                            let key = task.key.clone();
                            let factory = self.clone();
                            let handle =
                                running.spawn(async move { factory.resume_review(&task).await }.in_current_span());
                            keys.insert(handle.id(), key);
                            reviews.insert(handle.id());
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "reconcile failed");
                        self.status.error(format!("reconcile failed: {e}"));
                        // A drain mustn't conclude on a reconcile that couldn't look: a daemon may still have a run.
                        // Retry at the next poll; a shutdown still ends it.
                        dirty = draining;
                    }
                }
                self.status.control(ControlState::Up);
                if (&mut shutdown).now_or_never().is_some() {
                    break;
                }
            }
            // A run that just errored set `dirty`: its daemon may still have it, so the reconcile above runs first and
            // a drain concludes only if that found nothing, and
            // looked successfully. A poll re-dirtying an idle loop mustn't hold it open.
            if draining && running.is_empty() && !dirty {
                tracing::info!("drained; no runs left");
                break;
            }
            if !draining && let Err(e) = self.start_ready(&mut running, &mut keys, reviews.len()).await {
                tracing::warn!(error = %e, "couldn't start ready tasks");
                self.status.error(format!("couldn't start ready tasks: {e}"));
            }
            let resume = self
                .paused_until()
                .map(|(_, _, resets_at)| until(resets_at))
                .into_iter()
                .chain(self.code_host_pause().map(until))
                .min();
            let poll = self.polled_every(poll);
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
                        tracing::info!(task = key.as_ref().map(|k| k.0.as_str()), ?outcome, "run finished");
                    }
                    Ok((id, Err(e))) => {
                        reviews.remove(&id);
                        let key = keys.remove(&id);
                        tracing::warn!(task = key.as_ref().map(|k| k.0.as_str()), error = %e, "run failed; reconciling");
                        self.status.error(format!("{} failed: {e}", key.as_ref().map_or("a run", |k| k.0.as_str())));
                        dirty = true;
                    }
                    Err(e) => {
                        reviews.remove(&e.id());
                        let key = keys.remove(&e.id());
                        tracing::error!(task = key.as_ref().map(|k| k.0.as_str()), error = %e, "run panicked; reconciling");
                        dirty = true;
                    }
                },
            }
        }
        // Cancel rather than just drop the runs: a daemon that loses its stream keeps the run going for a while in
        // case the control plane reattaches, and a stop on purpose mustn't leave it working unobserved (TRAIL-142).
        if !running.is_empty() {
            tracing::info!(runs = running.len(), "cancelling runs in flight");
            for p in self.targets() {
                p.daemon.shutdown().await;
            }
            let drained = tokio::time::timeout(SHUTDOWN_GRACE, async { while running.join_next().await.is_some() {} });
            if drained.await.is_err() {
                tracing::warn!(runs = running.len(), "runs didn't stop in time; abandoning them");
            }
        }
        running.shutdown().await;
        self.status.control(ControlState::Stopped);
        Ok(())
    }

    async fn start_ready(
        self: &Arc<Self>,
        running: &mut JoinSet<Result<Outcome>>,
        keys: &mut HashMap<task::Id, TaskKey>,
        reviewing: usize,
    ) -> Result<()> {
        // Before any early return, so the status board's spend keeps moving while the factory is full or paused.
        self.capped();
        if self.usage_paused() {
            return Ok(());
        }
        let busy: HashSet<TaskKey> = keys.values().cloned().collect();
        let mut others = self.held_by_others(&busy).await?;
        let mut room = self.room(keys.len() - reviewing + others.len()).await?;
        if room == 0 && self.release_parked_for_waiting(&busy, &others).await > 0 {
            others = self.held_by_others(&busy).await?;
            room = self.room(keys.len() - reviewing + others.len()).await?;
        }
        // Skipped before taking `room`, so a task still backing off at the head of the queue can't keep the ones
        // behind it from starting (TRAIL-756).
        let startable: Vec<Task> = self
            .ready_graded()
            .await?
            .into_iter()
            .filter(|t| !busy.contains(&t.key) && self.setback_over(&t.key))
            .collect();
        if room == 0 && !startable.iter().any(|t| others.contains(&t.key)) {
            if keys.len() > reviewing {
                *self.starved.lock().unwrap() = None;
            } else if parked_fill_repo_cap(others.len(), self.config.repo.max_sandboxes) {
                self.warn_if_starved(&others).await;
            }
            return Ok(());
        }
        *self.starved.lock().unwrap() = None;
        let mut survey = self.survey().await;
        for task in startable {
            // A ready task that holds its own sandbox resumes in it, so it needs no room (TRAIL-1121).
            if !others.contains(&task.key) {
                if room == 0 {
                    continue;
                }
                room -= 1;
            }
            let Some(target) = self.place_task(&mut survey, &task) else {
                tracing::info!("{}", messages::no_target_has_room(&task.key.0));
                continue;
            };
            let key = task.key.clone();
            let factory = self.clone();
            let (target, place) = (target.clone(), self.hold(&key, Role::Worker, target));
            let handle =
                running.spawn(async move { factory.run_task_on(&task, &target, place).await }.in_current_span());
            keys.insert(handle.id(), key);
        }
        Ok(())
    }

    /// The sandboxes in the cap held for tasks this process isn't running: parked tasks', mostly.
    async fn held_by_others(&self, busy: &HashSet<TaskKey>) -> Result<Others> {
        let others = self
            .occupied_after_release(busy)
            .await?
            .iter()
            .map(|s| s.labels.get(label::TASK).map(|t| TaskKey(t.clone())))
            .filter(|t| t.as_ref().is_none_or(|t| !busy.contains(t)))
            .collect();
        Ok(Others(others))
    }

    /// With the cap full, gives up the pooled sandboxes of parked tasks to ready tasks that are waiting for a place,
    /// one per waiting task, longest parked first (TRAIL-601). A sandbox goes only when `release_parked_after` minutes
    /// have passed and releasing loses nothing: the daemon finds no work in the checkout that isn't on the remote
    /// branch the task resumes from. The task keeps its place in the queue; answered, it starts like a rework on a
    /// pooled sandbox. Returns how many were released. A failure is logged and the sandbox kept or left to reconcile.
    async fn release_parked_for_waiting(&self, busy: &HashSet<TaskKey>, others: &Others) -> usize {
        let after = self.config.repo.release_parked_after;
        if after == 0 {
            return 0;
        }
        let delay = Duration::from_secs(u64::from(after) * 60);
        let mut due: Vec<(TaskKey, Duration)> = self
            .parked_since
            .lock()
            .unwrap()
            .iter()
            .filter(|(key, _)| !busy.contains(*key))
            .map(|(key, since)| (key.clone(), since.elapsed()))
            .filter(|(_, parked_for)| *parked_for >= delay)
            .collect();
        if due.is_empty() {
            return 0;
        }
        due.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let held = match self.occupied().await {
            Ok(held) => held,
            Err(e) => {
                tracing::warn!(error = %e, "couldn't list the sandboxes held by parked tasks");
                return 0;
            }
        };
        let waiting = match self.ready_graded().await {
            Ok(ready) => ready
                .into_iter()
                .filter(|t| !busy.contains(&t.key) && self.setback_over(&t.key) && !others.contains(&t.key))
                .count(),
            Err(e) => {
                tracing::warn!(error = %e, "couldn't list the ready tasks to see whether a parked task's sandbox is wanted");
                return 0;
            }
        };
        if waiting == 0 {
            return 0;
        }
        let mut released = 0;
        for (key, parked_for) in due {
            if released == waiting {
                break;
            }
            let Some(sandbox) = held
                .iter()
                .find(|s| s.labels.contains_key(label::POOL) && s.labels.get(label::TASK).is_some_and(|t| *t == key.0))
            else {
                continue;
            };
            let task = match self.tracker.show(&key).await {
                Ok(task) => task,
                Err(e) => {
                    tracing::warn!(task = %key.0, error = %e, "couldn't look up a parked task to release its sandbox");
                    continue;
                }
            };
            // A budget breach leaves its sandbox suspended for a person to look at; that is what it is kept for.
            if task.status != Status::NeedsInput || task.run.worker_breach.is_some() {
                continue;
            }
            // The asker's lease keeps everyone else's writes out until it lapses, so the release couldn't be recorded
            // below; don't wake the suspended daemon to check for unpublished work until then.
            if task.holder.as_deref().is_some_and(|h| h != self.tracker.agent()) {
                tracing::debug!(task = %key.0, holder = ?task.holder, "a parked task is still claimed; keeping its sandbox for now");
                continue;
            }
            // What the task resumes from: its PR branch on a rework, else the base branch.
            let remote = if task.run.pr.is_some() { Self::branch(&task) } else { self.config.repo.base_branch.clone() };
            match self.daemon_of(sandbox).unpublished(&sandbox.id, &remote).await {
                Ok(false) => {}
                Ok(true) => {
                    tracing::debug!(task = %key.0, sandbox = %sandbox.name, "a parked task has unpublished work; keeping its sandbox");
                    continue;
                }
                Err(e) => {
                    tracing::debug!(task = %key.0, sandbox = %sandbox.name, error = %e, "couldn't tell whether a parked task has unpublished work; keeping its sandbox");
                    continue;
                }
            }
            // Recorded before the release, so reconcile and `factory status` don't look for a sandbox that is going.
            let record = RunUpdate { released: Some(unix_now()), ..RunUpdate::default() };
            if let Err(e) = self.tracker.report(&key, record).await {
                // Most likely the asker's lease on the task hasn't lapsed yet, which keeps everyone else out of it.
                tracing::debug!(task = %key.0, error = %e, "couldn't record that a parked task's sandbox is released; keeping it for now");
                continue;
            }
            self.parked_since.lock().unwrap().remove(&key);
            released += 1;
            tracing::info!(task = %key.0, parked_for = ?parked_for, "released a parked task's sandbox for waiting work");
            if let Err(e) = self.release_worker(sandbox).await {
                tracing::warn!(task = %key.0, sandbox = %sandbox.name, error = %e, "couldn't release a parked task's sandbox; reconcile will");
            }
        }
        released
    }

    /// With no place left and nothing running, the tasks the sandboxes are held for are what the queue waits on. If
    /// they are parked and ready tasks are waiting, say so loudly, once per change in who holds: the queue stalls
    /// until a person answers (TRAIL-538). Holders in setback or otherwise in flight free their place unaided, so
    /// they're named at info instead (TRAIL-1184).
    async fn warn_if_starved(&self, others: &Others) {
        let waiting = match self.ready_graded().await {
            Ok(ready) => ready.into_iter().filter(|t| !others.contains(&t.key)).count(),
            Err(e) => {
                tracing::warn!(error = %e, "couldn't list the ready tasks to see whether held sandboxes starve them");
                return;
            }
        };
        let mut holders: Vec<(TaskKey, bool)> = Vec::new();
        for key in others.0.iter().flatten() {
            let parked = match self.tracker.show(key).await {
                Ok(task) => task.status == Status::NeedsInput,
                Err(e) => {
                    tracing::warn!(task = %key.0, error = %e, "couldn't look up the task holding a sandbox");
                    false
                }
            };
            holders.push((key.clone(), parked));
        }
        holders.sort();
        let mut last = self.starved.lock().unwrap();
        if waiting == 0 {
            *last = None;
            return;
        }
        if last.as_ref() == Some(&holders) {
            return;
        }
        let parked: Vec<&str> = holders.iter().filter(|(_, p)| *p).map(|(k, _)| k.0.as_str()).collect();
        let max_sandboxes = self.config.repo.max_sandboxes;
        let tasks = if waiting == 1 { "task" } else { "tasks" };
        if parked_fill_repo_cap(parked.len(), max_sandboxes) {
            tracing::warn!(
                held_by = parked.join(", "),
                max_sandboxes,
                "{waiting} ready {tasks} waiting; all sandboxes held by parked tasks. Answer or cancel them to free a place",
            );
        } else {
            let held: Vec<String> = holders
                .iter()
                .map(|(k, parked)| {
                    let retry = self.setbacks.lock().unwrap().get(k).map(|(_, _, next_try)| *next_try);
                    describe_holder(&k.0, *parked, retry.map(|t| t.saturating_duration_since(Instant::now())))
                })
                .collect();
            tracing::info!(max_sandboxes, "{waiting} ready {tasks} waiting; sandboxes held by {}", held.join(", "));
        }
        *last = Some(holders);
    }

    /// The ready tasks that are the factory's work, in the tracker's order. Then reworks of open PRs go first within a priority (below). Under a queue that means graded ones: grading a task is
    /// what queues it. An ungraded one is skipped and reported once, in the log and on the task, rather than on every
    /// poll (`docs/factory/review.md#grades`).
    async fn ready_graded(&self) -> Result<Vec<Task>> {
        let queue = self.queue();
        // With no queue the whole project is the factory's by configuration, so a grade is not what releases a task.
        if queue.is_empty() {
            return Ok(in_start_order(self.tracker.ready(self.project(), &[], usize::MAX).await?));
        }
        let ready = self.tracker.ready(self.project(), &queue, usize::MAX).await?;
        let (graded, ungraded): (Vec<Task>, Vec<Task>) = ready.into_iter().partition(|t| t.grade.is_some());
        for task in ungraded {
            self.report_ungraded(&task).await;
        }
        for task in &graded {
            self.clear_ungraded(task).await;
        }
        Ok(in_start_order(graded))
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
            self.tracker.note(&task.key, &messages::ungraded_note()).await?;
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
        let listed = self.list_all(&workers).await?.0.into_iter().chain(self.list_all(&busy).await?.0);
        for s in listed {
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
        let listed = self.list_all(&workers).await?.0.into_iter().chain(self.list_all(&busy).await?.0);
        for s in listed {
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
            self.parked_since.lock().unwrap().clear();
            return Ok(held);
        }
        let mut parked = HashSet::new();
        let mut released = Vec::new();
        // One listing stands in for a lookup per sandbox: parked tasks, the common case, are in it. Only a task it
        // lacks (finished, cancelled, re-parented) is looked up on its own (TRAIL-258).
        let mut listing_ok = false;
        let listed: HashMap<TaskKey, Task> = match self.tracker.in_flight(self.project(), &self.queue()).await {
            Ok(tasks) => {
                listing_ok = true;
                tasks.into_iter().map(|t| (t.key.clone(), t)).collect()
            }
            Err(e) => {
                // Debug, not warn: this runs every poll, and the fallback below works, with its own warnings.
                tracing::debug!(error = %e, "couldn't list the tasks in flight; looking parked ones up one by one");
                HashMap::new()
            }
        };
        for sandbox in candidates {
            let Some(key) = sandbox.labels.get(label::TASK) else { continue };
            let found = match listed.get(&TaskKey(key.clone())) {
                Some(task) => Ok(task.clone()),
                None => self.tracker.show(&TaskKey(key.clone())).await,
            };
            // A live task the queues' listing lacks was taken off its queue by hand (TRAIL-1147): the factory no
            // longer owns it, so its sandbox is released rather than held until the task closes.
            let off_queue = listing_ok && !self.queue().is_empty() && !listed.contains_key(&TaskKey(key.clone()));
            let task = match found {
                Ok(task) if matches!(task.status, Status::Open | Status::Cancelled | Status::Done) => task,
                Ok(task)
                    if off_queue
                        && matches!(task.status, Status::NeedsInput | Status::InProgress | Status::InReview) =>
                {
                    tracing::info!(task = %task.key.0, sandbox = %sandbox.name, "a task left the factory queue; releasing its sandbox");
                    let mut done = Reconciled::default();
                    self.reconcile_pooled(sandbox, &mut done).await;
                    if !done.released.is_empty() || !done.abandoned.is_empty() || !done.destroyed.is_empty() {
                        released.push(sandbox.name.clone());
                    }
                    continue;
                }
                Ok(task) => {
                    // The wait before a sandbox is given up (`release_parked_after`) counts from the first look.
                    if task.status == Status::NeedsInput {
                        let key = task.key.clone();
                        self.parked_since.lock().unwrap().entry(key.clone()).or_insert_with(Instant::now);
                        parked.insert(key);
                    }
                    continue;
                }
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
        self.parked_since.lock().unwrap().retain(|key, _| parked.contains(key));
        if released.is_empty() {
            return Ok(held);
        }
        Ok(held.into_iter().filter(|s| !released.contains(&s.name)).collect())
    }

    /// Starts runs for ready tasks, up to the repo's sandbox cap, one after another. For tests and one-shot use.
    pub async fn tick(&self) -> Result<Vec<(Task, Outcome)>> {
        self.capped();
        if self.usage_paused() {
            return Ok(Vec::new());
        }
        let none = HashSet::new();
        let mut others = self.held_by_others(&none).await?;
        let mut room = self.room(others.len()).await?;
        if room == 0 && self.release_parked_for_waiting(&none, &others).await > 0 {
            others = self.held_by_others(&none).await?;
            room = self.room(others.len()).await?;
        }
        let ready = self.ready_graded().await?;
        let startable: Vec<Task> = ready.into_iter().filter(|t| self.setback_over(&t.key)).collect();
        if room == 0 && !startable.iter().any(|t| others.contains(&t.key)) {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let mut survey = self.survey().await;
        for task in startable {
            if !others.contains(&task.key) {
                if room == 0 {
                    continue;
                }
                room -= 1;
            }
            let Some(target) = self.place_task(&mut survey, &task) else {
                tracing::info!("{}", messages::no_target_has_room(&task.key.0));
                continue;
            };
            let place = self.hold(&task.key, Role::Worker, target);
            let outcome = self.run_task_on(&task, target, place).await?;
            out.push((task, outcome));
        }
        Ok(out)
    }

    /// The fullest usage window at or past its `pause_at`, if any, as (window, percent used, resets at). A reading
    /// whose window has since reset no longer counts.
    fn paused_until(&self) -> Option<(String, u32, i64)> {
        let now = unix_now();
        let quota = self.host.quota.lock().unwrap();
        self.host
            .pause_at
            .iter()
            .filter_map(|(window, limit)| {
                let (used, resets_at) = *quota.get(window)?;
                (used >= *limit && resets_at > now).then(|| (window.clone(), used, resets_at))
            })
            .max_by_key(|(_, _, resets_at)| *resets_at)
    }

    /// When the code host's rate limit resets, if it is nearly spent (10% or less left). The merger's passes wait for
    /// it: the limit is shared with Lane's own tools, so the factory leaves the last of it alone (TRAIL-535).
    pub(crate) fn code_host_pause(&self) -> Option<i64> {
        let rate = self.codehost.as_ref()?.rate_limit()?;
        (rate.remaining.saturating_mul(10) <= rate.limit && rate.resets_at > unix_now()).then_some(rate.resets_at)
    }

    /// `poll`, stretched as the code host's rate limit runs down: twice as long under half left, four times under 30%.
    pub fn polled_every(&self, poll: Duration) -> Duration {
        let Some(rate) = self.codehost.as_ref().and_then(|c| c.rate_limit()).filter(|r| r.resets_at > unix_now())
        else {
            return poll;
        };
        match rate.remaining.saturating_mul(100) {
            r if r < rate.limit.saturating_mul(30) => poll * 4,
            r if r < rate.limit.saturating_mul(50) => poll * 2,
            _ => poll,
        }
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

/// The sandboxes held for tasks that aren't running, by the key of the task each is held for.
struct Others(Vec<Option<TaskKey>>);

impl Others {
    fn len(&self) -> usize {
        self.0.len()
    }

    fn contains(&self, key: &TaskKey) -> bool {
        self.0.iter().any(|t| t.as_ref() == Some(key))
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

/// Whether the sandboxes held beyond running work fill the repo's own cap. With nothing running they are parked
/// tasks'; a full host cap alone isn't their doing, since the other repos' runs free their places unaided.
fn parked_fill_repo_cap(others: usize, max_sandboxes: usize) -> bool {
    others > 0 && others >= max_sandboxes
}

/// Puts the ready tasks in the order runs start: by priority, and within a priority a rework of a task whose PR is
/// open (changes requested for a conflict, CI or a review) before fresh tasks, since it is nearly done and goes stale
/// while it waits (TRAIL-868). The sort is stable, so the tracker's order (due date, age) holds among equals.
fn in_start_order(mut ready: Vec<Task>) -> Vec<Task> {
    let rework = |t: &Task| t.changes_requested || (t.run.pr.is_some() && t.run.pending_changes().is_some());
    ready.sort_by_key(|t| (t.priority, !rework(t)));
    ready
}

#[cfg(test)]
mod starvation_tests {
    use super::parked_fill_repo_cap;

    #[test]
    fn only_a_full_repo_cap_counts_as_starved_by_parked_tasks() {
        assert!(parked_fill_repo_cap(2, 2));
        assert!(!parked_fill_repo_cap(1, 2), "the host's cap is what's full, and other repos' runs will free it");
        assert!(!parked_fill_repo_cap(0, 2));
    }
}

/// How a task holding a sandbox reads in the starvation log: parked, backing off from an infra failure, or running.
fn describe_holder(key: &str, parked: bool, retry_in: Option<Duration>) -> String {
    match (parked, retry_in) {
        (true, _) => format!("{key} (parked)"),
        (false, Some(wait)) => format!("{key} (retrying in {}s)", wait.as_secs()),
        (false, None) => format!("{key} (in flight)"),
    }
}

#[cfg(test)]
mod holder_tests {
    use super::*;

    #[test]
    fn a_holder_in_setback_is_not_called_parked() {
        assert_eq!(describe_holder("TRAIL-1087", false, Some(Duration::from_secs(42))), "TRAIL-1087 (retrying in 42s)");
        assert_eq!(describe_holder("TRAIL-1", false, None), "TRAIL-1 (in flight)");
        assert_eq!(describe_holder("TRAIL-2", true, None), "TRAIL-2 (parked)");
    }
}
