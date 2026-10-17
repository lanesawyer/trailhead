use std::collections::HashMap;

use libsql::{Connection, Row};
use uuid::Uuid;

use crate::claims::live_claim;
use crate::db::{
    collect, first, fmt_ts, get_enum, get_opt_ts, get_opt_uuid, get_ts, get_uuid, json_ids, opt_ts_value,
    opt_uuid_value,
};
use crate::error::{Error, Result};
use crate::model::{Comment, CommentKind, Entity, Status};
use crate::store::{Ctx, Store, Tx};
use crate::tasks::{find_task, load_task, update_status};

pub(crate) const COMMENT_COLS: &str =
    "id, workspace_id, task_id, author_id, on_behalf_of, kind, addressed_to, body, resolved_at, created_at, deleted_at";

fn comment_from_row(row: &Row) -> Result<Comment> {
    Ok(Comment {
        id: get_uuid(row, 0)?,
        workspace_id: get_uuid(row, 1)?,
        task_id: get_uuid(row, 2)?,
        author_id: get_uuid(row, 3)?,
        on_behalf_of: get_uuid(row, 4)?,
        kind: get_enum(row, 5)?,
        addressed_to: get_opt_uuid(row, 6)?,
        body: row.get(7)?,
        resolved_at: get_opt_ts(row, 8)?,
        created_at: get_ts(row, 9)?,
        deleted_at: get_opt_ts(row, 10)?,
    })
}

pub(crate) async fn list_comments_for(conn: &Connection, task_id: Uuid) -> Result<Vec<Comment>> {
    let rows = conn
        .query(
            &format!(
                "SELECT {COMMENT_COLS} FROM comments WHERE task_id = ?1 AND deleted_at IS NULL ORDER BY created_at, rowid"
            ),
            [task_id.to_string()],
        )
        .await?;
    collect(rows, comment_from_row).await
}

pub(crate) async fn latest_handoff(conn: &Connection, task_id: Uuid) -> Result<Option<Comment>> {
    let rows = conn
        .query(
            &format!(
                "SELECT {COMMENT_COLS} FROM comments WHERE task_id = ?1 AND kind = 'handoff' AND deleted_at IS NULL
                 ORDER BY created_at DESC, rowid DESC LIMIT 1"
            ),
            [task_id.to_string()],
        )
        .await?;
    first(rows, comment_from_row).await
}

pub(crate) async fn load_comment(conn: &Connection, ws: Uuid, id: Uuid) -> Result<Comment> {
    let rows = conn
        .query(
            &format!("SELECT {COMMENT_COLS} FROM comments WHERE id = ?1 AND workspace_id = ?2"),
            [id.to_string(), ws.to_string()],
        )
        .await?;
    first(rows, comment_from_row).await?.ok_or_else(|| Error::NotFound(format!("comment {id}")))
}

pub(crate) async fn save_comment(tx: &Tx, c: &Comment) -> Result<Comment> {
    let rows = tx
        .conn
        .query(
            &format!(
                "UPDATE comments SET kind = ?1, addressed_to = ?2, body = ?3, resolved_at = ?4, deleted_at = ?5
                 WHERE id = ?6 RETURNING {COMMENT_COLS}"
            ),
            libsql::params![
                c.kind.as_str(),
                opt_uuid_value(c.addressed_to),
                c.body.clone(),
                opt_ts_value(c.resolved_at),
                opt_ts_value(c.deleted_at),
                c.id.to_string(),
            ],
        )
        .await?;
    first(rows, comment_from_row).await?.ok_or_else(|| Error::NotFound(format!("comment {}", c.id)))
}

async fn principal_exists(conn: &Connection, ws: Uuid, id: Uuid) -> Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM users WHERE id = ?1 AND workspace_id = ?2
             UNION ALL SELECT 1 FROM agents WHERE id = ?1 AND workspace_id = ?2",
            [id.to_string(), ws.to_string()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

async fn insert_comment(
    tx: &Tx,
    task_id: Uuid,
    kind: CommentKind,
    body: &str,
    addressed_to: Option<Uuid>,
) -> Result<Comment> {
    let id = Uuid::now_v7();
    let mut rows = tx
        .conn
        .query(
            &format!(
                "INSERT INTO comments (id, workspace_id, task_id, author_id, on_behalf_of, kind, addressed_to, body, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) RETURNING {COMMENT_COLS}"
            ),
            libsql::params![
                id.to_string(),
                tx.ws(),
                task_id.to_string(),
                tx.ctx.actor_id().to_string(),
                tx.ctx.user_id.to_string(),
                kind.as_str(),
                opt_uuid_value(addressed_to),
                body,
                fmt_ts(tx.now),
            ],
        )
        .await?;
    let comment = comment_from_row(&rows.next().await?.expect("RETURNING yields a row"))?;
    tx.emit_change(Entity::Comment, id.to_string(), Some(task_id), "comment.created", None, Some(&comment)).await?;
    Ok(comment)
}

#[derive(Debug, Clone)]
pub struct NewComment {
    pub kind: CommentKind,
    pub body: String,
    /// A user or agent id. Mostly for questions.
    pub addressed_to: Option<Uuid>,
}

impl Store {
    /// Unresolved questions on these tasks, oldest first, keyed by task id.
    pub async fn open_questions(&self, ctx: &Ctx, task_ids: &[Uuid]) -> Result<HashMap<Uuid, Vec<Comment>>> {
        let sql = format!(
            "SELECT {COMMENT_COLS} FROM comments WHERE workspace_id = ?1 AND task_id IN (SELECT value FROM json_each(?2))
               AND kind = 'question' AND resolved_at IS NULL AND deleted_at IS NULL
             ORDER BY created_at, rowid"
        );
        let mut out: HashMap<Uuid, Vec<Comment>> = HashMap::new();
        for c in self.comments_on(ctx, &sql, task_ids).await? {
            out.entry(c.task_id).or_default().push(c);
        }
        Ok(out)
    }

    /// The newest handoff on each of these tasks, keyed by task id.
    pub async fn last_handoffs(&self, ctx: &Ctx, task_ids: &[Uuid]) -> Result<HashMap<Uuid, Comment>> {
        let sql = format!(
            "SELECT {COMMENT_COLS} FROM comments WHERE workspace_id = ?1 AND task_id IN (SELECT value FROM json_each(?2))
               AND kind = 'handoff' AND deleted_at IS NULL
             ORDER BY created_at DESC, rowid DESC"
        );
        let mut out = HashMap::new();
        for c in self.comments_on(ctx, &sql, task_ids).await? {
            out.entry(c.task_id).or_insert(c);
        }
        Ok(out)
    }

    async fn comments_on(&self, ctx: &Ctx, sql: &str, task_ids: &[Uuid]) -> Result<Vec<Comment>> {
        let rows =
            self.reader()?.query(sql, libsql::params![ctx.workspace_id.to_string(), json_ids(task_ids)?]).await?;
        collect(rows, comment_from_row).await
    }

    /// Adds a comment. A `question` moves the task to `needs_input`, which takes it
    /// out of the ready queue until every question is resolved.
    pub async fn add_comment(&self, ctx: &Ctx, task: &str, new: NewComment) -> Result<Comment> {
        let body = new.body.trim().to_string();
        if body.is_empty() {
            return Err(Error::Invalid("comment body is empty".into()));
        }
        self.write(ctx, async |tx| {
            let task = find_task(&tx.conn, ctx, task, false).await?;
            if let Some(to) = new.addressed_to
                && !principal_exists(&tx.conn, ctx.workspace_id, to).await?
            {
                return Err(Error::NotFound(format!("user or agent {to}")));
            }
            let comment = insert_comment(tx, task.id, new.kind, &body, new.addressed_to).await?;
            if new.kind == CommentKind::Question && task.status != Status::NeedsInput {
                if !task.status.can_transition_to(Status::NeedsInput) {
                    return Err(Error::InvalidTransition { from: task.status, to: Status::NeedsInput });
                }
                update_status(tx, &task, Status::NeedsInput, "task.updated").await?;
            }
            Ok(comment)
        })
        .await
    }

    /// Resolves a comment, optionally posting an answer. Once a task's last open
    /// question is resolved it goes back to work: `in_progress` if still claimed,
    /// otherwise `open`.
    pub async fn resolve_comment(&self, ctx: &Ctx, comment_id: Uuid, answer: Option<String>) -> Result<Comment> {
        self.write(ctx, async |tx| {
            let before = load_comment(&tx.conn, ctx.workspace_id, comment_id).await?;
            let hidden = !ctx
                .in_scope(load_task(&tx.conn, ctx.workspace_id, &before.task_id.to_string(), true).await?.project_id);
            if before.deleted_at.is_some() || hidden {
                return Err(Error::NotFound(format!("comment {comment_id}")));
            }
            if before.resolved_at.is_some() {
                return Ok(before);
            }
            if let Some(a) = answer.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
                insert_comment(tx, before.task_id, CommentKind::Note, a, Some(before.author_id)).await?;
            }
            let after = save_comment(tx, &Comment { resolved_at: Some(tx.now), ..before.clone() }).await?;
            tx.emit_change(
                Entity::Comment,
                after.id.to_string(),
                Some(after.task_id),
                "comment.resolved",
                Some(&before),
                Some(&after),
            )
            .await?;
            if after.kind == CommentKind::Question {
                let mut rows = tx
                    .conn
                    .query(
                        "SELECT 1 FROM comments WHERE task_id = ?1 AND kind = 'question'
                         AND resolved_at IS NULL AND deleted_at IS NULL",
                        [after.task_id.to_string()],
                    )
                    .await?;
                let still_waiting = rows.next().await?.is_some();
                let task = load_task(&tx.conn, ctx.workspace_id, &after.task_id.to_string(), true).await?;
                if !still_waiting && task.status == Status::NeedsInput && task.deleted_at.is_none() {
                    let to = if live_claim(&tx.conn, task.id, tx.now).await?.is_some() {
                        Status::InProgress
                    } else {
                        Status::Open
                    };
                    update_status(tx, &task, to, "task.updated").await?;
                }
            }
            Ok(after)
        })
        .await
    }

    pub async fn list_comments(&self, ctx: &Ctx, task: &str) -> Result<Vec<Comment>> {
        let conn = self.reader()?;
        let task = find_task(&conn, ctx, task, false).await?;
        list_comments_for(&conn, task.id).await
    }
}
