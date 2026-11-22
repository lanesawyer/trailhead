use std::collections::HashSet;
use std::sync::atomic::Ordering;

use libsql::{Connection, Row};
use serde_json::{Map, Value};
use uuid::Uuid;

use super::AppTx;
use crate::db::{collect, first, get_json, get_ts, get_uuid};
use crate::error::{Error, Result};
use crate::store::{Ctx, Store};
use trailhead_model::app_data::{Collection, Declaration, Outcome, Record, ReplaceCounts, Written, check_key};

/// A record to create.
#[derive(Debug, Clone)]
pub struct NewRecord {
    pub collection: String,
    /// Unique in its collection.
    pub key: Option<String>,
    /// A JSON object of the collection's fields.
    pub data: Value,
}

/// How an update changes a record's data. Neither changes its key.
#[derive(Debug, Clone)]
pub enum RecordChange {
    /// The whole data, replaced.
    Replace(Value),
    /// A JSON merge patch (RFC 7396): the fields given are set, those given as `null` removed, the rest kept.
    Merge(Value),
}

/// A page of a collection's records, in id order (the order they were created).
#[derive(Debug, Clone, Default)]
pub struct RecordPage {
    /// Only records after this id.
    pub after: Option<Uuid>,
    /// At most this many; 100 if unset, and never more than [`MAX_PAGE`].
    pub limit: Option<i64>,
}

/// The most records one page holds.
pub const MAX_PAGE: i64 = 1000;

/// One write in [`Store::write_records`].
#[derive(Debug, Clone)]
pub enum AppWrite {
    Create(NewRecord),
    Update {
        id: Uuid,
        change: RecordChange,
        expected_version: Option<i64>,
    },
    Delete {
        id: Uuid,
        expected_version: Option<i64>,
    },
    /// Create or change the record with this key; see [`Store::put_record`].
    Put {
        collection: String,
        key: String,
        data: Value,
    },
}

const RECORD_COLS: &str = "id, collection, key, data, version, created_at, updated_at, created_by";

fn record_from_row(row: &Row) -> Result<Record> {
    let Value::Object(data) = get_json(row, 3)? else {
        return Err(Error::Corrupt("a record's data isn't an object".into()));
    };
    Ok(Record {
        id: get_uuid(row, 0)?,
        collection: row.get(1)?,
        key: row.get(2)?,
        data,
        version: row.get(4)?,
        created_at: get_ts(row, 5)?,
        updated_at: get_ts(row, 6)?,
        created_by: get_uuid(row, 7)?,
    })
}

pub(super) async fn load(conn: &Connection, id: Uuid) -> Result<Option<Record>> {
    let rows = conn.query(&format!("SELECT {RECORD_COLS} FROM records WHERE id = ?1"), [id.to_string()]).await?;
    first(rows, record_from_row).await
}

pub(super) async fn load_by_key(conn: &Connection, collection: &str, key: &str) -> Result<Option<Record>> {
    let rows = conn
        .query(&format!("SELECT {RECORD_COLS} FROM records WHERE collection = ?1 AND key = ?2"), [collection, key])
        .await?;
    first(rows, record_from_row).await
}

pub(super) async fn holds_records(conn: &Connection, collection: &str) -> Result<bool> {
    let mut rows = conn.query("SELECT 1 FROM records WHERE collection = ?1 LIMIT 1", [collection]).await?;
    Ok(rows.next().await?.is_some())
}

pub(super) async fn insert(tx: &AppTx, r: &Record) -> Result<()> {
    tx.conn
        .execute(
            &format!("INSERT INTO records ({RECORD_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"),
            libsql::params![
                r.id.to_string(),
                r.collection.clone(),
                r.key.clone(),
                Value::Object(r.data.clone()).to_string(),
                r.version,
                crate::db::fmt_ts(r.created_at),
                crate::db::fmt_ts(r.updated_at),
                r.created_by.to_string(),
            ],
        )
        .await?;
    Ok(())
}

pub(super) async fn save(tx: &AppTx, r: &Record) -> Result<()> {
    tx.conn
        .execute(
            "UPDATE records SET key = ?1, data = ?2, version = ?3, updated_at = ?4 WHERE id = ?5",
            libsql::params![
                r.key.clone(),
                Value::Object(r.data.clone()).to_string(),
                r.version,
                crate::db::fmt_ts(r.updated_at),
                r.id.to_string(),
            ],
        )
        .await?;
    Ok(())
}

pub(super) async fn remove(tx: &AppTx, id: Uuid) -> Result<()> {
    tx.conn.execute("DELETE FROM records WHERE id = ?1", [id.to_string()]).await?;
    Ok(())
}

/// Logs a change to a record in the app's file, in the write's transaction.
pub(super) async fn emit(
    tx: &AppTx,
    collection: &str,
    record_id: Uuid,
    kind: &str,
    before: Option<&Record>,
    after: Option<&Record>,
    undoes: Option<i64>,
) -> Result<i64> {
    let json = |r: Option<&Record>| r.map(serde_json::to_string).transpose();
    let mut rows = tx
        .conn
        .query(
            "INSERT INTO events (batch_id, actor_id, on_behalf_of, collection, record_id, kind, before, after, undoes, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) RETURNING id",
            libsql::params![
                tx.ctx.batch_id.to_string(),
                tx.ctx.actor_id().to_string(),
                tx.ctx.user_id.to_string(),
                collection,
                record_id.to_string(),
                kind,
                json(before)?,
                json(after)?,
                undoes,
                tx.now_str(),
            ],
        )
        .await?;
    let row = rows.next().await?.expect("RETURNING yields a row");
    tx.wrote.store(true, Ordering::Relaxed);
    Ok(row.get(0)?)
}

pub(super) fn collection<'a>(decl: &'a Declaration, name: &str) -> Result<&'a Collection> {
    decl.collections.get(name).ok_or_else(|| Error::NotFound(format!("collection {name}")))
}

/// A fed collection is written only by replacing its set from its feed (`feed`), and only it is.
fn writable(name: &str, c: &Collection, feed: bool) -> Result<()> {
    match (&c.from, feed) {
        (Some(from), false) => {
            Err(Error::Forbidden(format!("collection {name} is filled from feed {from}; only its connector writes it")))
        }
        (None, true) => Err(Error::Invalid(format!("collection {name} isn't filled from a feed"))),
        _ => Ok(()),
    }
}

fn check_version(r: &Record, expected: Option<i64>) -> Result<()> {
    match expected {
        Some(v) if v != r.version => Err(Error::VersionConflict { expected: v, current: r.version }),
        _ => Ok(()),
    }
}

/// RFC 7396.
fn merge_patch(target: &mut Value, patch: Value) {
    let Value::Object(patch) = patch else {
        *target = patch;
        return;
    };
    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    let t = target.as_object_mut().expect("made an object above");
    for (k, v) in patch {
        if v.is_null() {
            t.remove(&k);
        } else {
            merge_patch(t.entry(k).or_insert(Value::Null), v);
        }
    }
}

async fn create(tx: &AppTx, new: NewRecord) -> Result<Written> {
    let c = collection(&tx.decl, &new.collection)?;
    writable(&new.collection, c, false)?;
    let data = c.check_data(new.data)?;
    if let Some(key) = &new.key {
        check_key(key)?;
        if load_by_key(&tx.conn, &new.collection, key).await?.is_some() {
            return Err(Error::AlreadyExists(format!("a record with key {key:?} in {}", new.collection)));
        }
    }
    insert_new(tx, new.collection, new.key, data).await
}

async fn insert_new(tx: &AppTx, collection: String, key: Option<String>, data: Map<String, Value>) -> Result<Written> {
    let record = Record {
        id: Uuid::now_v7(),
        collection,
        key,
        data,
        version: 1,
        created_at: tx.now,
        updated_at: tx.now,
        created_by: tx.ctx.actor_id(),
    };
    insert(tx, &record).await?;
    emit(tx, &record.collection, record.id, "record.created", None, Some(&record), None).await?;
    Ok(Written { outcome: Outcome::Created, record })
}

/// Gives `cur` this data, writing nothing when it already has it.
async fn set_data(tx: &AppTx, cur: Record, data: Map<String, Value>) -> Result<Written> {
    if data == cur.data {
        return Ok(Written { outcome: Outcome::Unchanged, record: cur });
    }
    let record = Record { data, version: cur.version + 1, updated_at: tx.now, ..cur.clone() };
    save(tx, &record).await?;
    emit(tx, &record.collection, record.id, "record.updated", Some(&cur), Some(&record), None).await?;
    Ok(Written { outcome: Outcome::Changed, record })
}

async fn found(tx: &AppTx, id: Uuid) -> Result<Record> {
    load(&tx.conn, id).await?.ok_or_else(|| Error::NotFound(format!("record {id}")))
}

async fn change(tx: &AppTx, id: Uuid, change: RecordChange, expected: Option<i64>) -> Result<Written> {
    let cur = found(tx, id).await?;
    let c = collection(&tx.decl, &cur.collection)?;
    writable(&cur.collection, c, false)?;
    check_version(&cur, expected)?;
    let data = match change {
        RecordChange::Replace(data) => data,
        RecordChange::Merge(patch) => {
            let mut data = Value::Object(cur.data.clone());
            merge_patch(&mut data, patch);
            data
        }
    };
    let data = c.check_data(data)?;
    set_data(tx, cur, data).await
}

async fn delete(tx: &AppTx, id: Uuid, expected: Option<i64>) -> Result<Written> {
    let cur = found(tx, id).await?;
    let c = collection(&tx.decl, &cur.collection)?;
    writable(&cur.collection, c, false)?;
    check_version(&cur, expected)?;
    remove(tx, id).await?;
    emit(tx, &cur.collection, id, "record.deleted", Some(&cur), None, None).await?;
    Ok(Written { outcome: Outcome::Deleted, record: cur })
}

async fn put(tx: &AppTx, name: &str, key: String, data: Value, feed: bool) -> Result<Written> {
    let c = collection(&tx.decl, name)?;
    writable(name, c, feed)?;
    check_key(&key)?;
    let data = c.check_data(data)?;
    match load_by_key(&tx.conn, name, &key).await? {
        Some(cur) => set_data(tx, cur, data).await,
        None => insert_new(tx, name.to_string(), Some(key), data).await,
    }
}

async fn apply(tx: &AppTx, write: AppWrite) -> Result<Written> {
    match write {
        AppWrite::Create(new) => create(tx, new).await,
        AppWrite::Update { id, change: c, expected_version } => change(tx, id, c, expected_version).await,
        AppWrite::Delete { id, expected_version } => delete(tx, id, expected_version).await,
        AppWrite::Put { collection, key, data } => put(tx, &collection, key, data, false).await,
    }
}

async fn replace_set(tx: &AppTx, name: &str, set: Vec<(String, Value)>, feed: bool) -> Result<ReplaceCounts> {
    writable(name, collection(&tx.decl, name)?, feed)?;
    let mut keys = HashSet::new();
    if let Some((dup, _)) = set.iter().find(|(k, _)| !keys.insert(k.clone())) {
        return Err(Error::Invalid(format!("key {dup:?} is in the set twice")));
    }
    let mut counts = ReplaceCounts::default();
    for (key, data) in set {
        match put(tx, name, key, data, feed).await?.outcome {
            Outcome::Created => counts.created += 1,
            Outcome::Changed => counts.changed += 1,
            Outcome::Unchanged => counts.unchanged += 1,
            Outcome::Deleted => unreachable!("a put never deletes"),
        }
    }
    let rows = tx.conn.query(&format!("SELECT {RECORD_COLS} FROM records WHERE collection = ?1"), [name]).await?;
    for gone in collect(rows, record_from_row).await? {
        if gone.key.as_ref().is_none_or(|k| !keys.contains(k)) {
            remove(tx, gone.id).await?;
            emit(tx, name, gone.id, "record.deleted", Some(&gone), None, None).await?;
            counts.removed += 1;
        }
    }
    Ok(counts)
}

impl Store {
    /// Creates a record, checking its data against the collection's fields. A key already in the collection is
    /// refused (`already_exists`); [`Store::put_record`] writes by key instead.
    pub async fn create_record(&self, ctx: &Ctx, app: &str, new: NewRecord) -> Result<Record> {
        let db = self.app(ctx, app).await?;
        Ok(db.write(ctx, self.now(), async |tx| create(tx, new).await).await?.record)
    }

    pub async fn get_record(&self, ctx: &Ctx, app: &str, id: Uuid) -> Result<Record> {
        let db = self.app(ctx, app).await?;
        let conn = db.reader()?;
        load(&conn, id).await?.ok_or_else(|| Error::NotFound(format!("record {id}")))
    }

    pub async fn get_record_by_key(&self, ctx: &Ctx, app: &str, collection: &str, key: &str) -> Result<Record> {
        let db = self.app(ctx, app).await?;
        let conn = db.reader()?;
        load_by_key(&conn, collection, key)
            .await?
            .ok_or_else(|| Error::NotFound(format!("record {key:?} in {collection}")))
    }

    /// A page of a declared collection's records.
    pub async fn list_records(&self, ctx: &Ctx, app: &str, name: &str, page: RecordPage) -> Result<Vec<Record>> {
        let db = self.app(ctx, app).await?;
        collection(&db.declaration(), name)?;
        let conn = db.reader()?;
        let rows = conn
            .query(
                &format!(
                    "SELECT {RECORD_COLS} FROM records WHERE collection = ?1 AND (?2 IS NULL OR id > ?2) ORDER BY id LIMIT ?3"
                ),
                libsql::params![name, page.after.map(|a| a.to_string()), page.limit.unwrap_or(100).clamp(1, MAX_PAGE)],
            )
            .await?;
        collect(rows, record_from_row).await
    }

    /// Changes a record's data; with `expected_version`, only if the record is still at it (`version_conflict`
    /// otherwise). Data the record already has writes nothing.
    pub async fn update_record(
        &self,
        ctx: &Ctx,
        app: &str,
        id: Uuid,
        c: RecordChange,
        expected_version: Option<i64>,
    ) -> Result<Written> {
        let db = self.app(ctx, app).await?;
        db.write(ctx, self.now(), async |tx| change(tx, id, c, expected_version).await).await
    }

    /// Removes a record, returning it as it was.
    pub async fn delete_record(&self, ctx: &Ctx, app: &str, id: Uuid, expected_version: Option<i64>) -> Result<Record> {
        let db = self.app(ctx, app).await?;
        Ok(db.write(ctx, self.now(), async |tx| delete(tx, id, expected_version).await).await?.record)
    }

    /// Makes the record with this key look like `data`: creates it, changes it, or, when it already does, writes
    /// nothing (no event, same version). The outcome says which.
    pub async fn put_record(&self, ctx: &Ctx, app: &str, collection: &str, key: &str, data: Value) -> Result<Written> {
        let db = self.app(ctx, app).await?;
        db.write(ctx, self.now(), async |tx| put(tx, collection, key.to_string(), data, false).await).await
    }

    /// Several writes in one transaction and the context's batch: all of them, or none.
    pub async fn write_records(&self, ctx: &Ctx, app: &str, writes: Vec<AppWrite>) -> Result<Vec<Written>> {
        let db = self.app(ctx, app).await?;
        db.write(ctx, self.now(), async |tx| {
            let mut out = Vec::with_capacity(writes.len());
            for w in writes {
                out.push(apply(tx, w).await?);
            }
            Ok(out)
        })
        .await
    }

    /// Makes a collection hold exactly this keyed set, in one transaction: records are created, changed and removed
    /// (a record with no key, or a key not in the set, is removed), and those already matching write nothing.
    /// Refused for a collection filled from a feed.
    pub async fn replace_set(
        &self,
        ctx: &Ctx,
        app: &str,
        collection: &str,
        set: Vec<(String, Value)>,
    ) -> Result<ReplaceCounts> {
        let db = self.app(ctx, app).await?;
        db.write(ctx, self.now(), async |tx| replace_set(tx, collection, set, false).await).await
    }

    /// [`Store::replace_set`] for a collection filled from a feed, the only way one is written. Connectors don't
    /// exist yet: until they do, nothing outside the engine's callers reaches this, and a connector's run will be
    /// checked here to be the feed's own connector (`connector/<id>`) before it may call it.
    pub async fn replace_feed_set(
        &self,
        ctx: &Ctx,
        app: &str,
        collection: &str,
        set: Vec<(String, Value)>,
    ) -> Result<ReplaceCounts> {
        let db = self.app(ctx, app).await?;
        db.write(ctx, self.now(), async |tx| replace_set(tx, collection, set, true).await).await
    }
}
