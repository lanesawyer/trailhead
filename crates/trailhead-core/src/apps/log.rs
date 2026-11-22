use std::collections::{BTreeMap, HashSet};

use jiff::Timestamp;
use libsql::{Connection, Row};
use serde_json::Value;
use uuid::Uuid;

use super::AppTx;
use super::records::{collection, emit, insert, load, load_by_key, remove, save};
use crate::db::{collect, fmt_ts, get_opt_json, get_ts, get_uuid};
use crate::error::{Error, Result};
use crate::events::UNDO_WINDOW;
use crate::store::{Ctx, Store};
use trailhead_model::app_data::{AppEvent, AppReverted, AppSkipped, AppUndoReport, Record};

/// Which of an app's events to list, oldest first.
#[derive(Debug, Clone, Default)]
pub struct AppEventFilter {
    pub collection: Option<String>,
    pub record: Option<Uuid>,
    /// Only events with a greater id: where a stream resumes.
    pub after_id: Option<i64>,
    /// 100 if unset.
    pub limit: Option<i64>,
}

const EVENT_COLS: &str = "id, batch_id, actor_id, on_behalf_of, collection, record_id, kind, before, after, undoes, at";

fn event_from_row(row: &Row) -> Result<AppEvent> {
    Ok(AppEvent {
        id: row.get(0)?,
        batch_id: get_uuid(row, 1)?,
        actor_id: get_uuid(row, 2)?,
        on_behalf_of: get_uuid(row, 3)?,
        collection: row.get(4)?,
        record_id: get_uuid(row, 5)?,
        kind: row.get(6)?,
        before: get_opt_json(row, 7)?,
        after: get_opt_json(row, 8)?,
        undoes: row.get(9)?,
        at: get_ts(row, 10)?,
    })
}

async fn query_events(conn: &Connection, where_clause: &str, params: Vec<libsql::Value>) -> Result<Vec<AppEvent>> {
    let rows = conn.query(&format!("SELECT {EVENT_COLS} FROM events WHERE {where_clause} ORDER BY id"), params).await?;
    collect(rows, event_from_row).await
}

/// What [`restore`] did.
enum Restored {
    /// Restored (the new event id), or already in that state (`None`).
    Done(Option<i64>),
    /// Left alone, and why; nothing was written.
    Refused(Error),
}

fn as_record(v: Option<Value>) -> Result<Option<Record>> {
    Ok(v.map(serde_json::from_value).transpose()?)
}

/// Puts one record back to `target` (`None`: it didn't exist) and logs the change. A record in a fed collection, one
/// whose data the collection's fields no longer take, or one whose key another record now has is refused.
async fn restore(tx: &AppTx, name: &str, id: Uuid, target: Option<Record>, undoes: i64) -> Result<Restored> {
    let cur = load(&tx.conn, id).await?;
    match (&cur, &target) {
        (None, None) => return Ok(Restored::Done(None)),
        (Some(c), Some(t)) if c.data == t.data && c.key == t.key => return Ok(Restored::Done(None)),
        _ => {}
    }
    let c = match collection(&tx.decl, name) {
        Ok(c) => c,
        Err(e) => return Ok(Restored::Refused(e)),
    };
    if let Some(from) = &c.from {
        return Ok(Restored::Refused(Error::Forbidden(format!(
            "collection {name} is filled from feed {from}; its next run sets it"
        ))));
    }
    let after = match target {
        None => None,
        Some(t) => {
            if let Err(e) = c.check_data(Value::Object(t.data.clone())) {
                return Ok(Restored::Refused(Error::Invalid(format!("record {id} no longer fits {name}: {e}"))));
            }
            if let Some(key) = &t.key
                && let Some(other) = load_by_key(&tx.conn, name, key).await?
                && other.id != id
            {
                return Ok(Restored::Refused(Error::AlreadyExists(format!(
                    "record {} has key {key:?} in {name} now",
                    other.id
                ))));
            }
            Some(t)
        }
    };
    let after = match (&cur, after) {
        (Some(_), None) => {
            remove(tx, id).await?;
            None
        }
        (Some(c), Some(t)) => {
            let r = Record { key: t.key, data: t.data, version: c.version + 1, updated_at: tx.now, ..c.clone() };
            save(tx, &r).await?;
            Some(r)
        }
        (None, Some(t)) => {
            let r = Record { version: t.version + 1, updated_at: tx.now, ..t };
            insert(tx, &r).await?;
            Some(r)
        }
        (None, None) => unreachable!("returned above"),
    };
    let event = emit(tx, name, id, "record.reverted", cur.as_ref(), after.as_ref(), Some(undoes)).await?;
    Ok(Restored::Done(Some(event)))
}

/// Reverts every record `events` touched to its state before the earliest of them, as the task log's undo does. A
/// record someone else changed afterwards is a conflict: `strict` fails the whole undo, otherwise it's skipped and
/// reported, as is a record [`restore`] refuses.
async fn apply(tx: &AppTx, events: Vec<AppEvent>, strict: bool) -> Result<AppUndoReport> {
    let ids: HashSet<i64> = events.iter().map(|e| e.id).collect();
    let mut groups: BTreeMap<Uuid, Vec<AppEvent>> = BTreeMap::new();
    for e in events {
        groups.entry(e.record_id).or_default().push(e);
    }
    let mut groups: Vec<Vec<AppEvent>> = groups.into_values().collect();
    groups.sort_by_key(|g| std::cmp::Reverse(g.iter().map(|e| e.id).min()));

    let mut report = AppUndoReport { batch_id: tx.ctx.batch_id, reverted: Vec::new(), skipped: Vec::new() };
    for group in groups {
        let first = group.into_iter().min_by_key(|e| e.id).expect("groups are non-empty");
        let later = query_events(
            &tx.conn,
            "record_id = ?1 AND id > ?2",
            vec![first.record_id.to_string().into(), first.id.into()],
        )
        .await?;
        let skip = |later_event, reason| AppSkipped {
            collection: first.collection.clone(),
            record_id: first.record_id,
            later_event,
            reason,
        };
        if let Some(later) = later.iter().find(|e| !ids.contains(&e.id)) {
            if strict {
                return Err(Error::UndoConflict {
                    entity: format!("record {} in {}", first.record_id, first.collection),
                    later_event: later.id,
                });
            }
            report.skipped.push(skip(Some(later.id), format!("changed later by event #{}", later.id)));
            continue;
        }
        match restore(tx, &first.collection, first.record_id, as_record(first.before.clone())?, first.id).await? {
            Restored::Done(event_id) => report.reverted.push(AppReverted {
                collection: first.collection.clone(),
                record_id: first.record_id,
                event_id,
            }),
            Restored::Refused(e) if strict => return Err(e),
            Restored::Refused(e) => report.skipped.push(skip(None, e.to_string())),
        }
    }
    Ok(report)
}

/// Refuses to undo events older than [`UNDO_WINDOW`].
fn check_window(tx: &AppTx, what: &str, events: &[AppEvent]) -> Result<()> {
    if events.iter().any(|e| e.at < tx.now - UNDO_WINDOW) {
        return Err(Error::UndoWindowExpired(what.to_string()));
    }
    Ok(())
}

impl Store {
    /// The id of the app's newest event, so a stream can start from "now". Each app's file numbers its own.
    pub async fn latest_app_event_id(&self, ctx: &Ctx, app: &str) -> Result<i64> {
        let db = self.app(ctx, app).await?;
        let conn = db.reader()?;
        let mut rows = conn.query("SELECT coalesce(max(id), 0) FROM events", ()).await?;
        Ok(rows.next().await?.map(|r| r.get(0)).transpose()?.unwrap_or(0))
    }

    /// The app's events in id order.
    pub async fn list_app_events(&self, ctx: &Ctx, app: &str, filter: AppEventFilter) -> Result<Vec<AppEvent>> {
        let db = self.app(ctx, app).await?;
        let conn = db.reader()?;
        let rows = conn
            .query(
                &format!(
                    "SELECT {EVENT_COLS} FROM events WHERE (?1 IS NULL OR collection = ?1)
                     AND (?2 IS NULL OR record_id = ?2) AND (?3 IS NULL OR id > ?3) ORDER BY id LIMIT ?4"
                ),
                libsql::params![
                    filter.collection,
                    filter.record.map(|r| r.to_string()),
                    filter.after_id,
                    filter.limit.unwrap_or(100)
                ],
            )
            .await?;
        collect(rows, event_from_row).await
    }

    /// Undoes one of the app's events. Fails with `undo_conflict` if the record changed since.
    pub async fn undo_app_event(&self, ctx: &Ctx, app: &str, event_id: i64) -> Result<AppUndoReport> {
        let db = self.app(ctx, app).await?;
        db.write(ctx, self.now(), async |tx| {
            let events = query_events(&tx.conn, "id = ?1", vec![event_id.into()]).await?;
            if events.is_empty() {
                return Err(Error::NotFound(format!("event {event_id} in app {app}")));
            }
            check_window(tx, &format!("event {event_id}"), &events)?;
            apply(tx, events, true).await
        })
        .await
    }

    /// Undoes everything one batch wrote in the app's file. All or nothing.
    pub async fn undo_app_batch(&self, ctx: &Ctx, app: &str, batch_id: Uuid) -> Result<AppUndoReport> {
        let db = self.app(ctx, app).await?;
        db.write(ctx, self.now(), async |tx| {
            let events = query_events(&tx.conn, "batch_id = ?1", vec![batch_id.to_string().into()]).await?;
            if events.is_empty() {
                return Err(Error::NotFound(format!("batch {batch_id} in app {app}")));
            }
            check_window(tx, &format!("batch {batch_id}"), &events)?;
            apply(tx, events, true).await
        })
        .await
    }

    /// Reverts everything `actor_id` did in the app's file since `since`, skipping and reporting what someone else
    /// has touched since. A `since` before the undo window is clamped to it. Only for a person, as
    /// [`Store::revert_actor`] is.
    pub async fn revert_app_actor(
        &self,
        ctx: &Ctx,
        app: &str,
        actor_id: Uuid,
        since: Timestamp,
    ) -> Result<AppUndoReport> {
        if !ctx.is_human() {
            return Err(Error::Forbidden("only a human can revert an actor's changes".into()));
        }
        let db = self.app(ctx, app).await?;
        db.write(ctx, self.now(), async |tx| {
            let since = since.max(tx.now - UNDO_WINDOW);
            let events = query_events(
                &tx.conn,
                "actor_id = ?1 AND at >= ?2",
                vec![actor_id.to_string().into(), fmt_ts(since).into()],
            )
            .await?;
            apply(tx, events, false).await
        })
        .await
    }

    /// [`Store::revert_app_actor`] over every app's file the context reaches, one file at a time, each in its own
    /// transaction (SQLite can't commit across files): an error stops it there, and the apps reverted before it stay
    /// reverted. Returns each app's report by id.
    pub async fn revert_actor_in_apps(
        &self,
        ctx: &Ctx,
        actor_id: Uuid,
        since: Timestamp,
    ) -> Result<Vec<(String, AppUndoReport)>> {
        if !ctx.is_human() {
            return Err(Error::Forbidden("only a human can revert an actor's changes".into()));
        }
        let mut out = Vec::new();
        for app in self.apps.app_ids()? {
            if !ctx.reaches_app(&app) {
                continue;
            }
            match self.revert_app_actor(ctx, &app, actor_id, since).await {
                Ok(report) => out.push((app, report)),
                // Another workspace's app.
                Err(Error::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }
}
