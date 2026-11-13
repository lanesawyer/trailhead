use std::collections::{HashMap, HashSet};

use libsql::Connection;
use uuid::Uuid;

use crate::db::{collect, fmt_ts, get_uuid, json_ids};
use crate::error::{Error, Result};
use crate::store::{Ctx, Store, Tx};
use crate::tasks::{TASK_COLS, find_task, task_from_row};
use trailhead_model::{Dependency, Entity, Task, TaskKey};

pub(crate) fn dep_entity_id(blocker: Uuid, blocked: Uuid) -> String {
    format!("{blocker}:{blocked}")
}

pub(crate) async fn unfinished_blockers(conn: &Connection, task_id: Uuid) -> Result<Vec<Task>> {
    let cols = TASK_COLS.split(", ").map(|c| format!("b.{c}")).collect::<Vec<_>>().join(", ");
    let rows = conn
        .query(
            &format!(
                "SELECT {cols} FROM dependencies d JOIN tasks b ON b.id = d.blocker_id
                 WHERE d.blocked_id = ?1 AND b.deleted_at IS NULL AND b.status NOT IN ('done', 'cancelled')"
            ),
            [task_id.to_string()],
        )
        .await?;
    let mut tasks = collect(rows, task_from_row).await?;
    tasks.sort_by(|a, b| trailhead_model::cmp_keys(&a.key, &b.key));
    Ok(tasks)
}

/// Placeholder for a related task outside the caller's scope, so a scoped agent
/// still learns that something blocks it without learning what.
pub(crate) const HIDDEN_KEY: &str = "(out of scope)";

pub(crate) fn visible_key(ctx: &Ctx, task: &Task) -> TaskKey {
    if ctx.in_scope(task.project_id) { task.key.clone() } else { TaskKey(HIDDEN_KEY.into()) }
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

/// Makes `blocker` block `blocked` unless it already does, refusing a task blocking itself or a cycle.
pub(crate) async fn link(tx: &Tx, blocker: &Task, blocked: &Task) -> Result<Dependency> {
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
        workspace_id: tx.ctx.workspace_id,
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
}

impl Store {
    /// `blocker` must finish (done or cancelled) before `blocked` is ready.
    /// Works across projects.
    pub async fn add_dependency(&self, ctx: &Ctx, blocker: &str, blocked: &str) -> Result<Dependency> {
        self.write(ctx, async |tx| {
            let blocker = find_task(&tx.conn, ctx, blocker, false).await?;
            let blocked = find_task(&tx.conn, ctx, blocked, false).await?;
            link(tx, &blocker, &blocked).await
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

    /// The ids among `task_ids` that have a blocker which isn't done or cancelled, in one query.
    pub async fn with_unfinished_blockers(&self, task_ids: &[Uuid]) -> Result<HashSet<Uuid>> {
        let conn = self.reader()?;
        let rows = conn
            .query(
                "SELECT DISTINCT d.blocked_id FROM dependencies d JOIN tasks b ON b.id = d.blocker_id
                 WHERE b.deleted_at IS NULL AND b.status NOT IN ('done', 'cancelled')
                   AND d.blocked_id IN (SELECT value FROM json_each(?1))",
                [json_ids(task_ids)?],
            )
            .await?;
        Ok(collect(rows, |row| get_uuid(row, 0)).await?.into_iter().collect())
    }

    /// Every blocker of these tasks, finished or not, keyed by the blocked task's id; tasks without one are absent.
    /// Blockers outside the caller's scope are left out of the ids and the unfinished ones counted in `hidden`, as
    /// `ready` counts them. Deleted blockers are left out of both.
    pub async fn blockers_of(&self, ctx: &Ctx, task_ids: &[Uuid]) -> Result<BlockersOf> {
        let conn = self.reader()?;
        let rows = conn
            .query(
                "SELECT d.blocked_id, d.blocker_id, b.project_id, b.status NOT IN ('done', 'cancelled'), b.key
                 FROM dependencies d JOIN tasks b ON b.id = d.blocker_id
                 WHERE b.deleted_at IS NULL AND d.blocked_id IN (SELECT value FROM json_each(?1))",
                [json_ids(task_ids)?],
            )
            .await?;
        let mut edges = collect(rows, |row| {
            Ok((
                get_uuid(row, 0)?,
                get_uuid(row, 1)?,
                get_uuid(row, 2)?,
                row.get::<i64>(3)? != 0,
                row.get::<String>(4)?,
            ))
        })
        .await?;
        edges.sort_by(|a, b| trailhead_model::cmp_keys(&a.4, &b.4));
        let mut out = BlockersOf::default();
        for (blocked, blocker, project, unfinished, _) in edges {
            if ctx.in_scope(project) {
                out.visible.entry(blocked).or_default().push(blocker);
            } else if unfinished {
                *out.hidden.entry(blocked).or_default() += 1;
            }
        }
        Ok(out)
    }
}

/// What [`Store::blockers_of`] finds.
#[derive(Debug, Default)]
pub struct BlockersOf {
    /// The blockers in the caller's scope, by blocked task.
    pub visible: HashMap<Uuid, Vec<Uuid>>,
    /// How many unfinished blockers outside it each task has.
    pub hidden: HashMap<Uuid, usize>,
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
