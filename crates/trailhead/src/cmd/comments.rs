use anyhow::{Context, Result, bail};
use trailhead_client::http::NewComment;
use trailhead_core::{Comment, CommentKind, TaskKey, Verdict};
use uuid::Uuid;

use super::Ctx;
use crate::client::api_err;

pub(crate) async fn comment(cx: &Ctx, task: &str, kind: CommentKind, body: &str, to: Option<String>) -> Result<()> {
    let mut new = NewComment::new(kind, body);
    new.addressed_to = to.as_deref().map(|t| t.parse().context("--to takes a user or agent id")).transpose()?;
    let c = cx.c.api().add_comment(&TaskKey(task.into()), &new).await.map_err(api_err)?;
    cx.emit(&c, |c| format!("{} {}", c.kind.as_str(), c.id));
    Ok(())
}

pub(crate) async fn verdict(cx: &Ctx, task: &str, verdict: &str, body: &str, model: Option<String>) -> Result<()> {
    let verdict: Verdict = verdict.replace('-', "_").parse()?;
    let mut new = NewComment::new(CommentKind::Review, body);
    new.verdict = Some(verdict);
    new.model = model;
    let c = cx.c.api().add_comment(&TaskKey(task.into()), &new).await.map_err(api_err)?;
    cx.emit(&c, |c| format!("review {} {}", c.verdict.map_or("", |v| v.as_str()), c.id));
    Ok(())
}

/// Resolves a question given by its comment id, or a task's only open question given the task.
pub(crate) async fn answer(cx: &Ctx, comment_id: &str, body: Option<String>) -> Result<()> {
    let id = match Uuid::parse_str(comment_id) {
        Ok(id) => id,
        Err(_) => {
            let detail = cx.c.api().show_task(&TaskKey(comment_id.into())).await.map_err(api_err)?;
            let open: Vec<&Comment> =
                detail.comments.iter().filter(|q| q.kind == CommentKind::Question && q.resolved_at.is_none()).collect();
            match open.as_slice() {
                [q] => q.id,
                [] => bail!("{comment_id} has no open question"),
                many => bail!(
                    "{comment_id} has {} open questions; answer one by id:\n{}",
                    many.len(),
                    many.iter().map(|q| format!("  {}  {}", q.id, q.body)).collect::<Vec<_>>().join("\n")
                ),
            }
        }
    };
    let c = cx.c.api().resolve_comment(id, body.as_deref()).await.map_err(api_err)?;
    cx.emit(&c, |_| "resolved".into());
    Ok(())
}
