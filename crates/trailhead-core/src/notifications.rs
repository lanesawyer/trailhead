//! Notifications (`docs/app-platform.md#notifications`): rows a person is told by, raised through the API or by the
//! rules here, in the transaction of the write that caused them. Each change to one is an event, so the stream carries
//! it to every window, but none is a change undo reverts: a notification is told once and stays told.

use libsql::{Connection, Row};
use uuid::Uuid;

use crate::db::{collect, first, fmt_ts, get_opt_ts, get_opt_uuid, get_ts, get_uuid, opt_uuid_value};
use crate::error::{Error, Result};
use crate::store::{Ctx, EventIn, Store, Tx};
use crate::tasks::find_task;
use trailhead_model::{
    Comment, CommentKind, DUE_SOON, Entity, Notification, NotificationAction, OVERDUE, QUESTION, RAISED, READ,
    RESOLVED, REVIEW, SOURCE_SERVER, Status, Task, UNREAD, clip, notification_actions, notification_body,
    notification_name, notification_title,
};

const COLS: &str = "id, workspace_id, source, kind, title, body, subject, task_id, actions, raised_by, created_at, \
     read_at, resolved_at, (SELECT key FROM tasks WHERE tasks.id = notifications.task_id)";

/// The most one page of [`Store::list_notifications`] holds.
const MAX_PAGE: i64 = 200;
/// How much of a title or a question a rule puts in a notification.
const QUOTE: usize = 120;

fn from_row(row: &Row) -> Result<Notification> {
    Ok(Notification {
        id: get_uuid(row, 0)?,
        workspace_id: get_uuid(row, 1)?,
        source: row.get(2)?,
        kind: row.get(3)?,
        title: row.get(4)?,
        body: row.get(5)?,
        subject: row.get(6)?,
        task_id: get_opt_uuid(row, 7)?,
        actions: serde_json::from_str(&row.get::<String>(8)?)?,
        raised_by: get_uuid(row, 9)?,
        created_at: get_ts(row, 10)?,
        read_at: get_opt_ts(row, 11)?,
        resolved_at: get_opt_ts(row, 12)?,
        task_key: row.get(13)?,
    })
}

/// A notification to raise.
#[derive(Debug, Clone, Default)]
pub struct NewNotification {
    /// What raises it: an app's id, `factory` or `server`.
    pub source: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub subject: Option<String>,
    /// The key or id of the task it is about.
    pub task: Option<String>,
    pub actions: Vec<NotificationAction>,
}

#[derive(Debug, Clone, Default)]
pub struct NotificationFilter {
    /// Only those neither read nor resolved.
    pub unread: bool,
    /// Only those raised before this one, to page back: they are listed newest first.
    pub before: Option<Uuid>,
    /// At most this many, 50 unless given and never more than 200.
    pub limit: Option<i64>,
}

/// Which open notifications a resolve takes.
enum Open<'a> {
    Task(Uuid),
    TaskKind(Uuid, &'a [&'a str]),
    Subject(&'a str, &'a str),
}

impl Store {
    /// Raises a notification, as whoever asks: an app through the window, the factory, an agent.
    pub async fn raise_notification(&self, ctx: &Ctx, new: NewNotification) -> Result<Notification> {
        let invalid = |e: trailhead_model::ParseError| Error::Invalid(e.0);
        let raise = Raise {
            source: notification_name("source", &new.source).map_err(invalid)?,
            kind: notification_name("kind", &new.kind).map_err(invalid)?,
            title: notification_title(&new.title).map_err(invalid)?,
            body: notification_body(&new.body),
            subject: new.subject.as_deref().map(|s| notification_name("subject", s)).transpose().map_err(invalid)?,
            task_id: None,
            actions: notification_actions(new.actions).map_err(invalid)?,
        };
        self.write(ctx, async |tx| {
            let task_id = match &new.task {
                Some(t) => Some(find_task(&tx.conn, ctx, t, false).await?.id),
                None => None,
            };
            raise_in(tx, Raise { task_id, ..raise }).await
        })
        .await
    }

    /// Notifications, newest first. One read or resolved stays in the list unless `unread` is asked for. A context
    /// limited to some projects sees none: they are the person's.
    pub async fn list_notifications(&self, ctx: &Ctx, filter: NotificationFilter) -> Result<Vec<Notification>> {
        if ctx.is_scoped() {
            return Ok(Vec::new());
        }
        let rows = self
            .reader()?
            .query(
                &format!(
                    "SELECT {COLS} FROM notifications WHERE workspace_id = ?1
                     AND (?2 = 0 OR (read_at IS NULL AND resolved_at IS NULL)) AND (?3 IS NULL OR id < ?3)
                     ORDER BY id DESC LIMIT ?4"
                ),
                libsql::params![
                    ctx.workspace_id.to_string(),
                    filter.unread,
                    filter.before.map(|b| b.to_string()),
                    filter.limit.unwrap_or(50).clamp(1, MAX_PAGE),
                ],
            )
            .await?;
        collect(rows, from_row).await
    }

    /// How many are neither read nor resolved.
    pub async fn unread_notifications(&self, ctx: &Ctx) -> Result<i64> {
        if ctx.is_scoped() {
            return Ok(0);
        }
        let mut rows = self
            .reader()?
            .query(
                "SELECT COUNT(*) FROM notifications WHERE workspace_id = ?1 AND read_at IS NULL AND resolved_at IS NULL",
                [ctx.workspace_id.to_string()],
            )
            .await?;
        Ok(rows.next().await?.map(|r| r.get(0)).transpose()?.unwrap_or(0))
    }

    /// Marks one read, or unread again. Only the person does: it says what they have seen.
    pub async fn mark_notification(&self, ctx: &Ctx, id: Uuid, read: bool) -> Result<Notification> {
        person_only(ctx)?;
        self.write(ctx, async |tx| {
            let before = load(&tx.conn, ctx.workspace_id, id).await?;
            if before.read_at.is_some() == read {
                return Ok(before);
            }
            let after = Notification { read_at: read.then_some(tx.now), ..before.clone() };
            save(tx, &before, &after, if read { READ } else { UNREAD }).await
        })
        .await
    }

    /// Marks every unread one read, and says how many that was.
    pub async fn mark_all_notifications_read(&self, ctx: &Ctx) -> Result<usize> {
        person_only(ctx)?;
        self.write(ctx, async |tx| {
            let rows = tx
                .conn
                .query(
                    &format!(
                        "SELECT {COLS} FROM notifications WHERE workspace_id = ?1 AND read_at IS NULL ORDER BY id"
                    ),
                    [tx.ws()],
                )
                .await?;
            let unread = collect(rows, from_row).await?;
            for before in &unread {
                save(tx, before, &Notification { read_at: Some(tx.now), ..before.clone() }, READ).await?;
            }
            Ok(unread.iter().filter(|n| n.resolved_at.is_none()).count())
        })
        .await
    }

    /// Resolves one: the thing no longer needs the person. Whoever dealt with it may say so.
    pub async fn resolve_notification(&self, ctx: &Ctx, id: Uuid) -> Result<Notification> {
        unscoped(ctx)?;
        self.write(ctx, async |tx| {
            let before = load(&tx.conn, ctx.workspace_id, id).await?;
            if before.resolved_at.is_some() {
                return Ok(before);
            }
            let after = Notification { resolved_at: Some(tx.now), ..before.clone() };
            save(tx, &before, &after, RESOLVED).await
        })
        .await
    }

    /// Resolves every open one `source` raised about `subject`, and says how many that was.
    pub async fn resolve_notifications_about(&self, ctx: &Ctx, source: &str, subject: &str) -> Result<usize> {
        unscoped(ctx)?;
        self.write(ctx, async |tx| resolve_open(tx, Open::Subject(source.trim(), subject.trim())).await).await
    }
}

fn person_only(ctx: &Ctx) -> Result<()> {
    if ctx.is_human() { Ok(()) } else { Err(Error::Forbidden("only a person marks a notification read".into())) }
}

fn unscoped(ctx: &Ctx) -> Result<()> {
    if ctx.is_scoped() {
        return Err(Error::Forbidden("a token limited to some projects can't resolve notifications".into()));
    }
    Ok(())
}

async fn load(conn: &Connection, ws: Uuid, id: Uuid) -> Result<Notification> {
    let rows = conn
        .query(
            &format!("SELECT {COLS} FROM notifications WHERE id = ?1 AND workspace_id = ?2"),
            [id.to_string(), ws.to_string()],
        )
        .await?;
    first(rows, from_row).await?.ok_or_else(|| Error::NotFound(format!("notification {id}")))
}

/// A notification as a rule or the API raises it, already checked.
struct Raise {
    source: String,
    kind: String,
    title: String,
    body: String,
    subject: Option<String>,
    task_id: Option<Uuid>,
    actions: Vec<NotificationAction>,
}

impl Raise {
    /// One the engine raises about a task.
    fn by_rule(kind: &str, task: &Task, subject: String, title: String, body: &str) -> Raise {
        Raise {
            source: SOURCE_SERVER.into(),
            kind: kind.into(),
            title: clip(&title, trailhead_model::MAX_NOTIFICATION_TITLE),
            body: notification_body(body),
            subject: Some(subject),
            task_id: Some(task.id),
            actions: Vec::new(),
        }
    }
}

async fn raise_in(tx: &Tx, raise: Raise) -> Result<Notification> {
    let id = Uuid::now_v7();
    let rows = tx
        .conn
        .query(
            &format!(
                "INSERT INTO notifications (id, workspace_id, source, kind, title, body, subject, task_id, actions,
                                            raised_by, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) RETURNING {COLS}"
            ),
            libsql::params![
                id.to_string(),
                tx.ws(),
                raise.source,
                raise.kind,
                raise.title,
                raise.body,
                raise.subject,
                opt_uuid_value(raise.task_id),
                serde_json::to_string(&raise.actions)?,
                tx.ctx.actor_id().to_string(),
                tx.now_str(),
            ],
        )
        .await?;
    let n = first(rows, from_row).await?.expect("RETURNING yields a row");
    emit(tx, RAISED, None, &n).await?;
    Ok(n)
}

/// The notification's own event. It names no task, so a task's history holds only what changed the task.
async fn emit(tx: &Tx, kind: &str, before: Option<&Notification>, after: &Notification) -> Result<()> {
    tx.emit(EventIn {
        entity: Entity::Notification,
        entity_id: after.id.to_string(),
        task_id: None,
        kind,
        before: before.map(serde_json::to_value).transpose()?,
        after: Some(serde_json::to_value(after)?),
        undoes: None,
        actor: None,
    })
    .await?;
    Ok(())
}

/// Writes the change and its event, and gives back the row as stored.
async fn save(tx: &Tx, before: &Notification, after: &Notification, kind: &str) -> Result<Notification> {
    let rows = tx
        .conn
        .query(
            &format!("UPDATE notifications SET read_at = ?1, resolved_at = ?2 WHERE id = ?3 RETURNING {COLS}"),
            libsql::params![after.read_at.map(fmt_ts), after.resolved_at.map(fmt_ts), after.id.to_string()],
        )
        .await?;
    let saved = first(rows, from_row).await?.ok_or_else(|| Error::NotFound(format!("notification {}", after.id)))?;
    emit(tx, kind, Some(before), &saved).await?;
    Ok(saved)
}

async fn resolve_open(tx: &Tx, which: Open<'_>) -> Result<usize> {
    let (cond, a, b) = match which {
        Open::Task(task) => ("task_id = ?2 AND ?3 IS NULL", task.to_string(), None),
        Open::TaskKind(task, kinds) => (
            "task_id = ?2 AND kind IN (SELECT value FROM json_each(?3))",
            task.to_string(),
            Some(serde_json::to_string(kinds)?),
        ),
        Open::Subject(source, subject) => {
            ("source = ?2 AND subject = ?3", source.to_string(), Some(subject.to_string()))
        }
    };
    let rows = tx
        .conn
        .query(
            &format!(
                "SELECT {COLS} FROM notifications WHERE workspace_id = ?1 AND resolved_at IS NULL AND {cond} ORDER BY id"
            ),
            libsql::params![tx.ws(), a, b],
        )
        .await?;
    let open = collect(rows, from_row).await?;
    for before in &open {
        save(tx, before, &Notification { resolved_at: Some(tx.now), ..before.clone() }, RESOLVED).await?;
    }
    Ok(open.len())
}

async fn name_of(conn: &Connection, id: Uuid) -> Result<String> {
    let mut rows = conn
        .query("SELECT name FROM agents WHERE id = ?1 UNION ALL SELECT name FROM users WHERE id = ?1", [id.to_string()])
        .await?;
    Ok(match rows.next().await? {
        Some(row) => row.get(0)?,
        None => "someone".into(),
    })
}

async fn is_agent(conn: &Connection, id: Uuid) -> Result<bool> {
    Ok(conn.query("SELECT 1 FROM agents WHERE id = ?1", [id.to_string()]).await?.next().await?.is_some())
}

fn question_subject(comment: Uuid) -> String {
    format!("comment:{comment}")
}

/// A new comment. An agent's question that isn't put to another agent is one for the person.
pub(crate) async fn on_comment_created(tx: &Tx, task: &Task, comment: &Comment) -> Result<()> {
    if comment.kind != CommentKind::Question || tx.ctx.is_human() {
        return Ok(());
    }
    if let Some(to) = comment.addressed_to
        && is_agent(&tx.conn, to).await?
    {
        return Ok(());
    }
    let by = name_of(&tx.conn, comment.author_id).await?;
    let title = format!("{}: {by} asks about {}", task.key, clip(&task.title, QUOTE));
    raise_in(tx, Raise::by_rule(QUESTION, task, question_subject(comment.id), title, &comment.body)).await?;
    Ok(())
}

/// A comment changed. A question that is resolved, deleted or no longer a question needs nobody.
pub(crate) async fn on_comment_saved(tx: &Tx, comment: &Comment) -> Result<()> {
    if comment.resolved_at.is_some() || comment.deleted_at.is_some() || comment.kind != CommentKind::Question {
        resolve_open(tx, Open::Subject(SOURCE_SERVER, &question_subject(comment.id))).await?;
    }
    Ok(())
}

/// A task changed. One finished or deleted needs nobody; one that left review no longer waits for it; and one an
/// agent sent for review in a project no agent may approve in waits on the person.
pub(crate) async fn on_task_saved(tx: &Tx, before: &Task, after: &Task) -> Result<()> {
    if after.deleted_at.is_some() || matches!(after.status, Status::Done | Status::Cancelled) {
        resolve_open(tx, Open::Task(after.id)).await?;
        return Ok(());
    }
    if before.due_on != after.due_on || before.due_at != after.due_at || before.due_tz != after.due_tz {
        resolve_open(tx, Open::TaskKind(after.id, &[DUE_SOON, OVERDUE])).await?;
    }
    if before.status == after.status {
        return Ok(());
    }
    if before.status == Status::InReview {
        resolve_open(tx, Open::TaskKind(after.id, &[REVIEW])).await?;
    }
    if after.status == Status::InReview && !tx.ctx.is_human() && !has_agent_reviewer(&tx.conn, after).await? {
        let title = format!("{} is waiting for your review: {}", after.key, clip(&after.title, QUOTE));
        raise_in(tx, Raise::by_rule(REVIEW, after, format!("review:{}", after.id), title, "")).await?;
    }
    Ok(())
}

async fn has_agent_reviewer(conn: &Connection, task: &Task) -> Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM projects p, json_each(p.reviewers) r WHERE p.id = ?1 LIMIT 1",
            [task.project_id.to_string()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// A reminder fired for `task`. Its being overdue takes the place of its being due soon.
pub(crate) async fn on_reminder(tx: &Tx, task: &Task, kind: &str) -> Result<()> {
    resolve_open(tx, Open::TaskKind(task.id, &[DUE_SOON, OVERDUE])).await?;
    let what = if kind == OVERDUE { "is overdue" } else { "is due soon" };
    let title = format!("{} {what}: {}", task.key, clip(&task.title, QUOTE));
    raise_in(tx, Raise::by_rule(kind, task, format!("due:{}", task.id), title, "")).await?;
    Ok(())
}
