use libsql::{Connection, Row};
use serde_json::Value;
use uuid::Uuid;

use crate::db::{fmt_ts, get_enum, get_json, get_opt_ts, get_ts, get_uuid, opt_ts_value};
use crate::error::{Error, Result};
use crate::metadata::merge_patch;
use crate::model::{Entity, Project, ProjectStatus};
use crate::store::{Ctx, Store, Tx};

pub(crate) const PROJECT_COLS: &str = "id, workspace_id, key, name, brief, status, metadata, claim_ttl, require_review, version, created_at, updated_at, deleted_at";

pub(crate) fn project_from_row(row: &Row) -> Result<Project> {
    Ok(Project {
        id: get_uuid(row, 0)?,
        workspace_id: get_uuid(row, 1)?,
        key: row.get(2)?,
        name: row.get(3)?,
        brief: row.get(4)?,
        status: get_enum(row, 5)?,
        metadata: get_json(row, 6)?,
        claim_ttl: row.get(7)?,
        require_review: row.get::<i64>(8)? != 0,
        version: row.get(9)?,
        created_at: get_ts(row, 10)?,
        updated_at: get_ts(row, 11)?,
        deleted_at: get_opt_ts(row, 12)?,
    })
}

#[derive(Debug, Clone, Default)]
pub struct NewProject {
    pub key: String,
    pub name: String,
    pub brief: String,
    pub require_review: bool,
    pub claim_ttl: Option<i64>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Default)]
pub struct ProjectPatch {
    pub name: Option<String>,
    pub brief: Option<String>,
    pub status: Option<ProjectStatus>,
    pub require_review: Option<bool>,
    pub claim_ttl: Option<Option<i64>>,
    /// RFC 7386 merge patch: `null` removes a key.
    pub metadata: Option<Value>,
}

fn validate_key(key: &str) -> Result<()> {
    let ok = (2..=10).contains(&key.len())
        && key.starts_with(|c: char| c.is_ascii_uppercase())
        && key.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "project key {key:?} must be 2-10 uppercase letters or digits, starting with a letter"
        )))
    }
}

pub(crate) fn validate_ttl(ttl: i64) -> Result<()> {
    if (1..=86_400).contains(&ttl) {
        Ok(())
    } else {
        Err(Error::Invalid("claim TTL must be between 1 second and 24 hours".into()))
    }
}

pub(crate) async fn load_project(conn: &Connection, ws: Uuid, r: &str, include_deleted: bool) -> Result<Project> {
    let sql = format!(
        "SELECT {PROJECT_COLS} FROM projects WHERE workspace_id = ?1 AND (id = ?2 OR key = upper(?2)) {}",
        if include_deleted { "" } else { "AND deleted_at IS NULL" }
    );
    let mut rows = conn.query(&sql, [ws.to_string(), r.trim().to_string()]).await?;
    match rows.next().await? {
        Some(row) => project_from_row(&row),
        None => Err(Error::NotFound(format!("project {r}"))),
    }
}

/// Compare-and-swap on `version`. Used by updates and undo alike.
pub(crate) async fn save_project(tx: &Tx, before: &Project, after: Project) -> Result<Project> {
    let mut rows = tx
        .conn
        .query(
            &format!(
                "UPDATE projects SET name = ?1, brief = ?2, status = ?3, metadata = ?4, claim_ttl = ?5, require_review = ?6,
                 deleted_at = ?7, version = version + 1, updated_at = ?8
                 WHERE id = ?9 AND version = ?10 RETURNING {PROJECT_COLS}"
            ),
            libsql::params![
                after.name,
                after.brief,
                after.status.as_str(),
                after.metadata.to_string(),
                after.claim_ttl,
                after.require_review,
                opt_ts_value(after.deleted_at),
                tx.now_str(),
                before.id.to_string(),
                before.version,
            ],
        )
        .await?;
    match rows.next().await? {
        Some(row) => project_from_row(&row),
        None => {
            let current = load_project(&tx.conn, before.workspace_id, &before.id.to_string(), true).await?;
            Err(Error::VersionConflict { expected: before.version, current: current.version })
        }
    }
}

impl Store {
    pub async fn create_project(&self, ctx: &Ctx, new: NewProject) -> Result<Project> {
        let key = new.key.trim().to_ascii_uppercase();
        validate_key(&key)?;
        if new.name.trim().is_empty() {
            return Err(Error::Invalid("project name is empty".into()));
        }
        if let Some(ttl) = new.claim_ttl {
            validate_ttl(ttl)?;
        }
        let metadata = new.metadata.unwrap_or_else(|| Value::Object(Default::default()));
        if !metadata.is_object() {
            return Err(Error::Invalid("metadata must be a JSON object".into()));
        }
        self.write(ctx, async |tx| {
            if load_project(&tx.conn, ctx.workspace_id, &key, true).await.is_ok() {
                return Err(Error::AlreadyExists(format!("project {key}")));
            }
            let id = Uuid::now_v7();
            let mut rows = tx
                .conn
                .query(
                    &format!(
                        "INSERT INTO projects (id, workspace_id, key, name, brief, metadata, claim_ttl, require_review, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9) RETURNING {PROJECT_COLS}"
                    ),
                    libsql::params![
                        id.to_string(),
                        tx.ws(),
                        key.clone(),
                        new.name.trim(),
                        new.brief.clone(),
                        metadata.to_string(),
                        new.claim_ttl,
                        new.require_review,
                        fmt_ts(tx.now),
                    ],
                )
                .await?;
            let project = project_from_row(&rows.next().await?.expect("RETURNING yields a row"))?;
            tx.emit_change(Entity::Project, id.to_string(), None, "project.created", None, Some(&project))
                .await?;
            Ok(project)
        })
        .await
    }

    pub async fn update_project(
        &self,
        ctx: &Ctx,
        project: &str,
        patch: ProjectPatch,
        expected_version: Option<i64>,
    ) -> Result<Project> {
        self.write(ctx, async |tx| {
            let before = load_project(&tx.conn, ctx.workspace_id, project, false).await?;
            if let Some(v) = expected_version
                && v != before.version
            {
                return Err(Error::VersionConflict { expected: v, current: before.version });
            }
            let mut after = before.clone();
            if let Some(name) = patch.name {
                if name.trim().is_empty() {
                    return Err(Error::Invalid("project name is empty".into()));
                }
                after.name = name.trim().into();
            }
            if let Some(brief) = patch.brief {
                after.brief = brief;
            }
            if let Some(status) = patch.status {
                after.status = status;
            }
            if let Some(r) = patch.require_review {
                after.require_review = r;
            }
            if let Some(ttl) = patch.claim_ttl {
                if let Some(t) = ttl {
                    validate_ttl(t)?;
                }
                after.claim_ttl = ttl;
            }
            if let Some(m) = &patch.metadata {
                after.metadata = merge_patch(&before.metadata, m)?;
            }
            let after = save_project(tx, &before, after).await?;
            tx.emit_change(Entity::Project, after.id.to_string(), None, "project.updated", Some(&before), Some(&after))
                .await?;
            Ok(after)
        })
        .await
    }

    pub async fn get_project(&self, ctx: &Ctx, project: &str) -> Result<Project> {
        let conn = self.reader()?;
        load_project(&conn, ctx.workspace_id, project, false).await
    }

    pub async fn list_projects(&self, ctx: &Ctx, include_archived: bool) -> Result<Vec<Project>> {
        let conn = self.reader()?;
        let sql = format!(
            "SELECT {PROJECT_COLS} FROM projects WHERE workspace_id = ?1 AND deleted_at IS NULL {} ORDER BY key",
            if include_archived { "" } else { "AND status = 'active'" }
        );
        let mut rows = conn.query(&sql, [ctx.workspace_id.to_string()]).await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push(project_from_row(&row)?);
        }
        Ok(out)
    }
}
