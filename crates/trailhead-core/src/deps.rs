use libsql::Connection;
use uuid::Uuid;

use crate::db::{collect, fmt_ts};
use crate::error::{Error, Result};
use crate::model::{Dependency, Entity, Task};
use crate::store::{Ctx, Store, Tx};
use crate::tasks::{TASK_COLS, find_task, task_from_row};

pub(crate) fn dep_entity_id(blocker: Uuid, blocked: Uuid) -> String {
    format!("{blocker}:{blocked}")
}

pub(crate) async fn unfinished_blockers(conn: &Connection, task_id: Uuid) -> Result<Vec<Task>> {
    let cols = TASK_COLS.split(", ").map(|c| format!("b.{c}")).collect::<Vec<_>>().join(", ");
    let rows = conn
        .query(
            &format!(
                "SELECT {cols} FROM dependencies d JOIN tasks b ON b.id = d.blocker_id
                 WHERE d.blocked_id = ?1 AND b.deleted_at IS NULL AND b.status NOT IN ('done', 'cancelled')
                 ORDER BY b.key"
            ),
            [task_id.to_string()],
        )
        .await?;
    collect(rows, task_from_row).await
}

/// Placeholder for a related task outside the caller's scope, so a scoped agent
/// still learns that something blocks it without learning what.
pub(crate) const HIDDEN_KEY: &str = "(out of scope)";

pub(crate) fn visible_key(ctx: &Ctx, task: &Task) -> String {
    if ctx.in_scope(task.project_id) { task.key.clone() } else { HIDDEN_KEY.into() }
}

pub(crate) async fn insert_dependency(tx: &Tx, d: &Dependency) -> Result<bool> {
    let n = tx
        .conn
        .execute(
            "INSERT OR IGNORE INTO dependencies (workspace_id, blocker_id, blocked_id, created_at) VALUES (?1, ?2, ?3, ?4)",
            libsql::params![
                d.workspace_id.to_string(),
                d.blocker_id.to_string(),
                d.blocked_id.to_string(),
                fmt_ts(d.created_at)
            ],
        )
        .await?;
    Ok(n > 0)
}

pub(crate) async fn load_dependency(conn: &Connection, blocker: Uuid, blocked: Uuid) -> Result<Option<Dependency>> {
    let mut rows = conn
        .query(
            "SELECT workspace_id, created_at FROM dependencies WHERE blocker_id = ?1 AND blocked_id = ?2",
            [blocker.to_string(), blocked.to_string()],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(Dependency {
            workspace_id: crate::db::get_uuid(&row, 0)?,
            blocker_id: blocker,
            blocked_id: blocked,
            created_at: crate::db::get_ts(&row, 1)?,
        })),
        None => Ok(None),
    }
}

async fn would_cycle(conn: &Connection, blocker: Uuid, blocked: Uuid) -> Result<bool> {
    let mut rows = conn
        .query(
            "WITH RECURSIVE up(id) AS (
                 SELECT blocker_id FROM dependencies WHERE blocked_id = ?1
                 UNION SELECT d.blocker_id FROM dependencies d JOIN up ON d.blocked_id = up.id)
             SELECT 1 FROM up WHERE id = ?2 LIMIT 1",
            [blocker.to_string(), blocked.to_string()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

impl Store {
    /// `blocker` must finish (done or cancelled) before `blocked` is ready.
    /// Works across projects.
    pub async fn add_dependency(&self, ctx: &Ctx, blocker: &str, blocked: &str) -> Result<Dependency> {
        self.write(ctx, async |tx| {
            let blocker = find_task(&tx.conn, ctx, blocker, false).await?;
            let blocked = find_task(&tx.conn, ctx, blocked, false).await?;
            if blocker.id == blocked.id {
                return Err(Error::Invalid("a task can't block itself".into()));
            }
            if let Some(d) = load_dependency(&tx.conn, blocker.id, blocked.id).await? {
                return Ok(d);
            }
            if would_cycle(&tx.conn, blocker.id, blocked.id).await? {
                return Err(Error::DependencyCycle);
            }
            let dep = Dependency {
                workspace_id: ctx.workspace_id,
                blocker_id: blocker.id,
                blocked_id: blocked.id,
                created_at: tx.now,
            };
            insert_dependency(tx, &dep).await?;
            tx.emit_change(
                Entity::Dependency,
                dep_entity_id(blocker.id, blocked.id),
                Some(blocked.id),
                "dependency.added",
                None,
                Some(&dep),
            )
            .await?;
            Ok(dep)
        })
        .await
    }

    pub async fn remove_dependency(&self, ctx: &Ctx, blocker: &str, blocked: &str) -> Result<()> {
        self.write(ctx, async |tx| {
            let blocker = find_task(&tx.conn, ctx, blocker, true).await?;
            let blocked = find_task(&tx.conn, ctx, blocked, true).await?;
            let Some(dep) = load_dependency(&tx.conn, blocker.id, blocked.id).await? else {
                return Err(Error::NotFound(format!("dependency {} -> {}", blocker.key, blocked.key)));
            };
            remove_dependency_row(tx, &dep).await?;
            tx.emit_change(
                Entity::Dependency,
                dep_entity_id(blocker.id, blocked.id),
                Some(blocked.id),
                "dependency.removed",
                Some(&dep),
                None,
            )
            .await?;
            Ok(())
        })
        .await
    }

    pub async fn blockers(&self, ctx: &Ctx, task: &str) -> Result<Vec<Task>> {
        let conn = self.reader()?;
        let task = find_task(&conn, ctx, task, false).await?;
        let mut blockers = unfinished_blockers(&conn, task.id).await?;
        blockers.retain(|b| ctx.in_scope(b.project_id));
        Ok(blockers)
    }
}

pub(crate) async fn remove_dependency_row(tx: &Tx, dep: &Dependency) -> Result<()> {
    tx.conn
        .execute(
            "DELETE FROM dependencies WHERE blocker_id = ?1 AND blocked_id = ?2",
            [dep.blocker_id.to_string(), dep.blocked_id.to_string()],
        )
        .await?;
    Ok(())
}
