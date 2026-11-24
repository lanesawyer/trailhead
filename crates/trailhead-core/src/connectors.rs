//! What the engine keeps for connectors (`docs/app-platform.md`, "Connectors"): each one's agent, `connector/<id>`,
//! which the server makes with no parent agent, and the settings table the server keeps their state in.

use crate::error::{Error, Result};
use crate::identity::{find_agent, insert_agent_named};
use crate::store::{Ctx, Store};
use serde_json::Value;
use trailhead_model::Agent;
use trailhead_model::app_data::check_app_id;
use trailhead_model::connector::{AGENT_PREFIX, agent_name};

impl Store {
    /// The connector's agent, made the first time it is asked for: `connector/<id>`, with no parent agent, acting for
    /// the person. Only a person may ask. One that was revoked stays revoked.
    pub async fn connector_agent(&self, ctx: &Ctx, id: &str) -> Result<Agent> {
        if !ctx.is_human() || ctx.is_scoped() {
            return Err(Error::Forbidden("only a person turns a connector on".into()));
        }
        check_app_id(id).map_err(|_| Error::Invalid(format!("`{id}` isn't a connector's id")))?;
        let name = agent_name(id);
        let found = match find_agent(&*self.reader()?, ctx.workspace_id, &name).await {
            Ok(agent) => Some(agent),
            Err(Error::NotFound(_)) => None,
            Err(e) => return Err(e),
        };
        let agent = match found {
            Some(agent) => agent,
            None => self.write(ctx, async |tx| insert_agent_named(tx, name.clone(), None).await).await?,
        };
        if agent.parent_agent_id.is_some() {
            return Err(Error::Corrupt(format!("{name} has a parent agent, so it isn't the connector's own")));
        }
        if agent.revoked_at.is_some() {
            return Err(Error::Forbidden(format!("{name} has been revoked")));
        }
        Ok(agent)
    }

    /// The id of the connector the context acts as, when it is a connector's agent.
    pub async fn connector_of(&self, ctx: &Ctx) -> Result<Option<String>> {
        let Some(agent) = ctx.agent_id else { return Ok(None) };
        let agent = find_agent(&*self.reader()?, ctx.workspace_id, &agent.to_string()).await?;
        Ok(connector_id(&agent).map(String::from))
    }

    /// A value the person set that the server keeps, by name: what `settings` holds.
    pub async fn setting(&self, ctx: &Ctx, name: &str) -> Result<Option<Value>> {
        let mut rows = self
            .reader()?
            .query(
                "SELECT value FROM settings WHERE workspace_id = ?1 AND name = ?2",
                [ctx.workspace_id.to_string(), name.to_string()],
            )
            .await?;
        match rows.next().await? {
            Some(row) => Ok(Some(serde_json::from_str(&row.get::<String>(0)?)?)),
            None => Ok(None),
        }
    }

    /// Sets a value in `settings`, as a person. Not an event: undo leaves settings alone.
    pub async fn set_setting(&self, ctx: &Ctx, name: &str, value: &Value) -> Result<()> {
        if !ctx.is_human() || ctx.is_scoped() {
            return Err(Error::Forbidden("only a person changes the server's settings".into()));
        }
        let value = serde_json::to_string(value)?;
        self.write_quiet(ctx, async |tx| {
            tx.conn
                .execute(
                    "INSERT INTO settings (workspace_id, name, value, updated_at) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (workspace_id, name) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                    libsql::params![tx.ws(), name, value.clone(), tx.now_str()],
                )
                .await?;
            Ok(())
        })
        .await
    }
}

/// The connector `agent` is the agent of: one named `connector/<id>` with no parent.
pub(crate) fn connector_id(agent: &Agent) -> Option<&str> {
    agent.name.strip_prefix(AGENT_PREFIX).filter(|_| agent.parent_agent_id.is_none())
}
