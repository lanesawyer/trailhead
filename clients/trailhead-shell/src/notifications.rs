//! Notifications (`docs/app-platform.md#notifications`): what the server, the factory or an app tells a person needs
//! them. The server keeps them; the shell mirrors them ([`crate::mirror`], as [`Mirrored<Notification>`]) and shows
//! them: a bell at the rail's foot with the count of unread ones, which rings when one arrives while the window has
//! focus ([`NotificationArrived`]), a count of each app's own on its button on the rail, and the bell's panel listing
//! them (`notifications_panel`).
//!
//! The snapshot loads the newest [`NOTIFICATIONS`](crate::sync::NOTIFICATIONS), read or not; the panel reads older
//! ones a page at a time ([`ShowOlder`]), kept in [`NotificationHistory`] beside the mirror. Marking one read or unread
//! is a command; its event brings the change back to the mirror.
//!
//! An app raises one of its own with [`Notify`] and resolves what it raised about a subject with
//! [`ResolveNotifications`], each sent to the server as a command while the connection is live.
//!
//! A shell with one app has no rail: an app with a place for a bell (Lookout's sidebar footer) puts a button there with
//! [`NotificationBell`] on it while [`Apps::has_rail`](crate::Apps::has_rail) is false, and the shell gives it its count,
//! rings it and opens the panel when it is pressed, as it does the rail's.

use std::cmp::Reverse;
use std::collections::HashMap;

use bevy::prelude::*;
use trailhead_client::http::{Call, NewNotification, NotificationList};
use trailhead_model::{Notification, NotificationAction, SOURCE_FACTORY, SOURCE_SERVER};
use trailhead_ui::Icon;
use uuid::Uuid;

use crate::commands::{Request, Submit};
use crate::connection::{Connection, SendNet};
use crate::mirror::Mirrored;
pub use crate::mirror::NotificationArrived;
use crate::net::{Fetch, NetCommand};
use crate::sync::{NOTIFICATIONS, SyncItem};
use crate::{AppId, Apps};

/// How many older notifications one press of Show older reads.
pub const OLDER_PAGE: usize = 50;

/// What the hint on a bell says.
pub const BELL_HINT: &str = "Notifications";

/// Whether the counts of unread notifications are shown, on the bell and on each app's button on the rail. Shown
/// unless something (a setting) says otherwise.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotificationCounts {
    pub shown: bool,
}

impl Default for NotificationCounts {
    fn default() -> Self {
        NotificationCounts { shown: true }
    }
}

/// On a button that is a bell: the rail's, or an app's own in a shell with one app. A press opens the bell's panel;
/// the shell puts the count of unread notifications on it and rings it when one arrives.
#[derive(Component, Clone, Copy, Default, Debug)]
pub struct NotificationBell;

/// Marks every unread notification about the task with this id read: what an app does when the person opens the
/// task (Lookout's inspector).
#[derive(Event, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadNotificationsAbout {
    pub task_id: Uuid,
}

/// Raises a notification from an app: `commands.trigger(Notify::new(MY_APP, "ticklist.due", "Three are due today"))`,
/// with `.body(..)`, `.subject(..)`, `.task(..)` and `.link(label, url)` as it needs. It is sent to the server as a
/// command and comes back on the event stream like any other, so the person's settings say what it does. Only while
/// the connection is live: raised otherwise it is dropped, with a warning in the log.
#[derive(Event, Clone, Debug, PartialEq, Eq)]
pub struct Notify {
    /// The app raising it, which names it in the panel and counts it on its button.
    pub source: AppId,
    /// The sort of thing it is, as a short name: `ticklist.due`.
    pub kind: String,
    pub title: String,
    /// Plain text, empty for none.
    pub body: String,
    /// What it is about, in the app's own words, to resolve it by later with [`ResolveNotifications`].
    pub subject: Option<String>,
    /// The key or id of the task it is about, if any: it is resolved when the task is finished.
    pub task: Option<String>,
    /// Its link buttons, at most four.
    pub actions: Vec<NotificationAction>,
}

impl Notify {
    pub fn new(source: AppId, kind: impl Into<String>, title: impl Into<String>) -> Self {
        Notify {
            source,
            kind: kind.into(),
            title: title.into(),
            body: String::new(),
            subject: None,
            task: None,
            actions: Vec::new(),
        }
    }

    #[must_use]
    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.body = body.into();
        self
    }

    #[must_use]
    pub fn subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    #[must_use]
    pub fn task(mut self, task: impl Into<String>) -> Self {
        self.task = Some(task.into());
        self
    }

    /// Adds a button that opens `url` in the desktop's browser.
    #[must_use]
    pub fn link(mut self, label: impl Into<String>, url: impl Into<String>) -> Self {
        self.actions.push(NotificationAction { label: label.into(), url: url.into() });
        self
    }

    fn call(&self) -> Call {
        Call::RaiseNotification(NewNotification {
            source: self.source.0.to_string(),
            kind: self.kind.clone(),
            title: self.title.clone(),
            body: self.body.clone(),
            subject: self.subject.clone(),
            task: self.task.clone(),
            actions: self.actions.clone(),
        })
    }
}

/// Resolves every open notification the app `source` raised about `subject`: the thing no longer needs the person.
/// Sent as a command, only while the connection is live.
#[derive(Event, Clone, Debug, PartialEq, Eq)]
pub struct ResolveNotifications {
    pub source: AppId,
    pub subject: String,
}

/// Reads the page of notifications older than any the shell holds, into [`NotificationHistory`].
#[derive(Event, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShowOlder;

/// The notifications older than the snapshot's, read a page at a time, and whether there are any more.
#[derive(Resource, Debug, Default)]
pub struct NotificationHistory {
    older: Vec<Notification>,
    /// Nothing older is left to read.
    end: bool,
    /// The read in flight, by its number.
    asked: Option<u64>,
    next: u64,
    /// Why the last read failed.
    error: Option<String>,
}

impl NotificationHistory {
    /// The older notifications read so far.
    pub fn older(&self) -> &[Notification] {
        &self.older
    }

    /// Whether every notification the server keeps is held.
    pub fn at_end(&self) -> bool {
        self.end
    }

    /// Whether a page is being read.
    pub fn reading(&self) -> bool {
        self.asked.is_some()
    }

    /// Why the last read of a page failed.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// The answer to a read of older notifications.
#[derive(Message, Debug, Clone)]
pub struct OlderNotifications {
    pub seq: u64,
    pub result: Result<NotificationList, String>,
}

/// Every notification the shell holds, the mirror's and the older ones read since, newest first; the mirror's copy
/// wins where both have one, since the stream keeps it.
pub fn known<'a>(
    mirrored: impl IntoIterator<Item = &'a Notification>,
    history: &NotificationHistory,
) -> Vec<Notification> {
    let mut all: Vec<Notification> = mirrored.into_iter().cloned().collect();
    let held: std::collections::HashSet<Uuid> = all.iter().map(|n| n.id).collect();
    all.extend(history.older.iter().filter(|n| !held.contains(&n.id)).cloned());
    all.sort_by_key(|n| Reverse((n.created_at, n.id)));
    all
}

/// How many of `notifications` are unread: in all, and by source.
pub fn unread_counts(notifications: &[Notification]) -> (usize, HashMap<String, usize>) {
    let mut by_source: HashMap<String, usize> = HashMap::new();
    let mut total = 0;
    for n in notifications.iter().filter(|n| n.is_unread()) {
        total += 1;
        *by_source.entry(n.source.clone()).or_default() += 1;
    }
    (total, by_source)
}

/// Who raised a notification, as the panel shows it: an app's icon and name when the source is an app's id, Trailhead
/// for the server, the Factory, and otherwise the source as it is.
pub fn source_label(source: &str, apps: &Apps) -> (Icon, String) {
    if let Some(app) = apps.iter().find(|m| m.id.0 == source) {
        return (app.icon, app.name.to_string());
    }
    match source {
        SOURCE_SERVER => (Icon::Signpost, "Trailhead".into()),
        SOURCE_FACTORY => (Icon::Factory, "Factory".into()),
        other => (Icon::Bell, other.to_string()),
    }
}

/// How long ago `then` was, `now` being seconds since the epoch: "just now", minutes, hours and days, then the date.
pub fn age(then: &trailhead_model::Notification, now: i64) -> String {
    let secs = now.saturating_sub(then.created_at.as_second()).max(0);
    match secs {
        0..60 => "just now".into(),
        60..3_600 => format!("{}m ago", secs / 60),
        3_600..86_400 => format!("{}h ago", secs / 3_600),
        86_400..2_592_000 => format!("{}d ago", secs / 86_400),
        _ => then.created_at.to_string().chars().take(10).collect(),
    }
}

/// The unread count of one app's notifications, for its button on the rail.
pub fn app_count(counts: &HashMap<String, usize>, app: AppId) -> usize {
    counts.get(app.0).copied().unwrap_or_default()
}

/// The notifications' logic, for a shell with a connection.
pub(crate) struct NotificationsPlugin;

impl Plugin for NotificationsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NotificationCounts>()
            .init_resource::<NotificationHistory>()
            .add_message::<OlderNotifications>()
            .add_message::<SendNet>()
            .add_observer(on_show_older)
            .add_observer(on_read_about)
            .add_observer(on_notify)
            .add_observer(on_resolve)
            .add_systems(PreUpdate, forget_older.before(crate::mirror::apply_sync))
            .add_systems(Update, hear_older);
    }
}

/// A new snapshot holds the newest notifications afresh: the older ones read before it are forgotten, and whether there
/// are more is known only when it held fewer than it asked for.
fn forget_older(mut items: MessageReader<SyncItem>, mut history: ResMut<NotificationHistory>) {
    for item in items.read() {
        if let SyncItem::Snapshot(snapshot) = item {
            let end = snapshot.notifications.len() < NOTIFICATIONS;
            *history = NotificationHistory { end, next: history.next, ..default() };
        }
    }
}

fn on_show_older(
    _: On<ShowOlder>,
    connection: Res<State<Connection>>,
    mirrored: Query<&Mirrored<Notification>>,
    mut history: ResMut<NotificationHistory>,
    mut net: MessageWriter<SendNet>,
) {
    if history.end || history.asked.is_some() || *connection.get() != Connection::Live {
        return;
    }
    let oldest = known(mirrored.iter().map(|m| &m.0), &history).last().map(|n| n.id);
    let seq = history.next;
    history.next += 1;
    history.asked = Some(seq);
    history.error = None;
    net.write(SendNet(NetCommand::Fetch(read_older(seq, oldest))));
}

/// Reads the page of notifications raised before `before`, or the newest page when the shell holds none.
fn read_older(seq: u64, before: Option<Uuid>) -> Fetch {
    Fetch::new(move |api| async move {
        let result = match api {
            Some(api) => api.list_notifications(false, before, OLDER_PAGE as i64).await.map_err(|e| e.chain()),
            None => Err("not connected".into()),
        };
        OlderNotifications { seq, result }
    })
}

fn hear_older(mut replies: MessageReader<OlderNotifications>, mut history: ResMut<NotificationHistory>) {
    for reply in replies.read() {
        if history.asked != Some(reply.seq) {
            continue;
        }
        history.asked = None;
        match &reply.result {
            Ok(page) => {
                history.end = page.notifications.len() < OLDER_PAGE;
                let have: std::collections::HashSet<Uuid> = history.older.iter().map(|n| n.id).collect();
                let new: Vec<Notification> =
                    page.notifications.iter().filter(|n| !have.contains(&n.id)).cloned().collect();
                history.older.extend(new);
            }
            Err(why) => history.error = Some(why.clone()),
        }
    }
}

/// Sends a read for each unread notification about the task. Nothing waits on the answers: the stream brings each one
/// read, and one that fails stays unread, to be marked from the panel.
fn on_read_about(
    read: On<ReadNotificationsAbout>,
    connection: Option<Res<State<Connection>>>,
    mirrored: Query<&Mirrored<Notification>>,
    mut submit: MessageWriter<Submit>,
) {
    if connection.is_none_or(|c| *c.get() != Connection::Live) {
        return;
    }
    for n in mirrored.iter().map(|m| &m.0).filter(|n| n.task_id == Some(read.task_id) && n.is_unread()) {
        let request = Request::new(Call::ReadNotification(n.id.to_string()), None);
        submit.write(Submit { source: Entity::PLACEHOLDER, request });
    }
}

/// Sends a command no control waits on, while live.
fn send_while_live(connection: &State<Connection>, submit: &mut MessageWriter<Submit>, call: Call, what: &str) {
    if *connection.get() != Connection::Live {
        warn!(what, "not connected to the server, so it is dropped");
        return;
    }
    submit.write(Submit { source: Entity::PLACEHOLDER, request: Request::new(call, None) });
}

fn on_notify(notify: On<Notify>, connection: Res<State<Connection>>, mut submit: MessageWriter<Submit>) {
    send_while_live(&connection, &mut submit, notify.call(), "an app's notification");
}

fn on_resolve(
    resolve: On<ResolveNotifications>,
    connection: Res<State<Connection>>,
    mut submit: MessageWriter<Submit>,
) {
    let call = Call::ResolveNotifications { source: resolve.source.0.to_string(), subject: resolve.subject.clone() };
    send_while_live(&connection, &mut submit, call, "an app's resolve");
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::fixture::{notification, notification_json, sid, sync_event};
    use crate::sync::Snapshot;
    use crate::testing::{Other, fill, headless, sent, set_connection};
    use crate::{Manifest, ShellApp, ShellPlugin};

    const NOTES: AppId = AppId("notes");

    struct Notes;

    impl ShellApp for Notes {
        fn manifest(&self) -> Manifest {
            Manifest { id: NOTES, name: "Notes", icon: Icon::Flag }
        }

        fn build(&self, _: &mut App) {}
    }

    #[test]
    fn a_source_is_named_by_its_app_or_as_the_server_or_factory() {
        let apps = Apps::new(vec![Notes.manifest()]);
        assert_eq!(source_label("notes", &apps), (Icon::Flag, "Notes".into()));
        assert_eq!(source_label("server", &apps), (Icon::Signpost, "Trailhead".into()));
        assert_eq!(source_label("factory", &apps), (Icon::Factory, "Factory".into()));
        assert_eq!(source_label("ticklist", &apps), (Icon::Bell, "ticklist".into()));
    }

    #[test]
    fn an_age_is_said_in_the_largest_unit_then_as_the_date() {
        let n = notification(json!({"created_at": "2026-10-01T00:00:00Z"}));
        let at = n.created_at.as_second();
        let ages = [0, 59, 60, 3_599, 3_600, 86_399, 86_400, 2_591_999, 2_592_000].map(|s| age(&n, at + s));
        assert_eq!(
            ages,
            ["just now", "just now", "1m ago", "59m ago", "1h ago", "23h ago", "1d ago", "29d ago", "2026-10-01"]
        );
    }

    #[test]
    fn the_mirrors_and_the_older_ones_are_listed_newest_first_and_counted_by_source() {
        let mirrored = [
            notification(json!({"id": "n2", "source": "notes", "created_at": "2026-10-01T00:00:02Z"})),
            notification(json!({"id": "n3", "source": "server", "created_at": "2026-10-01T00:00:03Z",
                                "read_at": "2026-10-01T00:00:04Z"})),
        ];
        let history = NotificationHistory {
            older: vec![
                notification(json!({"id": "n1", "source": "notes", "created_at": "2026-10-01T00:00:01Z"})),
                notification(json!({"id": "n2", "source": "notes", "created_at": "2026-10-01T00:00:02Z",
                                    "resolved_at": "2026-10-01T00:00:05Z"})),
            ],
            ..default()
        };
        let all = known(&mirrored, &history);
        assert_eq!(all.iter().map(|n| n.id.to_string()).collect::<Vec<_>>(), [sid("n3"), sid("n2"), sid("n1")]);
        assert!(all[1].resolved_at.is_none(), "the mirror's copy of n2 wins");
        let (total, by_source) = unread_counts(&all);
        assert_eq!((total, app_count(&by_source, NOTES), by_source.get("server")), (2, 2, None));
    }

    fn snapshot(count: usize) -> Snapshot {
        let notifications =
            (0..count).map(|i| notification_json(json!({"id": format!("n{i}"), "title": format!("{i}")}))).collect();
        Snapshot { at: 5, notifications, ..default() }
    }

    #[test]
    fn a_snapshot_short_of_a_full_load_holds_them_all_and_a_full_one_may_not() {
        let mut app = headless(ShellPlugin::new().with(Notes).with(Other));
        fill(&mut app, snapshot(3));
        assert!(app.world().resource::<NotificationHistory>().at_end());
        fill(&mut app, snapshot(NOTIFICATIONS));
        assert!(!app.world().resource::<NotificationHistory>().at_end());
    }

    #[test]
    fn older_pages_are_read_before_the_oldest_held_until_one_comes_back_short() {
        let mut app = headless(ShellPlugin::new().with(Notes).with(Other));
        fill(&mut app, snapshot(NOTIFICATIONS));
        app.world_mut().trigger(ShowOlder);
        assert!(!app.world().resource::<NotificationHistory>().reading(), "not while offline");
        set_connection(&mut app, Connection::Live);
        app.world_mut().trigger(ShowOlder);
        app.world_mut().trigger(ShowOlder);
        let seq = {
            let history = app.world().resource::<NotificationHistory>();
            assert!(history.reading(), "one read at a time");
            history.asked.unwrap()
        };
        let page = |ids: &[&str]| NotificationList {
            notifications: ids
                .iter()
                .map(|id| notification(json!({"id": id, "created_at": "2026-09-01T00:00:00Z"})))
                .collect(),
            unread: 0,
        };
        app.world_mut().write_message(OlderNotifications { seq, result: Ok(page(&["o1", "o2"])) });
        app.update();
        let history = app.world().resource::<NotificationHistory>();
        assert_eq!(history.older().len(), 2);
        assert!(history.at_end() && !history.reading(), "a short page is the last");
    }

    #[test]
    fn an_app_raises_and_resolves_its_own_as_commands_while_live() {
        let mut app = headless(ShellPlugin::new().with(Notes).with(Other));
        let notify = Notify::new(NOTES, "notes.due", "Three notes are due")
            .body("Today")
            .subject("due")
            .link("Open", "https://example.com/notes");
        app.world_mut().trigger(notify.clone());
        app.update();
        assert!(sent(&mut app).is_empty(), "not while offline");
        set_connection(&mut app, Connection::Live);
        app.world_mut().trigger(notify);
        app.world_mut().trigger(ResolveNotifications { source: NOTES, subject: "due".into() });
        app.update();
        let out = sent(&mut app);
        let calls: Vec<(String, Option<serde_json::Value>)> =
            out.iter().map(|s| (s.request.call.path(), s.request.call.body())).collect();
        assert_eq!(
            calls,
            [
                (
                    "/v1/notifications".into(),
                    Some(json!({"source": "notes", "kind": "notes.due", "title": "Three notes are due",
                                "body": "Today", "subject": "due",
                                "actions": [{"label": "Open", "url": "https://example.com/notes"}]}))
                ),
                ("/v1/notifications/resolve".into(), Some(json!({"source": "notes", "subject": "due"}))),
            ]
        );
    }

    #[test]
    fn opening_a_task_reads_its_unread_notifications() {
        let mut app = headless(ShellPlugin::new().with(Notes).with(Other));
        let about = |id: &str, task: &str, read: bool| {
            let read_at = if read { json!("2026-10-01T00:00:09Z") } else { json!(null) };
            notification_json(json!({"id": id, "task_id": task, "read_at": read_at}))
        };
        let notifications = vec![about("n1", "t1", false), about("n2", "t1", true), about("n3", "t2", false)];
        fill(&mut app, Snapshot { at: 5, notifications, ..default() });
        set_connection(&mut app, Connection::Live);
        app.world_mut().trigger(ReadNotificationsAbout { task_id: crate::fixture::id("t1") });
        app.update();
        let paths: Vec<String> = sent(&mut app).iter().map(|s| s.request.call.path()).collect();
        assert_eq!(paths, [format!("/v1/notifications/{}/read", sid("n1"))]);
    }

    #[test]
    fn a_raised_one_off_the_stream_arrives_and_a_snapshots_or_a_read_one_does_not() {
        #[derive(Resource, Default)]
        struct Heard(Vec<String>);
        let mut app = headless(ShellPlugin::new().with(Notes).with(Other));
        app.init_resource::<Heard>().add_systems(
            Update,
            |mut arrived: MessageReader<NotificationArrived>, mut heard: ResMut<Heard>| {
                heard.0.extend(arrived.read().map(|a| a.0.title.clone()));
            },
        );
        fill(
            &mut app,
            Snapshot {
                at: 5,
                notifications: vec![notification_json(json!({"id": "n1", "title": "old"}))],
                ..default()
            },
        );
        let event = |id: i64, n: &str, kind: &str, read: bool| {
            let at = |on: bool| if on { json!("2026-10-01T00:00:09Z") } else { json!(null) };
            let resolved = kind == "notification.resolved";
            let after =
                notification_json(json!({"id": n, "title": n, "read_at": at(read), "resolved_at": at(resolved)}));
            sync_event(json!({"id": id, "entity": "notification", "entity_id": sid(n), "kind": kind, "after": after}))
        };
        for item in [
            event(6, "n1", "notification.raised", false),
            event(7, "n2", "notification.raised", false),
            event(7, "n2", "notification.raised", false),
            event(8, "n3", "notification.raised", true),
            event(9, "n2", "notification.read", true),
            event(10, "n4", "notification.unread", false),
            event(11, "n1", "notification.resolved", false),
        ] {
            app.world_mut().write_message(item);
        }
        app.update();
        app.update();
        assert_eq!(app.world().resource::<Heard>().0, ["n2"]);
        let world = app.world_mut();
        let mut held: Vec<(String, bool)> = world
            .query::<&Mirrored<Notification>>()
            .iter(world)
            .map(|m| (m.0.title.clone(), m.0.is_unread()))
            .collect();
        held.sort();
        assert_eq!(
            held,
            [("n1".into(), false), ("n2".into(), false), ("n3".into(), false), ("n4".into(), true)],
            "every change lands in the mirror"
        );
    }
}
