use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::Notify;
use trailhead_factory::{EventSink, Feedback, Harness, Result, Role, RunContext, RunEvent, RunOutcome, Usage};

/// What a scripted agent does in one run: a model call per step, then an outcome.
#[derive(Debug, Clone)]
pub struct Script {
    pub steps: Vec<Usage>,
    pub outcome: RunOutcome,
    /// Never finishes after its steps, like an agent stuck in a loop.
    pub hang: bool,
    /// Waits for this after its steps before finishing.
    pub gate: Option<Arc<Notify>>,
    /// Usage window readings reported before the first step, as Claude Code does at session start.
    pub quota: Vec<(String, u32, i64)>,
}

impl Script {
    pub fn new(outcome: RunOutcome) -> Script {
        Script { steps: Vec::new(), outcome, hang: false, gate: None, quota: Vec::new() }
    }

    pub fn hang(mut self) -> Script {
        self.hang = true;
        self
    }

    /// Reports `window` as `used_percent` full, resetting at `resets_at` (unix seconds).
    pub fn quota(mut self, window: &str, used_percent: u32, resets_at: i64) -> Script {
        self.quota.push((window.to_string(), used_percent, resets_at));
        self
    }

    pub fn wait_for(mut self, gate: Arc<Notify>) -> Script {
        self.gate = Some(gate);
        self
    }

    /// Adds `n` steps of `tokens` output tokens each.
    pub fn steps(mut self, n: u32, tokens: u64) -> Script {
        self.steps.extend((0..n).map(|_| Usage { output_tokens: tokens, ..Usage::default() }));
        self
    }
}

/// Plays a script per role; each run takes the next script queued for its role.
#[derive(Default)]
pub struct ScriptedHarness {
    scripts: Mutex<HashMap<Role, Vec<Script>>>,
    contexts: Mutex<Vec<RunContext>>,
}

impl ScriptedHarness {
    pub fn new() -> ScriptedHarness {
        ScriptedHarness::default()
    }

    pub fn push(&self, role: Role, script: Script) -> &Self {
        self.scripts.lock().unwrap().entry(role).or_default().push(script);
        self
    }

    /// Every context a run was started with.
    pub fn contexts(&self) -> Vec<RunContext> {
        self.contexts.lock().unwrap().clone()
    }
}

#[async_trait]
impl Harness for ScriptedHarness {
    async fn run(&self, ctx: RunContext, events: EventSink) -> Result<RunOutcome> {
        self.contexts.lock().unwrap().push(ctx.clone());
        let script = {
            let mut scripts = self.scripts.lock().unwrap();
            let queue = scripts.get_mut(&ctx.role).filter(|q| !q.is_empty());
            match queue {
                Some(q) => q.remove(0),
                None => return Ok(RunOutcome::Failed { reason: format!("no script for {}", ctx.role.as_str()) }),
            }
        };
        for (window, used_percent, resets_at) in script.quota {
            let _ = events.send(RunEvent::Quota { window, used_percent, resets_at });
        }
        for (i, usage) in script.steps.into_iter().enumerate() {
            let _ = events.send(RunEvent::StepStarted { step: i as u32 + 1 });
            let _ = events.send(RunEvent::Usage { model: ctx.model.clone(), usage });
            // Lets a canceller act between steps, as a real model call would.
            tokio::task::yield_now().await;
        }
        if let Some(gate) = &script.gate {
            gate.notified().await;
        }
        if script.hang {
            std::future::pending::<()>().await;
        }
        Ok(script.outcome)
    }

    async fn resume(&self, ctx: RunContext, _feedback: Feedback, events: EventSink) -> Result<RunOutcome> {
        self.run(ctx, events).await
    }
}
