//! A tracker that stops answering on cue, as a server that restarts mid-pass does.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use trailhead_factory::{
    Credential, Error, FollowUpTask, Grade, HumanContext, Priority, Result, RunPatch, Task, TaskKey, Tracker, Verdict,
};

/// An armed failure, shared by every handle on the tracker: the control plane's own, its sub-agents' and the
/// daemon's. `None` when nothing is armed.
pub type Outage = Arc<Mutex<Option<Armed>>>;

#[derive(Debug, Clone, Copy)]
pub struct Armed {
    /// Calls left before one fails.
    pub left: u32,
    /// Whether only writes count, and so only a write fails. A read that fails mostly ends the pass early; a write
    /// that fails leaves whatever the writes before it did.
    pub writes_only: bool,
}

/// Wraps a tracker so that the call the outage counts down to fails before it reaches the store. Which call that is
/// depends on what the factory is doing, so over many histories every write gets its turn to be the one cut short.
pub struct Flaky {
    inner: Arc<dyn Tracker>,
    outage: Outage,
}

impl Flaky {
    pub fn new(inner: Arc<dyn Tracker>, outage: Outage) -> Flaky {
        Flaky { inner, outage }
    }

    fn read(&self) -> Result<()> {
        self.call(false)
    }

    fn write(&self) -> Result<()> {
        self.call(true)
    }

    fn call(&self, write: bool) -> Result<()> {
        let mut outage = self.outage.lock().unwrap();
        match *outage {
            Some(armed) if armed.writes_only && !write => Ok(()),
            Some(Armed { left: 0 | 1, .. }) => {
                *outage = None;
                Err(Error::Other("the tracker didn't answer".into()))
            }
            Some(armed) => {
                *outage = Some(Armed { left: armed.left - 1, ..armed });
                Ok(())
            }
            None => Ok(()),
        }
    }
}

#[async_trait]
impl Tracker for Flaky {
    fn agent(&self) -> &str {
        self.inner.agent()
    }

    fn sub_agent(&self, name: &str) -> Arc<dyn Tracker> {
        Arc::new(Flaky { inner: self.inner.sub_agent(name), outage: self.outage.clone() })
    }

    async fn ready(&self, project: &str, queue: &[TaskKey], limit: usize) -> Result<Vec<Task>> {
        self.read()?;
        self.inner.ready(project, queue, limit).await
    }

    async fn show(&self, task: &TaskKey) -> Result<Task> {
        self.read()?;
        self.inner.show(task).await
    }

    async fn human_context(&self, task: &TaskKey) -> Result<HumanContext> {
        self.read()?;
        self.inner.human_context(task).await
    }

    async fn last_review(&self, task: &TaskKey) -> Result<Option<Verdict>> {
        self.read()?;
        self.inner.last_review(task).await
    }

    async fn last_review_by_others(&self, task: &TaskKey) -> Result<Option<Verdict>> {
        self.read()?;
        self.inner.last_review_by_others(task).await
    }

    async fn in_flight(&self, project: &str, queue: &[TaskKey]) -> Result<Vec<Task>> {
        self.read()?;
        self.inner.in_flight(project, queue).await
    }

    async fn strays(&self, project: &str, queues: &[TaskKey]) -> Result<Vec<Task>> {
        self.read()?;
        self.inner.strays(project, queues).await
    }

    async fn claim(&self, task: &TaskKey) -> Result<()> {
        self.write()?;
        self.inner.claim(task).await
    }

    async fn renew(&self, task: &TaskKey) -> Result<()> {
        self.write()?;
        self.inner.renew(task).await
    }

    async fn release(&self, task: &TaskKey) -> Result<()> {
        self.write()?;
        self.inner.release(task).await
    }

    async fn report(&self, task: &TaskKey, update: RunPatch) -> Result<()> {
        self.write()?;
        self.inner.report(task, update).await
    }

    async fn request_review(&self, task: &TaskKey, handoff: &str) -> Result<()> {
        self.write()?;
        self.inner.request_review(task, handoff).await
    }

    async fn handoff(&self, task: &TaskKey, body: &str) -> Result<()> {
        self.write()?;
        self.inner.handoff(task, body).await
    }

    async fn approve(&self, task: &TaskKey) -> Result<()> {
        self.write()?;
        self.inner.approve(task).await
    }

    async fn unapprove(&self, task: &TaskKey) -> Result<()> {
        self.write()?;
        self.inner.unapprove(task).await
    }

    async fn reopen(&self, task: &TaskKey) -> Result<()> {
        self.write()?;
        self.inner.reopen(task).await
    }

    async fn raise_grade(&self, task: &TaskKey, grade: Grade) -> Result<()> {
        self.write()?;
        self.inner.raise_grade(task, grade).await
    }

    async fn record_verdict(&self, task: &TaskKey, verdict: &Verdict) -> Result<()> {
        self.write()?;
        self.inner.record_verdict(task, verdict).await
    }

    async fn ask(&self, task: &TaskKey, question: &str) -> Result<()> {
        self.write()?;
        self.inner.ask(task, question).await
    }

    async fn note(&self, task: &TaskKey, body: &str) -> Result<()> {
        self.write()?;
        self.inner.note(task, body).await
    }

    async fn finish(&self, task: &TaskKey, handoff: &str) -> Result<()> {
        self.write()?;
        self.inner.finish(task, handoff).await
    }

    async fn create_task(
        &self,
        project: &str,
        title: &str,
        description: &str,
        priority: Option<Priority>,
    ) -> Result<TaskKey> {
        self.write()?;
        self.inner.create_task(project, title, description, priority).await
    }

    async fn follow_ups(&self, task: &TaskKey) -> Result<Vec<FollowUpTask>> {
        self.read()?;
        self.inner.follow_ups(task).await
    }

    async fn find_open_by_title(&self, project: &str, title: &str) -> Result<Option<TaskKey>> {
        self.read()?;
        self.inner.find_open_by_title(project, title).await
    }

    async fn create_sub_agent(&self, name: &str, project: &str, ttl: Duration) -> Result<Credential> {
        self.write()?;
        self.inner.create_sub_agent(name, project, ttl).await
    }

    async fn revoke_sub_agent(&self, name: &str) -> Result<()> {
        self.write()?;
        self.inner.revoke_sub_agent(name).await
    }
}
