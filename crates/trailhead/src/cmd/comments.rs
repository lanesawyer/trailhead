use anyhow::{Result, bail};
use serde_json::{Value, json};

use super::Ctx;
use crate::output::s;

pub(crate) async fn comment(cx: &Ctx, task: &str, kind: &str, body: &str, to: Option<String>) -> Result<()> {
    let v =
        cx.c.post(&format!("tasks/{task}/comments"), json!({ "kind": kind, "body": body, "addressed_to": to })).await?;
    cx.show(&v, |v| format!("{} {}", s(v, "kind"), s(v, "id")));
    Ok(())
}

pub(crate) async fn verdict(cx: &Ctx, task: &str, verdict: &str, body: &str, model: Option<String>) -> Result<()> {
    let verdict = verdict.replace('-', "_");
    let body = json!({ "kind": "review", "body": body, "verdict": verdict, "model": model });
    let v = cx.c.post(&format!("tasks/{task}/comments"), body).await?;
    cx.show(&v, |v| format!("review {} {}", s(v, "verdict"), s(v, "id")));
    Ok(())
}

/// Resolves a question given by its comment id, or a task's only open question given the task.
pub(crate) async fn answer(cx: &Ctx, comment_id: &str, body: Option<String>) -> Result<()> {
    let id = match uuid::Uuid::parse_str(comment_id) {
        Ok(id) => id.to_string(),
        Err(_) => {
            let detail = cx.c.get(&format!("tasks/{comment_id}"), &[]).await?;
            let open: Vec<&Value> = detail["comments"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|q| q["kind"] == "question" && q["resolved_at"].is_null())
                .collect();
            match open.as_slice() {
                [q] => s(q, "id").to_string(),
                [] => bail!("{comment_id} has no open question"),
                many => bail!(
                    "{comment_id} has {} open questions; answer one by id:\n{}",
                    many.len(),
                    many.iter().map(|q| format!("  {}  {}", s(q, "id"), s(q, "body"))).collect::<Vec<_>>().join("\n")
                ),
            }
        }
    };
    let v = cx.c.post(&format!("comments/{id}/resolve"), json!({ "answer": body })).await?;
    cx.show(&v, |_| "resolved".into());
    Ok(())
}
