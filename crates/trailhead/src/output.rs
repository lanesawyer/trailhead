use std::sync::OnceLock;

use jiff::tz::TimeZone;
use serde_json::Value;

pub fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}

static LOCAL_TZ: OnceLock<TimeZone> = OnceLock::new();

/// The system time zone (honours `TZ`), with DST rules from the zoneinfo database.
pub fn local_tz() -> &'static TimeZone {
    LOCAL_TZ.get_or_init(TimeZone::system)
}

/// `2026-10-15T04:05:00.123Z` -> `2026-10-14 21:05 PDT`, in local time with
/// the zone shown so nobody has to guess whether it's UTC.
pub fn short_time(ts: &str) -> String {
    match ts.parse::<jiff::Timestamp>() {
        Ok(t) => t.to_zoned(local_tz().clone()).strftime("%Y-%m-%d %H:%M %Z").to_string(),
        Err(_) => ts.to_string(),
    }
}

/// `2026-10-15 01:04 PDT (in 29m)`, for deadlines like lease expiry.
pub fn until(ts: &str) -> String {
    let Ok(t) = ts.parse::<jiff::Timestamp>() else { return ts.to_string() };
    let secs = t.as_second() - jiff::Timestamp::now().as_second();
    let (h, m) = (secs.abs() / 3600, secs.abs() % 3600 / 60);
    let span = if h > 0 { format!("{h}h {m}m") } else { format!("{m}m") };
    let rel = if secs >= 0 { format!("in {span}") } else { format!("{span} ago") };
    format!("{} ({rel})", short_time(ts))
}

fn short_date(ts: &str) -> String {
    match ts.parse::<jiff::Timestamp>() {
        Ok(t) => t.to_zoned(local_tz().clone()).strftime("%Y-%m-%d").to_string(),
        Err(_) => ts.to_string(),
    }
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
