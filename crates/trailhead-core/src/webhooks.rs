//! Outbound webhooks. Each webhook keeps a cursor into the events table, so
//! delivery is ordered, at-least-once and survives restarts (the outbox
//! pattern). The server does the HTTP; this module owns the state.

use hmac::{Hmac, KeyInit, Mac};
use libsql::Row;
use serde::Serialize;
use sha2::Sha256;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::db::{collect, fmt_ts, get_opt_ts, get_ts, get_uuid};
use crate::error::{Error, Result};
use crate::events::{EVENT_COLS, event_from_row};
use crate::model::Event;
use crate::store::{Ctx, Store};

/// Consecutive failures before a webhook is switched off.
pub const MAX_FAILURES: i64 = 20;
const SECRET_PREFIX: &str = "whsec_";

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Webhook {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub owner_user_id: Uuid,
    pub url: String,
    /// Event kind prefixes to send, e.g. `task.` or `comment.created`. Empty means every event.
    pub event_kinds: Vec<String>,
    /// The last event id delivered (or skipped as not matching).
    pub cursor: i64,
    pub failure_count: i64,
    #[serde(with = "time::serde::rfc3339::option")]
    pub next_attempt_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Set after too many consecutive failures, or by hand. Disabled webhooks get nothing.
    #[serde(with = "time::serde::rfc3339::option")]
    pub disabled_at: Option<OffsetDateTime>,
}

impl Webhook {
    pub fn wants(&self, kind: &str) -> bool {
        self.event_kinds.is_empty() || self.event_kinds.iter().any(|k| kind.starts_with(k.as_str()))
    }
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct WebhookDelivery {
    pub id: i64,
    pub webhook_id: Uuid,
    pub event_id: Option<i64>,
    /// The receiver's HTTP status, if it answered.
    pub status_code: Option<i64>,
    /// Set when the attempt failed.
    pub error: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

const WEBHOOK_COLS: &str = "id, workspace_id, owner_user_id, url, event_kinds, cursor, failure_count, next_attempt_at, created_at, disabled_at, secret";

fn webhook_from_row(row: &Row) -> Result<(Webhook, String)> {
    let kinds: String = row.get(4)?;
    let hook = Webhook {
        id: get_uuid(row, 0)?,
        workspace_id: get_uuid(row, 1)?,
        owner_user_id: get_uuid(row, 2)?,
        url: row.get(3)?,
        event_kinds: serde_json::from_str(&kinds)?,
        cursor: row.get(5)?,
        failure_count: row.get(6)?,
        next_attempt_at: get_opt_ts(row, 7)?,
        created_at: get_ts(row, 8)?,
        disabled_at: get_opt_ts(row, 9)?,
    };
    Ok((hook, row.get(10)?))
}

/// `t=<unix seconds>,v1=<hex HMAC-SHA256 of "<t>.<body>">`, sent as
/// `Trailhead-Signature`. Receivers recompute it with their secret and should
/// reject old timestamps to stop replays.
pub fn webhook_signature(secret: &str, timestamp: i64, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes any key length");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    format!("t={timestamp},v1={}", hex::encode(mac.finalize().into_bytes()))
}

/// Waits 2^n seconds after the nth consecutive failure, capped at an hour.
fn backoff(failures: i64) -> Duration {
    Duration::seconds(1i64 << failures.clamp(0, 12)).min(Duration::hours(1))
}

fn require_admin(ctx: &Ctx) -> Result<()> {
    if !ctx.is_human() || ctx.projects.is_some() {
        return Err(Error::Forbidden("only a human with an unscoped token can manage webhooks".into()));
    }
    Ok(())
}

impl Store {
    /// Registers a webhook that receives events from now on. The signing secret is
    /// only ever returned here.
    pub async fn create_webhook(&self, ctx: &Ctx, url: &str, event_kinds: Vec<String>) -> Result<(Webhook, String)> {
        require_admin(ctx)?;
        let url = url.trim();
        if !(url.starts_with("https://") || url.starts_with("http://")) || url.len() > 2048 {
            return Err(Error::Invalid("webhook url must be http(s)".into()));
        }
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| Error::Corrupt(format!("no randomness: {e}")))?;
        let secret = format!("{SECRET_PREFIX}{}", hex::encode(bytes));
        let kinds = event_kinds.into_iter().map(|k| k.trim().to_string()).filter(|k| !k.is_empty()).collect::<Vec<_>>();
        let cursor = self.latest_event_id(ctx).await?;
        self.write_quiet(ctx, async |tx| {
            let mut rows = tx
                .conn
                .query(
                    &format!(
                        "INSERT INTO webhooks (id, workspace_id, owner_user_id, url, event_kinds, secret, cursor, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) RETURNING {WEBHOOK_COLS}"
                    ),
                    libsql::params![
                        Uuid::now_v7().to_string(),
                        tx.ws(),
                        ctx.user_id.to_string(),
                        url,
                        serde_json::to_string(&kinds)?,
                        secret.clone(),
                        cursor,
                        tx.now_str(),
                    ],
                )
                .await?;
            Ok((webhook_from_row(&rows.next().await?.expect("RETURNING yields a row"))?.0, secret.clone()))
        })
        .await
    }

    pub async fn list_webhooks(&self, ctx: &Ctx) -> Result<Vec<Webhook>> {
        require_admin(ctx)?;
        let conn = self.reader()?;
        let rows = conn
            .query(
                &format!("SELECT {WEBHOOK_COLS} FROM webhooks WHERE workspace_id = ?1 ORDER BY created_at"),
                [ctx.workspace_id.to_string()],
            )
            .await?;
        collect(rows, |row| Ok(webhook_from_row(row)?.0)).await
    }

    pub async fn delete_webhook(&self, ctx: &Ctx, id: Uuid) -> Result<()> {
        require_admin(ctx)?;
        let deleted = self
            .write_quiet(ctx, async |tx| {
                Ok(tx
                    .conn
                    .execute("DELETE FROM webhooks WHERE id = ?1 AND workspace_id = ?2", [id.to_string(), tx.ws()])
                    .await?)
            })
            .await?;
        if deleted == 0 { Err(Error::NotFound(format!("webhook {id}"))) } else { Ok(()) }
    }

    /// Recent delivery attempts, newest first.
    pub async fn webhook_deliveries(&self, ctx: &Ctx, id: Uuid, limit: Option<i64>) -> Result<Vec<WebhookDelivery>> {
        require_admin(ctx)?;
        let conn = self.reader()?;
        let rows = conn
            .query(
                "SELECT d.id, d.webhook_id, d.event_id, d.status_code, d.error, d.at FROM webhook_deliveries d
                 JOIN webhooks w ON w.id = d.webhook_id
                 WHERE d.webhook_id = ?1 AND w.workspace_id = ?2 ORDER BY d.id DESC LIMIT ?3",
                libsql::params![id.to_string(), ctx.workspace_id.to_string(), limit.unwrap_or(50)],
            )
            .await?;
        collect(rows, |row| {
            Ok(WebhookDelivery {
                id: row.get(0)?,
                webhook_id: get_uuid(row, 1)?,
                event_id: row.get(2)?,
                status_code: row.get(3)?,
                error: row.get(4)?,
                at: get_ts(row, 5)?,
            })
        })
        .await
    }

    /// Enabled webhooks whose backoff has passed, with their secrets, across every
    /// workspace. For the delivery loop.
    pub async fn webhooks_due(&self) -> Result<Vec<(Webhook, String)>> {
        let conn = self.reader()?;
        let rows = conn
            .query(
                &format!(
                    "SELECT {WEBHOOK_COLS} FROM webhooks WHERE disabled_at IS NULL
                     AND (next_attempt_at IS NULL OR next_attempt_at <= ?1)"
                ),
                [fmt_ts(self.now())],
            )
            .await?;
        collect(rows, webhook_from_row).await
    }

    /// The next events after a webhook's cursor, matching or not, oldest first.
    pub async fn webhook_backlog(&self, hook: &Webhook, limit: i64) -> Result<Vec<Event>> {
        let conn = self.reader()?;
        let rows = conn
            .query(
                &format!("SELECT {EVENT_COLS} FROM events WHERE workspace_id = ?1 AND id > ?2 ORDER BY id LIMIT ?3"),
                libsql::params![hook.workspace_id.to_string(), hook.cursor, limit],
            )
            .await?;
        collect(rows, event_from_row).await
    }

    /// Moves the cursor past `event_id` after a successful delivery (`status`
    /// set, which is logged) or a skipped non-matching event, and clears any
    /// backoff.
    pub async fn webhook_advance(&self, hook: &Webhook, event_id: i64, status: Option<u16>) -> Result<()> {
        let ctx = Ctx::user(hook.workspace_id, hook.owner_user_id);
        self.write_quiet(&ctx, async |tx| {
            tx.conn
                .execute(
                    "UPDATE webhooks SET cursor = max(cursor, ?1), failure_count = 0, next_attempt_at = NULL WHERE id = ?2",
                    libsql::params![event_id, hook.id.to_string()],
                )
                .await?;
            if let Some(code) = status {
                tx.conn
                    .execute(
                        "INSERT INTO webhook_deliveries (webhook_id, event_id, status_code, at) VALUES (?1, ?2, ?3, ?4)",
                        libsql::params![hook.id.to_string(), event_id, code as i64, tx.now_str()],
                    )
                    .await?;
            }
            Ok(())
        })
        .await
    }

    /// Logs a failed attempt and backs off; disables the webhook after
    /// `MAX_FAILURES` in a row. The cursor stays put so the event is retried.
    pub async fn webhook_failed(&self, hook: &Webhook, event_id: i64, status: Option<u16>, error: &str) -> Result<()> {
        let ctx = Ctx::user(hook.workspace_id, hook.owner_user_id);
        let failures = hook.failure_count + 1;
        self.write_quiet(&ctx, async |tx| {
            let disabled = (failures >= MAX_FAILURES).then(|| tx.now_str());
            tx.conn
                .execute(
                    "UPDATE webhooks SET failure_count = ?1, next_attempt_at = ?2, disabled_at = coalesce(disabled_at, ?3)
                     WHERE id = ?4",
                    libsql::params![failures, fmt_ts(tx.now + backoff(failures)), disabled, hook.id.to_string()],
                )
                .await?;
            tx.conn
                .execute(
                    "INSERT INTO webhook_deliveries (webhook_id, event_id, status_code, error, at) VALUES (?1, ?2, ?3, ?4, ?5)",
                    libsql::params![hook.id.to_string(), event_id, status.map(i64::from), error, tx.now_str()],
                )
                .await?;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_is_hmac_sha256_over_timestamp_and_body() {
        // Reference value from Python's hmac module.
        let sig = webhook_signature("key", 1, b"body");
        assert_eq!(sig, "t=1,v1=91b5374b153842ad05b2c4eab9349b8321b14703165bd3fb8b034dfb8be98ae5");
        assert_ne!(sig, webhook_signature("other", 1, b"body"));
        assert_ne!(sig, webhook_signature("key", 2, b"body"));
    }

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff(1), Duration::seconds(2));
        assert_eq!(backoff(5), Duration::seconds(32));
        assert_eq!(backoff(19), Duration::hours(1));
    }
}
