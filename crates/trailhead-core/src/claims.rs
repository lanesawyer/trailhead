use std::collections::HashMap;

use libsql::{Connection, Row};
use serde::Serialize;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::comments::latest_handoff;
use crate::db::{collect, first, fmt_ts, get_enum, get_opt_ts, get_ts, get_uuid, json_ids, opt_ts_value};
use crate::deps::{unfinished_blockers, visible_key};
use crate::error::{Error, Result};
use crate::identity::load_workspace;
use crate::model::{Claim, Comment, Entity, Status, Task};
use crate::projects::{load_project, validate_ttl};
use crate::store::{Ctx, EventIn, Store, Tx};
use crate::tasks::{find_task, load_task, update_status};

const CLAIM_COLS: &str = "task_id, workspace_id, holder_id, on_behalf_of, claimed_at, expires_at";

fn claim_from_row(row: &Row) -> Result<Claim> {
    Ok(Claim {
        task_id: get_uuid(row, 0)?,
        workspace_id: get_uuid(row, 1)?,
        holder_id: get_uuid(row, 2)?,
        on_behalf_of: get_uuid(row, 3)?,
        claimed_at: get_ts(row, 4)?,
        expires_at: get_opt_ts(row, 5)?,
    })
}

/// The claim row, expired or not.
pub(crate) async fn any_claim(conn: &Connection, task_id: Uuid) -> Result<Option<Claim>> {
    let rows =
        conn.query(&format!("SELECT {CLAIM_COLS} FROM claims WHERE task_id = ?1"), [task_id.to_string()]).await?;
    first(rows, claim_from_row).await
}

/// A `claim_held` error naming the holder, so people and agents can tell who to ask.
pub(crate) async fn claim_held(conn: &Connection, c: &Claim) -> Error {
    let id = c.holder_id.to_string();
    let name = async {
        let mut rows = conn
            .query("SELECT name FROM agents WHERE id = ?1 UNION ALL SELECT name FROM users WHERE id = ?1", [id.clone()])
            .await?;
        Ok::<_, Error>(match rows.next().await? {
            Some(row) => Some(row.get::<String>(0)?),
            None => None,
        })
    }
    .await;
    Error::ClaimHeld {
        holder: name.ok().flatten().unwrap_or_else(|| id.clone()),
        holder_id: id,
        expires_at: c.expires_at.map(fmt_ts),
    }
}

pub(crate) async fn live_claim(conn: &Connection, task_id: Uuid, now: OffsetDateTime) -> Result<Option<Claim>> {
    Ok(any_claim(conn, task_id).await?.filter(|c| c.is_live(now)))
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
                opt_ts_value(claim.expires_at),
            ],
        )
        .await?;
    Ok(())
}

/// Deletes expired claims and puts their in-progress tasks back to open. The
/// events are attributed to the lapsed holder.
pub(crate) async fn reap_expired(tx: &Tx, task_id: Option<Uuid>) -> Result<usize> {
    let rows = tx
        .conn
        .query(
            &format!(
                "SELECT {CLAIM_COLS} FROM claims WHERE workspace_id = ?1 AND expires_at <= ?2 AND (?3 IS NULL OR task_id = ?3)"
            ),
            libsql::params![tx.ws(), tx.now_str(), task_id.map(|t| t.to_string())],
        )
        .await?;
    let expired = collect(rows, claim_from_row).await?;
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

/// When a new or renewed claim lapses. People's claims don't, unless they pass
/// a TTL: leases exist so a crashed agent's work comes back, and anyone human
/// can release a forgotten claim by hand.
async fn expiry(tx: &Tx, task: &Task, ttl: Option<i64>) -> Result<Option<OffsetDateTime>> {
    if ttl.is_none() && tx.ctx.is_human() {
        return Ok(None);
    }
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
    Ok(Some(tx.now + Duration::seconds(ttl)))
}

/// Tells an agent that renews a lease it no longer holds what happened, so it
/// stops working on a task someone else may now own.
async fn lost_or_never_held(tx: &Tx, task: &Task) -> Result<Error> {
    let me = tx.ctx.actor_id().to_string();
    if let Some(c) = any_claim(&tx.conn, task.id).await?
        && c.holder_id.to_string() == me
    {
        let at = c.expires_at.map_or_else(|| "an unknown time".into(), fmt_ts);
        return Ok(Error::ClaimLost(format!("it expired at {at}")));
    }
    let mut rows = tx
        .conn
        .query(
            "SELECT kind, actor_id, at FROM events WHERE entity = 'claim' AND entity_id = ?1 ORDER BY id DESC LIMIT 1",
            [task.id.to_string()],
        )
        .await?;
    let Some(row) = rows.next().await? else { return Ok(Error::NotClaimHolder) };
    let (kind, actor, at): (String, String, String) = (row.get(0)?, row.get(1)?, row.get(2)?);
    let mut rows = tx
        .conn
        .query(
            "SELECT 1 FROM events WHERE entity = 'claim' AND entity_id = ?1 AND kind = 'claim.acquired' AND actor_id = ?2 LIMIT 1",
            [task.id.to_string(), me.clone()],
        )
        .await?;
    let held_before = rows.next().await?.is_some();
    Ok(match kind.as_str() {
        "claim.expired" if actor == me => Error::ClaimLost(format!("it expired at {at}")),
        "claim.acquired" | "claim.renewed" if held_before && actor != me => {
            let taker = match live_claim(&tx.conn, task.id, tx.now).await? {
                Some(c) => match claim_held(&tx.conn, &c).await {
                    Error::ClaimHeld { holder, .. } => holder,
                    _ => actor,
                },
                None => actor,
            };
            Error::ClaimLost(format!("{taker} claimed it at {at}"))
        }
        "claim.released" if held_before && actor != me => Error::ClaimLost(format!("someone else released it at {at}")),
        _ => Error::NotClaimHolder,
    })
}

/// A claim the caller holds, with enough of the task to act on it.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct HeldClaim {
    pub key: String,
    pub title: String,
    pub status: Status,
    pub claim: Claim,
}

/// A live claim with its holder's name, for labelling task lists.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NamedClaim {
    #[serde(flatten)]
    pub claim: Claim,
    /// The holder's user or agent name, e.g. `lane` or `claude/openapi`.
    pub holder: String,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
            let task = find_task(&tx.conn, ctx, task, false).await?;
            reap_expired(tx, Some(task.id)).await?;
            let task = load_task(&tx.conn, ctx.workspace_id, &task.id.to_string(), false).await?;
            let existing = live_claim(&tx.conn, task.id, tx.now).await?;
            let mine = existing.as_ref().is_some_and(|c| c.holder_id == ctx.actor_id());
            if let Some(c) = &existing
                && !mine
            {
                return Err(claim_held(&tx.conn, c).await);
            }
            let claimable = task.status == Status::Open || (mine && task.status == Status::InProgress);
            if !claimable {
                return Err(Error::InvalidTransition { from: task.status, to: Status::InProgress });
            }
            let blockers = unfinished_blockers(&tx.conn, task.id).await?;
            if !blockers.is_empty() {
                return Err(Error::BlockedBy(blockers.iter().map(|t| visible_key(ctx, t)).collect()));
            }
            let expires_at = expiry(tx, &task, ttl).await?;
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
                         WHERE (claims.expires_at IS NOT NULL AND claims.expires_at <= ?5) OR claims.holder_id = excluded.holder_id
                         RETURNING {CLAIM_COLS}"
                    ),
                    libsql::params![
                        task.id.to_string(),
                        tx.ws(),
                        ctx.actor_id().to_string(),
                        ctx.user_id.to_string(),
                        tx.now_str(),
                        opt_ts_value(expires_at),
                    ],
                )
                .await?;
            let Some(row) = rows.next().await? else {
                return Err(match live_claim(&tx.conn, task.id, tx.now).await? {
                    Some(c) => claim_held(&tx.conn, &c).await,
                    None => Error::Corrupt("claim lost a race it can't lose under the write lock".into()),
                });
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

    /// The live claims on these tasks, keyed by task id. Tasks without one are absent.
    pub async fn live_claims(&self, ctx: &Ctx, task_ids: &[Uuid]) -> Result<HashMap<Uuid, NamedClaim>> {
        let conn = self.reader()?;
        let rows = conn
            .query(
                &format!(
                    "SELECT {CLAIM_COLS}, COALESCE((SELECT name FROM agents WHERE id = holder_id),
                         (SELECT name FROM users WHERE id = holder_id), holder_id)
                     FROM claims WHERE workspace_id = ?1 AND (expires_at IS NULL OR expires_at > ?2)
                       AND task_id IN (SELECT value FROM json_each(?3))"
                ),
                libsql::params![ctx.workspace_id.to_string(), fmt_ts(self.now()), json_ids(task_ids)?],
            )
            .await?;
        let named = collect(rows, |row| Ok(NamedClaim { claim: claim_from_row(row)?, holder: row.get(6)? })).await?;
        Ok(named.into_iter().map(|n| (n.claim.task_id, n)).collect())
    }

    /// Live claims the caller holds, soonest to expire first, then those that
    /// never expire, oldest first.
    pub async fn my_claims(&self, ctx: &Ctx) -> Result<Vec<HeldClaim>> {
        let conn = self.reader()?;
        let rows = conn
            .query(
                &format!(
                    // Claim columns first, so claim_from_row reads them as usual.
                    "SELECT {}, t.key, t.title, t.status FROM claims c JOIN tasks t ON t.id = c.task_id
                     WHERE c.workspace_id = ?1 AND c.holder_id = ?2 AND (c.expires_at IS NULL OR c.expires_at > ?3)
                       AND t.deleted_at IS NULL
                     ORDER BY c.expires_at IS NULL, c.expires_at, c.claimed_at",
                    CLAIM_COLS.split(", ").map(|c| format!("c.{c}")).collect::<Vec<_>>().join(", ")
                ),
                [ctx.workspace_id.to_string(), ctx.actor_id().to_string(), fmt_ts(self.now())],
            )
            .await?;
        collect(rows, |row| {
            Ok(HeldClaim {
                claim: claim_from_row(row)?,
                key: row.get(6)?,
                title: row.get(7)?,
                status: get_enum(row, 8)?,
            })
        })
        .await
    }

    /// Heartbeat. Renew roughly every third of the TTL. Renewing a claim that
    /// never expires, without a TTL, changes nothing.
    pub async fn renew(&self, ctx: &Ctx, task: &str, ttl: Option<i64>) -> Result<Claim> {
        self.write(ctx, async |tx| {
            let task = find_task(&tx.conn, ctx, task, false).await?;
            let before = match live_claim(&tx.conn, task.id, tx.now).await? {
                Some(c) if c.holder_id == ctx.actor_id() => c,
                _ => return Err(lost_or_never_held(tx, &task).await?),
            };
            let after = Claim { expires_at: expiry(tx, &task, ttl).await?, ..before.clone() };
            if after.expires_at.is_none() && before.expires_at.is_none() {
                return Ok(before);
            }
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
            let task = find_task(&tx.conn, ctx, task, false).await?;
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
