//! Wakes the run loop when the factory's projects change in Trailhead, so a task added under the queue, or in a project
//! pointed at the repo, starts in seconds instead of at the next poll. A project that starts or stops pointing at the
//! repo wakes it too, and is watched from then on.
//!
//! The store's in-process commit signal says when to look, and the events table, read with a cursor as the server's
//! own event stream does, says what changed: the signal is only a hint that something committed.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use trailhead_core::{Ctx, Entity, Event, EventFilter, ProjectRepo, Store, TaskKey};

/// How long to wait before reading the event log again after it failed.
const RETRY: Duration = Duration::from_secs(1);

/// Event kinds that can make a task ready: one created, one updated (status or parent) or a claim let go; and any
/// change to a project, which may point it at the repo or away.
const KINDS: [&str; 4] = ["task.created", "task.updated", "claim.released", "project."];
const PAGE: i64 = 100;

/// The projects a repo's loop works, as far as waking it goes: its file's own and those pointed at it.
struct Watched {
    project: String,
    slug: String,
    pointed: HashSet<String>,
}

impl Watched {
    /// Whether `event` concerns the repo's work, keeping track of projects that start or stop pointing at it.
    fn concerns(&mut self, event: &Event) -> bool {
        if event.entity == Entity::Project {
            let repo = |v: &Option<serde_json::Value>| {
                let v = v.as_ref()?;
                Some((v.get("key")?.as_str()?.to_string(), ProjectRepo::of(v.get("metadata")?)))
            };
            let (before, after) = (repo(&event.before), repo(&event.after));
            let here = |r: &Option<(String, Option<ProjectRepo>)>| {
                r.as_ref().is_some_and(|(_, to)| to.as_ref().is_some_and(|to| to.is(&self.slug)))
            };
            let (was, is) = (here(&before), here(&after));
            if let Some((key, _)) = after.as_ref().or(before.as_ref()) {
                if is {
                    self.pointed.insert(key.clone());
                } else {
                    self.pointed.remove(key);
                }
                return was || is || *key == self.project;
            }
            return false;
        }
        let Some(key) = &event.task_key else { return false };
        let project = TaskKey(key.clone());
        let project = project.project();
        project == self.project || self.pointed.contains(project)
    }
}

/// Notifies `wake` whenever a relevant event lands in `project`, or in a project pointed at the repo `slug`
/// (`owner/name`), after this starts, looking after each commit ([`Store::changes`]). Runs until dropped or aborted, or
/// the store is gone.
pub async fn wake_on_changes(store: Arc<Store>, ctx: Ctx, project: String, slug: String, wake: Arc<Notify>) {
    let mut changes = store.changes();
    let (mut after, pointed) = loop {
        let start = async {
            let id = store.latest_event_id(&ctx).await?;
            let projects = store.list_projects(&ctx, false).await?;
            let pointed = projects.into_iter().filter(|p| p.repo().is_some_and(|to| to.is(&slug))).map(|p| p.key);
            Ok::<_, trailhead_core::Error>((id, pointed.collect()))
        };
        match start.await {
            Ok(found) => break found,
            Err(e) => {
                tracing::warn!(error = %e, "can't read the event log or the projects; retrying");
                tokio::time::sleep(RETRY).await;
            }
        }
    };
    let mut watched = Watched { project, slug, pointed };
    while changes.changed().await.is_ok() {
        loop {
            let filter = EventFilter {
                after_id: Some(after),
                kinds: KINDS.map(String::from).to_vec(),
                limit: Some(PAGE),
                ..Default::default()
            };
            match store.list_events(&ctx, filter).await {
                Ok(page) => {
                    let Some(last) = page.last() else { break };
                    after = last.id;
                    // Every event is read, so a project that starts pointing is tracked whether or not it wakes.
                    let mut relevant = false;
                    for event in &page {
                        relevant |= watched.concerns(event);
                    }
                    if relevant {
                        wake.notify_one();
                    }
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
    use trailhead_core::{NewProject, NewTask, ProjectPatch};

    #[tokio::test]
    async fn a_commit_in_the_project_wakes_the_run_loop() {
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
            tokio::spawn(wake_on_changes(store.clone(), ctx.clone(), "TRAIL".into(), "o/r".into(), wake.clone()));
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

    #[tokio::test]
    async fn a_project_pointed_at_the_repo_wakes_the_run_loop_from_the_moment_it_points() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open_local(dir.path().join("t.db")).await.unwrap());
        let (ws, me) = store.bootstrap_local("lane").await.unwrap();
        let ctx = Ctx::user(ws.id, me.id);
        for key in ["TRAIL", "TICK"] {
            let new = NewProject { key: key.into(), name: key.into(), ..Default::default() };
            store.create_project(&ctx, new).await.unwrap();
        }
        let wake = Arc::new(Notify::new());
        let watcher =
            tokio::spawn(wake_on_changes(store.clone(), ctx.clone(), "TRAIL".into(), "o/apps".into(), wake.clone()));
        tokio::time::sleep(Duration::from_millis(50)).await;
        let quiet = |wake: Arc<Notify>| async move {
            tokio::time::timeout(Duration::from_millis(200), wake.notified()).await.is_err()
        };

        let tick = || NewTask { project: "TICK".into(), title: "tick".into(), ..Default::default() };
        store.create_task(&ctx, tick()).await.unwrap();
        assert!(quiet(wake.clone()).await, "a project not pointed at the repo woke the loop");

        let point = |to: serde_json::Value| ProjectPatch { metadata: Some(to), ..Default::default() };
        let elsewhere = serde_json::json!({"factory": {"repo": "o/other"}});
        store.update_project(&ctx, "TICK", point(elsewhere), None).await.unwrap();
        assert!(quiet(wake.clone()).await, "pointing at another repo woke the loop");

        let here = serde_json::json!({"factory": {"repo": "o/apps", "path": "apps/tick"}});
        store.update_project(&ctx, "TICK", point(here), None).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), wake.notified()).await.expect("pointing woke the loop");
        store.create_task(&ctx, tick()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), wake.notified()).await.expect("the pointed task woke the loop");

        store.update_project(&ctx, "TICK", point(serde_json::json!({"factory": null})), None).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), wake.notified()).await.expect("un-pointing woke the loop");
        store.create_task(&ctx, tick()).await.unwrap();
        assert!(quiet(wake.clone()).await, "a project no longer pointed woke the loop");
        watcher.abort();
    }
}
