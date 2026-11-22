//! Notifications: what tells a person something needs them (`docs/app-platform.md#notifications`). The server keeps
//! them; the shell shows them.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ParseError;

/// The source of what the engine raises by its own rules.
pub const SOURCE_SERVER: &str = "server";
/// The source of what the factory raises.
pub const SOURCE_FACTORY: &str = "factory";

/// An agent asked a question on a task.
pub const QUESTION: &str = "task.question";
/// A task is in review and only a person can approve it.
pub const REVIEW: &str = "task.review";
/// A task is within the reminder window of its deadline. Also the kind of the reminder's event.
pub const DUE_SOON: &str = "task.due_soon";
/// A task's deadline has passed. Also the kind of the reminder's event.
pub const OVERDUE: &str = "task.overdue";

/// The kinds of the events a notification's life is told in. Each carries the notification as its `after`.
pub const RAISED: &str = "notification.raised";
pub const READ: &str = "notification.read";
pub const UNREAD: &str = "notification.unread";
pub const RESOLVED: &str = "notification.resolved";

/// Whether an event of this kind tells a person something rather than changes anything: the reminders, and a
/// notification's own events. Undo leaves them alone, and a client tracking what can be undone leaves them out.
pub fn is_notification(kind: &str) -> bool {
    kind == DUE_SOON || kind == OVERDUE || kind.starts_with("notification.")
}

/// The longest source, kind or subject, in characters.
pub const MAX_NOTIFICATION_NAME: usize = 64;
/// The longest title, in characters.
pub const MAX_NOTIFICATION_TITLE: usize = 200;
/// The longest body, in characters.
pub const MAX_NOTIFICATION_BODY: usize = 2000;
/// The most actions one notification carries.
pub const MAX_NOTIFICATION_ACTIONS: usize = 4;
/// The longest action label, in characters.
pub const MAX_ACTION_LABEL: usize = 40;

/// Something a person is told.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Notification {
    pub id: Uuid,
    pub workspace_id: Uuid,
    /// What raised it: an app's id, [`SOURCE_FACTORY`] or [`SOURCE_SERVER`].
    pub source: String,
    /// The sort of thing it is, as a short name such as [`QUESTION`].
    pub kind: String,
    pub title: String,
    /// Plain text.
    pub body: String,
    /// What it is about, as the raiser names it. The raiser resolves by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// The task it is about, if it is about one. It is resolved when the task is finished or deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_key: Option<String>,
    #[serde(default)]
    pub actions: Vec<NotificationAction>,
    pub raised_by: Uuid,
    pub created_at: Timestamp,
    /// When the person saw it.
    pub read_at: Option<Timestamp>,
    /// When the thing stopped needing them, whoever dealt with it.
    pub resolved_at: Option<Timestamp>,
}

impl Notification {
    /// Whether it still asks for the person: neither read nor resolved.
    pub fn is_unread(&self) -> bool {
        self.read_at.is_none() && self.resolved_at.is_none()
    }
}

/// A button on a notification: a link, opened in the desktop's browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NotificationAction {
    pub label: String,
    pub url: String,
}

/// How far a notification reaches for the person's attention. Every one is recorded whatever its level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum NotificationLevel {
    /// In the list only.
    Silent,
    /// The count, the bell's ring and the sound.
    Bell,
    /// A desktop notification too, when the window has no focus.
    #[default]
    Interrupt,
}

/// A daily span, in minutes from midnight in the local time of the machine showing the window, in which nothing
/// interrupts. One that ends before it starts runs through midnight; one that starts and ends together is empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct QuietHours {
    pub start: u16,
    pub end: u16,
}

impl QuietHours {
    pub fn holds(&self, minute: u16) -> bool {
        if self.start <= self.end {
            (self.start..self.end).contains(&minute)
        } else {
            minute >= self.start || minute < self.end
        }
    }
}

/// Focus, switched on by hand: nothing interrupts until `until`, or until it is switched off when there is none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Focus {
    pub until: Option<Timestamp>,
}

/// What a notification does on arriving, once its source's level, quiet hours and focus are counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Alert {
    /// It is listed, and nothing else.
    None,
    /// It is counted, without a ring, a sound or a desktop notification: its source rings, but it is quiet now.
    Count,
    /// The bell rings, with the sound if that is on.
    Ring,
    /// The bell rings while the window has focus; without focus the desktop shows it.
    Interrupt,
}

/// How a person wants to be told, kept on the server so every machine agrees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NotificationSettings {
    /// The level of each source that has one of its own: an app's id, `factory` or `server`. The rest interrupt.
    #[serde(default)]
    pub levels: std::collections::BTreeMap<String, NotificationLevel>,
    #[serde(default)]
    pub quiet_hours: Option<QuietHours>,
    #[serde(default)]
    pub focus: Option<Focus>,
    /// Whether a sound plays with the ring and the desktop notification.
    #[serde(default = "yes")]
    pub sound: bool,
    /// Whether the bell and the apps on the rail show how many are unread.
    #[serde(default = "yes")]
    pub counts: bool,
}

fn yes() -> bool {
    true
}

impl Default for NotificationSettings {
    fn default() -> Self {
        NotificationSettings { levels: Default::default(), quiet_hours: None, focus: None, sound: true, counts: true }
    }
}

impl NotificationSettings {
    pub fn level_of(&self, source: &str) -> NotificationLevel {
        self.levels.get(source).copied().unwrap_or_default()
    }

    /// Whether focus is on at `now`.
    pub fn focused(&self, now: Timestamp) -> bool {
        self.focus.is_some_and(|f| f.until.is_none_or(|until| now < until))
    }

    /// Whether nothing interrupts at `now`, which is `minute` minutes past midnight where the window is.
    pub fn is_quiet(&self, now: Timestamp, minute: u16) -> bool {
        self.focused(now) || self.quiet_hours.is_some_and(|q| q.holds(minute))
    }

    /// What a notification from `source` does on arriving at `now`.
    pub fn alert(&self, source: &str, now: Timestamp, minute: u16) -> Alert {
        match (self.level_of(source), self.is_quiet(now, minute)) {
            (NotificationLevel::Silent, _) => Alert::None,
            (_, true) => Alert::Count,
            (NotificationLevel::Bell, false) => Alert::Ring,
            (NotificationLevel::Interrupt, false) => Alert::Interrupt,
        }
    }

    /// The settings, checked: sources named as [`notification_name`] has them, quiet hours inside a day.
    pub fn checked(mut self) -> Result<Self, ParseError> {
        self.levels = std::mem::take(&mut self.levels)
            .into_iter()
            .map(|(source, level)| Ok((notification_name("source", &source)?, level)))
            .collect::<Result<_, ParseError>>()?;
        if let Some(q) = self.quiet_hours
            && (q.start >= 24 * 60 || q.end >= 24 * 60)
        {
            return Err(ParseError("quiet hours start and end at a minute of the day, 0 to 1439".into()));
        }
        Ok(self)
    }
}

/// A source, kind or subject, trimmed: 1 to [`MAX_NOTIFICATION_NAME`] characters, none of them a control character.
pub fn notification_name(what: &str, name: &str) -> Result<String, ParseError> {
    text(what, name, MAX_NOTIFICATION_NAME, false)
}

/// A title, trimmed: 1 to [`MAX_NOTIFICATION_TITLE`] characters on one line.
pub fn notification_title(title: &str) -> Result<String, ParseError> {
    text("title", title, MAX_NOTIFICATION_TITLE, false)
}

/// A body, trimmed, cut to [`MAX_NOTIFICATION_BODY`] characters. It may be empty and may hold line breaks.
pub fn notification_body(body: &str) -> String {
    clip(body.trim(), MAX_NOTIFICATION_BODY)
}

/// `s`, cut to `max` characters with an ellipsis in the last place when it was longer.
pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The actions, checked: at most [`MAX_NOTIFICATION_ACTIONS`], each with a label and an `http` or `https` link.
pub fn notification_actions(actions: Vec<NotificationAction>) -> Result<Vec<NotificationAction>, ParseError> {
    if actions.len() > MAX_NOTIFICATION_ACTIONS {
        return Err(ParseError(format!("a notification carries at most {MAX_NOTIFICATION_ACTIONS} actions")));
    }
    actions
        .into_iter()
        .map(|a| {
            let url = a.url.trim();
            let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"));
            if rest.is_none_or(|r| r.is_empty()) || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
                return Err(ParseError(format!("an action's link is an http or https address: {url:?}")));
            }
            Ok(NotificationAction { label: text("action label", &a.label, MAX_ACTION_LABEL, false)?, url: url.into() })
        })
        .collect()
}

fn text(what: &str, s: &str, max: usize, empty: bool) -> Result<String, ParseError> {
    let s = s.trim();
    let n = s.chars().count();
    if (n == 0 && !empty) || n > max || s.chars().any(char::is_control) {
        return Err(ParseError(format!("a notification's {what} is 1 to {max} characters on one line: {s:?}")));
    }
    Ok(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reminders_and_a_notifications_own_events_change_nothing() {
        for kind in [DUE_SOON, OVERDUE, RAISED, READ, UNREAD, RESOLVED] {
            assert!(is_notification(kind), "{kind}");
        }
        for kind in ["task.updated", "comment.created", QUESTION] {
            assert!(!is_notification(kind), "{kind}");
        }
    }

    #[test]
    fn names_and_titles_are_one_short_line() {
        assert_eq!(notification_name("source", " lookout ").unwrap(), "lookout");
        assert!(notification_name("source", "").is_err());
        assert!(notification_name("kind", &"k".repeat(MAX_NOTIFICATION_NAME + 1)).is_err());
        assert!(notification_title("two\nlines").is_err());
        assert_eq!(notification_title(" A question ").unwrap(), "A question");
    }

    #[test]
    fn a_long_body_is_cut_with_an_ellipsis() {
        let body = notification_body(&"é".repeat(MAX_NOTIFICATION_BODY + 5));
        assert_eq!(body.chars().count(), MAX_NOTIFICATION_BODY);
        assert!(body.ends_with('…'));
        assert_eq!(notification_body("  line one\nline two  "), "line one\nline two");
    }

    #[test]
    fn an_action_is_a_labelled_web_link() {
        let link = |url: &str| NotificationAction { label: "Open the PR".into(), url: url.into() };
        assert_eq!(
            notification_actions(vec![link(" https://github.com/a/b/pull/1 ")]).unwrap()[0].url,
            "https://github.com/a/b/pull/1"
        );
        for bad in ["file:///etc/passwd", "https://", "javascript:alert(1)", "https://a b", ""] {
            assert!(notification_actions(vec![link(bad)]).is_err(), "{bad}");
        }
        assert!(
            notification_actions(vec![NotificationAction { label: " ".into(), url: "https://a.b".into() }]).is_err()
        );
        assert!(notification_actions(vec![link("https://a.b"); MAX_NOTIFICATION_ACTIONS + 1]).is_err());
    }
    #[test]
    fn quiet_hours_run_through_midnight() {
        let night = QuietHours { start: 22 * 60, end: 7 * 60 };
        assert!(night.holds(23 * 60) && night.holds(0) && night.holds(6 * 60 + 59));
        assert!(!night.holds(7 * 60) && !night.holds(12 * 60) && !night.holds(21 * 60 + 59));
        let lunch = QuietHours { start: 12 * 60, end: 13 * 60 };
        assert!(lunch.holds(12 * 60) && !lunch.holds(13 * 60) && !lunch.holds(0));
        assert!(!QuietHours { start: 60, end: 60 }.holds(60));
    }

    #[test]
    fn a_level_quiet_hours_and_focus_decide_what_an_arrival_does() {
        let now: Timestamp = "2026-11-21T20:00:00Z".parse().unwrap();
        let mut s = NotificationSettings::default();
        s.levels.insert("ticklist".into(), NotificationLevel::Bell);
        s.levels.insert("noisy".into(), NotificationLevel::Silent);
        assert_eq!(s.alert("lookout", now, 600), Alert::Interrupt, "a source with no level of its own interrupts");
        assert_eq!((s.alert("ticklist", now, 600), s.alert("noisy", now, 600)), (Alert::Ring, Alert::None));

        s.quiet_hours = Some(QuietHours { start: 22 * 60, end: 7 * 60 });
        assert_eq!(s.alert("lookout", now, 23 * 60), Alert::Count);
        assert_eq!(s.alert("noisy", now, 23 * 60), Alert::None);
        assert_eq!(s.alert("lookout", now, 12 * 60), Alert::Interrupt);

        s.focus = Some(Focus { until: Some("2026-11-21T21:00:00Z".parse().unwrap()) });
        assert_eq!(s.alert("lookout", now, 12 * 60), Alert::Count);
        assert_eq!(s.alert("lookout", "2026-11-21T21:00:00Z".parse().unwrap(), 12 * 60), Alert::Interrupt);
        s.focus = Some(Focus { until: None });
        assert!(s.focused("2030-01-01T00:00:00Z".parse().unwrap()), "focus with no end holds until switched off");
    }

    #[test]
    fn settings_read_from_nothing_and_are_checked() {
        let s: NotificationSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, NotificationSettings::default());
        assert!(s.sound && s.counts);
        let late = NotificationSettings { quiet_hours: Some(QuietHours { start: 0, end: 1440 }), ..Default::default() };
        assert!(late.checked().is_err());
        let mut named = NotificationSettings::default();
        named.levels.insert(" lookout ".into(), NotificationLevel::Bell);
        assert_eq!(named.clone().checked().unwrap().level_of("lookout"), NotificationLevel::Bell);
        named.levels.insert(String::new(), NotificationLevel::Bell);
        assert!(named.checked().is_err());
    }
}
