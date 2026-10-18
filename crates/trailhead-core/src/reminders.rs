use libsql::Connection;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::db::{collect, fmt_ts, get_opt_ts, get_uuid};
use crate::error::{Error, Result};
use crate::events::{EVENT_COLS, event_from_row};
use crate::model::{Entity, Event, Task};
use crate::store::{Ctx, EventIn, Store};
use crate::tasks::{TASK_COLS, task_from_row};

/// How long before a deadline a `task.due_soon` reminder fires.
pub const DEFAULT_REMINDER_WINDOW: Duration = Duration::hours(24);

pub const DUE_SOON: &str = "task.due_soon";
pub const OVERDUE: &str = "task.overdue";

/// Notifications about a task, not changes to it. Undo leaves them alone.
pub(crate) fn is_notification(kind: &str) -> bool {
    kind == DUE_SOON || kind == OVERDUE
}

/// SQL condition on `events.kind` that excludes notifications.
pub(crate) const NOT_NOTIFICATION_SQL: &str = "kind NOT IN ('task.due_soon', 'task.overdue')";

/// Open tasks whose reminder is due and hasn't fired for their current deadline,
/// with that deadline and the reminder kind. Only `task.overdue` fires once the due date has passed,
/// so a task first seen late doesn't get both.
async fn pending(
    conn: &Connection,
    ws: Uuid,
    now: OffsetDateTime,
    window: Duration,
) -> Result<Vec<(Task, OffsetDateTime, String)>> {
    let rows = conn
        .query(
            &format!(
                "SELECT {TASK_COLS}, due_by, kind FROM (
                     SELECT t.*, CASE WHEN t.due_by <= ?2 THEN '{OVERDUE}' ELSE '{DUE_SOON}' END AS kind
                     FROM tasks t
                     WHERE t.workspace_id = ?1 AND t.deleted_at IS NULL AND t.due_by IS NOT NULL
                       AND t.due_by <= ?3 AND t.status NOT IN ('done', 'cancelled')
                       AND t.project_id IN (SELECT id FROM projects WHERE deleted_at IS NULL AND status = 'active')
                 ) c
                 WHERE NOT EXISTS (
                     SELECT 1 FROM reminders_sent r WHERE r.task_id = c.id AND r.kind = c.kind AND r.due_at = c.due_by
                 )
                 ORDER BY due_by, id"
            ),
            libsql::params![ws.to_string(), fmt_ts(now), fmt_ts(now + window)],
        )
        .await?;
    let due_by = TASK_COLS.split(", ").count() as i32;
    collect(rows, |row| {
        let at = get_opt_ts(row, due_by)?.expect("filtered on due_by");
        Ok((task_from_row(row)?, at, row.get::<String>(due_by + 1)?))
    })
    .await
}

/// The workspace's first user, who reminders are attributed to.
async fn owner(conn: &Connection, ws: Uuid) -> Result<Uuid> {
    let mut rows = conn
        .query("SELECT id FROM users WHERE workspace_id = ?1 ORDER BY created_at, id LIMIT 1", [ws.to_string()])
        .await?;
    match rows.next().await? {
        Some(row) => get_uuid(&row, 0),
        None => Err(Error::NotFound(format!("owner of workspace {ws}"))),
    }
}

impl Store {
    /// Emits `task.due_soon` once a task is within `window` of its due date and
    /// `task.overdue` once the due date passes, each at most once per due date.
    /// Done, cancelled and deleted tasks are skipped. Returns the new events, all
    /// in `ctx`'s batch and attributed to the workspace owner.
    pub async fn emit_due_reminders(&self, ctx: &Ctx, window: Duration) -> Result<Vec<Event>> {
        if pending(&*self.reader()?, ctx.workspace_id, self.now(), window).await?.is_empty() {
            return Ok(Vec::new());
        }
        self.write(ctx, async |tx| {
            let due = pending(&tx.conn, ctx.workspace_id, tx.now, window).await?;
            if due.is_empty() {
                return Ok(Vec::new());
            }
            let owner = owner(&tx.conn, ctx.workspace_id).await?;
            let mut ids = Vec::with_capacity(due.len());
            for (task, due_by, kind) in &due {
                let snapshot = serde_json::to_value(task)?;
                let id = tx
                    .emit(EventIn {
                        entity: Entity::Task,
                        entity_id: task.id.to_string(),
                        task_id: Some(task.id),
                        kind,
                        before: Some(snapshot.clone()),
                        after: Some(snapshot),
                        undoes: None,
                        actor: Some((owner, owner)),
                    })
                    .await?;
                tx.conn
                    .execute(
                        "INSERT INTO reminders_sent (task_id, kind, due_at, event_id, sent_at) VALUES (?1, ?2, ?3, ?4, ?5)
                         ON CONFLICT (task_id, kind) DO UPDATE SET due_at = excluded.due_at,
                             event_id = excluded.event_id, sent_at = excluded.sent_at",
                        libsql::params![task.id.to_string(), kind.as_str(), fmt_ts(*due_by), id, tx.now_str()],
                    )
                    .await?;
                ids.push(id);
            }
            let rows = tx
                .conn
                .query(
                    &format!("SELECT {EVENT_COLS} FROM events WHERE batch_id = ?1 AND id >= ?2 ORDER BY id"),
                    libsql::params![ctx.batch_id.to_string(), ids[0]],
                )
                .await?;
            collect(rows, event_from_row).await
        })
        .await
    }
}
