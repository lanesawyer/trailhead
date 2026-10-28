use jiff::SignedDuration;
use sha2::{Digest, Sha256};

use crate::db::{fmt_ts, get_ts};
use crate::error::{Error, Result};
use crate::store::{Ctx, Store};

/// How long a key is remembered, counted from the first request.
const KEY_TTL: SignedDuration = SignedDuration::from_hours(24);

/// A first request that hasn't finished after this long is presumed dead (say,
/// the server crashed), and a retry may take its key over.
const IN_FLIGHT_TTL: SignedDuration = SignedDuration::from_mins(5);

/// A response saved under an idempotency key, replayed verbatim on retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// What makes two requests "the same": method, path with query, and body.
pub fn idempotency_fingerprint(method: &str, path: &str, body: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(method.as_bytes());
    h.update(b"\n");
    h.update(path.as_bytes());
    h.update(b"\n");
    h.update(body);
    hex::encode(h.finalize())
}

enum Begin {
    Proceed,
    Replay(StoredResponse),
    Reused,
    InFlight,
}

impl Store {
    /// Reserves `key` for the caller, or finds what an earlier request with it did.
    /// `Ok(None)`: go ahead, then call [`Store::idempotency_complete`] or
    /// [`Store::idempotency_abandon`]. `Ok(Some(_))`: replay that response.
    /// Keys are scoped to the acting principal, so two agents never collide.
    pub async fn idempotency_begin(&self, ctx: &Ctx, key: &str, fingerprint: &str) -> Result<Option<StoredResponse>> {
        let begin = self
            .write_quiet(ctx, async |tx| {
                tx.conn
                    .execute("DELETE FROM idempotency_keys WHERE created_at < ?1", [fmt_ts(tx.now - KEY_TTL)])
                    .await?;
                let pk = [tx.ws(), ctx.actor_id().to_string(), key.to_string()];
                let mut rows = tx
                    .conn
                    .query(
                        "SELECT fingerprint, status, headers, body, created_at FROM idempotency_keys
                         WHERE workspace_id = ?1 AND actor_id = ?2 AND key = ?3",
                        pk.clone(),
                    )
                    .await?;
                let Some(row) = rows.next().await? else {
                    tx.conn
                        .execute(
                            "INSERT INTO idempotency_keys (workspace_id, actor_id, key, fingerprint, created_at)
                             VALUES (?1, ?2, ?3, ?4, ?5)",
                            libsql::params![pk[0].clone(), pk[1].clone(), pk[2].clone(), fingerprint, tx.now_str()],
                        )
                        .await?;
                    return Ok(Begin::Proceed);
                };
                let stored_fp: String = row.get(0)?;
                let status: Option<i64> = row.get(1)?;
                let Some(status) = status else {
                    if get_ts(&row, 4)? < tx.now - IN_FLIGHT_TTL {
                        tx.conn
                            .execute(
                                "UPDATE idempotency_keys SET fingerprint = ?4, created_at = ?5
                                 WHERE workspace_id = ?1 AND actor_id = ?2 AND key = ?3",
                                libsql::params![pk[0].clone(), pk[1].clone(), pk[2].clone(), fingerprint, tx.now_str()],
                            )
                            .await?;
                        return Ok(Begin::Proceed);
                    }
                    return Ok(if stored_fp == fingerprint { Begin::InFlight } else { Begin::Reused });
                };
                if stored_fp != fingerprint {
                    return Ok(Begin::Reused);
                }
                let headers: Option<String> = row.get(2)?;
                let headers = headers.as_deref().map(serde_json::from_str).transpose()?.unwrap_or_default();
                let body: Option<Vec<u8>> = row.get(3)?;
                let status = u16::try_from(status).map_err(|_| Error::Corrupt(format!("http status {status}")))?;
                Ok(Begin::Replay(StoredResponse { status, headers, body: body.unwrap_or_default() }))
            })
            .await?;
        match begin {
            Begin::Proceed => Ok(None),
            Begin::Replay(r) => Ok(Some(r)),
            Begin::Reused => Err(Error::IdempotencyKeyReused),
            Begin::InFlight => Err(Error::IdempotencyKeyInFlight),
        }
    }

    /// Saves the response for a key reserved by [`Store::idempotency_begin`].
    pub async fn idempotency_complete(&self, ctx: &Ctx, key: &str, res: &StoredResponse) -> Result<()> {
        let headers = serde_json::to_string(&res.headers)?;
        self.write_quiet(ctx, async |tx| {
            tx.conn
                .execute(
                    "UPDATE idempotency_keys SET status = ?4, headers = ?5, body = ?6
                     WHERE workspace_id = ?1 AND actor_id = ?2 AND key = ?3 AND status IS NULL",
                    libsql::params![
                        tx.ws(),
                        ctx.actor_id().to_string(),
                        key,
                        i64::from(res.status),
                        headers,
                        res.body.clone()
                    ],
                )
                .await?;
            Ok(())
        })
        .await
    }

    /// Frees a reserved key whose request failed, so the caller can retry with it.
    pub async fn idempotency_abandon(&self, ctx: &Ctx, key: &str) -> Result<()> {
        self.write_quiet(ctx, async |tx| {
            tx.conn
                .execute(
                    "DELETE FROM idempotency_keys
                     WHERE workspace_id = ?1 AND actor_id = ?2 AND key = ?3 AND status IS NULL",
                    [tx.ws(), ctx.actor_id().to_string(), key.to_string()],
                )
                .await?;
            Ok(())
        })
        .await
    }
}
