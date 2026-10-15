use libsql::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::{fmt_ts, get_enum, get_opt_json, get_opt_uuid, get_ts, get_uuid};
use crate::error::Result;
use crate::model::Event;
use crate::store::{Ctx, Store};
use crate::tasks::find_task;

/// Undo and revert reach back this far. Older events move to `events_archive`.
pub const UNDO_WINDOW: time::Duration = time::Duration::days(90);

/// Events moved per transaction by [`Store::archive_events`].
const ARCHIVE_BATCH: i64 = 500;
/// Transactions per [`Store::archive_events`] call, so one call's work stays bounded.
const ARCHIVE_MAX_BATCHES: usize = 20;

const RAW_COLS: &str =
    "id, workspace_id, batch_id, actor_id, on_behalf_of, task_id, entity, entity_id, kind, before, after, undoes, at";

pub(crate) const EVENT_COLS: &str =
    "id, workspace_id, batch_id, actor_id, on_behalf_of, task_id, entity, entity_id, kind, before, after, undoes, at,
     (SELECT key FROM tasks WHERE tasks.id = events.task_id)";

pub(crate) fn event_from_row(row: &Row) -> Result<Event> {
    Ok(Event {
        id: row.get(0)?,
        workspace_id: get_uuid(row, 1)?,
        batch_id: get_uuid(row, 2)?,
        actor_id: get_uuid(row, 3)?,
        on_behalf_of: get_uuid(row, 4)?,
        task_id: get_opt_uuid(row, 5)?,
        entity: get_enum(row, 6)?,
        entity_id: row.get(7)?,
        kind: row.get(8)?,
        before: get_opt_json(row, 9)?,
        after: get_opt_json(row, 10)?,
        undoes: row.get(11)?,
        at: get_ts(row, 12)?,
        task_key: row.get(13)?,
    })
}

pub(crate) async fn collect_events(mut rows: libsql::Rows) -> Result<Vec<Event>> {
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(event_from_row(&row)?);
    }
    Ok(out)
}

/// Events a scoped context may see: those on its projects' tasks, and on the
/// projects themselves.
pub(crate) fn scope_sql(ctx: &Ctx) -> String {
    if !ctx.is_scoped() {
        return "1".into();
    }
    format!(
        "(task_id IN (SELECT id FROM tasks WHERE {}) OR (entity = 'project' AND {}))",
        ctx.scope_sql("project_id"),
        ctx.scope_sql("entity_id")
    )
}

#[derive(Debug, Clone, Default)]
pub struct EventFilter {
    /// Cursor: only events with a greater id. This is how SSE and webhook
    /// delivery resume after a restart.
    pub after_id: Option<i64>,
    pub task: Option<String>,
    pub actor_id: Option<Uuid>,
    pub batch_id: Option<Uuid>,
    pub since: Option<OffsetDateTime>,
    pub limit: Option<i64>,
    pub newest_first: bool,
    /// Also read `events_archive`. Ids are kept on archiving, so order is unchanged.
    pub include_archived: bool,
    /// Leave out notifications such as `task.due_soon`, which change nothing.
    pub changes_only: bool,
}

impl Store {
    /// The id of the newest event, so a stream can start from "now".
    pub async fn latest_event_id(&self, ctx: &Ctx) -> Result<i64> {
        let conn = self.reader()?;
        let mut rows = conn
            .query("SELECT coalesce(max(id), 0) FROM events WHERE workspace_id = ?1", [ctx.workspace_id.to_string()])
            .await?;
        Ok(rows.next().await?.map(|r| r.get(0)).transpose()?.unwrap_or(0))
    }

    /// Events in id order, oldest first unless `newest_first`.
    pub async fn list_events(&self, ctx: &Ctx, filter: EventFilter) -> Result<Vec<Event>> {
        let conn = self.reader()?;
        let task_id = match &filter.task {
            Some(t) => Some(find_task(&conn, ctx, t, true).await?.id.to_string()),
            None => None,
        };
        let rows = conn
            .query(
                &format!(
                    "SELECT {EVENT_COLS} FROM {} WHERE workspace_id = ?1
                     AND (?2 IS NULL OR id > ?2) AND (?3 IS NULL OR task_id = ?3)
                     AND (?4 IS NULL OR actor_id = ?4) AND (?5 IS NULL OR batch_id = ?5)
                     AND (?6 IS NULL OR at >= ?6) AND {} AND (?8 = 0 OR {})
                     ORDER BY id {} LIMIT ?7",
                    if filter.include_archived {
                        "(SELECT * FROM events UNION ALL SELECT * FROM events_archive) AS events"
                    } else {
                        "events"
                    },
                    scope_sql(ctx),
                    crate::reminders::NOT_NOTIFICATION_SQL,
                    if filter.newest_first { "DESC" } else { "ASC" }
                ),
                libsql::params![
                    ctx.workspace_id.to_string(),
                    filter.after_id,
                    task_id,
                    filter.actor_id.map(|a| a.to_string()),
                    filter.batch_id.map(|b| b.to_string()),
                    filter.since.map(crate::db::fmt_ts),
                    filter.limit.unwrap_or(100),
                    filter.changes_only,
                ],
            )
            .await?;
        collect_events(rows).await
    }

    /// Moves events older than [`UNDO_WINDOW`] to `events_archive`, in every
    /// workspace, keeping their ids. Only a prefix of the log by id moves, so an
    /// event still in `events` never has a later event in the archive, which keeps
    /// undo's conflict check reading `events` alone. Each batch is its own
    /// transaction and a call does a bounded amount of work; it returns how many
    /// events moved, so call again while that equals the bound.
    pub async fn archive_events(&self) -> Result<usize> {
        let mut moved = 0;
        for _ in 0..ARCHIVE_MAX_BATCHES {
            let cutoff = fmt_ts(self.now() - UNDO_WINDOW);
            let n = self
                .maintain(async |conn| {
                    let mut rows = conn
                        .query("SELECT id, at FROM events ORDER BY id LIMIT ?1", libsql::params![ARCHIVE_BATCH])
                        .await?;
                    let mut last = None;
                    while let Some(row) = rows.next().await? {
                        let at: String = row.get(1)?;
                        if at >= cutoff {
                            break;
                        }
                        last = Some(row.get::<i64>(0)?);
                    }
                    drop(rows);
                    let Some(last) = last else { return Ok(0) };
                    let n = conn
                        .execute(
                            &format!(
                                "INSERT INTO events_archive ({RAW_COLS}) SELECT {RAW_COLS} FROM events WHERE id <= ?1"
                            ),
                            [last],
                        )
                        .await?;
                    conn.execute("DELETE FROM events WHERE id <= ?1", [last]).await?;
                    Ok(n as usize)
                })
                .await?;
            moved += n;
            if n < ARCHIVE_BATCH as usize {
                break;
            }
        }
        Ok(moved)
    }
}
