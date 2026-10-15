use libsql::{Connection, Row};
use serde::Serialize;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::comments::latest_handoff;
use crate::db::{fmt_ts, get_ts, get_uuid};
use crate::deps::unfinished_blockers;
use crate::error::{Error, Result};
use crate::identity::load_workspace;
use crate::model::{Claim, Comment, Entity, Status, Task};
use crate::projects::{load_project, validate_ttl};
use crate::store::{Ctx, EventIn, Store, Tx};
use crate::tasks::{load_task, update_status};

const CLAIM_COLS: &str = "task_id, workspace_id, holder_id, on_behalf_of, claimed_at, expires_at";

fn claim_from_row(row: &Row) -> Result<Claim> {
    Ok(Claim {
        task_id: get_uuid(row, 0)?,
        workspace_id: get_uuid(row, 1)?,
        holder_id: get_uuid(row, 2)?,
        on_behalf_of: get_uuid(row, 3)?,
        claimed_at: get_ts(row, 4)?,
        expires_at: get_ts(row, 5)?,
    })
}

/// The claim row, expired or not.
pub(crate) async fn any_claim(conn: &Connection, task_id: Uuid) -> Result<Option<Claim>> {
    let mut rows =
        conn.query(&format!("SELECT {CLAIM_COLS} FROM claims WHERE task_id = ?1"), [task_id.to_string()]).await?;
    rows.next().await?.map(|r| claim_from_row(&r)).transpose()
}

pub(crate) async fn live_claim(conn: &Connection, task_id: Uuid, now: OffsetDateTime) -> Result<Option<Claim>> {
    Ok(any_claim(conn, task_id).await?.filter(|c| c.expires_at > now))
}

pub(crate) async fn drop_claim(tx: &Tx, claim: &Claim, kind: &str) -> Result<()> {
    tx.conn.execute("DELETE FROM claims WHERE task_id = ?1", [claim.task_id.to_string()]).await?;
    tx.emit(EventIn {
        entity: Entity::Claim,
        entity_id: claim.task_id.to_string(),
        task_id: Some(claim.task_id),
        kind,
        before: Some(serde_json::to_value(claim)?),
        after: None,
        undoes: None,
        actor: (kind == "claim.expired").then_some((claim.holder_id, claim.on_behalf_of)),
    })
    .await?;
    Ok(())
}

pub(crate) async fn put_claim(tx: &Tx, claim: &Claim) -> Result<()> {
    tx.conn
        .execute(
            &format!("INSERT OR REPLACE INTO claims ({CLAIM_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6)"),
            libsql::params![
                claim.task_id.to_string(),
                claim.workspace_id.to_string(),
                claim.holder_id.to_string(),
                claim.on_behalf_of.to_string(),
                fmt_ts(claim.claimed_at),
                fmt_ts(claim.expires_at),
            ],
        )
        .await?;
    Ok(())
}

/// Deletes expired claims and puts their in-progress tasks back to open. The
/// events are attributed to the lapsed holder.
pub(crate) async fn reap_expired(tx: &Tx, task_id: Option<Uuid>) -> Result<usize> {
    let mut rows = tx
        .conn
        .query(
            &format!(
                "SELECT {CLAIM_COLS} FROM claims WHERE workspace_id = ?1 AND expires_at <= ?2 AND (?3 IS NULL OR task_id = ?3)"
            ),
            libsql::params![tx.ws(), tx.now_str(), task_id.map(|t| t.to_string())],
        )
        .await?;
    let mut expired = Vec::new();
    while let Some(row) = rows.next().await? {
        expired.push(claim_from_row(&row)?);
    }
    for claim in &expired {
        drop_claim(tx, claim, "claim.expired").await?;
        let task = load_task(&tx.conn, claim.workspace_id, &claim.task_id.to_string(), true).await?;
        if task.status == Status::InProgress {
            let after = crate::tasks::save_task(tx, &task, Task { status: Status::Open, ..task.clone() }).await?;
            tx.emit(EventIn {
                entity: Entity::Task,
                entity_id: task.id.to_string(),
                task_id: Some(task.id),
                kind: "task.updated",
                before: Some(serde_json::to_value(&task)?),
                after: Some(serde_json::to_value(&after)?),
                undoes: None,
                actor: Some((claim.holder_id, claim.on_behalf_of)),
            })
            .await?;
        }
    }
    Ok(expired.len())
}

async fn resolve_ttl(tx: &Tx, task: &Task, ttl: Option<i64>) -> Result<i64> {
    let ttl = match ttl {
        Some(t) => t,
        None => {
            match load_project(&tx.conn, tx.ctx.workspace_id, &task.project_id.to_string(), true).await?.claim_ttl {
                Some(t) => t,
                None => load_workspace(&tx.conn, tx.ctx.workspace_id).await?.default_claim_ttl,
            }
        }
    };
    validate_ttl(ttl)?;
    Ok(ttl)
}

#[derive(Debug, Clone, Serialize)]
pub struct ClaimResult {
    pub task: Task,
    pub claim: Claim,
    /// The last handoff left on this task, so the next worker picks up where it stopped.
    pub handoff: Option<Comment>,
}

impl Store {
    /// Takes a lease on a task and moves it to `in_progress`. Claiming a task you
    /// already hold extends the lease.
    pub async fn claim(&self, ctx: &Ctx, task: &str, ttl: Option<i64>) -> Result<ClaimResult> {
        self.write(ctx, async |tx| {
            let task = load_task(&tx.conn, ctx.workspace_id, task, false).await?;
            reap_expired(tx, Some(task.id)).await?;
            let task = load_task(&tx.conn, ctx.workspace_id, &task.id.to_string(), false).await?;
            let existing = live_claim(&tx.conn, task.id, tx.now).await?;
            let mine = existing.as_ref().is_some_and(|c| c.holder_id == ctx.actor_id());
            if let Some(c) = &existing
                && !mine
            {
                return Err(Error::ClaimHeld { holder: c.holder_id.to_string(), expires_at: fmt_ts(c.expires_at) });
            }
            let claimable = task.status == Status::Open || (mine && task.status == Status::InProgress);
            if !claimable {
                return Err(Error::InvalidTransition { from: task.status, to: Status::InProgress });
            }
            let blockers = unfinished_blockers(&tx.conn, task.id).await?;
            if !blockers.is_empty() {
                return Err(Error::BlockedBy(blockers.into_iter().map(|t| t.key).collect()));
            }
            let ttl = resolve_ttl(tx, &task, ttl).await?;
            let expires_at = tx.now + Duration::seconds(ttl);
            // The WHERE guard makes the claim itself atomic: a live claim held by
            // someone else can never be overwritten, whatever the caller checked.
            let mut rows = tx
                .conn
                .query(
                    &format!(
                        "INSERT INTO claims ({CLAIM_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                         ON CONFLICT (task_id) DO UPDATE SET holder_id = excluded.holder_id,
                             on_behalf_of = excluded.on_behalf_of, claimed_at = excluded.claimed_at,
                             expires_at = excluded.expires_at
                         WHERE claims.expires_at <= ?5 OR claims.holder_id = excluded.holder_id
                         RETURNING {CLAIM_COLS}"
                    ),
                    libsql::params![
                        task.id.to_string(),
                        tx.ws(),
                        ctx.actor_id().to_string(),
                        ctx.user_id.to_string(),
                        tx.now_str(),
                        fmt_ts(expires_at),
                    ],
                )
                .await?;
            let Some(row) = rows.next().await? else {
                return Err(Error::ClaimHeld { holder: "another actor".into(), expires_at: String::new() });
            };
            let claim = claim_from_row(&row)?;
            tx.emit_change(
                Entity::Claim,
                task.id.to_string(),
                Some(task.id),
                if mine { "claim.renewed" } else { "claim.acquired" },
                existing.as_ref(),
                Some(&claim),
            )
            .await?;
            let task = if task.status == Status::InProgress {
                task
            } else {
                update_status(tx, &task, Status::InProgress, "task.updated").await?
            };
            let handoff = latest_handoff(&tx.conn, task.id).await?;
            Ok(ClaimResult { task, claim, handoff })
        })
        .await
    }

    /// Heartbeat. Renew roughly every third of the TTL.
    pub async fn renew(&self, ctx: &Ctx, task: &str, ttl: Option<i64>) -> Result<Claim> {
        self.write(ctx, async |tx| {
            let task = load_task(&tx.conn, ctx.workspace_id, task, false).await?;
            let Some(before) = live_claim(&tx.conn, task.id, tx.now).await? else {
                return Err(Error::NotClaimHolder);
            };
            if before.holder_id != ctx.actor_id() {
                return Err(Error::NotClaimHolder);
            }
            let ttl = resolve_ttl(tx, &task, ttl).await?;
            let after = Claim { expires_at: tx.now + Duration::seconds(ttl), ..before.clone() };
            put_claim(tx, &after).await?;
            tx.emit_change(
                Entity::Claim,
                task.id.to_string(),
                Some(task.id),
                "claim.renewed",
                Some(&before),
                Some(&after),
            )
            .await?;
            Ok(after)
        })
        .await
    }

    /// Gives a task back. An `in_progress` task returns to `open`. Humans can release
    /// anyone's claim.
    pub async fn release(&self, ctx: &Ctx, task: &str) -> Result<Task> {
        self.write(ctx, async |tx| {
            let task = load_task(&tx.conn, ctx.workspace_id, task, false).await?;
            let Some(claim) = live_claim(&tx.conn, task.id, tx.now).await? else {
                return Err(Error::NotClaimHolder);
            };
            if claim.holder_id != ctx.actor_id() && !ctx.is_human() {
                return Err(Error::NotClaimHolder);
            }
            drop_claim(tx, &claim, "claim.released").await?;
            if task.status == Status::InProgress {
                update_status(tx, &task, Status::Open, "task.updated").await
            } else {
                Ok(task)
            }
        })
        .await
    }

    /// Frees every expired claim in the workspace. `ready` and `claim` do this lazily;
    /// a server can also call it on a timer so events fire promptly.
    pub async fn reap_expired_claims(&self, ctx: &Ctx) -> Result<usize> {
        self.write(ctx, async |tx| reap_expired(tx, None).await).await
    }
}
