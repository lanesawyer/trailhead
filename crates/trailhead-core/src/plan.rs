//! Approving a plan: a person takes the tasks a planner drew up under a spec task and grades them, which is what
//! lets the factory start on them (`docs/app-platform.md`, "Planning an ask").

use std::collections::BTreeMap;

use serde_json::json;
use trailhead_model::{CommentKind, Grade, Status, TASK_PLAN_KEY, Task, TaskPlan};

use crate::comments::{NewComment, insert_comment};
use crate::db::collect;
use crate::error::{Error, Result};
use crate::metadata::merge_patch;
use crate::set_aside::{open_questions, resolve, user_name};
use crate::store::{Ctx, Store, Tx};
use crate::tasks::{TASK_COLS, check_claim_access, find_task, save_update, task_from_row};

impl Store {
    /// Approves the plan under the spec task `task`, as a person, in one write. The plan is the spec task's direct
    /// subtasks that are open and ungraded; `grades` gives each of them its grade, by key, and has to name every one
    /// and nothing else, so nothing is approved unseen. The spec task's open questions are resolved with a note
    /// saying who approved, it goes back to `open`, and its plan is marked approved, after which no planner runs on
    /// it. Refused while a planner's run is in progress on it, and to agents: grading is a person's call.
    pub async fn approve_plan(&self, ctx: &Ctx, task: &str, grades: &BTreeMap<String, Grade>) -> Result<Task> {
        if !ctx.is_human() {
            return Err(Error::Forbidden("only a person can approve a plan".into()));
        }
        self.write(ctx, async |tx| {
            let before = find_task(&tx.conn, ctx, task, false).await?;
            let Some(spec) = TaskPlan::of(&before.metadata).and_then(|plan| plan.spec) else {
                return Err(Error::Invalid(format!("{} isn't a spec task: it has no plan to approve", before.key)));
            };
            if spec.approved.is_some() {
                return Err(Error::Invalid(format!("{}'s plan is already approved", before.key)));
            }
            match before.status {
                Status::Open | Status::NeedsInput => {}
                Status::InProgress => {
                    return Err(Error::Invalid(format!(
                        "the planner is working on {}; approve its plan once it has answered",
                        before.key
                    )));
                }
                status => {
                    return Err(Error::Invalid(format!("{} is {status}; its plan can't be approved", before.key)));
                }
            }
            let plan = planned(tx, &before).await?;
            if plan.is_empty() {
                return Err(Error::Invalid(format!("{} has no open, ungraded subtasks to approve", before.key)));
            }
            let graded = |child: &Task| grades.iter().find(|(key, _)| key.trim().eq_ignore_ascii_case(&child.key.0));
            let missing: Vec<&str> = plan.iter().filter(|c| graded(c).is_none()).map(|c| c.key.0.as_str()).collect();
            if !missing.is_empty() {
                return Err(Error::Invalid(format!("every task in the plan needs a grade: {}", missing.join(", "))));
            }
            let stray: Vec<&str> = grades
                .keys()
                .filter(|key| !plan.iter().any(|c| key.trim().eq_ignore_ascii_case(&c.key.0)))
                .map(String::as_str)
                .collect();
            if !stray.is_empty() {
                return Err(Error::Invalid(format!(
                    "not open, ungraded subtasks of {}: {}",
                    before.key,
                    stray.join(", ")
                )));
            }
            for child in &plan {
                let grade = graded(child).map(|(_, grade)| *grade);
                let claim = check_claim_access(tx, child).await?;
                save_update(tx, claim, child, Task { grade, ..child.clone() }).await?;
            }
            let questions = open_questions(tx, &before).await?;
            let body = format!("Plan approved by {}: {} tasks graded.", user_name(tx).await?, plan.len());
            let note = NewComment {
                addressed_to: questions.last().map(|q| q.author_id),
                ..NewComment::new(CommentKind::Note, &body)
            };
            insert_comment(tx, before.id, &note, &body).await?;
            resolve(tx, &questions).await?;
            let patch = json!({ TASK_PLAN_KEY: { "spec": { "approved": tx.now.as_second() } } });
            let claim = check_claim_access(tx, &before).await?;
            let after =
                Task { status: Status::Open, metadata: merge_patch(&before.metadata, &patch)?, ..before.clone() };
            save_update(tx, claim, &before, after).await
        })
        .await
    }
}

/// The plan under `spec`: its direct subtasks that are open and ungraded, oldest first. One a person has graded, or
/// that has been started or closed, is no longer the plan's to grade.
async fn planned(tx: &Tx, spec: &Task) -> Result<Vec<Task>> {
    let rows = tx
        .conn
        .query(
            &format!(
                "SELECT {TASK_COLS} FROM tasks WHERE parent_id = ?1 AND deleted_at IS NULL ORDER BY created_at, rowid"
            ),
            [spec.id.to_string()],
        )
        .await?;
    let children = collect(rows, task_from_row).await?;
    Ok(children.into_iter().filter(|c| c.status == Status::Open && c.grade.is_none()).collect())
}
