//! The text the CLI and MCP show for tasks, projects, comments, claims and events: one formatter, so the two read
//! the same. What differs between them is a [`Text`] setting, not separate code:
//!
//! - **Times.** The CLI shows them in the reader's zone. MCP's server has no reader's zone, so it shows a planned time
//!   in the zone it was planned in, and anything else at UTC.
//! - **Layout.** The CLI pads keys and statuses into columns. MCP keeps lines compact and shows each task's and
//!   project's version, which an agent needs for `If-Match`.

use jiff::Timestamp;
use jiff::tz::TimeZone;
use trailhead_model::{Claim, Comment, CommentKind, Event, Project, ProjectStatus, Task, TaskDetail};
use uuid::Uuid;

/// How a [`Text`] lays things out; see the module docs.
#[derive(Debug, Clone)]
pub struct Text {
    /// The reader's zone, or none to show times where they were planned, else at UTC.
    zone: Option<TimeZone>,
    /// Padded columns and two-space gaps rather than compact lines.
    columns: bool,
    /// Show each task's and project's version.
    versions: bool,
}

impl Text {
    /// For a person at a terminal: their own zone, in columns.
    pub fn terminal(zone: TimeZone) -> Text {
        Text { zone: Some(zone), columns: true, versions: false }
    }

    /// For an agent over MCP: compact lines with versions, times where they were planned or at UTC.
    pub fn agent() -> Text {
        Text { zone: None, columns: false, versions: true }
    }

    fn gap(&self) -> &'static str {
        if self.columns { "  " } else { " " }
    }

    /// `2026-10-17 13:00 PDT`. `planned` is the zone a time was set in, if it was.
    pub fn time(&self, t: Timestamp, planned: Option<&str>) -> String {
        let zone = match &self.zone {
            Some(z) => z.clone(),
            None => planned.and_then(|z| TimeZone::get(z).ok()).unwrap_or(TimeZone::UTC),
        };
        t.to_zoned(zone).strftime("%Y-%m-%d %H:%M %Z").to_string()
    }

    /// A task's scheduled day and time, if it has one.
    pub fn scheduled(&self, t: &Task) -> Option<String> {
        self.day_or_time(t.scheduled_on, t.scheduled_at, t.scheduled_tz.as_deref())
    }

    /// A task's due day and time, if it has one.
    pub fn due(&self, t: &Task) -> Option<String> {
        self.day_or_time(t.due_on, t.due_at, t.due_tz.as_deref())
    }

    fn day_or_time(&self, on: Option<jiff::civil::Date>, at: Option<Timestamp>, tz: Option<&str>) -> Option<String> {
        match (at, on) {
            (Some(at), _) => Some(self.time(at, tz)),
            (None, Some(day)) => Some(day.to_string()),
            (None, None) => None,
        }
    }

    /// One line per task: key, status, priority, (version,) title, then its grade and dates.
    pub fn task_line(&self, t: &Task) -> String {
        self.task_line_flagged(t, false)
    }

    /// [`Self::task_line`], saying so when the task is in review with changes requested.
    pub fn task_line_flagged(&self, t: &Task, changes_requested: bool) -> String {
        let mut line = if self.columns {
            format!("{:<10} {:<11} p{}", t.key, t.status.as_str(), t.priority)
        } else {
            format!("{} {} p{}", t.key, t.status, t.priority)
        };
        if self.versions {
            line.push_str(&format!(" v{}", t.version));
        }
        line.push_str(&format!("{}{}", if self.columns { "  " } else { " " }, t.title));
        let gap = self.gap();
        if let Some(g) = t.grade {
            line.push_str(&format!("{gap}(grade {g})"));
        }
        if let Some(w) = self.scheduled(t) {
            line.push_str(&format!("{gap}(scheduled {w})"));
        }
        if let Some(w) = self.due(t) {
            line.push_str(&format!("{gap}(due {w})"));
        }
        if changes_requested {
            line.push_str(&format!("{gap}(changes requested)"));
        }
        line
    }

    /// The tasks a line each, or `empty` when there are none.
    pub fn task_list(&self, tasks: &[Task], empty: &str) -> String {
        if tasks.is_empty() {
            empty.to_string()
        } else {
            tasks.iter().map(|t| self.task_line(t)).collect::<Vec<_>>().join("\n")
        }
    }

    pub fn project_line(&self, p: &Project) -> String {
        let mut line = if self.columns { format!("{:<8} ", p.key) } else { format!("{} ", p.key) };
        if self.versions {
            line.push_str(&format!("v{} ", p.version));
        }
        line.push_str(&p.name);
        let gap = self.gap();
        if p.status == ProjectStatus::Archived {
            line.push_str(&format!("{gap}(archived)"));
        }
        if p.require_review {
            line.push_str(&format!("{gap}[review]"));
        }
        line
    }

    /// A project's line and its brief.
    pub fn project(&self, p: &Project) -> String {
        let brief = if p.brief.is_empty() { "(no brief yet)" } else { &p.brief };
        format!("{}\n\n{brief}", self.project_line(p))
    }

    /// `[kind, to <name>, open, verdict, model] <time> <author>: <body>`, the id on the next line. `names` turns a
    /// user or agent id into a name.
    pub fn comment_line(&self, c: &Comment, names: &dyn Fn(Uuid) -> String) -> String {
        let mut tags = vec![c.kind.to_string()];
        if let Some(to) = c.addressed_to {
            tags.push(format!("to {}", names(to)));
        }
        if c.kind == CommentKind::Question {
            tags.push(if c.resolved_at.is_some() { "resolved".into() } else { "open".into() });
        }
        if let Some(v) = c.verdict {
            tags.push(v.as_str().replace('_', " "));
        }
        if let Some(m) = &c.model {
            tags.push(m.clone());
        }
        format!(
            "[{}] {} {}: {}\n    id {}",
            tags.join(", "),
            self.time(c.created_at, None),
            names(c.author_id),
            c.body.replace('\n', "\n    "),
            c.id
        )
    }

    /// `until <time> (in 29m)`, or `since <time> (3h ago), no expiry` for a claim that never lapses.
    pub fn lease(&self, c: &Claim, now: Timestamp) -> String {
        match c.expires_at {
            Some(e) => {
                let secs = e.as_second() - now.as_second();
                let rel = if secs >= 0 { format!("in {}", span(secs)) } else { format!("{} ago", span(secs)) };
                format!("until {} ({rel})", self.time(e, None))
            }
            None => {
                let age = (now.as_second() - c.claimed_at.as_second()).max(0);
                format!("since {} ({} ago), no expiry", self.time(c.claimed_at, None), span(age))
            }
        }
    }

    /// A task in full: its key and title, a line of its status, priority, grade, version and dates, its claim and
    /// links, its metadata, its description and its comments.
    pub fn detail(&self, d: &TaskDetail, names: &dyn Fn(Uuid) -> String, now: Timestamp) -> String {
        let t = &d.task;
        let mut out = vec![format!("{}  {}", t.key, t.title)];
        let status = if d.changes_requested.is_some() {
            format!("{} (changes requested)", t.status)
        } else {
            t.status.to_string()
        };
        let mut meta = vec![status, format!("p{}", t.priority)];
        if let Some(g) = t.grade {
            meta.push(format!("grade {g}"));
        }
        meta.push(format!("v{}", t.version));
        if let Some(w) = self.scheduled(t) {
            meta.push(format!("scheduled {w}"));
        }
        if let Some(w) = self.due(t) {
            meta.push(format!("due {w}"));
        }
        out.push(meta.join(" · "));
        if let Some(c) = &d.claim {
            out.push(format!("claimed by {} {}", names(c.holder_id), self.lease(c, now)));
        }
        for (label, keys) in [("blocked by", &d.blocked_by), ("blocks", &d.blocks)] {
            if !keys.is_empty() {
                out.push(format!("{label} {}", keys.iter().map(|k| k.0.as_str()).collect::<Vec<_>>().join(", ")));
            }
        }
        if let Some(k) = &d.duplicate_of {
            out.push(format!("duplicate of {k}"));
        }
        if !d.duplicates.is_empty() {
            out.push(format!(
                "duplicates {}",
                d.duplicates.iter().map(|k| k.0.as_str()).collect::<Vec<_>>().join(", ")
            ));
        }
        if t.metadata.as_object().is_some_and(|m| !m.is_empty()) {
            out.push(format!("metadata {}", t.metadata));
        }
        if !t.description.is_empty() {
            out.push(String::new());
            out.push(t.description.clone());
        }
        if !d.comments.is_empty() {
            out.push(String::new());
            out.extend(d.comments.iter().map(|c| self.comment_line(c, names)));
        }
        out.join("\n")
    }

    /// `#41    2026-10-17 13:00 PDT  task.updated         AUTH-3 (undoes #40)`.
    pub fn event_line(&self, e: &Event) -> String {
        let key = |v: &Option<serde_json::Value>| v.as_ref().and_then(|v| v["key"].as_str().map(str::to_string));
        let what = e.task_key.clone().or_else(|| key(&e.after)).or_else(|| key(&e.before)).unwrap_or_default();
        let undo = e.undoes.map(|u| format!(" (undoes #{u})")).unwrap_or_default();
        let id = format!("#{}", e.id);
        format!("{id:<6} {}  {:<20} {what}{undo}", self.time(e.at, None), e.kind)
    }
}

/// `29m`, `3h 5m` or `2d 4h`.
fn span(secs: i64) -> String {
    let (d, h, m) = (secs.abs() / 86_400, secs.abs() % 86_400 / 3600, secs.abs() % 3600 / 60);
    match (d, h) {
        (0, 0) => format!("{m}m"),
        (0, _) => format!("{h}h {m}m"),
        _ => format!("{d}d {h}h"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn task(extra: serde_json::Value) -> Task {
        let mut t = json!({
            "id": "00000000-0000-0000-0000-000000000001", "workspace_id": "00000000-0000-0000-0000-000000000002",
            "project_id": "00000000-0000-0000-0000-000000000003", "key": "AUTH-3", "title": "plan it",
            "description": "", "status": "in_progress", "priority": 2, "grade": 3, "due_at": null, "due_tz": null,
            "parent_id": null, "metadata": {}, "version": 4, "source": null, "external_id": null,
            "created_by": "00000000-0000-0000-0000-000000000004", "created_at": "2026-10-01T00:00:00Z",
            "updated_at": "2026-10-01T00:00:00Z", "deleted_at": null,
        });
        t.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        serde_json::from_value(t).unwrap()
    }

    #[test]
    fn a_terminal_pads_columns_and_an_agent_sees_versions() {
        let t = task(json!({"scheduled_on": "2026-10-17", "due_at": "2026-11-01T12:00:00Z"}));
        assert_eq!(
            Text::terminal(TimeZone::UTC).task_line(&t),
            "AUTH-3     in_progress p2  plan it  (grade 3)  (scheduled 2026-10-17)  (due 2026-11-01 12:00 UTC)"
        );
        assert_eq!(
            Text::agent().task_line(&t),
            "AUTH-3 in_progress p2 v4 plan it (grade 3) (scheduled 2026-10-17) (due 2026-11-01 12:00 UTC)"
        );
    }

    #[test]
    fn an_agent_sees_a_planned_time_in_its_zone_and_a_terminal_in_its_own() {
        let t = task(json!({"scheduled_at": "2026-10-17T20:00:00Z", "scheduled_tz": "America/Los_Angeles"}));
        assert_eq!(Text::agent().scheduled(&t).unwrap(), "2026-10-17 13:00 PDT");
        let tokyo = TimeZone::get("Asia/Tokyo").unwrap();
        assert_eq!(Text::terminal(tokyo).scheduled(&t).unwrap(), "2026-10-18 05:00 JST");
    }

    #[test]
    fn a_lease_says_how_long_is_left_or_that_it_never_lapses() {
        let claim = |expires: serde_json::Value| -> Claim {
            serde_json::from_value(json!({
                "task_id": "00000000-0000-0000-0000-000000000001", "workspace_id": "00000000-0000-0000-0000-000000000002",
                "holder_id": "00000000-0000-0000-0000-000000000005", "on_behalf_of": "00000000-0000-0000-0000-000000000005",
                "claimed_at": "2026-10-15T01:00:00Z", "expires_at": expires,
            }))
            .unwrap()
        };
        let now: Timestamp = "2026-10-15T04:00:00Z".parse().unwrap();
        let text = Text::agent();
        assert_eq!(text.lease(&claim(json!("2026-10-15T04:29:00Z")), now), "until 2026-10-15 04:29 UTC (in 29m)");
        assert_eq!(text.lease(&claim(json!(null)), now), "since 2026-10-15 01:00 UTC (3h 0m ago), no expiry");
    }
}
