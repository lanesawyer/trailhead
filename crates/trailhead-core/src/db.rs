use jiff::Timestamp;
use jiff::civil::Date;
use libsql::{Connection, Row, Rows};
use serde_json::Value;
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
    (12, "scheduled_on", include_str!("../migrations/0012_scheduled_on.sql")),
    (13, "scheduled_at", include_str!("../migrations/0013_scheduled_at.sql")),
    (14, "due_days", include_str!("../migrations/0014_due_days.sql")),
    (15, "review_verdicts", include_str!("../migrations/0015_review_verdicts.sql")),
    (16, "token_expiry", include_str!("../migrations/0016_token_expiry.sql")),
    (17, "task_grade", include_str!("../migrations/0017_task_grade.sql")),
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
        if version == 14 {
            backfill_due_days(&tx).await?;
        }
        tx.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
            libsql::params![version, name, fmt_ts(Timestamp::now())],
        )
        .await?;
        tx.commit().await?;
    }
    Ok(())
}

/// Splits each existing deadline into a day and a time. A deadline at exactly
/// 23:59:59 local is what a date-only `--due` used to store, so it becomes day-only.
/// `due_by` keeps the old instant either way, so reminders don't fire again.
async fn backfill_due_days(conn: &Connection) -> Result<()> {
    let rows = conn.query("SELECT id, due_at, due_tz FROM tasks WHERE due_at IS NOT NULL", ()).await?;
    let deadlines = collect(rows, |row| {
        let at = get_opt_ts(row, 1)?.expect("filtered on due_at");
        Ok((row.get::<String>(0)?, at, row.get::<Option<String>>(2)?))
    })
    .await?;
    for (id, at, tz) in deadlines {
        let (day, day_only) = crate::when::split_legacy_deadline(at, tz.as_deref())?;
        conn.execute(
            "UPDATE tasks SET due_on = ?1, due_by = due_at, due_at = CASE WHEN ?2 THEN NULL ELSE due_at END WHERE id = ?3",
            libsql::params![day.to_string(), day_only, id],
        )
        .await?;
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
pub(crate) fn fmt_ts(t: Timestamp) -> String {
    t.strftime("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()
}

pub(crate) fn opt_ts_value(t: Option<Timestamp>) -> libsql::Value {
    t.map(fmt_ts).into()
}

pub(crate) fn opt_date_value(d: Option<Date>) -> libsql::Value {
    d.map(|d| d.to_string()).into()
}

pub(crate) fn get_opt_date(row: &Row, i: i32) -> Result<Option<Date>> {
    let s: Option<String> = row.get(i)?;
    s.as_deref().map(|s| s.parse::<Date>().map_err(|e| Error::Corrupt(format!("date {s}: {e}")))).transpose()
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

pub(crate) fn get_ts(row: &Row, i: i32) -> Result<Timestamp> {
    let s: String = row.get(i)?;
    parse_ts(&s)
}

pub(crate) fn get_opt_ts(row: &Row, i: i32) -> Result<Option<Timestamp>> {
    let s: Option<String> = row.get(i)?;
    s.as_deref().map(parse_ts).transpose()
}

fn parse_ts(s: &str) -> Result<Timestamp> {
    s.parse::<Timestamp>().map_err(|e| Error::Corrupt(format!("timestamp {s}: {e}")))
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
