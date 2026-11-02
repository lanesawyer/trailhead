#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use jiff::{SignedDuration, Timestamp, Zoned};
use tempfile::TempDir;
use trailhead_core::{Ctx, GradeFilter, NewProject, NewTask, Store, Task};

pub struct Env {
    pub store: Arc<Store>,
    pub human: Ctx,
    pub clock: Arc<Mutex<Timestamp>>,
    _dir: TempDir,
}

impl Env {
    pub async fn new() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(Mutex::new(Timestamp::now()));
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

    /// A second connection to the database file, for writing rows the API refuses to.
    pub async fn raw_conn(&self) -> libsql::Connection {
        let db = libsql::Builder::new_local(self._dir.path().join("t.db")).build().await.unwrap();
        db.connect().unwrap()
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
        *self.clock.lock().unwrap() += SignedDuration::from_secs(secs);
    }

    pub fn now(&self) -> Timestamp {
        *self.clock.lock().unwrap()
    }

    /// A task time `d` from now, as given in UTC.
    pub fn at_in(&self, d: SignedDuration) -> Zoned {
        (self.now() + d).to_zoned(jiff::tz::TimeZone::UTC)
    }

    pub async fn ready_keys(&self) -> Vec<String> {
        self.store.ready(&self.human, None, None, GradeFilter::Any).await.unwrap().into_iter().map(|t| t.key).collect()
    }
}
