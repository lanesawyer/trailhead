use jiff::civil::Date;
use jiff::{Timestamp, Zoned};
use libsql::{Connection, Row};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::claims::{drop_claim, live_claim, reap_expired};
use crate::comments::list_comments_for;
use crate::db::{
    collect, first, fmt_ts, get_enum, get_json, get_opt_date, get_opt_ts, get_opt_uuid, get_ts, get_uuid, json_ids,
    opt_date_value, opt_ts_value, opt_uuid_value,
};
use crate::deps::{unfinished_blockers, visible_key};
use crate::error::{Error, Result};
use crate::metadata::{merge_patch, validate_path};
use crate::model::{Claim, Comment, Entity, Status, Task};
use crate::projects::{check_project_write, find_project, load_project};
use crate::store::{Ctx, Store, Tx};
use crate::when;

pub(crate) const TASK_COLS: &str = "id, workspace_id, project_id, key, title, description, status, priority, due_at, due_tz, parent_id, metadata, version, source, external_id, created_by, created_at, updated_at, deleted_at, scheduled_on, scheduled_at, scheduled_tz, due_on, grade";

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
        scheduled_on: get_opt_date(row, 19)?,
        scheduled_at: get_opt_ts(row, 20)?,
        scheduled_tz: row.get(21)?,
        due_on: get_opt_date(row, 22)?,
        grade: row.get(23)?,
    })
}

/// Resolves a task by UUID or key (`AUTH-42`).
pub(crate) async fn load_task(conn: &Connection, ws: Uuid, r: &str, include_deleted: bool) -> Result<Task> {
    let sql = format!(
        "SELECT {TASK_COLS} FROM tasks WHERE workspace_id = ?1 AND (id = ?2 OR key = upper(?2)) {}",
        if include_deleted { "" } else { "AND deleted_at IS NULL" }
    );
    let rows = conn.query(&sql, [ws.to_string(), r.trim().to_string()]).await?;
    first(rows, task_from_row).await?.ok_or_else(|| Error::NotFound(format!("task {r}")))
}

/// [`load_task`] for a task the caller named: one outside the context's project
/// scope looks like it doesn't exist.
pub(crate) async fn find_task(conn: &Connection, ctx: &Ctx, r: &str, include_deleted: bool) -> Result<Task> {
    let task = load_task(conn, ctx.workspace_id, r, include_deleted).await?;
    if ctx.in_scope(task.project_id) { Ok(task) } else { Err(Error::NotFound(format!("task {r}"))) }
}

/// Compare-and-swap on `version`. Used by updates and undo alike.
pub(crate) async fn save_task(tx: &Tx, before: &Task, mut after: Task) -> Result<Task> {
    check_grade(&tx.ctx, before.grade, after.grade)?;
    // Snapshots from before due days had only a time.
    if after.due_on.is_none()
        && let Some(at) = after.due_at
    {
        when::settle("due", &mut after.due_on, Some(&when::utc(at)), &after.due_tz, false)?;
    }
    let due_by = when::deadline(after.due_on, after.due_at, after.due_tz.as_deref())?;
    let mut rows = tx
        .conn
        .query(
            &format!(
                "UPDATE tasks SET title = ?1, description = ?2, status = ?3, priority = ?4, due_at = ?5, due_tz = ?6,
                 parent_id = ?7, metadata = ?8, deleted_at = ?9, version = version + 1, updated_at = ?10,
                 scheduled_on = ?13, scheduled_at = ?14, scheduled_tz = ?15, due_on = ?16, due_by = ?17, grade = ?18 WHERE id = ?11 AND version = ?12 RETURNING {TASK_COLS}"
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
                opt_date_value(after.scheduled_on),
                opt_ts_value(after.scheduled_at),
                after.scheduled_tz,
                opt_date_value(after.due_on),
                opt_ts_value(due_by),
                after.grade,
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

fn validate_grade(g: i64) -> Result<()> {
    if (1..=4).contains(&g) { Ok(()) } else { Err(Error::Invalid("grade must be 1 to 4".into())) }
}

/// Parses a grade for an update: `1` to `4`, or `none` to clear it.
pub fn parse_grade(s: &str) -> Result<Option<i64>> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    let g = s.parse::<i64>().map_err(|_| Error::Invalid("grade must be 1 to 4, or none".into()))?;
    validate_grade(g)?;
    Ok(Some(g))
}

/// A task's grade is a human's call: an agent can only raise an existing grade,
/// not set the first one, lower it or clear it, whether by an update or by
/// undoing or reverting someone else's.
pub(crate) fn check_grade(ctx: &Ctx, before: Option<i64>, after: Option<i64>) -> Result<()> {
    if ctx.is_human() || before == after {
        return Ok(());
    }
    match (before, after) {
        (Some(b), Some(a)) if a > b => Ok(()),
        (None, Some(_)) => Err(Error::Forbidden("only a human can set a task's first grade".into())),
        _ => Err(Error::Forbidden("only a human can lower or clear a task's grade".into())),
    }
}

fn validate_priority(p: i64) -> Result<()> {
    if (1..=4).contains(&p) { Ok(()) } else { Err(Error::Invalid("priority must be 1 (urgent) to 4 (low)".into())) }
}

/// Agents may only finish work through review when the project requires it, and
/// only a human (or a delegated reviewer who didn't do the work) can approve.
async fn check_review(tx: &Tx, task: &Task, to: Status) -> Result<()> {
    let finishes = to == Status::Done || to == Status::Approved;
    if !finishes || tx.ctx.is_human() {
        return Ok(());
    }
    let project = load_project(&tx.conn, tx.ctx.workspace_id, &task.project_id.to_string(), true).await?;
    let in_review = matches!(task.status, Status::InReview | Status::Approved);
    let gated = to == Status::Approved || in_review || project.require_review;
    // A delegated reviewer approves like a human would, but not work it did: the
    // claim is released on the way to review, so the event log says who did it.
    if tx.ctx.agent_id.is_some_and(|a| project.reviewers.contains(&a)) {
        return if in_review && did_work(tx, task).await? { Err(Error::ReviewRequired) } else { Ok(()) };
    }
    if gated { Err(Error::ReviewRequired) } else { Ok(()) }
}

/// Whether the acting agent did the work now in review: it made the latest move into review,
/// or took the last claim before that move. Claims from earlier rounds don't count, so the
/// factory can still approve a task it once claimed only to park it. An undone send to review
/// still counts: the agent did the work either way.
async fn did_work(tx: &Tx, task: &Task) -> Result<bool> {
    let me = tx.ctx.actor_id().to_string();
    let mut rows = tx
        .conn
        .query(
            "WITH sent AS (
                 SELECT id, actor_id FROM events
                 WHERE task_id = ?1 AND undoes IS NULL AND entity = 'task'
                   AND json_extract(after, '$.status') = 'in_review'
                   AND COALESCE(json_extract(before, '$.status'), '') NOT IN ('in_review', 'approved')
                 ORDER BY id DESC LIMIT 1
             )
             SELECT 1 FROM sent WHERE sent.actor_id = ?2 OR (
                 SELECT actor_id FROM events
                 WHERE task_id = ?1 AND undoes IS NULL AND kind = 'claim.acquired' AND id < sent.id
                 ORDER BY id DESC LIMIT 1
             ) = ?2",
            libsql::params![task.id.to_string(), me],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// Agents can't touch a task another actor holds. Humans can, so they can always
/// rescue a task from a stuck agent.
pub(crate) async fn check_claim_access(tx: &Tx, task: &Task) -> Result<Option<Claim>> {
    let claim = live_claim(&tx.conn, task.id, tx.now).await?;
    if let Some(c) = &claim
        && c.holder_id != tx.ctx.actor_id()
        && !tx.ctx.is_human()
    {
        return Err(crate::claims::claim_held(&tx.conn, c).await);
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
    /// 1 to 4. Only a human can create a task with a grade.
    pub grade: Option<i64>,
    /// The day it's due. Optional when `due_at` is set: it's that time's day.
    pub due_on: Option<Date>,
    /// Times keep the offset they were written with ([`parse_at`](crate::parse_at)): without a
    /// zone, that offset decides the day.
    pub due_at: Option<Zoned>,
    pub due_tz: Option<String>,
    /// The day to work on it. Optional when `scheduled_at` is set: it's that time's day.
    pub scheduled_on: Option<Date>,
    pub scheduled_at: Option<Zoned>,
    pub scheduled_tz: Option<String>,
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
    /// `Some(None)` clears the grade. Agents can only raise an existing one.
    pub grade: Option<Option<i64>>,
    /// A new day drops any due time unless `due_at` comes with it; clearing it
    /// removes the deadline.
    pub due_on: Option<Option<Date>>,
    /// Clearing the time keeps the day.
    pub due_at: Option<Option<Zoned>>,
    pub due_tz: Option<Option<String>>,
    /// A new day drops any scheduled time unless `scheduled_at` comes with it;
    /// clearing it clears the time too.
    pub scheduled_on: Option<Option<Date>>,
    /// Clearing the time keeps the day.
    pub scheduled_at: Option<Option<Zoned>>,
    pub scheduled_tz: Option<Option<String>>,
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
    pub due_before: Option<Timestamp>,
    /// Only tasks with exactly this grade, or only graded / ungraded ones.
    pub grade: GradeFilter,
    /// Tasks scheduled on or before this day, so overdue plans still show up "today".
    pub scheduled_through: Option<Date>,
    /// `(path, value)` pairs matched against `metadata` as text, e.g. `("team", "web")`.
    pub metadata: Vec<(String, String)>,
    pub sort: TaskSort,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// Which grades [`Store::list_tasks`] and [`Store::ready`] return.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GradeFilter {
    #[default]
    Any,
    /// Has any grade.
    Graded,
    /// Has no grade.
    Ungraded,
    /// Has exactly this grade.
    Is(i64),
}

impl GradeFilter {
    /// Parses `graded`, `ungraded` or `1` to `4`.
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_lowercase().as_str() {
            "graded" => Ok(Self::Graded),
            "ungraded" | "none" => Ok(Self::Ungraded),
            n => match n.parse::<i64>() {
                Ok(g) => {
                    validate_grade(g)?;
                    Ok(Self::Is(g))
                }
                Err(_) => Err(Error::Invalid("grade filter must be 1 to 4, graded or ungraded".into())),
            },
        }
    }

    fn sql(self, col: &str) -> Option<String> {
        match self {
            Self::Any => None,
            Self::Graded => Some(format!(" AND {col} IS NOT NULL")),
            Self::Ungraded => Some(format!(" AND {col} IS NULL")),
            Self::Is(g) => Some(format!(" AND {col} = {g}")),
        }
    }
}

/// Order for [`Store::list_tasks`]. Parses from `priority`, `updated_at`,
/// `created_at`, with a leading `-` for newest first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TaskSort {
    /// Priority, then due date (undated last), then oldest first.
    #[default]
    Priority,
    UpdatedAt,
    UpdatedAtDesc,
    CreatedAt,
    CreatedAtDesc,
    /// Earliest scheduled day first; within a day, timed tasks in time order, then
    /// the rest by priority. Unscheduled last.
    Scheduled,
}

impl TaskSort {
    fn order_by(self) -> &'static str {
        match self {
            TaskSort::Priority => "priority, due_by IS NULL, due_by, created_at, id",
            TaskSort::UpdatedAt => "updated_at, id",
            TaskSort::UpdatedAtDesc => "updated_at DESC, id DESC",
            TaskSort::CreatedAt => "created_at, id",
            TaskSort::CreatedAtDesc => "created_at DESC, id DESC",
            TaskSort::Scheduled => {
                "scheduled_on IS NULL, scheduled_on, scheduled_at IS NULL, scheduled_at, priority, due_by IS NULL, due_by, created_at, id"
            }
        }
    }
}

impl std::str::FromStr for TaskSort {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "priority" => Ok(TaskSort::Priority),
            "updated_at" => Ok(TaskSort::UpdatedAt),
            "-updated_at" => Ok(TaskSort::UpdatedAtDesc),
            "created_at" => Ok(TaskSort::CreatedAt),
            "-created_at" => Ok(TaskSort::CreatedAtDesc),
            "scheduled" => Ok(TaskSort::Scheduled),
            _ => Err(Error::Invalid(format!("unknown sort: {s}"))),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SearchFilter {
    pub query: String,
    pub project: Option<String>,
    /// Empty means every status.
    pub statuses: Vec<Status>,
    pub limit: Option<i64>,
}

/// A search result: the task and where the query matched it.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SearchHit {
    #[serde(flatten)]
    pub task: Task,
    #[serde(rename = "match")]
    pub matched: SearchMatch,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SearchMatch {
    /// The first of title, description or a comment that matched.
    pub field: MatchField,
    /// A short excerpt around the match, `…` where text was cut.
    pub snippet: String,
    /// The newest matching comment, when `field` is `comment`.
    pub comment_id: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum MatchField {
    Title,
    Description,
    Comment,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
        if let Some(g) = new.grade {
            validate_grade(g)?;
        }
        let mut due_on = new.due_on;
        when::settle("due", &mut due_on, new.due_at.as_ref(), &new.due_tz, false)?;
        let due_at = new.due_at.as_ref().map(Zoned::timestamp);
        let due_by = when::deadline(due_on, due_at, new.due_tz.as_deref())?;
        let mut scheduled_on = new.scheduled_on;
        when::settle("scheduled", &mut scheduled_on, new.scheduled_at.as_ref(), &new.scheduled_tz, true)?;
        let scheduled_at = new.scheduled_at.as_ref().map(Zoned::timestamp);
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
            check_project_write(ctx, &project)?;
            check_grade(ctx, None, new.grade)?;
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
                Some(p) => Some(find_task(&tx.conn, ctx, p, false).await?.id),
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
                         parent_id, metadata, source, external_id, created_by, created_at, updated_at, scheduled_on, scheduled_at, scheduled_tz, due_on, due_by, grade)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?15, ?16, ?17, ?18, ?19, ?20, ?21) RETURNING {TASK_COLS}"
                    ),
                    libsql::params![
                        id.to_string(),
                        ws.to_string(),
                        project.id.to_string(),
                        format!("{}-{seq}", project.key),
                        title.clone(),
                        new.description.clone(),
                        priority,
                        opt_ts_value(due_at),
                        new.due_tz.clone(),
                        opt_uuid_value(parent_id),
                        metadata.to_string(),
                        new.source.clone(),
                        new.external_id.clone(),
                        ctx.actor_id().to_string(),
                        tx.now_str(),
                        opt_date_value(scheduled_on),
                        opt_ts_value(scheduled_at),
                        new.scheduled_tz.clone(),
                        opt_date_value(due_on),
                        opt_ts_value(due_by),
                        new.grade,
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
            let before = find_task(&tx.conn, ctx, task, false).await?;
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
            if let Some(g) = patch.grade {
                if let Some(g) = g {
                    validate_grade(g)?;
                }
                after.grade = g;
            }
            if let Some(d) = patch.due_on {
                after.due_on = d;
                after.due_at = None;
                if d.is_none() {
                    after.due_tz = None;
                }
            }
            if let Some(at) = &patch.due_at {
                after.due_at = at.as_ref().map(Zoned::timestamp);
                if at.is_some() && patch.due_on.is_none() {
                    after.due_on = None;
                }
            }
            let placed = patch.due_at.is_some() || patch.due_tz.is_some();
            if let Some(tz) = patch.due_tz {
                after.due_tz = tz;
            }
            if placed {
                let at = patch.due_at.clone().unwrap_or_else(|| after.due_at.map(when::utc));
                when::settle("due", &mut after.due_on, at.as_ref(), &after.due_tz, false)?;
            }
            if let Some(d) = patch.scheduled_on {
                after.scheduled_on = d;
                after.scheduled_at = None;
                after.scheduled_tz = None;
            }
            if let Some(at) = &patch.scheduled_at {
                after.scheduled_at = at.as_ref().map(Zoned::timestamp);
                if at.is_none() {
                    after.scheduled_tz = None;
                } else if patch.scheduled_on.is_none() {
                    after.scheduled_on = None;
                }
            }
            let placed = patch.scheduled_at.is_some() || patch.scheduled_tz.is_some();
            if let Some(tz) = patch.scheduled_tz {
                after.scheduled_tz = tz;
            }
            if placed {
                let at = patch.scheduled_at.clone().unwrap_or_else(|| after.scheduled_at.map(when::utc));
                when::settle("scheduled", &mut after.scheduled_on, at.as_ref(), &after.scheduled_tz, true)?;
            }
            if let Some(parent) = &patch.parent {
                after.parent_id = match parent {
                    Some(p) => {
                        let p = find_task(&tx.conn, ctx, p, false).await?;
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
            let before = find_task(&tx.conn, ctx, task, false).await?;
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
        find_task(&conn, ctx, task, false).await
    }

    pub async fn task_detail(&self, ctx: &Ctx, task: &str) -> Result<TaskDetail> {
        let conn = self.reader()?;
        let task = find_task(&conn, ctx, task, false).await?;
        let claim = live_claim(&conn, task.id, self.now()).await?;
        let blocked_by = unfinished_blockers(&conn, task.id).await?.iter().map(|t| visible_key(ctx, t)).collect();
        let rows = conn
            .query(
                &format!(
                    "SELECT {} FROM dependencies d JOIN tasks t ON t.id = d.blocked_id
                     WHERE d.blocker_id = ?1 AND t.deleted_at IS NULL ORDER BY t.key",
                    prefixed_cols("t")
                ),
                [task.id.to_string()],
            )
            .await?;
        let blocks = collect(rows, task_from_row).await?.iter().map(|t| visible_key(ctx, t)).collect();
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
            let project = find_project(&conn, ctx, p).await?;
            push(&mut sql, " AND project_id = ?", project.id.to_string().into());
        }
        if let Some(p) = &filter.parent {
            let parent = find_task(&conn, ctx, p, false).await?;
            push(&mut sql, " AND parent_id = ?", parent.id.to_string().into());
        }
        if let Some(d) = filter.due_before {
            push(&mut sql, " AND due_by IS NOT NULL AND due_by < ?", fmt_ts(d).into());
        }
        if let Some(d) = filter.scheduled_through {
            push(&mut sql, " AND scheduled_on IS NOT NULL AND scheduled_on <= ?", d.to_string().into());
        }
        if let Some(c) = filter.grade.sql("grade") {
            sql.push_str(&c);
        }
        for (path, value) in &filter.metadata {
            validate_path(path)?;
            push(&mut sql, &format!(" AND CAST(json_extract(metadata, '$.{path}') AS TEXT) = ?"), value.clone().into());
        }
        sql.push_str(&format!(" AND {}", ctx.scope_sql("project_id")));
        if filter.statuses.is_empty() {
            sql.push_str(" AND status NOT IN ('done', 'cancelled')");
        } else {
            sql.push_str(&format!(" AND status IN ({})", sql_list(&filter.statuses)));
        }
        sql.push_str(&format!(" ORDER BY {}", filter.sort.order_by()));
        push(&mut sql, " LIMIT ?", filter.limit.unwrap_or(100).into());
        push(&mut sql, " OFFSET ?", filter.offset.unwrap_or(0).into());
        collect(conn.query(&sql, params).await?, task_from_row).await
    }

    /// Open, unclaimed tasks with no unfinished blockers, by priority then due date.
    /// Expired claims are reaped first so their tasks come back.
    pub async fn ready(
        &self,
        ctx: &Ctx,
        project: Option<&str>,
        limit: Option<i64>,
        grade: GradeFilter,
    ) -> Result<Vec<Task>> {
        let conn = self.reader()?;
        self.write(ctx, async |tx| reap_expired(tx, None).await).await?;
        let project_id = match project {
            Some(p) => Some(find_project(&conn, ctx, p).await?.id),
            None => None,
        };
        let sql = format!(
            "SELECT {} FROM tasks t JOIN projects p ON p.id = t.project_id
             WHERE t.workspace_id = ?1 AND t.deleted_at IS NULL AND t.status = 'open'
               AND p.deleted_at IS NULL AND p.status = 'active'
               AND (?2 IS NULL OR t.project_id = ?2) AND {}{}
               AND NOT EXISTS (SELECT 1 FROM claims c WHERE c.task_id = t.id AND (c.expires_at IS NULL OR c.expires_at > ?3))
               AND NOT EXISTS (
                   SELECT 1 FROM dependencies d JOIN tasks b ON b.id = d.blocker_id
                   WHERE d.blocked_id = t.id AND b.deleted_at IS NULL AND b.status NOT IN ('done', 'cancelled'))
             ORDER BY t.priority, t.due_by IS NULL, t.due_by, t.created_at
             LIMIT ?4",
            prefixed_cols("t"),
            ctx.scope_sql("t.project_id"),
            grade.sql("t.grade").unwrap_or_default()
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
        collect(rows, task_from_row).await
    }

    /// Full-text search over titles, descriptions and comments.
    pub async fn search(&self, ctx: &Ctx, query: &str, limit: Option<i64>) -> Result<Vec<Task>> {
        let filter = SearchFilter { query: query.into(), limit, ..Default::default() };
        Ok(self.search_hits(ctx, filter).await?.into_iter().map(|h| h.task).collect())
    }

    /// Full-text search over titles, descriptions and comments, open work first,
    /// then most recently updated. Each hit says where it matched.
    pub async fn search_hits(&self, ctx: &Ctx, filter: SearchFilter) -> Result<Vec<SearchHit>> {
        let conn = self.reader()?;
        let q = fts_query(&filter.query);
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let project_id = match &filter.project {
            Some(p) => Some(find_project(&conn, ctx, p).await?.id.to_string()),
            None => None,
        };
        let statuses = if filter.statuses.is_empty() {
            String::new()
        } else {
            format!(" AND t.status IN ({})", sql_list(&filter.statuses))
        };
        let sql = format!(
            "SELECT {TASK_COLS} FROM tasks t WHERE t.workspace_id = ?1 AND t.deleted_at IS NULL AND {}{statuses}
               AND (?4 IS NULL OR t.project_id = ?4) AND (
                 t.key = upper(?5)
                 OR t.rowid IN (SELECT rowid FROM tasks_fts WHERE tasks_fts MATCH ?2)
                 OR t.id IN (SELECT c.task_id FROM comments c WHERE c.deleted_at IS NULL
                             AND c.rowid IN (SELECT rowid FROM comments_fts WHERE comments_fts MATCH ?2)))
             ORDER BY t.key = upper(?5) DESC, t.status IN ('done', 'cancelled'), t.updated_at DESC LIMIT ?3",
            ctx.scope_sql("t.project_id")
        );
        let rows = conn
            .query(
                &sql,
                libsql::params![
                    ctx.workspace_id.to_string(),
                    q.clone(),
                    filter.limit.unwrap_or(20),
                    project_id,
                    filter.query.trim()
                ],
            )
            .await?;
        let tasks = collect(rows, task_from_row).await?;
        let ids = json_ids(&tasks.iter().map(|t| t.id).collect::<Vec<_>>())?;

        // snippet() wraps matched terms in these markers, which tell us which column
        // matched; they're stripped before the snippet goes out.
        let mut matches: std::collections::HashMap<String, SearchMatch> = std::collections::HashMap::new();
        let mut rows = conn
            .query(
                &format!(
                    "SELECT t.id, snippet(tasks_fts, 0, '{MARK_ON}', '{MARK_OFF}', '…', 16),
                            snippet(tasks_fts, 1, '{MARK_ON}', '{MARK_OFF}', '…', 16)
                     FROM tasks_fts JOIN tasks t ON t.rowid = tasks_fts.rowid
                     WHERE tasks_fts MATCH ?1 AND t.id IN (SELECT value FROM json_each(?2))"
                ),
                libsql::params![q.clone(), ids.clone()],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            let (id, title, description): (String, String, String) = (row.get(0)?, row.get(1)?, row.get(2)?);
            let m = if title.contains(MARK_ON) {
                SearchMatch { field: MatchField::Title, snippet: unmark(&title), comment_id: None }
            } else {
                SearchMatch { field: MatchField::Description, snippet: unmark(&description), comment_id: None }
            };
            matches.insert(id, m);
        }
        let mut rows = conn
            .query(
                &format!(
                    "SELECT c.task_id, c.id, snippet(comments_fts, 0, '{MARK_ON}', '{MARK_OFF}', '…', 16)
                     FROM comments_fts JOIN comments c ON c.rowid = comments_fts.rowid
                     WHERE comments_fts MATCH ?1 AND c.deleted_at IS NULL
                       AND c.task_id IN (SELECT value FROM json_each(?2))
                     ORDER BY c.created_at DESC"
                ),
                libsql::params![q, ids],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            let task_id: String = row.get(0)?;
            matches.entry(task_id).or_insert(SearchMatch {
                field: MatchField::Comment,
                snippet: unmark(&row.get::<String>(2)?),
                comment_id: Some(get_uuid(&row, 1)?),
            });
        }
        Ok(tasks
            .into_iter()
            .map(|task| {
                let matched = matches.remove(&task.id.to_string()).unwrap_or_else(|| SearchMatch {
                    field: MatchField::Title,
                    snippet: task.title.clone(),
                    comment_id: None,
                });
                SearchHit { task, matched }
            })
            .collect())
    }
}

/// Statuses as a SQL list. They're fixed identifiers, so inlining them is safe.
fn sql_list(statuses: &[Status]) -> String {
    statuses.iter().map(|s| format!("'{}'", s.as_str())).collect::<Vec<_>>().join(", ")
}

const MARK_ON: char = '\u{2}';
const MARK_OFF: char = '\u{3}';

fn unmark(s: &str) -> String {
    s.replace([MARK_ON, MARK_OFF], "")
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
