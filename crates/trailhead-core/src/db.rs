use libsql::{Connection, Row, Rows};
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;
use uuid::Uuid;

use crate::error::{Error, Result};

const MIGRATIONS: &[(i64, &str, &str)] = &[
    (1, "init", include_str!("../migrations/0001_init.sql")),
    (2, "tokens", include_str!("../migrations/0002_tokens.sql")),
    (3, "idempotency", include_str!("../migrations/0003_idempotency.sql")),
    (4, "token_scopes", include_str!("../migrations/0004_token_scopes.sql")),
    (5, "reminders", include_str!("../migrations/0005_reminders.sql")),
    (6, "events_archive", include_str!("../migrations/0006_events_archive.sql")),
    (7, "webhooks", include_str!("../migrations/0007_webhooks.sql")),
    (8, "unique_agent_names", include_str!("../migrations/0008_unique_agent_names.sql")),
    (9, "project_reviewers", include_str!("../migrations/0009_project_reviewers.sql")),
    (10, "sub_agents", include_str!("../migrations/0010_sub_agents.sql")),
    (11, "claims_without_expiry", include_str!("../migrations/0011_claims_without_expiry.sql")),
];

pub(crate) async fn migrate(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL)",
        (),
    )
    .await?;
    for &(version, name, sql) in MIGRATIONS {
        let mut rows = conn.query("SELECT 1 FROM schema_migrations WHERE version = ?1", [version]).await?;
        if rows.next().await?.is_some() {
            continue;
        }
        let tx = conn.transaction().await?;
        tx.execute_batch(sql).await?;
        tx.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
            libsql::params![version, name, fmt_ts(OffsetDateTime::now_utc())],
        )
        .await?;
        tx.commit().await?;
    }
    Ok(())
}

/// Reads every row with `f`.
pub(crate) async fn collect<T>(mut rows: Rows, f: impl Fn(&Row) -> Result<T>) -> Result<Vec<T>> {
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(f(&row)?);
    }
    Ok(out)
}

/// Reads the first row with `f`, if there is one.
pub(crate) async fn first<T>(mut rows: Rows, f: impl FnOnce(&Row) -> Result<T>) -> Result<Option<T>> {
    rows.next().await?.map(|row| f(&row)).transpose()
}

/// Ids as a JSON array, for `IN (SELECT value FROM json_each(?))`.
pub(crate) fn json_ids(ids: &[Uuid]) -> Result<String> {
    Ok(serde_json::to_string(&ids.iter().map(Uuid::to_string).collect::<Vec<_>>())?)
}

/// Fixed-width UTC so timestamps compare correctly as text.
pub(crate) fn fmt_ts(t: OffsetDateTime) -> String {
    let fmt = format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:6]Z");
    t.to_offset(time::UtcOffset::UTC).format(&fmt).expect("valid timestamp")
}

pub(crate) fn opt_ts_value(t: Option<OffsetDateTime>) -> libsql::Value {
    t.map(fmt_ts).into()
}

pub(crate) fn opt_uuid_value(u: Option<Uuid>) -> libsql::Value {
    u.map(|u| u.to_string()).into()
}

pub(crate) fn get_uuid(row: &Row, i: i32) -> Result<Uuid> {
    let s: String = row.get(i)?;
    Uuid::parse_str(&s).map_err(|e| Error::Corrupt(format!("uuid {s}: {e}")))
}

pub(crate) fn get_opt_uuid(row: &Row, i: i32) -> Result<Option<Uuid>> {
    let s: Option<String> = row.get(i)?;
    s.map(|s| Uuid::parse_str(&s).map_err(|e| Error::Corrupt(format!("uuid {s}: {e}")))).transpose()
}

pub(crate) fn get_ts(row: &Row, i: i32) -> Result<OffsetDateTime> {
    let s: String = row.get(i)?;
    parse_ts(&s)
}

pub(crate) fn get_opt_ts(row: &Row, i: i32) -> Result<Option<OffsetDateTime>> {
    let s: Option<String> = row.get(i)?;
    s.as_deref().map(parse_ts).transpose()
}

fn parse_ts(s: &str) -> Result<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339).map_err(|e| Error::Corrupt(format!("timestamp {s}: {e}")))
}

pub(crate) fn get_json(row: &Row, i: i32) -> Result<Value> {
    let s: String = row.get(i)?;
    Ok(serde_json::from_str(&s)?)
}

pub(crate) fn get_opt_json(row: &Row, i: i32) -> Result<Option<Value>> {
    let s: Option<String> = row.get(i)?;
    Ok(s.as_deref().map(serde_json::from_str).transpose()?)
}

pub(crate) fn get_enum<T: std::str::FromStr<Err = Error>>(row: &Row, i: i32) -> Result<T> {
    let s: String = row.get(i)?;
    s.parse()
}
