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

fn agent_from_row(row: &Row) -> Result<Agent> {
    Ok(Agent {
        id: get_uuid(row, 0)?,
        workspace_id: get_uuid(row, 1)?,
        owner_user_id: get_uuid(row, 2)?,
        name: row.get(3)?,
        created_at: get_ts(row, 4)?,
    })
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
        .query(
            "SELECT id, workspace_id, owner_user_id, name, created_at FROM agents WHERE workspace_id = ?1 ORDER BY created_at",
            [ws.to_string()],
        )
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
    pub async fn create_agent(&self, ctx: &Ctx, name: &str) -> Result<Agent> {
        if name.trim().is_empty() {
            return Err(Error::Invalid("agent name is empty".into()));
        }
        if ctx.is_scoped() {
            return Err(Error::Forbidden("a token scoped to projects can't create agents".into()));
        }
        // Names are how people and `--as` refer to agents, so they must be unambiguous.
        let taken = self.list_agents(ctx).await?.iter().any(|a| a.name == name.trim());
        if taken {
            return Err(Error::AlreadyExists(format!("agent {:?}", name.trim())));
        }
        let agent = Agent {
            id: Uuid::now_v7(),
            workspace_id: ctx.workspace_id,
            owner_user_id: ctx.user_id,
            name: name.trim().into(),
            created_at: self.now(),
        };
        self.write(ctx, async |tx| {
            tx.conn
                .execute(
                    "INSERT INTO agents (id, workspace_id, owner_user_id, name, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                    libsql::params![
                        agent.id.to_string(),
                        tx.ws(),
                        agent.owner_user_id.to_string(),
                        agent.name.clone(),
                        tx.now_str()
                    ],
                )
                .await?;
            Ok(())
        })
        .await?;
        Ok(agent)
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
        match rows.next().await? {
            Some(row) => Ok(Ctx::agent(workspace_id, get_uuid(&row, 0)?, agent_id)),
            None => Err(Error::NotFound(format!("agent {agent_id}"))),
        }
    }
}
