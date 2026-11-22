//! Setting a task aside: a person stops the factory working a task and keeps it for later
//! (`docs/factory/operating.md`, "What happens to a task").

use serde_json::json;
use trailhead_model::{CommentKind, Entity, SetAside, Status, TASK_FACTORY_KEY, Task, set_aside_note};

use crate::comments::{NewComment, insert_comment, save_comment};
use crate::db::{collect, first};
use crate::error::{Error, Result};
use crate::metadata::merge_patch;
use crate::store::{Ctx, Store, Tx};
use crate::tasks::{check_claim_access, find_task, save_update};

impl Store {
    /// Sets the task aside, as a person, in one write: every open question on it is resolved, with a note saying who
    /// set it aside (and `note`, if given, on its second line) that answers the newest one; its grade is cleared and
    /// it goes back to `open`, which drops any claim, an agent's included; and the factory's record of it gets a
    /// [`SetAside`]. The factory takes only graded tasks, so it leaves the task until a person grades it again, and
    /// then starts it afresh. A task in progress may have a run going, which this can't stop, so it is refused, as is
    /// a closed one. Agents are refused: grading is a person's call.
    pub async fn set_aside(&self, ctx: &Ctx, task: &str, note: Option<String>) -> Result<Task> {
        if !ctx.is_human() {
            return Err(Error::Forbidden("only a person can set a task aside".into()));
        }
        let note = note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
        self.write(ctx, async |tx| {
            let before = find_task(&tx.conn, ctx, task, false).await?;
            match before.status {
                Status::Open | Status::NeedsInput | Status::InReview | Status::Approved => {}
                Status::InProgress => {
                    return Err(Error::Invalid(format!(
                        "{} is in progress, and a run going on it can't be stopped from here; set it aside once it \
                         stops, or cancel it",
                        before.key
                    )));
                }
                Status::Done | Status::Cancelled => {
                    return Err(Error::Invalid(format!(
                        "{} is {}; only an unfinished task can be set aside",
                        before.key, before.status
                    )));
                }
            }
            let claim = check_claim_access(tx, &before).await?;
            let by = user_name(tx).await?;
            let questions = open_questions(tx, &before).await?;
            let newest = questions.last();
            let body = set_aside_note(&by, note.as_deref());
            let answer =
                NewComment { addressed_to: newest.map(|q| q.author_id), ..NewComment::new(CommentKind::Note, &body) };
            insert_comment(tx, before.id, &answer, &body).await?;
            resolve(tx, &questions).await?;
            let marker =
                SetAside { by, at: tx.now.as_second(), question: newest.map(|q| q.body.clone()), note: note.clone() };
            let patch = json!({ TASK_FACTORY_KEY: { "set_aside": marker } });
            let after = Task {
                grade: None,
                status: Status::Open,
                duplicate_of: None,
                metadata: merge_patch(&before.metadata, &patch)?,
                ..before.clone()
            };
            save_update(tx, claim, &before, after).await
        })
        .await
    }
}

/// Marks `questions` resolved, each with its event.
pub(crate) async fn resolve(tx: &Tx, questions: &[trailhead_model::Comment]) -> Result<()> {
    for q in questions {
        let after = save_comment(tx, &trailhead_model::Comment { resolved_at: Some(tx.now), ..q.clone() }).await?;
        tx.emit_change(
            Entity::Comment,
            after.id.to_string(),
            Some(after.task_id),
            "comment.resolved",
            Some(q),
            Some(&after),
        )
        .await?;
    }
    Ok(())
}

/// The acting person's name.
pub(crate) async fn user_name(tx: &Tx) -> Result<String> {
    let rows = tx
        .conn
        .query("SELECT name FROM users WHERE id = ?1 AND workspace_id = ?2", [tx.ctx.user_id.to_string(), tx.ws()])
        .await?;
    first(rows, |row| Ok(row.get::<String>(0)?))
        .await?
        .ok_or_else(|| Error::NotFound(format!("user {}", tx.ctx.user_id)))
}

/// The task's unresolved questions, oldest first.
pub(crate) async fn open_questions(tx: &Tx, task: &Task) -> Result<Vec<trailhead_model::Comment>> {
    let rows = tx
        .conn
        .query(
            &format!(
                "SELECT {} FROM comments WHERE task_id = ?1 AND kind = 'question' AND resolved_at IS NULL
                   AND deleted_at IS NULL ORDER BY created_at, rowid",
                crate::comments::COMMENT_COLS
            ),
            [task.id.to_string()],
        )
        .await?;
    collect(rows, crate::comments::comment_from_row).await
}
