use libsql::Row;
use serde::Serialize;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::{get_opt_ts, get_ts, get_uuid};
use crate::error::{Error, Result};
use crate::store::{Ctx, Store};

const TOKEN_PREFIX: &str = "th_";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Principal {
    User(Uuid),
    Agent(Uuid),
}

#[derive(Debug, Clone, Serialize)]
pub struct Token {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub principal: Principal,
    pub name: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
}

fn token_from_row(row: &Row) -> Result<Token> {
    let kind: String = row.get(2)?;
    let pid = get_uuid(row, 3)?;
    Ok(Token {
        id: get_uuid(row, 0)?,
        workspace_id: get_uuid(row, 1)?,
        principal: if kind == "agent" { Principal::Agent(pid) } else { Principal::User(pid) },
        name: row.get(4)?,
        created_at: get_ts(row, 5)?,
        revoked_at: get_opt_ts(row, 6)?,
    })
}

const TOKEN_COLS: &str = "id, workspace_id, principal_kind, principal_id, name, created_at, revoked_at";

fn hash(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

impl Store {
    /// Issues a bearer token. Humans can issue tokens for themselves and for their own
    /// agents; agents can't issue tokens at all. The secret is only ever returned here.
    pub async fn create_token(&self, ctx: &Ctx, principal: Principal, name: &str) -> Result<(Token, String)> {
        if !ctx.is_human() {
            return Err(Error::Forbidden("agents can't issue tokens".into()));
        }
        let conn = self.reader()?;
        let (kind, pid) = match principal {
            Principal::User(id) if id == ctx.user_id => ("user", id),
            Principal::User(_) => return Err(Error::Forbidden("you can only issue tokens for yourself".into())),
            Principal::Agent(id) => {
                let mut rows = conn
                    .query(
                        "SELECT 1 FROM agents WHERE id = ?1 AND owner_user_id = ?2 AND workspace_id = ?3",
                        [id.to_string(), ctx.user_id.to_string(), ctx.workspace_id.to_string()],
                    )
                    .await?;
                if rows.next().await?.is_none() {
                    return Err(Error::NotFound(format!("agent {id}")));
                }
                ("agent", id)
            }
        };
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| Error::Corrupt(format!("no randomness: {e}")))?;
        let secret = format!("{TOKEN_PREFIX}{}", hex::encode(bytes));
        let id = Uuid::now_v7();
        let token = self
            .write(ctx, async |tx| {
                let mut rows = tx
                    .conn
                    .query(
                        &format!(
                            "INSERT INTO tokens (id, workspace_id, principal_kind, principal_id, name, secret_hash, created_at)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) RETURNING {TOKEN_COLS}"
                        ),
                        libsql::params![id.to_string(), tx.ws(), kind, pid.to_string(), name, hash(&secret), tx.now_str()],
                    )
                    .await?;
                token_from_row(&rows.next().await?.expect("RETURNING yields a row"))
            })
            .await?;
        Ok((token, secret))
    }

    /// Resolves a bearer secret to a context with a fresh batch.
    pub async fn authenticate(&self, secret: &str) -> Result<Ctx> {
        if !secret.starts_with(TOKEN_PREFIX) {
            return Err(Error::Unauthorized);
        }
        let conn = self.reader()?;
        let mut rows = conn
            .query(
                &format!("SELECT {TOKEN_COLS} FROM tokens WHERE secret_hash = ?1 AND revoked_at IS NULL"),
                [hash(secret)],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Err(Error::Unauthorized);
        };
        let token = token_from_row(&row)?;
        match token.principal {
            Principal::User(u) => Ok(Ctx::user(token.workspace_id, u)),
            Principal::Agent(a) => self.agent_ctx(token.workspace_id, a).await,
        }
    }

    /// Tokens the caller can see: their own and their agents'.
    pub async fn list_tokens(&self, ctx: &Ctx) -> Result<Vec<Token>> {
        let conn = self.reader()?;
        let mut rows = conn
            .query(
                &format!(
                    "SELECT {TOKEN_COLS} FROM tokens WHERE workspace_id = ?1 AND (principal_id = ?2
                     OR principal_id IN (SELECT id FROM agents WHERE owner_user_id = ?2)) ORDER BY created_at"
                ),
                [ctx.workspace_id.to_string(), ctx.user_id.to_string()],
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push(token_from_row(&row)?);
        }
        Ok(out)
    }

    /// Takes effect on the next request.
    pub async fn revoke_token(&self, ctx: &Ctx, id: Uuid) -> Result<Token> {
        if !ctx.is_human() {
            return Err(Error::Forbidden("agents can't revoke tokens".into()));
        }
        self.write(ctx, async |tx| {
            let mut rows = tx
                .conn
                .query(
                    &format!(
                        "UPDATE tokens SET revoked_at = coalesce(revoked_at, ?1) WHERE id = ?2 AND workspace_id = ?3
                         AND (principal_id = ?4 OR principal_id IN (SELECT id FROM agents WHERE owner_user_id = ?4))
                         RETURNING {TOKEN_COLS}"
                    ),
                    [tx.now_str(), id.to_string(), tx.ws(), ctx.user_id.to_string()],
                )
                .await?;
            match rows.next().await? {
                Some(row) => token_from_row(&row),
                None => Err(Error::NotFound(format!("token {id}"))),
            }
        })
        .await
    }
}
