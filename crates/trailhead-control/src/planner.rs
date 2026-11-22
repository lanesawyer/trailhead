//! The planner's run (`docs/app-platform.md`, "Planning an ask"): a spec task is talked through with a person by
//! turns, each a run that ends in a question, and the plan a turn proposes is written into the engine by the control
//! plane, not the agent.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use trailhead_factory::messages;
use trailhead_factory::{
    Error, ModelAlias, Patch, PlanChanges, PlanTaskUpdate, PlanTurn, Planning, Profile, ProposedTask, Result, Role,
    RunPatch, RunPhase, RunRequest, Sandbox, Task, TaskKey, Tracker, plan,
};

use crate::lifecycle::{Begin, spec, worker_name};
use crate::place::Placement;
use crate::timing::unix_now;
use crate::{Factory, Outcome, Placed};

impl Factory {
    /// One turn of a planner on a spec task, on `target`. It is claimed and given a sandbox like a worker's, the one
    /// it held for its last turn when it still holds it, so the agent's session carries on; the run publishes nothing
    /// and is concluded like a worker's, except that a turn it ends is [`Factory::conclude_plan_turn`]'s.
    pub(crate) async fn run_planner_on(&self, task: &Task, target: &Placed, place: Placement) -> Result<Outcome> {
        let Some((profile_name, profile)) = self.config.planner().map(|(n, p)| (n.to_string(), p.clone())) else {
            return self.park_without_planner(task).await;
        };
        let attempt = task.run.attempt.unwrap_or(0) + 1;
        let name = worker_name(&task.key, attempt, task.run.retries.unwrap_or(0));
        let credential = self.new_sub_agent(task, &name, &profile).await?;
        let sub = self.tracker.sub_agent(&name);
        let infra_prior = task.run.infra_failures;
        if let Err(e) = sub.claim(&task.key).await {
            return Err(self.infra_failed(task, &sub, &name, e, infra_prior, Patch::Keep).await);
        }
        let started = RunPatch {
            attempt: Patch::Set(attempt),
            sandbox: Patch::Set(name.clone()),
            profile: Patch::Set(profile_name),
            started: Patch::Set(unix_now()),
            released: if task.run.released.is_some() { Patch::Clear } else { Patch::Keep },
            ..RunPatch::default()
        };
        if let Err(e) = sub.report(&task.key, started).await {
            return Err(self.infra_failed(task, &sub, &name, e, infra_prior, Patch::Keep).await);
        }
        let _run = self.status.begin(&task.key, RunPhase::Preparing { cold: false });
        tracing::info!(task = %task.key, run = %name, "preparing a planner sandbox");
        let prepared = async {
            let context = self.tracker.human_context(&task.key).await?;
            let plan = self.tracker.plan_tasks(&task.key).await?;
            let sandbox = self.acquire_planner(target, task).await?;
            let at = RunPatch {
                host: Patch::Set(sandbox.name.clone()),
                target: Patch::Set(target.target.name.clone()),
                ..RunPatch::default()
            };
            sub.report(&task.key, at).await?;
            Ok::<_, Error>((sandbox, context, plan))
        };
        let (sandbox, context, plan) = match prepared.await {
            Ok(prepared) => prepared,
            Err(e) => return Err(self.infra_failed(task, &sub, &name, e, infra_prior, Patch::Keep).await),
        };
        tracing::info!(task = %task.key, run = %name, sandbox = %sandbox.name, "starting planner");
        self.status.phase(&task.key, RunPhase::Agent, Some(&sandbox.name));
        let ask = task.plan.spec.as_ref().and_then(|s| s.ask.clone()).unwrap_or_else(|| task.description.clone());
        let req = RunRequest {
            run: name.clone(),
            role: Role::Planner,
            task: spec(task, context, self.path_of(&task.key)),
            repo: self.config.repo.repo(),
            base_branch: self.config.repo.base_branch.clone(),
            branch: Self::branch(task),
            grade: None,
            pr: None,
            feedback: None,
            model: target.provider.resolve(planner_model(task, &profile)),
            endpoint: target.provider.endpoint(),
            max_steps: profile.budget.steps,
            required_checks: Vec::new(),
            worker_checks: Vec::new(),
            blocked_hosts: self.blocked_hosts(&sandbox),
            allowed_hosts: self.allowed_hosts(),
            env: self.config.stack.env.clone(),
            credential,
            shots: None,
            plan: Some(Planning { ask, plan }),
        };
        let prior = task.run.spend.unwrap_or_default();
        let run =
            self.run_metered(&sandbox, Begin::Start(Box::new(req)), &task.key, &profile, prior, Duration::ZERO).await;
        self.conclude(task, &sub, &sandbox, &name, attempt, run, Some(place)).await
    }

    /// A spec task in a repo with no planner profile: parked, as the factory's own identity, with a question saying
    /// to add one. Never planned on the worker's profile instead.
    async fn park_without_planner(&self, task: &Task) -> Result<Outcome> {
        tracing::warn!(task = %task.key, "a spec task, but the repo has no planner profile; parking it");
        self.tracker.claim(&task.key).await?;
        let question = messages::no_planner_profile_question();
        if let Err(e) = self.tracker.ask(&task.key, &question).await {
            if let Err(re) = self.tracker.release(&task.key).await {
                tracing::warn!(task = %task.key, error = %re, "couldn't release a spec task it failed to park");
            }
            return Err(e);
        }
        Ok(Outcome::Parked { reason: question })
    }

    /// What a planner's turn comes to, once its spend is recorded: the spec, the plan and whether it is ready are
    /// written, then the person is asked the turn's message, which parks the task with its sandbox kept for the
    /// next turn, as a worker that asked keeps its own. What couldn't be written is said after the message.
    pub(crate) async fn conclude_plan_turn(
        &self,
        task: &Task,
        sub: &Arc<dyn Tracker>,
        sandbox: &Sandbox,
        name: &str,
        attempt: u32,
        turn: PlanTurn,
    ) -> Result<Outcome> {
        let mut problems = Vec::new();
        if let Some(text) = turn.spec.as_deref().filter(|s| !s.trim().is_empty()) {
            problems.extend(self.write_spec(task, sub, text).await);
        }
        if let Some(tasks) = &turn.tasks {
            problems.extend(self.apply_plan(&task.key, sub, tasks).await);
        }
        if let Err(e) = sub.patch_plan(&task.key, json!({ "spec": { "ready": turn.ready } })).await {
            tracing::warn!(task = %task.key, error = %e, "couldn't record whether the plan is ready");
            problems.push(format!("whether the plan is ready to approve wasn't recorded: {e}"));
        }
        // Each turn ends in a question, and a failure counts from the last turn, not from the conversation's start.
        let mark = RunPatch { asked: Patch::Set(attempt), attempt_base: Patch::Set(attempt), ..RunPatch::default() };
        if let Err(e) = sub.report(&task.key, mark).await {
            tracing::warn!(task = %task.key, error = %e, "couldn't record the turn that asked");
        }
        let question = match problems.is_empty() {
            true => turn.message.clone(),
            false => format!("{}{}", turn.message.trim_end(), messages::plan_not_saved(&problems)),
        };
        if let Err(e) = sub.ask(&task.key, &question).await {
            return Err(self.infra_failed(task, sub, name, e, task.run.infra_failures, Patch::Keep).await);
        }
        self.status.paused(&sandbox.name);
        Ok(Outcome::Parked { reason: turn.message })
    }

    /// Replaces the spec task's description with the spec, keeping the person's ask in `metadata.plan.spec.ask`
    /// first, so the description is never overwritten while the ask exists nowhere else. What failed, if anything.
    async fn write_spec(&self, task: &Task, sub: &Arc<dyn Tracker>, text: &str) -> Vec<String> {
        if task.plan.spec.as_ref().is_none_or(|s| s.ask.is_none())
            && let Err(e) = sub.patch_plan(&task.key, json!({ "spec": { "ask": task.description } })).await
        {
            tracing::warn!(task = %task.key, error = %e, "couldn't keep the ask; leaving the description as it is");
            return vec![format!("the spec wasn't written, since the ask couldn't be kept first: {e}")];
        }
        match sub.set_description(&task.key, text).await {
            Ok(()) => Vec::new(),
            Err(e) => {
                tracing::warn!(task = %task.key, error = %e, "couldn't write the spec");
                vec![format!("the spec wasn't written: {e}")]
            }
        }
    }

    /// Makes the engine's plan under `spec` match the turn's `tasks` ([`plan::changes`]), one write per task where
    /// the tracker allows. Each write stands on its own, so one that fails leaves the rest done and the next turn,
    /// which is shown the plan as it then stands, can repair it. What wasn't done, and why.
    async fn apply_plan(&self, spec: &TaskKey, sub: &Arc<dyn Tracker>, tasks: &[ProposedTask]) -> Vec<String> {
        let current = match sub.plan_tasks(spec).await {
            Ok(current) => current,
            Err(e) => {
                tracing::warn!(task = %spec, error = %e, "couldn't read the plan; leaving it as it is");
                return vec![format!("the plan couldn't be read, so it wasn't changed: {e}")];
            }
        };
        let PlanChanges { create, revise, external, cancel, mut warnings } = plan::changes(&current, tasks);
        let mut made: HashMap<String, TaskKey> = HashMap::new();
        for c in create {
            let mut new = c.task;
            for handle in &c.after {
                match made.get(handle) {
                    Some(key) => new.blocked_by.push(key.clone()),
                    None => warnings.push(format!("`{}` doesn't wait on `{handle}`, which wasn't created", c.handle)),
                }
            }
            match sub.create_plan_task(spec, &new).await {
                Ok(key) => {
                    made.insert(c.handle, key);
                }
                Err(e) => warnings.push(format!("`{}` ({}) wasn't created: {e}", c.handle, new.title)),
            }
        }
        for (handle, blockers) in external {
            let Some(key) = made.get(&handle) else { continue };
            for blocker in blockers {
                let update = PlanTaskUpdate { add_blockers: vec![blocker.clone()], ..PlanTaskUpdate::default() };
                if let Err(e) = sub.update_plan_task(key, &update).await {
                    warnings.push(format!("{key} doesn't wait on {blocker}: {e}"));
                }
            }
        }
        for r in revise {
            let mut update = r.update;
            for handle in &r.add_new {
                match made.get(handle) {
                    Some(key) => update.add_blockers.push(key.clone()),
                    None => warnings.push(format!("{} doesn't wait on `{handle}`, which wasn't created", r.key)),
                }
            }
            if let Err(e) = sub.update_plan_task(&r.key, &update).await {
                warnings.push(format!("{} wasn't changed: {e}", r.key));
            }
        }
        for key in cancel {
            if let Err(e) = sub.cancel(&key).await {
                warnings.push(format!("{key} wasn't cancelled: {e}"));
            }
        }
        for w in &warnings {
            tracing::warn!(task = %spec, "{w}");
        }
        warnings
    }
}

impl Factory {
    /// Gives up the pooled sandbox of a spec task that has waited on its person for `release_planner_after`
    /// minutes, whether or not other work waits. The release is recorded first, so the next turn starts cold: a new
    /// sandbox, a new session and the whole conversation in its prompt. A breach's sandbox is kept for a person, as
    /// a worker's is. True when it was given up.
    pub(crate) async fn release_idle_planner(&self, task: &Task, sandbox: &Sandbox) -> bool {
        let after = self.config.repo.release_planner_after;
        if after == 0 || !task.plan.awaits_planner() || task.run.worker_breach.is_some() {
            return false;
        }
        let waited = self.pool.parked_since.lock().unwrap().get(&task.key).map(|since| since.elapsed());
        if waited.is_none_or(|w| w < Duration::from_secs(u64::from(after) * 60)) {
            return false;
        }
        // The asker's claim outlives the question until its lease lapses, and only the holder can write meanwhile.
        let writer = match &task.run.sandbox {
            Some(run) if task.holder.as_deref() == Some(self.tracker.sub_agent(run).agent()) => {
                self.tracker.sub_agent(run)
            }
            _ => self.tracker.clone(),
        };
        let record = RunPatch { released: Patch::Set(unix_now()), ..RunPatch::default() };
        if let Err(e) = writer.report(&task.key, record).await {
            tracing::debug!(task = %task.key, error = %e, "couldn't record that an idle planner's sandbox is released; keeping it for now");
            return false;
        }
        self.pool.parked_since.lock().unwrap().remove(&task.key);
        tracing::info!(task = %task.key, sandbox = %sandbox.name, waited = ?waited, "released an idle planner's sandbox");
        if let Err(e) = self.release_worker(sandbox).await {
            tracing::warn!(task = %task.key, sandbox = %sandbox.name, error = %e, "couldn't release an idle planner's sandbox; reconcile will");
        }
        true
    }
}

/// The model a spec task's planner runs on: the one the person chose when asking, when the config can name it, else
/// the planner profile's.
fn planner_model(task: &Task, profile: &Profile) -> ModelAlias {
    let Some(chosen) = task.plan.spec.as_ref().and_then(|s| s.model.as_deref()) else { return profile.model };
    match serde_json::from_value::<ModelAlias>(json!(chosen)) {
        Ok(model) => model,
        Err(_) => {
            tracing::warn!(task = %task.key, model = chosen, "the plan asks for a model the factory doesn't know; using the planner profile's");
            profile.model
        }
    }
}
