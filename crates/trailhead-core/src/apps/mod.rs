//! The app data store: each app's records and their event log in a SQLite file of its own, `<app dir>/<app id>.db`,
//! which only the store opens. A file is created when its app's declaration is first registered, and keeps that
//! declaration, so a restart knows the last shape.

mod log;
mod records;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use jiff::Timestamp;
use libsql::{Connection, Database, TransactionBehavior};
use tokio::sync::{Mutex, watch};
use uuid::Uuid;

use crate::db::{fmt_ts, migrate_app};
use crate::error::{Error, Result};
use crate::store::{Ctx, ReadConn, Store};
use trailhead_model::app_data::{Declaration, check_app_id};

pub use log::AppEventFilter;
pub use records::{AppWrite, NewRecord, RecordChange, RecordPage};

/// The apps' data files a store keeps, and those it has open.
pub(crate) struct AppFiles {
    dir: Option<PathBuf>,
    open: Mutex<HashMap<String, Arc<AppDb>>>,
}

/// One app's open data file.
pub(crate) struct AppDb {
    db: Database,
    writer: Mutex<Connection>,
    readers: std::sync::Mutex<Vec<Connection>>,
    /// The workspace whose app this is; another workspace's context finds no such app.
    workspace_id: Uuid,
    /// The declaration last registered, changed only under the writer's lock.
    declaration: RwLock<Declaration>,
    changes: watch::Sender<u64>,
}

/// A write in one app's file.
pub(crate) struct AppTx {
    pub conn: Connection,
    pub ctx: Ctx,
    pub now: Timestamp,
    /// The declaration as the write began.
    pub decl: Declaration,
    /// Set by an event, so a write that changed nothing wakes no stream.
    wrote: AtomicBool,
    /// A declaration to take once the write commits.
    new_decl: std::sync::Mutex<Option<Declaration>>,
}

const DECLARATION: &str = "declaration";
const WORKSPACE: &str = "workspace_id";

impl AppFiles {
    pub(crate) fn new(dir: Option<PathBuf>) -> Self {
        AppFiles { dir, open: Mutex::default() }
    }

    fn dir(&self) -> Result<&PathBuf> {
        self.dir.as_ref().ok_or_else(|| Error::Invalid("this store keeps no app data".into()))
    }

    /// The ids of the apps with a data file.
    fn app_ids(&self) -> Result<Vec<String>> {
        let Some(dir) = &self.dir else { return Ok(Vec::new()) };
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::Corrupt(format!("reading {}: {e}", dir.display()))),
        };
        let mut ids: Vec<String> = entries
            .filter_map(|e| e.ok()?.file_name().to_str()?.strip_suffix(".db").map(str::to_string))
            .filter(|id| check_app_id(id).is_ok())
            .collect();
        ids.sort();
        Ok(ids)
    }

    /// The app's file, opened on first use. Only `create` makes a file that isn't there.
    async fn get(&self, app: &str, workspace_id: Uuid, create: bool) -> Result<Arc<AppDb>> {
        let mut open = self.open.lock().await;
        let db = match open.get(app) {
            Some(db) => db.clone(),
            None => {
                let path = self.dir()?.join(format!("{app}.db"));
                if !create && !path.exists() {
                    return Err(Error::NotFound(format!("app {app}")));
                }
                let db = Arc::new(AppDb::open(path, workspace_id).await?);
                open.insert(app.to_string(), db.clone());
                db
            }
        };
        if db.workspace_id != workspace_id {
            return Err(Error::NotFound(format!("app {app}")));
        }
        Ok(db)
    }
}

impl AppDb {
    /// Opens or creates the file, as [`Store::open_local`] opens the tasks database. A new file belongs to
    /// `workspace_id`.
    async fn open(path: PathBuf, workspace_id: Uuid) -> Result<AppDb> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::Corrupt(format!("creating {}: {e}", dir.display())))?;
        }
        let db = libsql::Builder::new_local(&path).build().await?;
        let writer = db.connect()?;
        writer.busy_timeout(Duration::from_secs(5))?;
        writer.query("PRAGMA journal_mode = WAL", ()).await?;
        migrate_app(&writer).await?;
        writer
            .execute(
                "INSERT INTO meta (name, value) VALUES (?1, ?2) ON CONFLICT (name) DO NOTHING",
                [WORKSPACE, &workspace_id.to_string()],
            )
            .await?;
        let owner = meta(&writer, WORKSPACE).await?.expect("set above");
        let owner = Uuid::parse_str(&owner).map_err(|e| Error::Corrupt(format!("app workspace {owner}: {e}")))?;
        let declaration = match meta(&writer, DECLARATION).await? {
            Some(json) => serde_json::from_str(&json)?,
            None => Declaration::default(),
        };
        Ok(AppDb {
            db,
            writer: Mutex::new(writer),
            readers: std::sync::Mutex::default(),
            workspace_id: owner,
            declaration: RwLock::new(declaration),
            changes: watch::Sender::new(0),
        })
    }

    pub(crate) fn reader(&self) -> Result<ReadConn<'_>> {
        let idle = self.readers.lock().expect("reader pool poisoned").pop();
        let conn = match idle {
            Some(c) => c,
            None => {
                let c = self.db.connect()?;
                c.busy_timeout(Duration::from_secs(5))?;
                c
            }
        };
        Ok(ReadConn::pooled(conn, &self.readers))
    }

    pub(crate) fn declaration(&self) -> Declaration {
        self.declaration.read().expect("declaration lock poisoned").clone()
    }

    /// Runs `f` in one immediate transaction on this app's file, serialized with its other writes.
    pub(crate) async fn write<T>(
        &self,
        ctx: &Ctx,
        now: Timestamp,
        f: impl AsyncFnOnce(&AppTx) -> Result<T>,
    ) -> Result<T> {
        let conn = self.writer.lock().await;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).await?;
        let w = AppTx {
            conn: (*tx).clone(),
            ctx: ctx.clone(),
            // As stored, so a record a write returns equals the one read back.
            now: Timestamp::from_microsecond(now.as_microsecond()).expect("in range"),
            decl: self.declaration(),
            wrote: AtomicBool::new(false),
            new_decl: std::sync::Mutex::default(),
        };
        match f(&w).await {
            Ok(v) => {
                tx.commit().await?;
                if let Some(decl) = w.new_decl.lock().expect("declaration lock poisoned").take() {
                    *self.declaration.write().expect("declaration lock poisoned") = decl;
                }
                if w.wrote.load(Ordering::Relaxed) {
                    self.changes.send_modify(|n| *n += 1);
                }
                Ok(v)
            }
            Err(e) => {
                tx.rollback().await.ok();
                Err(e)
            }
        }
    }
}

async fn meta(conn: &Connection, name: &str) -> Result<Option<String>> {
    let mut rows = conn.query("SELECT value FROM meta WHERE name = ?1", [name]).await?;
    Ok(rows.next().await?.map(|r| r.get(0)).transpose()?)
}

impl AppTx {
    pub fn now_str(&self) -> String {
        fmt_ts(self.now)
    }
}

impl Store {
    /// The app's open file, for a context that reaches it.
    pub(crate) async fn app(&self, ctx: &Ctx, app: &str) -> Result<Arc<AppDb>> {
        self.app_file(ctx, app, false).await
    }

    async fn app_file(&self, ctx: &Ctx, app: &str, create: bool) -> Result<Arc<AppDb>> {
        check_app_id(app)?;
        if !ctx.reaches_app(app) {
            return Err(Error::Forbidden(format!("this token can't reach app {app}")));
        }
        self.apps.get(app, ctx.workspace_id, create).await
    }

    /// Sets the app's collections, creating its data file on first use. Registering again replaces the declaration,
    /// but a collection that holds records only changes by addition (a new collection, or a new optional field);
    /// anything else is refused, saying what. The declaration is kept in the app's file.
    pub async fn register_app(&self, ctx: &Ctx, app: &str, declaration: Declaration) -> Result<()> {
        declaration.check()?;
        let db = self.app_file(ctx, app, true).await?;
        db.write(ctx, self.now(), async |tx| {
            for (collection, why) in declaration.changes_from(&tx.decl) {
                if records::holds_records(&tx.conn, &collection).await? {
                    return Err(Error::Invalid(format!(
                        "{why}, and it holds records: a collection's shape changes only by addition \
                         (a new collection or a new optional field)"
                    )));
                }
            }
            tx.conn
                .execute(
                    "INSERT INTO meta (name, value) VALUES (?1, ?2) ON CONFLICT (name) DO UPDATE SET value = excluded.value",
                    [DECLARATION.to_string(), serde_json::to_string(&declaration)?],
                )
                .await?;
            *tx.new_decl.lock().expect("declaration lock poisoned") = Some(declaration);
            Ok(())
        })
        .await
    }

    /// The app's collections as last registered.
    pub async fn app_declaration(&self, ctx: &Ctx, app: &str) -> Result<Declaration> {
        Ok(self.app(ctx, app).await?.declaration())
    }

    /// Changes whenever a write that records an event commits in the app's file. Only a wake-up hint, as
    /// [`Store::changes`] is for the task log.
    pub async fn app_changes(&self, ctx: &Ctx, app: &str) -> Result<watch::Receiver<u64>> {
        Ok(self.app(ctx, app).await?.changes.subscribe())
    }
}
