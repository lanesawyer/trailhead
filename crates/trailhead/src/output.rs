use std::sync::OnceLock;

use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

pub fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}

static LOCAL_OFFSET: OnceLock<UtcOffset> = OnceLock::new();

/// Must run before any threads start: the local offset can't be read safely after.
pub fn init_local_offset() {
    LOCAL_OFFSET.get_or_init(|| UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC));
}

pub fn local_offset() -> UtcOffset {
    *LOCAL_OFFSET.get().unwrap_or(&UtcOffset::UTC)
}

/// `2026-10-15T04:05:00.123Z` -> `2026-10-14 21:05` in local time.
pub fn short_time(ts: &str) -> String {
    let fmt = time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]");
    match OffsetDateTime::parse(ts, &Rfc3339) {
        Ok(t) => t.to_offset(local_offset()).format(&fmt).unwrap_or_else(|_| ts.to_string()),
        Err(_) => ts.to_string(),
    }
}

fn short_date(ts: &str) -> String {
    short_time(ts).chars().take(10).collect()
}

pub fn task_line(t: &Value) -> String {
    let mut line = format!("{:<10} {:<11} p{}  {}", s(t, "key"), s(t, "status"), t["priority"], s(t, "title"));
    if let Some(due) = t["due_at"].as_str() {
        line.push_str(&format!("  (due {})", short_date(due)));
    }
    line
}

pub fn task_list(tasks: &Value, empty: &str) {
    match tasks.as_array() {
        Some(items) if !items.is_empty() => items.iter().for_each(|t| println!("{}", task_line(t))),
        _ => println!("{empty}"),
    }
}

pub fn comment_line(c: &Value, names: &dyn Fn(&str) -> String) -> String {
    let mut tags = vec![s(c, "kind").to_string()];
    if let Some(to) = c["addressed_to"].as_str() {
        tags.push(format!("to {}", names(to)));
    }
    if c["kind"] == "question" {
        tags.push(if c["resolved_at"].is_null() { "open".into() } else { "resolved".into() });
    }
    format!(
        "[{}] {} {}: {}\n    id {}",
        tags.join(", "),
        short_time(s(c, "created_at")),
        names(s(c, "author_id")),
        s(c, "body").replace('\n', "\n    "),
        s(c, "id")
    )
}

pub fn project_line(p: &Value) -> String {
    let mut line = format!("{:<8} {}", s(p, "key"), s(p, "name"));
    if p["status"] == "archived" {
        line.push_str("  (archived)");
    }
    if p["require_review"] == true {
        line.push_str("  [review]");
    }
    line
}

pub fn event_line(e: &Value) -> String {
    let what = e["task_key"].as_str().or(e["after"]["key"].as_str()).or(e["before"]["key"].as_str()).unwrap_or("");
    let undo = e["undoes"].as_i64().map(|u| format!(" (undoes #{u})")).unwrap_or_default();
    let id = format!("#{}", e["id"].as_i64().unwrap_or_default());
    format!("{id:<6} {}  {:<20} {what}{undo}", short_time(s(e, "at")), s(e, "kind"))
}
