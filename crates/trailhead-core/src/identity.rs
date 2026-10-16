use libsql::{Connection, Row};
use uuid::Uuid;

use crate::db::{fmt_ts, get_ts, get_uuid};
use crate::error::{Error, Result};
use crate::model::{Agent, User, Workspace};
use crate::store::{Ctx, Store};

fn workspace_from_row(row: &Row) -> Result<Workspace> {
    Ok(Workspace {
        id: get_uuid(row, 0)?,
        name: row.get(1)?,
        default_claim_ttl: row.get(2)?,
        created_at: get_ts(row, 3)?,
    })
}

fn user_from_row(row: &Row) -> Result<User> {
    Ok(User {
        id: get_uuid(row, 0)?,
        workspace_id: get_uuid(row, 1)?,
        name: row.get(2)?,
        email: row.get(3)?,
        created_at: get_ts(row, 4)?,
    })
}

const AGENT_COLS: &str = "id, workspace_id, owner_user_id, name, created_at, parent_agent_id, revoked_at";

fn agent_from_row(row: &Row) -> Result<Agent> {
    Ok(Agent {
        id: get_uuid(row, 0)?,
        workspace_id: get_uuid(row, 1)?,
        owner_user_id: get_uuid(row, 2)?,
        name: row.get(3)?,
        created_at: get_ts(row, 4)?,
        parent_agent_id: crate::db::get_opt_uuid(row, 5)?,
        revoked_at: crate::db::get_opt_ts(row, 6)?,
    })
}

/// An agent by id or full name (`claude` or `claude/openapi`).
pub(crate) async fn find_agent(conn: &Connection, ws: Uuid, r: &str) -> Result<Agent> {
    let mut rows = conn
        .query(
            &format!("SELECT {AGENT_COLS} FROM agents WHERE workspace_id = ?1 AND (id = ?2 OR name = ?2)"),
            [ws.to_string(), r.trim().to_string()],
        )
        .await?;
    match rows.next().await? {
        Some(row) => agent_from_row(&row),
        None => Err(Error::NotFound(format!("agent {r}"))),
    }
}

/// Whether `agent` sits somewhere under `ancestor` (not counting itself).
pub(crate) async fn is_descendant(conn: &Connection, ancestor: Uuid, agent: Uuid) -> Result<bool> {
    let mut rows = conn
        .query(
            "WITH RECURSIVE up(id) AS (
                 SELECT parent_agent_id FROM agents WHERE id = ?1
                 UNION SELECT a.parent_agent_id FROM agents a JOIN up ON a.id = up.id)
             SELECT 1 FROM up WHERE id = ?2 LIMIT 1",
            [agent.to_string(), ancestor.to_string()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// Whether the agent or any agent above it has been revoked.
pub(crate) async fn lineage_revoked(conn: &Connection, agent: Uuid) -> Result<bool> {
    let mut rows = conn
        .query(
            "WITH RECURSIVE up(id) AS (
                 SELECT ?1
                 UNION SELECT a.parent_agent_id FROM agents a JOIN up ON a.id = up.id WHERE a.parent_agent_id IS NOT NULL)
             SELECT 1 FROM agents WHERE id IN (SELECT id FROM up) AND revoked_at IS NOT NULL LIMIT 1",
            [agent.to_string()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// Agent name segments: what `--as` and file names can carry safely.
fn validate_agent_name(name: &str) -> Result<()> {
    let ok = (1..=40).contains(&name.len()) && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid(format!("agent name {name:?} must be 1-40 letters, digits, '-' or '_'")))
    }
}

pub(crate) async fn load_workspace(conn: &Connection, id: Uuid) -> Result<Workspace> {
    let mut rows = conn
        .query("SELECT id, name, default_claim_ttl, created_at FROM workspaces WHERE id = ?1", [id.to_string()])
        .await?;
    match rows.next().await? {
        Some(row) => workspace_from_row(&row),
        None => Err(Error::NotFound(format!("workspace {id}"))),
    }
}

pub(crate) async fn agents_in(conn: &Connection, ws: Uuid) -> Result<Vec<Agent>> {
    let mut rows = conn
        .query(&format!("SELECT {AGENT_COLS} FROM agents WHERE workspace_id = ?1 ORDER BY name"), [ws.to_string()])
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(agent_from_row(&row)?);
    }
    Ok(out)
}

impl Store {
    /// A local install is one workspace with one user. Creates them on first run
    /// and returns the existing pair afterwards.
    pub async fn bootstrap_local(&self, user_name: &str) -> Result<(Workspace, User)> {
        let conn = self.reader()?;
        let mut rows = conn
            .query(
                "SELECT w.id, w.name, w.default_claim_ttl, w.created_at, u.id, u.workspace_id, u.name, u.email, u.created_at
                 FROM workspaces w JOIN users u ON u.workspace_id = w.id ORDER BY w.created_at, u.created_at LIMIT 1",
                (),
            )
            .await?;
        if let Some(row) = rows.next().await? {
            let ws = workspace_from_row(&row)?;
            let user = User {
                id: get_uuid(&row, 4)?,
                workspace_id: get_uuid(&row, 5)?,
                name: row.get(6)?,
                email: row.get(7)?,
                created_at: get_ts(&row, 8)?,
            };
            return Ok((ws, user));
        }
        let now = self.now();
        let ws = Workspace { id: Uuid::now_v7(), name: "local".into(), default_claim_ttl: 1800, created_at: now };
        let user =
            User { id: Uuid::now_v7(), workspace_id: ws.id, name: user_name.into(), email: None, created_at: now };
        let ctx = Ctx::user(ws.id, user.id);
        self.write(&ctx, async |tx| {
            tx.conn
                .execute(
                    "INSERT INTO workspaces (id, name, default_claim_ttl, created_at) VALUES (?1, ?2, ?3, ?4)",
                    libsql::params![ws.id.to_string(), ws.name.clone(), ws.default_claim_ttl, fmt_ts(now)],
                )
                .await?;
            tx.conn
                .execute(
                    "INSERT INTO users (id, workspace_id, name, email, created_at) VALUES (?1, ?2, ?3, NULL, ?4)",
                    libsql::params![user.id.to_string(), ws.id.to_string(), user.name.clone(), fmt_ts(now)],
                )
                .await?;
            Ok(())
        })
        .await?;
        Ok((ws, user))
    }

    pub async fn get_workspace(&self, ctx: &Ctx) -> Result<Workspace> {
        let conn = self.reader()?;
        load_workspace(&conn, ctx.workspace_id).await
    }

    pub async fn get_user(&self, ctx: &Ctx, id: Uuid) -> Result<User> {
        let conn = self.reader()?;
        let mut rows = conn
            .query(
                "SELECT id, workspace_id, name, email, created_at FROM users WHERE id = ?1 AND workspace_id = ?2",
                [id.to_string(), ctx.workspace_id.to_string()],
            )
            .await?;
        match rows.next().await? {
            Some(row) => user_from_row(&row),
            None => Err(Error::NotFound(format!("user {id}"))),
        }
    }

    /// Creates an agent owned by the context's user.
    /// Creates an agent. A human creates a top-level agent acting for them; an
    /// agent creates a sub-agent named `<its name>/<name>`, acting for the same
    /// human. Agents can't be created any other way, so no agent ever needs a
    /// human's credentials.
    pub async fn create_agent(&self, ctx: &Ctx, name: &str) -> Result<Agent> {
        let name = name.trim();
        validate_agent_name(name)?;
        let conn = self.reader()?;
        let (full, parent) = match ctx.agent_id {
            None if ctx.is_scoped() => {
                return Err(Error::Forbidden("a token scoped to projects can't create agents".into()));
            }
            None => (name.to_string(), None),
            Some(me) => {
                (format!("{}/{name}", find_agent(&conn, ctx.workspace_id, &me.to_string()).await?.name), Some(me))
            }
        };
        // Names are how people and `--as` refer to agents, so they must be unambiguous.
        if find_agent(&conn, ctx.workspace_id, &full).await.is_ok() {
            return Err(Error::AlreadyExists(format!("agent {full:?}")));
        }
        let agent = Agent {
            id: Uuid::now_v7(),
            workspace_id: ctx.workspace_id,
            owner_user_id: ctx.user_id,
            parent_agent_id: parent,
            name: full,
            created_at: self.now(),
            revoked_at: None,
        };
        self.write(ctx, async |tx| {
            tx.conn
                .execute(
                    "INSERT INTO agents (id, workspace_id, owner_user_id, name, created_at, parent_agent_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    libsql::params![
                        agent.id.to_string(),
                        tx.ws(),
                        agent.owner_user_id.to_string(),
                        agent.name.clone(),
                        tx.now_str(),
                        agent.parent_agent_id.map(|p| p.to_string()),
                    ],
                )
                .await?;
            Ok(())
        })
        .await?;
        Ok(agent)
    }

    /// A context for one of the caller's registered sub-agents, by full name or by
    /// the part after the caller's own name (`researcher` for `openclaw/researcher`).
    /// It inherits the caller's projects, rate limit and batch. Names that aren't
    /// registered are refused: sub-agents must be created explicitly.
    pub async fn sub_agent_ctx(&self, ctx: &Ctx, name: &str) -> Result<Ctx> {
        let Some(me) = ctx.agent_id else {
            return Err(Error::Forbidden("only an agent can act as its sub-agents".into()));
        };
        let conn = self.reader()?;
        let mine = find_agent(&conn, ctx.workspace_id, &me.to_string()).await?;
        let full = if name.contains('/') { name.trim().to_string() } else { format!("{}/{}", mine.name, name.trim()) };
        let kid = find_agent(&conn, ctx.workspace_id, &full).await?;
        if !is_descendant(&conn, me, kid.id).await? {
            return Err(Error::Forbidden(format!("{} isn't one of your sub-agents", kid.name)));
        }
        if lineage_revoked(&conn, kid.id).await? {
            return Err(Error::Forbidden(format!("{} has been revoked", kid.name)));
        }
        Ok(Ctx { agent_id: Some(kid.id), ..ctx.clone() })
    }

    /// Revokes an agent and every sub-agent under it; their tokens stop working
    /// at once. The owning human or any agent above it may do this.
    pub async fn revoke_agent(&self, ctx: &Ctx, agent: &str) -> Result<Agent> {
        let conn = self.reader()?;
        let target = find_agent(&conn, ctx.workspace_id, agent).await?;
        let allowed = match ctx.agent_id {
            None => !ctx.is_scoped() && target.owner_user_id == ctx.user_id,
            Some(me) => is_descendant(&conn, me, target.id).await?,
        };
        if !allowed {
            return Err(Error::Forbidden(format!("you can't revoke agent {}", target.name)));
        }
        self.write(ctx, async |tx| {
            tx.conn
                .execute(
                    "WITH RECURSIVE down(id) AS (
                         SELECT ?1 UNION SELECT a.id FROM agents a JOIN down d ON a.parent_agent_id = d.id)
                     UPDATE agents SET revoked_at = coalesce(revoked_at, ?2) WHERE id IN (SELECT id FROM down)",
                    [target.id.to_string(), tx.now_str()],
                )
                .await?;
            Ok(())
        })
        .await?;
        let conn = self.reader()?;
        find_agent(&conn, ctx.workspace_id, &target.id.to_string()).await
    }

    pub async fn list_agents(&self, ctx: &Ctx) -> Result<Vec<Agent>> {
        let conn = self.reader()?;
        agents_in(&conn, ctx.workspace_id).await
    }

    /// A context for an agent, acting for its owner.
    pub async fn agent_ctx(&self, workspace_id: Uuid, agent_id: Uuid) -> Result<Ctx> {
        let conn = self.reader()?;
        let mut rows = conn
            .query(
                "SELECT owner_user_id FROM agents WHERE id = ?1 AND workspace_id = ?2",
                [agent_id.to_string(), workspace_id.to_string()],
            )
            .await?;
        let owner = match rows.next().await? {
            Some(row) => get_uuid(&row, 0)?,
            None => return Err(Error::NotFound(format!("agent {agent_id}"))),
        };
        if lineage_revoked(&conn, agent_id).await? {
            return Err(Error::Unauthorized);
        }
        Ok(Ctx::agent(workspace_id, owner, agent_id))
    }
}
