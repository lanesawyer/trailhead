use std::sync::OnceLock;

use std::collections::{HashMap, HashSet};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde_json::Value;
use trailhead_client::text::Text;
use trailhead_core::{Claim, Event, Task, TaskDetail};
use uuid::Uuid;

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

/// The shared formatter ([`trailhead_client::text`]), set for a person at this terminal.
pub fn text() -> &'static Text {
    static TEXT: OnceLock<Text> = OnceLock::new();
    TEXT.get_or_init(|| Text::terminal(local_tz().clone()))
}

/// `v` as a `T`, or `v` itself as JSON when it isn't one, so nothing the server sent is hidden.
fn typed<T: serde::de::DeserializeOwned>(v: &Value, show: impl FnOnce(&T) -> String) -> String {
    match serde_json::from_value::<T>(v.clone()) {
        Ok(t) => show(&t),
        Err(_) => v.to_string(),
    }
}

/// "until <when>" for a lease, or "since <when>, no expiry" for a claim that never lapses.
pub fn lease(claim: &Claim) -> String {
    text().lease(claim, Timestamp::now())
}

/// [`task_list`], marking the tasks whose key is in `changes_requested`.
pub fn task_list_flagged(tasks: &[Task], empty: &str, changes_requested: &HashSet<String>) {
    if tasks.is_empty() {
        println!("{empty}");
    }
    for t in tasks {
        println!("{}", text().task_line_flagged(t, changes_requested.contains(t.key.as_str())));
    }
}

pub fn task_list(tasks: &[Task], empty: &str) {
    println!("{}", text().task_list(tasks, empty));
}

/// A task in full, as `GET /v1/tasks/{key}` returns it. `names` maps user and agent ids to names.
pub fn detail(d: &TaskDetail, names: &HashMap<Uuid, String>) -> String {
    let name = |id: Uuid| names.get(&id).cloned().unwrap_or_else(|| id.to_string().chars().take(8).collect());
    text().detail(d, &name, Timestamp::now())
}

pub fn event_line(e: &Value) -> String {
    typed(e, |e: &Event| text().event_line(e))
}
