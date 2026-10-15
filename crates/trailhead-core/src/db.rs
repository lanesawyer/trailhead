use libsql::{Connection, Row};
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;
use uuid::Uuid;

use crate::error::{Error, Result};

const MIGRATIONS: &[(i64, &str, &str)] = &[
    (1, "init", include_str!("../migrations/0001_init.sql")),
    (2, "tokens", include_str!("../migrations/0002_tokens.sql")),
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
