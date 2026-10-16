use libsql::Row;
use serde::Serialize;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::{get_opt_json, get_opt_ts, get_ts, get_uuid};
use crate::error::{Error, Result};
use crate::identity::{is_descendant, lineage_revoked};
use crate::projects::load_project;
use crate::store::{Ctx, Store};

const TOKEN_PREFIX: &str = "th_";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Principal {
    User(Uuid),
    Agent(Uuid),
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Token {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub principal: Principal,
    pub name: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
    /// Projects the token is limited to. `None` means every project.
    pub projects: Option<Vec<Uuid>>,
    /// `None` means unlimited.
    pub rate_limit_per_minute: Option<i64>,
}

/// Optional limits on a new token.
#[derive(Debug, Clone, Default)]
pub struct TokenOptions {
    /// Project keys or ids. `None` or empty means every project.
    pub projects: Option<Vec<String>>,
    pub rate_limit_per_minute: Option<i64>,
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
        projects: get_opt_json(row, 7)?.map(serde_json::from_value).transpose()?,
        rate_limit_per_minute: row.get(8)?,
    })
}

const TOKEN_COLS: &str =
    "id, workspace_id, principal_kind, principal_id, name, created_at, revoked_at, projects, rate_limit_per_minute";

fn hash(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

impl Store {
    /// Issues a bearer token. Humans can issue tokens for themselves and for their own
    /// agents; agents can't issue tokens at all. The secret is only ever returned here.
    pub async fn create_token(&self, ctx: &Ctx, principal: Principal, name: &str) -> Result<(Token, String)> {
        self.create_token_with(ctx, principal, name, TokenOptions::default()).await
    }

    /// [`Store::create_token`] with a project scope and rate limit.
    pub async fn create_token_with(
        &self,
        ctx: &Ctx,
        principal: Principal,
        name: &str,
        opts: TokenOptions,
    ) -> Result<(Token, String)> {
        if ctx.is_human() && ctx.is_scoped() {
            return Err(Error::Forbidden("a token scoped to projects can't issue tokens".into()));
        }
        if let Some(n) = opts.rate_limit_per_minute
            && n < 1
        {
            return Err(Error::Invalid("rate limit must be at least 1 request per minute".into()));
        }
        // A token never gets more than the caller has: an agent's sub-agent
        // inherits its rate limit and projects unless it asks for less.
        let mut opts = opts;
        if let Some(cap) = ctx.rate_limit {
            match opts.rate_limit_per_minute {
                Some(n) if n > cap => {
                    return Err(Error::Forbidden(format!("rate limit can't exceed your own ({cap}/min)")));
                }
                None => opts.rate_limit_per_minute = Some(cap),
                _ => {}
            }
        }
        if let Some(scope) = &ctx.projects
            && opts.projects.as_ref().is_none_or(|p| p.is_empty())
        {
            opts.projects = Some(scope.iter().map(Uuid::to_string).collect());
        }
        let conn = self.reader()?;
        let (kind, pid) = match principal {
            Principal::User(id) if id == ctx.user_id && ctx.is_human() => ("user", id),
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
                if let Some(me) = ctx.agent_id
                    && !is_descendant(&conn, me, id).await?
                {
                    return Err(Error::Forbidden("an agent can only issue tokens for its own sub-agents".into()));
                }
                if lineage_revoked(&conn, id).await? {
                    return Err(Error::Forbidden("that agent has been revoked".into()));
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
                let projects = match opts.projects.as_deref() {
                    None | Some([]) => None,
                    Some(refs) => {
                        let mut ids = Vec::new();
                        for r in refs {
                            ids.push(load_project(&tx.conn, ctx.workspace_id, r, false).await?.id);
                        }
                        ids.sort();
                        ids.dedup();
                        if let Some(scope) = &ctx.projects
                            && !ids.iter().all(|i| scope.contains(i))
                        {
                            return Err(Error::Forbidden("a token can't reach projects you can't".into()));
                        }
                        Some(serde_json::to_string(&ids)?)
                    }
                };
                let mut rows = tx
                    .conn
                    .query(
                        &format!(
                            "INSERT INTO tokens (id, workspace_id, principal_kind, principal_id, name, secret_hash, created_at,
                             projects, rate_limit_per_minute)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) RETURNING {TOKEN_COLS}"
                        ),
                        libsql::params![
                            id.to_string(),
                            tx.ws(),
                            kind,
                            pid.to_string(),
                            name,
                            hash(&secret),
                            tx.now_str(),
                            projects,
                            opts.rate_limit_per_minute
                        ],
                    )
                    .await?;
                token_from_row(&rows.next().await?.expect("RETURNING yields a row"))
            })
            .await?;
        Ok((token, secret))
    }

    /// Resolves a bearer secret to a context with a fresh batch.
    pub async fn authenticate(&self, secret: &str) -> Result<Ctx> {
        Ok(self.authenticate_token(secret).await?.0)
    }

    /// [`Store::authenticate`], also returning the token so a server can apply its
    /// rate limit. The context carries the token's project scope.
    pub async fn authenticate_token(&self, secret: &str) -> Result<(Ctx, Token)> {
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
        let ctx = match token.principal {
            Principal::User(u) => Ctx::user(token.workspace_id, u),
            Principal::Agent(a) => self.agent_ctx(token.workspace_id, a).await?,
        };
        let mut ctx = match &token.projects {
            Some(p) => ctx.scoped_to(p.clone()),
            None => ctx,
        };
        ctx.rate_limit = token.rate_limit_per_minute;
        Ok((ctx, token))
    }

    /// Tokens the caller can see: their own and their agents'.
    pub async fn list_tokens(&self, ctx: &Ctx) -> Result<Vec<Token>> {
        let conn = self.reader()?;
        // Humans see their own and all their agents' tokens; an agent sees its own
        // and its sub-agents'.
        let mut rows = match ctx.agent_id {
            None => {
                conn.query(
                    &format!(
                        "SELECT {TOKEN_COLS} FROM tokens WHERE workspace_id = ?1 AND (principal_id = ?2
                         OR principal_id IN (SELECT id FROM agents WHERE owner_user_id = ?2)) ORDER BY created_at"
                    ),
                    [ctx.workspace_id.to_string(), ctx.user_id.to_string()],
                )
                .await?
            }
            Some(me) => {
                conn.query(
                    &format!(
                        "WITH RECURSIVE down(id) AS (
                             SELECT ?2 UNION SELECT a.id FROM agents a JOIN down d ON a.parent_agent_id = d.id)
                         SELECT {TOKEN_COLS} FROM tokens WHERE workspace_id = ?1
                         AND principal_id IN (SELECT id FROM down) ORDER BY created_at"
                    ),
                    [ctx.workspace_id.to_string(), me.to_string()],
                )
                .await?
            }
        };
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
