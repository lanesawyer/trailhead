use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use libsql::{Connection, Database, TransactionBehavior};
use serde::Serialize;
use serde_json::Value;
use time::OffsetDateTime;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::db::{self, fmt_ts, opt_uuid_value};
use crate::error::Result;
use crate::model::Entity;

pub type Clock = Arc<dyn Fn() -> OffsetDateTime + Send + Sync>;

/// Who is acting, for whom, and which batch their writes belong to.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub workspace_id: Uuid,
    pub user_id: Uuid,
    pub agent_id: Option<Uuid>,
    /// Every write made with this context shares one batch, so it can be undone together.
    pub batch_id: Uuid,
}

impl Ctx {
    pub fn user(workspace_id: Uuid, user_id: Uuid) -> Self {
        Ctx { workspace_id, user_id, agent_id: None, batch_id: Uuid::now_v7() }
    }

    pub fn agent(workspace_id: Uuid, user_id: Uuid, agent_id: Uuid) -> Self {
        Ctx { workspace_id, user_id, agent_id: Some(agent_id), batch_id: Uuid::now_v7() }
    }

    /// Same actor, fresh batch.
    pub fn next_batch(&self) -> Self {
        Ctx { batch_id: Uuid::now_v7(), ..self.clone() }
    }

    pub fn actor_id(&self) -> Uuid {
        self.agent_id.unwrap_or(self.user_id)
    }

    pub fn is_human(&self) -> bool {
        self.agent_id.is_none()
    }
}

pub struct Store {
    db: Database,
    writer: Mutex<Connection>,
    /// Idle read connections. A connection must never be shared by concurrent
    /// reads, or one read's snapshot leaks into another.
    readers: std::sync::Mutex<Vec<Connection>>,
    /// `:memory:` databases exist per connection, so everything uses the writer.
    in_memory: Option<Connection>,
    clock: Clock,
}

pub(crate) struct ReadConn<'a> {
    conn: Option<Connection>,
    pool: Option<&'a std::sync::Mutex<Vec<Connection>>>,
}

impl std::ops::Deref for ReadConn<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn.as_ref().expect("present until drop")
    }
}

impl Drop for ReadConn<'_> {
    fn drop(&mut self) {
        if let (Some(pool), Some(conn)) = (self.pool, self.conn.take()) {
            pool.lock().expect("reader pool poisoned").push(conn);
        }
    }
}

impl Store {
    /// Opens (or creates) a local SQLite file in WAL mode. `:memory:` works for tests.
    pub async fn open_local(path: impl AsRef<Path>) -> Result<Store> {
        let path = path.as_ref();
        let db = libsql::Builder::new_local(path).build().await?;
        let writer = db.connect()?;
        writer.busy_timeout(Duration::from_secs(5))?;
        writer.query("PRAGMA journal_mode = WAL", ()).await?;
        writer.execute("PRAGMA foreign_keys = ON", ()).await?;
        let in_memory = (path.as_os_str() == ":memory:").then(|| writer.clone());
        Self::finish(db, writer, in_memory).await
    }

    /// Opens a remote libSQL database such as Turso Cloud.
    pub async fn open_remote(url: String, auth_token: String) -> Result<Store> {
        let db = libsql::Builder::new_remote(url, auth_token).build().await?;
        let conn = db.connect()?;
        Self::finish(db, conn, None).await
    }

    async fn finish(db: Database, writer: Connection, in_memory: Option<Connection>) -> Result<Store> {
        db::migrate(&writer).await?;
        Ok(Store {
            db,
            writer: Mutex::new(writer),
            readers: Default::default(),
            in_memory,
            clock: Arc::new(OffsetDateTime::now_utc),
        })
    }

    pub(crate) fn reader(&self) -> Result<ReadConn<'_>> {
        if let Some(c) = &self.in_memory {
            return Ok(ReadConn { conn: Some(c.clone()), pool: None });
        }
        let idle = self.readers.lock().expect("reader pool poisoned").pop();
        let conn = match idle {
            Some(c) => c,
            None => {
                let c = self.db.connect()?;
                c.busy_timeout(Duration::from_secs(5))?;
                c
            }
        };
        Ok(ReadConn { conn: Some(conn), pool: Some(&self.readers) })
    }

    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    pub(crate) fn now(&self) -> OffsetDateTime {
        (self.clock)()
    }

    /// Runs `f` in one immediate transaction. Writes are serialized in-process so
    /// SQLite never has to arbitrate between our own connections.
    pub(crate) async fn write<T>(&self, ctx: &Ctx, f: impl AsyncFnOnce(&Tx) -> Result<T>) -> Result<T> {
        let conn = self.writer.lock().await;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).await?;
        let w = Tx { conn: (*tx).clone(), ctx: ctx.clone(), now: self.now() };
        match f(&w).await {
            Ok(v) => {
                tx.commit().await?;
                Ok(v)
            }
            Err(e) => {
                tx.rollback().await.ok();
                Err(e)
            }
        }
    }
}

pub(crate) struct Tx {
    pub conn: Connection,
    pub ctx: Ctx,
    pub now: OffsetDateTime,
}

pub(crate) struct EventIn<'a> {
    pub entity: Entity,
    pub entity_id: String,
    pub task_id: Option<Uuid>,
    pub kind: &'a str,
    pub before: Option<Value>,
    pub after: Option<Value>,
    pub undoes: Option<i64>,
    /// Overrides the context's actor, e.g. when expiring someone else's claim.
    pub actor: Option<(Uuid, Uuid)>,
}

impl Tx {
    pub fn ws(&self) -> String {
        self.ctx.workspace_id.to_string()
    }

    pub fn now_str(&self) -> String {
        fmt_ts(self.now)
    }

    pub async fn emit(&self, e: EventIn<'_>) -> Result<i64> {
        let (actor, behalf) = e.actor.unwrap_or((self.ctx.actor_id(), self.ctx.user_id));
        let mut rows = self
            .conn
            .query(
                "INSERT INTO events (workspace_id, batch_id, actor_id, on_behalf_of, task_id, entity, entity_id, kind, before, after, undoes, at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) RETURNING id",
                libsql::params![
                    self.ws(),
                    self.ctx.batch_id.to_string(),
                    actor.to_string(),
                    behalf.to_string(),
                    opt_uuid_value(e.task_id),
                    e.entity.as_str(),
                    e.entity_id,
                    e.kind,
                    e.before.map(|v| v.to_string()),
                    e.after.map(|v| v.to_string()),
                    e.undoes,
                    self.now_str(),
                ],
            )
            .await?;
        let row = rows.next().await?.expect("RETURNING yields a row");
        Ok(row.get(0)?)
    }

    pub async fn emit_change<T: Serialize>(
        &self,
        entity: Entity,
        entity_id: String,
        task_id: Option<Uuid>,
        kind: &str,
        before: Option<&T>,
        after: Option<&T>,
    ) -> Result<i64> {
        self.emit(EventIn {
            entity,
            entity_id,
            task_id,
            kind,
            before: before.map(serde_json::to_value).transpose()?,
            after: after.map(serde_json::to_value).transpose()?,
            undoes: None,
            actor: None,
        })
        .await
    }
}
