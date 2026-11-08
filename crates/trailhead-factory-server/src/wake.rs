//! Wakes the run loop when the factory's project changes in Trailhead, so a task added under the queue starts in
//! seconds instead of at the next poll.
//!
//! Inside the server, the store's in-process commit signal says when to look; a standalone control plane shares the
//! database but not the process, so that signal never fires for the server's writes and it checks on a timer
//! instead. Either way the events table, read with a cursor as the server's own event stream does, says what
//! changed: the commit signal is only a hint that something committed.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use trailhead_core::{Ctx, EventFilter, Store};

/// How often the events table is checked.
pub const INTERVAL: Duration = Duration::from_secs(1);

/// Event kinds that can make a task ready: one created, one updated (status or parent) or a claim let go.
const KINDS: [&str; 3] = ["task.created", "task.updated", "claim.released"];
const PAGE: i64 = 100;

/// When to look at the events table.
pub enum Trigger {
    /// On a timer, for a control plane outside the server's process.
    Every(Duration),
    /// After each commit in this process: the store's [`Store::changes`].
    Commits(tokio::sync::watch::Receiver<u64>),
}

impl Trigger {
    /// Waits for the next time to look. A closed commit signal (the store is gone) never fires again.
    async fn next(&mut self) {
        match self {
            Trigger::Every(interval) => tokio::time::sleep(*interval).await,
            Trigger::Commits(changes) => {
                if changes.changed().await.is_err() {
                    std::future::pending::<()>().await;
                }
            }
        }
    }
}

/// Notifies `wake` whenever a relevant event lands in `project` after this starts, looking when `trigger` says.
/// Runs until dropped or aborted.
pub async fn wake_on_changes(store: Arc<Store>, ctx: Ctx, project: String, wake: Arc<Notify>, mut trigger: Trigger) {
    let mut after = loop {
        match store.latest_event_id(&ctx).await {
            Ok(id) => break id,
            Err(e) => {
                tracing::warn!(error = %e, "can't read the event log; retrying");
                tokio::time::sleep(INTERVAL).await;
            }
        }
    };
    loop {
        trigger.next().await;
        loop {
            let filter = EventFilter {
                after_id: Some(after),
                project: Some(project.clone()),
                kinds: KINDS.map(String::from).to_vec(),
                limit: Some(PAGE),
                ..Default::default()
            };
            match store.list_events(&ctx, filter).await {
                Ok(page) => {
                    let Some(last) = page.last() else { break };
                    after = last.id;
                    wake.notify_one();
                    if (page.len() as i64) < PAGE {
                        break;
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "can't read the event log");
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trailhead_core::{NewProject, NewTask};

    #[tokio::test]
    async fn a_new_task_in_the_project_wakes_the_run_loop() {
        let every = |_: &Store| Trigger::Every(Duration::from_millis(10));
        wakes_only_for_its_project(every).await;
    }

    #[tokio::test]
    async fn in_the_server_a_commit_wakes_it_with_no_timer() {
        wakes_only_for_its_project(|store: &Store| Trigger::Commits(store.changes())).await;
    }

    async fn wakes_only_for_its_project(trigger: impl Fn(&Store) -> Trigger) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open_local(dir.path().join("t.db")).await.unwrap());
        let (ws, me) = store.bootstrap_local("lane").await.unwrap();
        let ctx = Ctx::user(ws.id, me.id);
        for key in ["TRAIL", "OTHER"] {
            let new = NewProject { key: key.into(), name: key.into(), ..Default::default() };
            store.create_project(&ctx, new).await.unwrap();
        }
        let wake = Arc::new(Notify::new());
        let watcher =
            tokio::spawn(wake_on_changes(store.clone(), ctx.clone(), "TRAIL".into(), wake.clone(), trigger(&store)));
        tokio::time::sleep(Duration::from_millis(50)).await;

        let other = NewTask { project: "OTHER".into(), title: "elsewhere".into(), ..Default::default() };
        store.create_task(&ctx, other).await.unwrap();
        let quiet = tokio::time::timeout(Duration::from_millis(200), wake.notified()).await;
        assert!(quiet.is_err(), "another project's task woke the loop");

        let new = NewTask { project: "TRAIL".into(), title: "mine".into(), ..Default::default() };
        store.create_task(&ctx, new).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), wake.notified()).await.expect("the new task woke the loop");
        watcher.abort();
    }
}
