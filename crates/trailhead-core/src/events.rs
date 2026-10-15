use libsql::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::{get_enum, get_opt_json, get_opt_uuid, get_ts, get_uuid};
use crate::error::Result;
use crate::model::Event;
use crate::store::{Ctx, Store};
use crate::tasks::load_task;

pub(crate) const EVENT_COLS: &str =
    "id, workspace_id, batch_id, actor_id, on_behalf_of, task_id, entity, entity_id, kind, before, after, undoes, at";

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
    })
}

pub(crate) async fn collect_events(mut rows: libsql::Rows) -> Result<Vec<Event>> {
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(event_from_row(&row)?);
    }
    Ok(out)
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
}

impl Store {
    /// Events in id order.
    pub async fn list_events(&self, ctx: &Ctx, filter: EventFilter) -> Result<Vec<Event>> {
        let conn = self.reader()?;
        let task_id = match &filter.task {
            Some(t) => Some(load_task(&conn, ctx.workspace_id, t, true).await?.id.to_string()),
            None => None,
        };
        let rows = conn
            .query(
                &format!(
                    "SELECT {EVENT_COLS} FROM events WHERE workspace_id = ?1
                     AND (?2 IS NULL OR id > ?2) AND (?3 IS NULL OR task_id = ?3)
                     AND (?4 IS NULL OR actor_id = ?4) AND (?5 IS NULL OR batch_id = ?5)
                     AND (?6 IS NULL OR at >= ?6)
                     ORDER BY id LIMIT ?7"
                ),
                libsql::params![
                    ctx.workspace_id.to_string(),
                    filter.after_id,
                    task_id,
                    filter.actor_id.map(|a| a.to_string()),
                    filter.batch_id.map(|b| b.to_string()),
                    filter.since.map(crate::db::fmt_ts),
                    filter.limit.unwrap_or(100),
                ],
            )
            .await?;
        collect_events(rows).await
    }
}
