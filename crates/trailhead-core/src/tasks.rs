use libsql::{Connection, Row};
use serde::Serialize;
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::claims::{drop_claim, live_claim, reap_expired};
use crate::comments::list_comments_for;
use crate::db::{fmt_ts, get_enum, get_json, get_opt_ts, get_opt_uuid, get_ts, get_uuid, opt_ts_value, opt_uuid_value};
use crate::deps::unfinished_blockers;
use crate::error::{Error, Result};
use crate::metadata::{merge_patch, validate_path};
use crate::model::{Claim, Comment, Entity, Status, Task};
use crate::projects::load_project;
use crate::store::{Ctx, Store, Tx};

pub(crate) const TASK_COLS: &str = "id, workspace_id, project_id, key, title, description, status, priority, due_at, due_tz, parent_id, metadata, version, source, external_id, created_by, created_at, updated_at, deleted_at";

fn prefixed_cols(prefix: &str) -> String {
    TASK_COLS.split(", ").map(|c| format!("{prefix}.{c}")).collect::<Vec<_>>().join(", ")
}

pub(crate) fn task_from_row(row: &Row) -> Result<Task> {
    Ok(Task {
        id: get_uuid(row, 0)?,
        workspace_id: get_uuid(row, 1)?,
        project_id: get_uuid(row, 2)?,
        key: row.get(3)?,
        title: row.get(4)?,
        description: row.get(5)?,
        status: get_enum(row, 6)?,
        priority: row.get(7)?,
        due_at: get_opt_ts(row, 8)?,
        due_tz: row.get(9)?,
        parent_id: get_opt_uuid(row, 10)?,
        metadata: get_json(row, 11)?,
        version: row.get(12)?,
        source: row.get(13)?,
        external_id: row.get(14)?,
        created_by: get_uuid(row, 15)?,
        created_at: get_ts(row, 16)?,
        updated_at: get_ts(row, 17)?,
        deleted_at: get_opt_ts(row, 18)?,
    })
}

pub(crate) async fn collect_tasks(mut rows: libsql::Rows) -> Result<Vec<Task>> {
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(task_from_row(&row)?);
    }
    Ok(out)
}

/// Resolves a task by UUID or key (`AUTH-42`).
pub(crate) async fn load_task(conn: &Connection, ws: Uuid, r: &str, include_deleted: bool) -> Result<Task> {
    let sql = format!(
        "SELECT {TASK_COLS} FROM tasks WHERE workspace_id = ?1 AND (id = ?2 OR key = upper(?2)) {}",
        if include_deleted { "" } else { "AND deleted_at IS NULL" }
    );
    let mut rows = conn.query(&sql, [ws.to_string(), r.to_string()]).await?;
    match rows.next().await? {
        Some(row) => task_from_row(&row),
        None => Err(Error::NotFound(format!("task {r}"))),
    }
}

/// Compare-and-swap on `version`. Used by updates and undo alike.
pub(crate) async fn save_task(tx: &Tx, before: &Task, after: Task) -> Result<Task> {
    let mut rows = tx
        .conn
        .query(
            &format!(
                "UPDATE tasks SET title = ?1, description = ?2, status = ?3, priority = ?4, due_at = ?5, due_tz = ?6,
                 parent_id = ?7, metadata = ?8, deleted_at = ?9, version = version + 1, updated_at = ?10
                 WHERE id = ?11 AND version = ?12 RETURNING {TASK_COLS}"
            ),
            libsql::params![
                after.title,
                after.description,
                after.status.as_str(),
                after.priority,
                opt_ts_value(after.due_at),
                after.due_tz,
                opt_uuid_value(after.parent_id),
                after.metadata.to_string(),
                opt_ts_value(after.deleted_at),
                tx.now_str(),
                before.id.to_string(),
                before.version,
            ],
        )
        .await?;
    match rows.next().await? {
        Some(row) => task_from_row(&row),
        None => {
            let current = load_task(&tx.conn, before.workspace_id, &before.id.to_string(), true).await?;
            Err(Error::VersionConflict { expected: before.version, current: current.version })
        }
    }
}

pub(crate) async fn update_status(tx: &Tx, before: &Task, status: Status, kind: &str) -> Result<Task> {
    let after = save_task(tx, before, Task { status, ..before.clone() }).await?;
    tx.emit_change(Entity::Task, after.id.to_string(), Some(after.id), kind, Some(before), Some(&after)).await?;
    Ok(after)
}

fn validate_priority(p: i64) -> Result<()> {
    if (1..=4).contains(&p) { Ok(()) } else { Err(Error::Invalid("priority must be 1 (urgent) to 4 (low)".into())) }
}

fn validate_due(due_at: Option<OffsetDateTime>, due_tz: &Option<String>) -> Result<()> {
    if due_tz.is_some() && due_at.is_none() {
        return Err(Error::Invalid("due_tz needs due_at".into()));
    }
    Ok(())
}

/// Agents may only finish work through review when the project requires it, and
/// only a human can approve.
async fn check_review(tx: &Tx, task: &Task, to: Status) -> Result<()> {
    if to != Status::Done || tx.ctx.is_human() {
        return Ok(());
    }
    if task.status == Status::InReview {
        return Err(Error::ReviewRequired);
    }
    let project = load_project(&tx.conn, tx.ctx.workspace_id, &task.project_id.to_string(), true).await?;
    if project.require_review { Err(Error::ReviewRequired) } else { Ok(()) }
}

/// Agents can't touch a task another actor holds. Humans can, so they can always
/// rescue a task from a stuck agent.
pub(crate) async fn check_claim_access(tx: &Tx, task: &Task) -> Result<Option<Claim>> {
    let claim = live_claim(&tx.conn, task.id, tx.now).await?;
    if let Some(c) = &claim
        && c.holder_id != tx.ctx.actor_id()
        && !tx.ctx.is_human()
    {
        return Err(Error::ClaimHeld { holder: c.holder_id.to_string(), expires_at: fmt_ts(c.expires_at) });
    }
    Ok(claim)
}

#[derive(Debug, Clone, Default)]
pub struct NewTask {
    /// Project key or id.
    pub project: String,
    pub title: String,
    pub description: String,
    pub priority: Option<i64>,
    pub due_at: Option<OffsetDateTime>,
    pub due_tz: Option<String>,
    /// Parent task key or id.
    pub parent: Option<String>,
    pub metadata: Option<Value>,
    pub source: Option<String>,
    pub external_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TaskPatch {
    pub title: Option<String>,
    pub description: Option<String>,
    pub status: Option<Status>,
    pub priority: Option<i64>,
    pub due_at: Option<Option<OffsetDateTime>>,
    pub due_tz: Option<Option<String>>,
    pub parent: Option<Option<String>>,
    /// RFC 7386 merge patch: `null` removes a key.
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Default)]
pub struct TaskFilter {
    pub project: Option<String>,
    /// Empty means every status that isn't done or cancelled.
    pub statuses: Vec<Status>,
    pub parent: Option<String>,
    pub due_before: Option<OffsetDateTime>,
    /// `(path, value)` pairs matched against `metadata` as text, e.g. `("team", "web")`.
    pub metadata: Vec<(String, String)>,
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskDetail {
    pub task: Task,
    pub claim: Option<Claim>,
    /// Keys of unfinished tasks blocking this one.
    pub blocked_by: Vec<String>,
    /// Keys of tasks this one blocks.
    pub blocks: Vec<String>,
    pub comments: Vec<Comment>,
}

impl Store {
    pub async fn create_task(&self, ctx: &Ctx, new: NewTask) -> Result<Task> {
        let title = new.title.trim().to_string();
        if title.is_empty() {
            return Err(Error::Invalid("title is empty".into()));
        }
        let priority = new.priority.unwrap_or(3);
        validate_priority(priority)?;
        validate_due(new.due_at, &new.due_tz)?;
        let metadata = new.metadata.unwrap_or_else(|| Value::Object(Default::default()));
        if !metadata.is_object() {
            return Err(Error::Invalid("metadata must be a JSON object".into()));
        }
        if new.source.is_some() != new.external_id.is_some() {
            return Err(Error::Invalid("source and external_id go together".into()));
        }
        self.write(ctx, async |tx| {
            let ws = ctx.workspace_id;
            let project = load_project(&tx.conn, ws, &new.project, false).await?;
            if let (Some(source), Some(ext)) = (&new.source, &new.external_id) {
                let mut rows = tx
                    .conn
                    .query(
                        &format!("SELECT {TASK_COLS} FROM tasks WHERE workspace_id = ?1 AND source = ?2 AND external_id = ?3"),
                        [ws.to_string(), source.clone(), ext.clone()],
                    )
                    .await?;
                if let Some(row) = rows.next().await? {
                    return task_from_row(&row);
                }
            }
            let parent_id = match &new.parent {
                Some(p) => Some(load_task(&tx.conn, ws, p, false).await?.id),
                None => None,
            };
            let mut rows = tx
                .conn
                .query(
                    "UPDATE projects SET next_task_seq = next_task_seq + 1 WHERE id = ?1 RETURNING next_task_seq - 1",
                    [project.id.to_string()],
                )
                .await?;
            let seq: i64 = rows.next().await?.expect("project exists").get(0)?;
            let id = Uuid::now_v7();
            let mut rows = tx
                .conn
                .query(
                    &format!(
                        "INSERT INTO tasks (id, workspace_id, project_id, key, title, description, priority, due_at, due_tz,
                         parent_id, metadata, source, external_id, created_by, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?15) RETURNING {TASK_COLS}"
                    ),
                    libsql::params![
                        id.to_string(),
                        ws.to_string(),
                        project.id.to_string(),
                        format!("{}-{seq}", project.key),
                        title.clone(),
                        new.description.clone(),
                        priority,
                        opt_ts_value(new.due_at),
                        new.due_tz.clone(),
                        opt_uuid_value(parent_id),
                        metadata.to_string(),
                        new.source.clone(),
                        new.external_id.clone(),
                        ctx.actor_id().to_string(),
                        tx.now_str(),
                    ],
                )
                .await?;
            let task = task_from_row(&rows.next().await?.expect("RETURNING yields a row"))?;
            tx.emit_change(Entity::Task, id.to_string(), Some(id), "task.created", None, Some(&task))
                .await?;
            Ok(task)
        })
        .await
    }

    pub async fn update_task(
        &self,
        ctx: &Ctx,
        task: &str,
        patch: TaskPatch,
        expected_version: Option<i64>,
    ) -> Result<Task> {
        self.write(ctx, async |tx| {
            let ws = ctx.workspace_id;
            let before = load_task(&tx.conn, ws, task, false).await?;
            if let Some(v) = expected_version
                && v != before.version
            {
                return Err(Error::VersionConflict { expected: v, current: before.version });
            }
            let claim = check_claim_access(tx, &before).await?;
            let mut after = before.clone();
            if let Some(title) = patch.title {
                if title.trim().is_empty() {
                    return Err(Error::Invalid("title is empty".into()));
                }
                after.title = title.trim().into();
            }
            if let Some(d) = patch.description {
                after.description = d;
            }
            if let Some(p) = patch.priority {
                validate_priority(p)?;
                after.priority = p;
            }
            if let Some(d) = patch.due_at {
                after.due_at = d;
                if d.is_none() {
                    after.due_tz = None;
                }
            }
            if let Some(tz) = patch.due_tz {
                after.due_tz = tz;
            }
            validate_due(after.due_at, &after.due_tz)?;
            if let Some(parent) = &patch.parent {
                after.parent_id = match parent {
                    Some(p) => {
                        let p = load_task(&tx.conn, ws, p, false).await?;
                        if p.id == before.id {
                            return Err(Error::Invalid("a task can't be its own parent".into()));
                        }
                        Some(p.id)
                    }
                    None => None,
                };
            }
            if let Some(m) = &patch.metadata {
                after.metadata = merge_patch(&before.metadata, m)?;
            }
            if let Some(to) = patch.status {
                if !before.status.can_transition_to(to) {
                    return Err(Error::InvalidTransition { from: before.status, to });
                }
                check_review(tx, &before, to).await?;
                after.status = to;
            }
            if after == before {
                return Ok(before);
            }
            let keeps_claim = matches!(after.status, Status::InProgress | Status::NeedsInput);
            if let Some(c) = claim
                && !keeps_claim
            {
                drop_claim(tx, &c, "claim.released").await?;
            }
            let after = save_task(tx, &before, after).await?;
            tx.emit_change(
                Entity::Task,
                after.id.to_string(),
                Some(after.id),
                "task.updated",
                Some(&before),
                Some(&after),
            )
            .await?;
            Ok(after)
        })
        .await
    }

    /// Soft delete. Undo brings it back.
    pub async fn delete_task(&self, ctx: &Ctx, task: &str, expected_version: Option<i64>) -> Result<Task> {
        self.write(ctx, async |tx| {
            let before = load_task(&tx.conn, ctx.workspace_id, task, false).await?;
            if let Some(v) = expected_version
                && v != before.version
            {
                return Err(Error::VersionConflict { expected: v, current: before.version });
            }
            if let Some(c) = check_claim_access(tx, &before).await? {
                drop_claim(tx, &c, "claim.released").await?;
            }
            let after = save_task(tx, &before, Task { deleted_at: Some(tx.now), ..before.clone() }).await?;
            tx.emit_change(
                Entity::Task,
                after.id.to_string(),
                Some(after.id),
                "task.deleted",
                Some(&before),
                Some(&after),
            )
            .await?;
            Ok(after)
        })
        .await
    }

    pub async fn get_task(&self, ctx: &Ctx, task: &str) -> Result<Task> {
        let conn = self.reader()?;
        load_task(&conn, ctx.workspace_id, task, false).await
    }

    pub async fn task_detail(&self, ctx: &Ctx, task: &str) -> Result<TaskDetail> {
        let conn = self.reader()?;
        let task = load_task(&conn, ctx.workspace_id, task, false).await?;
        let claim = live_claim(&conn, task.id, self.now()).await?;
        let blocked_by = unfinished_blockers(&conn, task.id).await?.into_iter().map(|t| t.key).collect();
        let mut rows = conn
            .query(
                "SELECT t.key FROM dependencies d JOIN tasks t ON t.id = d.blocked_id
                 WHERE d.blocker_id = ?1 AND t.deleted_at IS NULL ORDER BY t.key",
                [task.id.to_string()],
            )
            .await?;
        let mut blocks = Vec::new();
        while let Some(row) = rows.next().await? {
            blocks.push(row.get(0)?);
        }
        let comments = list_comments_for(&conn, task.id).await?;
        Ok(TaskDetail { task, claim, blocked_by, blocks, comments })
    }

    pub async fn list_tasks(&self, ctx: &Ctx, filter: TaskFilter) -> Result<Vec<Task>> {
        let conn = self.reader()?;
        let mut sql = format!("SELECT {TASK_COLS} FROM tasks WHERE workspace_id = ?1 AND deleted_at IS NULL");
        let mut params: Vec<libsql::Value> = vec![ctx.workspace_id.to_string().into()];
        let mut push = |sql: &mut String, clause: &str, v: libsql::Value| {
            params.push(v);
            sql.push_str(&clause.replace('?', &format!("?{}", params.len())));
        };
        if let Some(p) = &filter.project {
            let project = load_project(&conn, ctx.workspace_id, p, false).await?;
            push(&mut sql, " AND project_id = ?", project.id.to_string().into());
        }
        if let Some(p) = &filter.parent {
            let parent = load_task(&conn, ctx.workspace_id, p, false).await?;
            push(&mut sql, " AND parent_id = ?", parent.id.to_string().into());
        }
        if let Some(d) = filter.due_before {
            push(&mut sql, " AND due_at IS NOT NULL AND due_at < ?", fmt_ts(d).into());
        }
        for (path, value) in &filter.metadata {
            validate_path(path)?;
            push(&mut sql, &format!(" AND CAST(json_extract(metadata, '$.{path}') AS TEXT) = ?"), value.clone().into());
        }
        if filter.statuses.is_empty() {
            sql.push_str(" AND status NOT IN ('done', 'cancelled')");
        } else {
            let list = filter.statuses.iter().map(|s| format!("'{}'", s.as_str())).collect::<Vec<_>>().join(", ");
            sql.push_str(&format!(" AND status IN ({list})"));
        }
        push(
            &mut sql,
            " ORDER BY priority, due_at IS NULL, due_at, created_at LIMIT ?",
            filter.limit.unwrap_or(100).into(),
        );
        collect_tasks(conn.query(&sql, params).await?).await
    }

    /// Open, unclaimed tasks with no unfinished blockers, by priority then due date.
    /// Expired claims are reaped first so their tasks come back.
    pub async fn ready(&self, ctx: &Ctx, project: Option<&str>, limit: Option<i64>) -> Result<Vec<Task>> {
        let conn = self.reader()?;
        self.write(ctx, async |tx| reap_expired(tx, None).await).await?;
        let project_id = match project {
            Some(p) => Some(load_project(&conn, ctx.workspace_id, p, false).await?.id),
            None => None,
        };
        let sql = format!(
            "SELECT {} FROM tasks t JOIN projects p ON p.id = t.project_id
             WHERE t.workspace_id = ?1 AND t.deleted_at IS NULL AND t.status = 'open'
               AND p.deleted_at IS NULL AND p.status = 'active'
               AND (?2 IS NULL OR t.project_id = ?2)
               AND NOT EXISTS (SELECT 1 FROM claims c WHERE c.task_id = t.id AND c.expires_at > ?3)
               AND NOT EXISTS (
                   SELECT 1 FROM dependencies d JOIN tasks b ON b.id = d.blocker_id
                   WHERE d.blocked_id = t.id AND b.deleted_at IS NULL AND b.status NOT IN ('done', 'cancelled'))
             ORDER BY t.priority, t.due_at IS NULL, t.due_at, t.created_at
             LIMIT ?4",
            prefixed_cols("t")
        );
        let rows = conn
            .query(
                &sql,
                libsql::params![
                    ctx.workspace_id.to_string(),
                    opt_uuid_value(project_id),
                    fmt_ts(self.now()),
                    limit.unwrap_or(20)
                ],
            )
            .await?;
        collect_tasks(rows).await
    }

    /// Full-text search over titles, descriptions and comments.
    pub async fn search(&self, ctx: &Ctx, query: &str, limit: Option<i64>) -> Result<Vec<Task>> {
        let conn = self.reader()?;
        let q = fts_query(query);
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT {TASK_COLS} FROM tasks t WHERE t.workspace_id = ?1 AND t.deleted_at IS NULL AND (
                 t.rowid IN (SELECT rowid FROM tasks_fts WHERE tasks_fts MATCH ?2)
                 OR t.id IN (SELECT c.task_id FROM comments c WHERE c.deleted_at IS NULL
                             AND c.rowid IN (SELECT rowid FROM comments_fts WHERE comments_fts MATCH ?2)))
             ORDER BY t.status IN ('done', 'cancelled'), t.updated_at DESC LIMIT ?3"
        );
        let rows = conn.query(&sql, libsql::params![ctx.workspace_id.to_string(), q, limit.unwrap_or(20)]).await?;
        collect_tasks(rows).await
    }
}

/// Quotes each term so user input can't be parsed as FTS5 syntax. Terms are ANDed;
/// the last one is a prefix match.
fn fts_query(input: &str) -> String {
    let terms: Vec<_> = input.split_whitespace().map(|t| format!("\"{}\"", t.replace('"', "\"\""))).collect();
    match terms.split_last() {
        Some((last, rest)) => {
            let mut v = rest.to_vec();
            v.push(format!("{last}*"));
            v.join(" ")
        }
        None => String::new(),
    }
}
