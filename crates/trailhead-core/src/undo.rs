use std::collections::{BTreeMap, HashSet};

use serde::Serialize;
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::claims::{any_claim, put_claim};
use crate::comments::{load_comment, save_comment};
use crate::deps::{insert_dependency, load_dependency, remove_dependency_row};
use crate::error::{Error, Result};
use crate::events::{EVENT_COLS, UNDO_WINDOW, collect_events, scope_sql};
use crate::model::{Claim, Comment, Dependency, Entity, Event, Project, Task};
use crate::projects::{load_project, save_project};
use crate::store::{Ctx, EventIn, Store, Tx};
use crate::tasks::{load_task, save_task};

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UndoReport {
    pub batch_id: Uuid,
    pub reverted: Vec<Reverted>,
    pub skipped: Vec<Skipped>,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Reverted {
    pub entity: Entity,
    pub entity_id: String,
    /// The event that recorded the revert, if anything actually changed.
    pub event_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Skipped {
    pub entity: Entity,
    pub entity_id: String,
    pub task_id: Option<Uuid>,
    /// A later event by someone else that would be overwritten.
    pub later_event: i64,
}

fn parse_uuid(s: &str) -> Result<Uuid> {
    Uuid::parse_str(s).map_err(|e| Error::Corrupt(format!("entity id {s}: {e}")))
}

fn same_task(a: &Task, b: &Task) -> bool {
    Task { version: 0, updated_at: a.created_at, ..a.clone() }
        == Task { version: 0, updated_at: b.created_at, ..b.clone() }
}

fn same_project(a: &Project, b: &Project) -> bool {
    Project { version: 0, updated_at: a.created_at, ..a.clone() }
        == Project { version: 0, updated_at: b.created_at, ..b.clone() }
}

/// Puts one entity back to `target` (`None` meaning it didn't exist) and logs the
/// change. Returns the new event id, or `None` if it was already in that state.
async fn restore(tx: &Tx, entity: Entity, entity_id: &str, target: Option<Value>, undoes: i64) -> Result<Option<i64>> {
    let ws = tx.ctx.workspace_id;
    let (task_id, before, after): (Option<Uuid>, Option<Value>, Option<Value>) = match entity {
        Entity::Task => {
            let cur = load_task(&tx.conn, ws, entity_id, true).await?;
            let want = match target {
                None => Task { deleted_at: Some(cur.deleted_at.unwrap_or(tx.now)), ..cur.clone() },
                Some(v) => serde_json::from_value(v)?,
            };
            if same_task(&cur, &want) {
                return Ok(None);
            }
            let saved = save_task(tx, &cur, want).await?;
            (Some(cur.id), Some(serde_json::to_value(&cur)?), Some(serde_json::to_value(&saved)?))
        }
        Entity::Project => {
            let cur = load_project(&tx.conn, ws, entity_id, true).await?;
            let want = match target {
                None => Project { deleted_at: Some(cur.deleted_at.unwrap_or(tx.now)), ..cur.clone() },
                Some(v) => serde_json::from_value(v)?,
            };
            if same_project(&cur, &want) {
                return Ok(None);
            }
            let saved = save_project(tx, &cur, want).await?;
            (None, Some(serde_json::to_value(&cur)?), Some(serde_json::to_value(&saved)?))
        }
        Entity::Comment => {
            let cur = load_comment(&tx.conn, ws, parse_uuid(entity_id)?).await?;
            let want = match target {
                None => Comment { deleted_at: Some(cur.deleted_at.unwrap_or(tx.now)), ..cur.clone() },
                Some(v) => serde_json::from_value(v)?,
            };
            if cur == want {
                return Ok(None);
            }
            let saved = save_comment(tx, &want).await?;
            (Some(cur.task_id), Some(serde_json::to_value(&cur)?), Some(serde_json::to_value(&saved)?))
        }
        Entity::Dependency => {
            let (a, b) =
                entity_id.split_once(':').ok_or_else(|| Error::Corrupt(format!("dependency id {entity_id}")))?;
            let (blocker, blocked) = (parse_uuid(a)?, parse_uuid(b)?);
            let cur = load_dependency(&tx.conn, blocker, blocked).await?;
            let want: Option<Dependency> = target.map(serde_json::from_value).transpose()?;
            match (&cur, &want) {
                (Some(_), None) => remove_dependency_row(tx, cur.as_ref().unwrap()).await?,
                (None, Some(d)) => {
                    insert_dependency(tx, d).await?;
                }
                _ => return Ok(None),
            }
            (Some(blocked), cur.map(serde_json::to_value).transpose()?, want.map(serde_json::to_value).transpose()?)
        }
        Entity::Claim => {
            let task_id = parse_uuid(entity_id)?;
            let cur = any_claim(&tx.conn, task_id).await?;
            let want: Option<Claim> = target.map(serde_json::from_value).transpose()?;
            if cur == want {
                return Ok(None);
            }
            match &want {
                Some(c) => put_claim(tx, c).await?,
                None => {
                    tx.conn.execute("DELETE FROM claims WHERE task_id = ?1", [task_id.to_string()]).await?;
                }
            }
            (Some(task_id), cur.map(serde_json::to_value).transpose()?, want.map(serde_json::to_value).transpose()?)
        }
    };
    let kind = format!("{entity}.reverted");
    let id = tx
        .emit(EventIn {
            entity,
            entity_id: entity_id.to_string(),
            task_id,
            kind: &kind,
            before,
            after,
            undoes: Some(undoes),
            actor: None,
        })
        .await?;
    Ok(Some(id))
}

/// Reverts every entity touched by `events` to its state before the earliest of
/// them. An entity that someone else changed afterwards is a conflict: `strict`
/// fails the whole undo, otherwise it's skipped and reported.
async fn apply(tx: &Tx, events: Vec<Event>, strict: bool) -> Result<UndoReport> {
    let ids: HashSet<i64> = events.iter().map(|e| e.id).collect();
    let mut groups: BTreeMap<(String, String), Vec<Event>> = BTreeMap::new();
    for e in events {
        groups.entry((e.entity.as_str().to_string(), e.entity_id.clone())).or_default().push(e);
    }
    let mut groups: Vec<Vec<Event>> = groups.into_values().collect();
    groups.sort_by_key(|g| std::cmp::Reverse(g.iter().map(|e| e.id).min()));

    let mut report = UndoReport { batch_id: tx.ctx.batch_id, reverted: Vec::new(), skipped: Vec::new() };
    for group in groups {
        let first = group.iter().min_by_key(|e| e.id).expect("groups are non-empty");
        let mut rows = tx
            .conn
            .query(
                "SELECT id FROM events WHERE entity = ?1 AND entity_id = ?2 AND id > ?3 ORDER BY id",
                libsql::params![first.entity.as_str(), first.entity_id.clone(), first.id],
            )
            .await?;
        let mut conflict = None;
        while let Some(row) = rows.next().await? {
            let id: i64 = row.get(0)?;
            if !ids.contains(&id) {
                conflict = Some(id);
                break;
            }
        }
        if let Some(later_event) = conflict {
            if strict {
                return Err(Error::UndoConflict {
                    entity: format!("{} {}", first.entity, first.entity_id),
                    later_event,
                });
            }
            report.skipped.push(Skipped {
                entity: first.entity,
                entity_id: first.entity_id.clone(),
                task_id: first.task_id,
                later_event,
            });
            continue;
        }
        let event_id = restore(tx, first.entity, &first.entity_id, first.before.clone(), first.id).await?;
        report.reverted.push(Reverted { entity: first.entity, entity_id: first.entity_id.clone(), event_id });
    }
    Ok(report)
}

/// Whether every entity the event touched is inside the context's scope.
async fn event_in_scope(tx: &Tx, e: &Event) -> Result<bool> {
    let ctx = &tx.ctx;
    if !ctx.is_scoped() {
        return Ok(true);
    }
    let mut tasks: Vec<String> = e.task_id.map(|t| t.to_string()).into_iter().collect();
    match e.entity {
        Entity::Project => return Ok(ctx.in_scope(parse_uuid(&e.entity_id)?)),
        Entity::Dependency => tasks.extend(e.entity_id.split(':').map(str::to_string)),
        _ => {}
    }
    for t in &tasks {
        if !ctx.in_scope(load_task(&tx.conn, ctx.workspace_id, t, true).await?.project_id) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Scoped tokens can only undo work entirely inside their projects.
async fn check_scope(tx: &Tx, events: &[Event]) -> Result<()> {
    for e in events {
        if !event_in_scope(tx, e).await? {
            return Err(Error::Forbidden(format!("event {} touches a project outside this token's scope", e.id)));
        }
    }
    Ok(())
}

/// Refuses to undo events older than [`UNDO_WINDOW`], including those already
/// moved to `events_archive`.
async fn check_window(
    tx: &Tx,
    what: &str,
    events: &[Event],
    archive_where: &str,
    params: Vec<libsql::Value>,
) -> Result<()> {
    let cutoff = tx.now - UNDO_WINDOW;
    if events.iter().any(|e| e.at < cutoff) {
        return Err(Error::UndoWindowExpired(what.to_string()));
    }
    // Scoped like reads, so a scoped token can't learn of events outside its projects.
    let sql = format!("SELECT 1 FROM events_archive WHERE {archive_where} AND {} LIMIT 1", scope_sql(&tx.ctx));
    let mut rows = tx.conn.query(&sql, params).await?;
    if rows.next().await?.is_some() {
        return Err(Error::UndoWindowExpired(what.to_string()));
    }
    Ok(())
}

async fn query_events(tx: &Tx, where_clause: &str, params: Vec<libsql::Value>) -> Result<Vec<Event>> {
    let rows =
        tx.conn.query(&format!("SELECT {EVENT_COLS} FROM events WHERE {where_clause} ORDER BY id"), params).await?;
    collect_events(rows).await
}

impl Store {
    /// Undoes one event. Fails with `undo_conflict` if the entity changed since.
    pub async fn undo_event(&self, ctx: &Ctx, event_id: i64) -> Result<UndoReport> {
        self.write(ctx, async |tx| {
            let params = vec![tx.ws().into(), event_id.into()];
            let events = query_events(tx, "workspace_id = ?1 AND id = ?2", params.clone()).await?;
            if let Some(e) = events.first()
                && !event_in_scope(tx, e).await?
            {
                return Err(Error::NotFound(format!("event {event_id}")));
            }
            check_window(tx, &format!("event {event_id}"), &events, "workspace_id = ?1 AND id = ?2", params).await?;
            if events.is_empty() {
                return Err(Error::NotFound(format!("event {event_id}")));
            }
            apply(tx, events, true).await
        })
        .await
    }

    /// Undoes everything from one request or MCP call. All or nothing.
    pub async fn undo_batch(&self, ctx: &Ctx, batch_id: Uuid) -> Result<UndoReport> {
        self.write(ctx, async |tx| {
            let params: Vec<libsql::Value> = vec![tx.ws().into(), batch_id.to_string().into()];
            let events = query_events(tx, "workspace_id = ?1 AND batch_id = ?2", params.clone()).await?;
            check_window(tx, &format!("batch {batch_id}"), &events, "workspace_id = ?1 AND batch_id = ?2", params)
                .await?;
            if events.is_empty() {
                return Err(Error::NotFound(format!("batch {batch_id}")));
            }
            check_scope(tx, &events).await?;
            apply(tx, events, true).await
        })
        .await
    }

    /// Reverts everything `actor_id` did since `since`. Anything someone else has
    /// touched since is skipped and reported rather than overwritten. A `since`
    /// before the undo window is clamped to it: older events aren't reverted.
    pub async fn revert_actor(&self, ctx: &Ctx, actor_id: Uuid, since: OffsetDateTime) -> Result<UndoReport> {
        self.write(ctx, async |tx| {
            let since = since.max(tx.now - UNDO_WINDOW);
            let events = query_events(
                tx,
                "workspace_id = ?1 AND actor_id = ?2 AND at >= ?3",
                vec![tx.ws().into(), actor_id.to_string().into(), crate::db::fmt_ts(since).into()],
            )
            .await?;
            check_scope(tx, &events).await?;
            apply(tx, events, false).await
        })
        .await
    }
}
