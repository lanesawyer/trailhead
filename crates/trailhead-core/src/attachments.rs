//! Pictures on a task. The metadata is an attachment row, logged and undone like a comment; the bytes are a blob keyed
//! by their SHA-256, stored once however many attachments name them and never in an event.

use std::collections::HashMap;

use libsql::{Connection, Row};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::db::{collect, first, fmt_ts, get_opt_ts, get_ts, get_uuid, json_ids, opt_ts_value};
use crate::error::{Error, Result};
use crate::identity::is_descendant;
use crate::store::{Ctx, Store, Tx};
use crate::tasks::{find_task, load_task};
use trailhead_model::{
    Attachment, AttachmentRole, Entity, MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS_PER_TASK, MAX_TASK_ATTACHMENT_BYTES, PNG,
    attachment_name, attachment_set, png_size,
};

const ATTACHMENT_COLS: &str = "id, workspace_id, task_id, name, content_type, size, sha256, width, height, role, \
     set_name, created_by, on_behalf_of, created_at, deleted_at";

fn attachment_from_row(row: &Row) -> Result<Attachment> {
    let dim = |i: i32| -> Result<u32> {
        u32::try_from(row.get::<i64>(i)?).map_err(|e| Error::Corrupt(format!("attachment size: {e}")))
    };
    Ok(Attachment {
        id: get_uuid(row, 0)?,
        workspace_id: get_uuid(row, 1)?,
        task_id: get_uuid(row, 2)?,
        name: row.get(3)?,
        content_type: row.get(4)?,
        size: row.get(5)?,
        sha256: row.get(6)?,
        width: dim(7)?,
        height: dim(8)?,
        role: row.get::<Option<String>>(9)?.map(|r| r.parse::<AttachmentRole>()).transpose()?,
        set: row.get(10)?,
        created_by: get_uuid(row, 11)?,
        on_behalf_of: get_uuid(row, 12)?,
        created_at: get_ts(row, 13)?,
        deleted_at: get_opt_ts(row, 14)?,
    })
}

/// A picture to attach. The engine checks everything: the content type, that the bytes are a PNG, the sizes and the
/// task's limits.
#[derive(Debug, Clone)]
pub struct NewAttachment {
    pub name: String,
    pub content_type: String,
    pub role: Option<AttachmentRole>,
    pub set: Option<String>,
    pub bytes: Vec<u8>,
}

impl NewAttachment {
    pub fn png(name: impl Into<String>, bytes: Vec<u8>) -> NewAttachment {
        NewAttachment { name: name.into(), content_type: PNG.into(), role: None, set: None, bytes }
    }
}

/// The checked parts of a [`NewAttachment`].
struct Checked {
    name: String,
    set: Option<String>,
    sha256: String,
    width: u32,
    height: u32,
}

fn check_new(new: &NewAttachment) -> Result<Checked> {
    let content_type = new.content_type.split(';').next().unwrap_or_default().trim();
    if !content_type.eq_ignore_ascii_case(PNG) {
        return Err(Error::Invalid(format!("only {PNG} pictures can be attached, not {:?}", new.content_type)));
    }
    if new.bytes.len() > MAX_ATTACHMENT_BYTES {
        return Err(Error::Invalid(format!(
            "the picture is {} bytes; at most {MAX_ATTACHMENT_BYTES} can be attached",
            new.bytes.len()
        )));
    }
    let (width, height) = png_size(&new.bytes)?;
    Ok(Checked {
        name: attachment_name(&new.name)?,
        set: new.set.as_deref().map(attachment_set).transpose()?,
        sha256: hex::encode(Sha256::digest(&new.bytes)),
        width,
        height,
    })
}

/// Refuses `adding` more bytes when the task's live attachments are at their limits.
async fn check_room(conn: &Connection, task_id: Uuid, adding: i64) -> Result<()> {
    let rows = conn
        .query(
            "SELECT COUNT(*), COALESCE(SUM(size), 0) FROM attachments WHERE task_id = ?1 AND deleted_at IS NULL",
            [task_id.to_string()],
        )
        .await?;
    let (count, bytes) =
        first(rows, |r| Ok((r.get::<i64>(0)?, r.get::<i64>(1)?))).await?.expect("an aggregate yields a row");
    if count >= MAX_ATTACHMENTS_PER_TASK as i64 {
        return Err(Error::Invalid(format!("a task holds at most {MAX_ATTACHMENTS_PER_TASK} pictures")));
    }
    if bytes + adding > MAX_TASK_ATTACHMENT_BYTES as i64 {
        return Err(Error::Invalid(format!(
            "a task's pictures hold at most {MAX_TASK_ATTACHMENT_BYTES} bytes together; this one has {bytes}"
        )));
    }
    Ok(())
}

/// A person may remove any picture; an agent only one it or an agent under it added.
pub(crate) async fn check_remove_access(tx: &Tx, a: &Attachment) -> Result<()> {
    match tx.ctx.agent_id {
        Some(me) if a.created_by != me && !is_descendant(&tx.conn, me, a.created_by).await? => {
            Err(Error::Forbidden("an agent can remove only the pictures it or its sub-agents added".into()))
        }
        _ => Ok(()),
    }
}

/// Undo putting a removed picture back goes by the limits adding one does.
pub(crate) async fn check_restore_room(conn: &Connection, a: &Attachment) -> Result<()> {
    check_room(conn, a.task_id, a.size).await
}

pub(crate) async fn load_attachment(conn: &Connection, ws: Uuid, id: Uuid) -> Result<Attachment> {
    let rows = conn
        .query(
            &format!("SELECT {ATTACHMENT_COLS} FROM attachments WHERE id = ?1 AND workspace_id = ?2"),
            [id.to_string(), ws.to_string()],
        )
        .await?;
    first(rows, attachment_from_row).await?.ok_or_else(|| Error::NotFound(format!("attachment {id}")))
}

/// Only `deleted_at` ever changes.
pub(crate) async fn save_attachment(tx: &Tx, a: &Attachment) -> Result<Attachment> {
    let rows = tx
        .conn
        .query(
            &format!("UPDATE attachments SET deleted_at = ?1 WHERE id = ?2 RETURNING {ATTACHMENT_COLS}"),
            libsql::params![opt_ts_value(a.deleted_at), a.id.to_string()],
        )
        .await?;
    first(rows, attachment_from_row).await?.ok_or_else(|| Error::NotFound(format!("attachment {}", a.id)))
}

pub(crate) async fn list_attachments_for(conn: &Connection, task_id: Uuid) -> Result<Vec<Attachment>> {
    let rows = conn
        .query(
            &format!(
                "SELECT {ATTACHMENT_COLS} FROM attachments WHERE task_id = ?1 AND deleted_at IS NULL
                 ORDER BY created_at, rowid"
            ),
            [task_id.to_string()],
        )
        .await?;
    collect(rows, attachment_from_row).await
}

/// A live attachment the context may see: not removed, on a live task in its scope.
async fn find_attachment(conn: &Connection, ctx: &Ctx, id: Uuid) -> Result<Attachment> {
    let a = load_attachment(conn, ctx.workspace_id, id).await?;
    let task = load_task(conn, ctx.workspace_id, &a.task_id.to_string(), true).await?;
    if a.deleted_at.is_some() || task.deleted_at.is_some() || !ctx.in_scope(task.project_id) {
        return Err(Error::NotFound(format!("attachment {id}")));
    }
    Ok(a)
}

impl Store {
    /// Attaches a picture to a task. Whoever may comment on the task may attach one. The bytes are stored once
    /// however many attachments share them, and the event carries the metadata alone.
    pub async fn add_attachment(&self, ctx: &Ctx, task: &str, new: NewAttachment) -> Result<Attachment> {
        let checked = check_new(&new)?;
        let size = new.bytes.len() as i64;
        self.write(ctx, async |tx| {
            let task = find_task(&tx.conn, ctx, task, false).await?;
            check_room(&tx.conn, task.id, size).await?;
            tx.conn
                .execute(
                    "INSERT INTO blobs (sha256, size, bytes, created_at) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (sha256) DO NOTHING",
                    libsql::params![checked.sha256.clone(), size, new.bytes.clone(), tx.now_str()],
                )
                .await?;
            let id = Uuid::now_v7();
            let rows = tx
                .conn
                .query(
                    &format!(
                        "INSERT INTO attachments (id, workspace_id, task_id, name, content_type, size, sha256, width,
                                                  height, role, set_name, created_by, on_behalf_of, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                         RETURNING {ATTACHMENT_COLS}"
                    ),
                    libsql::params![
                        id.to_string(),
                        tx.ws(),
                        task.id.to_string(),
                        checked.name.clone(),
                        PNG,
                        size,
                        checked.sha256.clone(),
                        i64::from(checked.width),
                        i64::from(checked.height),
                        new.role.map(AttachmentRole::as_str),
                        checked.set.clone(),
                        tx.ctx.actor_id().to_string(),
                        tx.ctx.user_id.to_string(),
                        fmt_ts(tx.now),
                    ],
                )
                .await?;
            let a = first(rows, attachment_from_row).await?.expect("RETURNING yields a row");
            tx.emit_change(Entity::Attachment, id.to_string(), Some(task.id), "attachment.created", None, Some(&a))
                .await?;
            Ok(a)
        })
        .await
    }

    /// The task's live pictures, oldest first, without their bytes.
    pub async fn list_attachments(&self, ctx: &Ctx, task: &str) -> Result<Vec<Attachment>> {
        let conn = self.reader()?;
        let task = find_task(&conn, ctx, task, false).await?;
        list_attachments_for(&conn, task.id).await
    }

    /// The live pictures on each of these tasks, oldest first and without their bytes, keyed by task id.
    pub async fn attachments_on(&self, ctx: &Ctx, task_ids: &[Uuid]) -> Result<HashMap<Uuid, Vec<Attachment>>> {
        let rows = self
            .reader()?
            .query(
                &format!(
                    "SELECT {ATTACHMENT_COLS} FROM attachments WHERE workspace_id = ?1
                       AND task_id IN (SELECT value FROM json_each(?2)) AND deleted_at IS NULL
                     ORDER BY created_at, rowid"
                ),
                libsql::params![ctx.workspace_id.to_string(), json_ids(task_ids)?],
            )
            .await?;
        let mut out: HashMap<Uuid, Vec<Attachment>> = HashMap::new();
        for a in collect(rows, attachment_from_row).await? {
            out.entry(a.task_id).or_default().push(a);
        }
        Ok(out)
    }

    /// A live picture and its bytes, to whoever may read its task.
    pub async fn attachment_bytes(&self, ctx: &Ctx, id: Uuid) -> Result<(Attachment, Vec<u8>)> {
        let conn = self.reader()?;
        let a = find_attachment(&conn, ctx, id).await?;
        let rows = conn.query("SELECT bytes FROM blobs WHERE sha256 = ?1", [a.sha256.clone()]).await?;
        let bytes = first(rows, |r| Ok(r.get::<Vec<u8>>(0)?))
            .await?
            .ok_or_else(|| Error::Corrupt(format!("attachment {id} has no bytes stored")))?;
        Ok((a, bytes))
    }

    /// Removes a picture (a soft delete, which undo restores). A person may remove any; an agent only those it or
    /// its sub-agents added. The bytes stay, for undo and for any other attachment sharing them.
    pub async fn remove_attachment(&self, ctx: &Ctx, id: Uuid) -> Result<Attachment> {
        self.write(ctx, async |tx| {
            let before = find_attachment(&tx.conn, ctx, id).await?;
            check_remove_access(tx, &before).await?;
            let after = save_attachment(tx, &Attachment { deleted_at: Some(tx.now), ..before.clone() }).await?;
            tx.emit_change(
                Entity::Attachment,
                id.to_string(),
                Some(after.task_id),
                "attachment.deleted",
                Some(&before),
                Some(&after),
            )
            .await?;
            Ok(after)
        })
        .await
    }
}
