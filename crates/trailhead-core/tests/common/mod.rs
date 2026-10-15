#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use tempfile::TempDir;
use time::{Duration, OffsetDateTime};
use trailhead_core::{Ctx, NewProject, NewTask, Store, Task};

pub struct Env {
    pub store: Arc<Store>,
    pub human: Ctx,
    pub clock: Arc<Mutex<OffsetDateTime>>,
    _dir: TempDir,
}

impl Env {
    pub async fn new() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(Mutex::new(OffsetDateTime::now_utc()));
        let c = clock.clone();
        let store =
            Store::open_local(dir.path().join("t.db")).await.unwrap().with_clock(Arc::new(move || *c.lock().unwrap()));
        let (ws, user) = store.bootstrap_local("Lane").await.unwrap();
        let human = Ctx::user(ws.id, user.id);
        store
            .create_project(&human, NewProject { key: "AUTH".into(), name: "Auth".into(), ..Default::default() })
            .await
            .unwrap();
        Env { store: Arc::new(store), human, clock, _dir: dir }
    }

    pub async fn agent(&self, name: &str) -> Ctx {
        let a = self.store.create_agent(&self.human, name).await.unwrap();
        self.store.agent_ctx(self.human.workspace_id, a.id).await.unwrap()
    }

    pub async fn task(&self, title: &str) -> Task {
        self.task_with(NewTask { title: title.into(), ..Default::default() }).await
    }

    pub async fn task_with(&self, new: NewTask) -> Task {
        let new = NewTask { project: if new.project.is_empty() { "AUTH".into() } else { new.project }, ..new };
        self.store.create_task(&self.human.next_batch(), new).await.unwrap()
    }

    pub fn advance(&self, secs: i64) {
        *self.clock.lock().unwrap() += Duration::seconds(secs);
    }

    pub fn now(&self) -> OffsetDateTime {
        *self.clock.lock().unwrap()
    }

    pub async fn ready_keys(&self) -> Vec<String> {
        self.store.ready(&self.human, None, None).await.unwrap().into_iter().map(|t| t.key).collect()
    }
}
