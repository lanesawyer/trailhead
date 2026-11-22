//! The desktop's own notifications (`docs/app-platform.md#notifications`, "On the desktop"): what the shell shows
//! there for a notification that arrives while the window has no focus. With focus the bell's ring is the notice, and
//! this shows nothing.
//!
//! - A few arriving together are shown one by one; more than [`BURST`] in [`BURST_WAIT`] become one saying how many.
//! - What was raised while the window wasn't running is one too, at launch, if the window starts without focus. The
//!   newest notification announced is kept in the shell's `LocalStore` entry (`announced`), per machine.
//! - One read or resolved since, here or on another machine, is taken down.
//! - Pressing one brings the window forward with the bell's panel open. Its link buttons open in the browser and need
//!   no window.
//!
//! The desktop is reached through a [`DesktopLink`]: the session's notification service on Linux ([`dbus`]), run on a
//! thread of its own so a slow bus never holds a frame, and a fake in tests. A shell with no [`Desktop`] resource
//! shows nothing there.

use std::collections::HashMap;
use std::time::Duration;

use bevy::prelude::*;
use trailhead_model::Notification;
use uuid::Uuid;

use crate::mirror::{Mirror, Mirrored, NotificationArrived};
use crate::notifications::source_label;
use crate::notifications_panel::OpenNotifications;
use crate::{Apps, LocalStore, SHELL};

/// The most that arrive together and are still shown one by one.
pub const BURST: usize = 3;
/// How long arrivals are gathered before they are shown, so a burst is seen whole.
pub const BURST_WAIT: Duration = Duration::from_millis(1500);
/// Where the shell keeps the id of the newest notification it has announced on this machine.
const ANNOUNCED: &str = "announced";
/// The key of the one notification that stands for several.
const DIGEST: &str = "digest";
/// How many titles the one that stands for several lists.
const DIGEST_TITLES: usize = 4;

/// One notification on the desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    /// What the shell knows it by: a notification's id, or [`DIGEST`]. Showing one again under its key replaces it.
    pub key: String,
    pub title: String,
    pub body: String,
    /// The labels of its link buttons, in order.
    pub actions: Vec<String>,
    /// Whether the desktop plays its own sound for a message with it.
    pub sound: bool,
}

/// What the shell asks of the desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToDesktop {
    Show(Toast),
    /// Takes down the one with this key, if it is still up.
    Close(String),
}

/// What the desktop says back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromDesktop {
    /// The notification itself was pressed.
    Pressed { key: String },
    /// One of its link buttons was, by its place among them.
    Action { key: String, index: usize },
}

/// A way to the desktop's notifications. Neither call may block.
pub trait DesktopLink: Send + Sync + 'static {
    fn send(&self, message: ToDesktop);
    /// The next thing the desktop said, if it said anything since.
    fn next(&self) -> Option<FromDesktop>;
}

/// The desktop the shell notifies on.
#[derive(Resource)]
pub struct Desktop(pub Box<dyn DesktopLink>);

impl Desktop {
    /// This machine's desktop, if the shell knows how to notify on it and it answers.
    pub fn of_this_machine() -> Option<Desktop> {
        #[cfg(target_os = "linux")]
        {
            dbus::connect().map(|link| Desktop(Box::new(link)))
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }
}

/// Whether a notification that arrives interrupts on the desktop with a sound. A later settings page sets it.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopSound(pub bool);

impl Default for DesktopSound {
    fn default() -> Self {
        DesktopSound(true)
    }
}

/// What is waiting to be shown, and what is up.
#[derive(Resource, Debug, Default)]
struct Announcer {
    /// Arrivals not yet shown, and when the first of them came.
    pending: Vec<Notification>,
    since: Option<Duration>,
    /// The notifications up on the desktop one by one.
    up: HashMap<Uuid, ()>,
    /// Those the one standing for several stands for, while it is up.
    digest: Vec<Uuid>,
    /// Whether the first snapshot has been looked through for what a launch missed.
    launched: bool,
}

/// Where a notification is shown and how.
#[derive(bevy::ecs::system::SystemParam)]
struct Out<'w> {
    desktop: Res<'w, Desktop>,
    sound: Res<'w, DesktopSound>,
    apps: Res<'w, Apps>,
}

fn focused(windows: &Query<&mut Window>) -> bool {
    windows.iter().any(|w| w.focused)
}

fn toast(n: &Notification, apps: &Apps, sound: bool) -> Toast {
    let (_, from) = source_label(&n.source, apps);
    Toast {
        key: n.id.to_string(),
        title: n.title.clone(),
        body: if n.body.is_empty() { from } else { format!("{from}: {}", n.body) },
        actions: n.actions.iter().map(|a| a.label.clone()).collect(),
        sound,
    }
}

fn digest(of: &[Notification], sound: bool) -> Toast {
    let mut body: Vec<String> = of.iter().take(DIGEST_TITLES).map(|n| n.title.clone()).collect();
    if of.len() > DIGEST_TITLES {
        body.push(format!("and {} more", of.len() - DIGEST_TITLES));
    }
    Toast {
        key: DIGEST.into(),
        title: format!("{} things need you", of.len()),
        body: body.join("\n"),
        actions: Vec::new(),
        sound,
    }
}

/// Remembers the newest of `seen` as announced on this machine.
fn remember(store: &mut LocalStore, seen: impl IntoIterator<Item = Uuid>) {
    let kept: Option<Uuid> = store.get(SHELL, ANNOUNCED);
    if let Some(newest) = seen.into_iter().chain(kept).max()
        && Some(newest) != kept
    {
        store.set(SHELL, ANNOUNCED, newest);
    }
}

/// Gathers what arrives while the window has no focus. With focus the bell rings for it, and it is only remembered.
fn gather(
    mut arrived: MessageReader<NotificationArrived>,
    windows: Query<&mut Window>,
    time: Res<Time<Real>>,
    mut announcer: ResMut<Announcer>,
    mut store: ResMut<LocalStore>,
) {
    let new: Vec<Notification> = arrived.read().map(|a| a.0.clone()).collect();
    if new.is_empty() {
        return;
    }
    remember(&mut store, new.iter().map(|n| n.id));
    if focused(&windows) {
        return;
    }
    announcer.since.get_or_insert(time.elapsed());
    announcer.pending.extend(new);
}

/// Shows what was gathered, once [`BURST_WAIT`] has passed since the first: each on its own, or one for them all.
fn announce(
    time: Res<Time<Real>>,
    out: Out,
    mirrored: Query<&Mirrored<Notification>>,
    mut announcer: ResMut<Announcer>,
) {
    let Some(since) = announcer.since else { return };
    if time.elapsed().saturating_sub(since) < BURST_WAIT {
        return;
    }
    announcer.since = None;
    let unread: HashMap<Uuid, bool> = mirrored.iter().map(|m| (m.0.id, m.0.is_unread())).collect();
    let pending: Vec<Notification> =
        std::mem::take(&mut announcer.pending).into_iter().filter(|n| unread.get(&n.id) != Some(&false)).collect();
    show(&out, &mut announcer, &pending);
}

fn show(out: &Out, announcer: &mut Announcer, of: &[Notification]) {
    let (desktop, apps, sound) = (&out.desktop, &out.apps, out.sound.0);
    match of.len() {
        0 => {}
        n if n <= BURST => {
            for (i, n) in of.iter().enumerate() {
                desktop.0.send(ToDesktop::Show(toast(n, apps, sound && i == 0)));
                announcer.up.insert(n.id, ());
            }
        }
        _ => {
            desktop.0.send(ToDesktop::Show(digest(of, sound)));
            announcer.digest = of.iter().map(|n| n.id).collect();
        }
    }
}

/// At the first snapshot: what was raised since this machine last announced one, if the window started without focus.
fn at_launch(
    mirror: Res<Mirror>,
    out: Out,
    windows: Query<&mut Window>,
    mirrored: Query<&Mirrored<Notification>>,
    mut announcer: ResMut<Announcer>,
    mut store: ResMut<LocalStore>,
) {
    if announcer.launched || mirror.snapshots() == 0 {
        return;
    }
    announcer.launched = true;
    let kept: Option<Uuid> = store.get(SHELL, ANNOUNCED);
    let mut missed: Vec<Notification> =
        mirrored.iter().map(|m| &m.0).filter(|n| n.is_unread() && kept.is_none_or(|k| n.id > k)).cloned().collect();
    missed.sort_by_key(|n| std::cmp::Reverse(n.id));
    remember(&mut store, mirrored.iter().map(|m| m.0.id));
    // A machine that has never announced one has nothing it missed: everything unread is old news there.
    if kept.is_some() && !focused(&windows) {
        show(&out, &mut announcer, &missed);
    }
}

/// Takes down what has been read or resolved since it went up.
fn withdraw(
    desktop: Res<Desktop>,
    changed: Query<&Mirrored<Notification>, Changed<Mirrored<Notification>>>,
    mut announcer: ResMut<Announcer>,
) {
    for n in changed.iter().map(|m| &m.0).filter(|n| !n.is_unread()) {
        if announcer.up.remove(&n.id).is_some() {
            desktop.0.send(ToDesktop::Close(n.id.to_string()));
        }
        if !announcer.digest.is_empty() {
            announcer.digest.retain(|id| *id != n.id);
            if announcer.digest.is_empty() {
                desktop.0.send(ToDesktop::Close(DIGEST.into()));
            }
        }
    }
}

/// Hears a press: the notification itself brings the window forward with the bell's panel open, and a link button
/// opens its link.
fn hear(
    desktop: Res<Desktop>,
    mirrored: Query<&Mirrored<Notification>>,
    mut windows: Query<&mut Window>,
    mut announcer: ResMut<Announcer>,
    mut commands: Commands,
) {
    while let Some(said) = desktop.0.next() {
        match said {
            FromDesktop::Pressed { key } => {
                if key == DIGEST {
                    announcer.digest.clear();
                } else if let Ok(id) = key.parse::<Uuid>() {
                    announcer.up.remove(&id);
                }
                for mut window in &mut windows {
                    window.focused = true;
                }
                commands.trigger(OpenNotifications);
            }
            FromDesktop::Action { key, index } => {
                let link = mirrored
                    .iter()
                    .find(|m| m.0.id.to_string() == key)
                    .and_then(|m| m.0.actions.get(index))
                    .map(|a| a.url.clone());
                if let Some(url) = link {
                    trailhead_ui::open_link(&url);
                }
            }
        }
    }
}

/// The desktop's notifications, for a windowed shell with a connection. It does nothing until a [`Desktop`] is there.
pub(crate) struct DesktopPlugin;

impl Plugin for DesktopPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Announcer>().init_resource::<DesktopSound>().add_systems(
            Update,
            (gather, at_launch, announce, withdraw, hear).chain().run_if(resource_exists::<Desktop>),
        );
    }
}

/// The session's notification service (`org.freedesktop.Notifications`), which GNOME and the other desktops answer.
#[cfg(target_os = "linux")]
mod dbus {
    use std::collections::HashMap;
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::sync::{Arc, Mutex};

    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::Value;

    use super::{DesktopLink, FromDesktop, ToDesktop, Toast};

    const SERVICE: &str = "org.freedesktop.Notifications";
    const PATH: &str = "/org/freedesktop/Notifications";
    /// The desktop entry the window runs under, which gives a notification Trailhead's name and icon.
    const ENTRY: &str = "trailhead";
    /// The action a press on the notification itself reports.
    const DEFAULT: &str = "default";
    /// The desktop theme's sound for a message.
    const SOUND: &str = "message-new-instant";

    pub(super) struct Link {
        to: Sender<ToDesktop>,
        from: Mutex<Receiver<FromDesktop>>,
    }

    impl DesktopLink for Link {
        fn send(&self, message: ToDesktop) {
            // The thread is gone only if the bus was lost, and then there is nowhere to show it.
            let _ = self.to.send(message);
        }

        fn next(&self) -> Option<FromDesktop> {
            self.from.lock().ok()?.try_recv().ok()
        }
    }

    /// The desktop's ids of what is up, by the shell's keys.
    type Up = Arc<Mutex<HashMap<String, u32>>>;

    pub(super) fn connect() -> Option<Link> {
        let conn = match Connection::session() {
            Ok(conn) => conn,
            Err(error) => {
                tracing::warn!(%error, "no session bus, so no desktop notifications");
                return None;
            }
        };
        let (to, asked) = channel::<ToDesktop>();
        let (said, from) = channel::<FromDesktop>();
        let up: Up = Arc::default();
        let (listening, heard) = (conn.clone(), up.clone());
        let spawned = std::thread::Builder::new()
            .name("desktop-notify".into())
            .spawn(move || {
                let Ok(proxy) = Proxy::new(&conn, SERVICE, PATH, SERVICE) else { return };
                for message in asked {
                    if let Err(error) = act(&proxy, &up, message) {
                        tracing::warn!(%error, "the desktop's notification service refused");
                    }
                }
            })
            .and_then(|_| {
                std::thread::Builder::new()
                    .name("desktop-notify-heard".into())
                    .spawn(move || listen(&listening, &heard, &said))
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "couldn't start the desktop notification threads");
            return None;
        }
        Some(Link { to, from: Mutex::new(from) })
    }

    fn act(proxy: &Proxy<'_>, up: &Up, message: ToDesktop) -> zbus::Result<()> {
        match message {
            ToDesktop::Show(toast) => {
                let replaces = up.lock().ok().and_then(|up| up.get(&toast.key).copied()).unwrap_or(0);
                let id = notify(proxy, &toast, replaces)?;
                if let Ok(mut up) = up.lock() {
                    up.insert(toast.key, id);
                }
            }
            ToDesktop::Close(key) => {
                let id = up.lock().ok().and_then(|mut up| up.remove(&key));
                if let Some(id) = id {
                    proxy.call::<_, _, ()>("CloseNotification", &(id,))?;
                }
            }
        }
        Ok(())
    }

    fn notify(proxy: &Proxy<'_>, toast: &Toast, replaces: u32) -> zbus::Result<u32> {
        let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
        hints.insert("desktop-entry", Value::from(ENTRY));
        if toast.sound {
            hints.insert("sound-name", Value::from(SOUND));
        }
        let mut actions = vec![DEFAULT.to_string(), "Open".to_string()];
        for (i, label) in toast.actions.iter().enumerate() {
            actions.extend([i.to_string(), label.clone()]);
        }
        let actions: Vec<&str> = actions.iter().map(String::as_str).collect();
        // No markup: the body is another's text, and the service reads `<` as a tag where it supports them.
        let body = toast.body.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        proxy
            .call("Notify", &("Trailhead", replaces, ENTRY, toast.title.as_str(), body.as_str(), actions, hints, -1i32))
    }

    /// Passes on a press of one of ours, and forgets one the desktop closed.
    fn listen(conn: &Connection, up: &Up, said: &Sender<FromDesktop>) {
        let Ok(proxy) = Proxy::new(conn, SERVICE, PATH, SERVICE) else { return };
        let Ok(signals) = proxy.receive_all_signals() else { return };
        for message in signals {
            let header = message.header();
            let key_of = |id: u32| {
                up.lock().ok().and_then(|up| up.iter().find(|(_, held)| **held == id).map(|(key, _)| key.clone()))
            };
            match header.member().map(|m| m.as_str()) {
                Some("ActionInvoked") => {
                    let Ok((id, action)) = message.body().deserialize::<(u32, String)>() else { continue };
                    let Some(key) = key_of(id) else { continue };
                    let heard = match action.parse::<usize>() {
                        Ok(index) => FromDesktop::Action { key, index },
                        Err(_) => FromDesktop::Pressed { key },
                    };
                    if said.send(heard).is_err() {
                        return;
                    }
                }
                Some("NotificationClosed") => {
                    let Ok((id, _reason)) = message.body().deserialize::<(u32, u32)>() else { continue };
                    if let (Some(key), Ok(mut up)) = (key_of(id), up.lock()) {
                        up.remove(&key);
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use bevy::time::TimeUpdateStrategy;
    use serde_json::json;
    use trailhead_ui::{Icon, ModalPlugin};

    use super::*;
    use crate::fixture::{notification_json, sid, sync_event};
    use crate::sync::Snapshot;
    use crate::testing::{Other, feed, fill, settle};
    use crate::{AppId, Manifest, ShellApp, ShellPlugin};

    struct Notes;

    impl ShellApp for Notes {
        fn manifest(&self) -> Manifest {
            Manifest { id: AppId("notes"), name: "Notes", icon: Icon::Flag }
        }

        fn build(&self, _: &mut App) {}
    }

    /// A desktop that keeps what it was asked and says what a test has it say.
    #[derive(Default, Clone)]
    struct Fake {
        asked: Arc<Mutex<Vec<ToDesktop>>>,
        says: Arc<Mutex<VecDeque<FromDesktop>>>,
    }

    impl DesktopLink for Fake {
        fn send(&self, message: ToDesktop) {
            self.asked.lock().unwrap().push(message);
        }

        fn next(&self) -> Option<FromDesktop> {
            self.says.lock().unwrap().pop_front()
        }
    }

    impl Fake {
        fn take(&self) -> Vec<ToDesktop> {
            std::mem::take(&mut self.asked.lock().unwrap())
        }
    }

    /// A windowed shell with a desktop, its window with or without focus, and each update a second long.
    fn shell(has_focus: bool) -> (App, Fake, Entity) {
        let fake = Fake::default();
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            bevy::state::app::StatesPlugin,
            AssetPlugin::default(),
            bevy::scene::ScenePlugin,
            crate::testing::TestConnection,
        ))
        .init_asset::<bevy::text::Font>()
        .init_asset::<Image>()
        .add_plugins(ModalPlugin)
        .insert_resource(Desktop(Box::new(fake.clone())))
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(1)))
        .add_plugins(ShellPlugin::new().with(Notes).with(Other).windowed());
        let window = app.world_mut().spawn(Window { focused: has_focus, ..default() }).id();
        settle(&mut app);
        (app, fake, window)
    }

    fn raised(id: &str, source: &str, extra: serde_json::Value) -> serde_json::Value {
        let mut fields = json!({"id": id, "source": source, "title": format!("about {id}")});
        fields.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        notification_json(fields)
    }

    fn event(app: &mut App, id: i64, n: &str, kind: &str, after: serde_json::Value) {
        feed(
            app,
            sync_event(json!({"id": id, "entity": "notification", "entity_id": sid(n), "kind": kind, "after": after})),
        );
    }

    fn arrive(app: &mut App, id: i64, n: &str, source: &str, extra: serde_json::Value) {
        event(app, id, n, "notification.raised", raised(n, source, extra));
    }

    fn shown(asked: &[ToDesktop]) -> Vec<&Toast> {
        asked.iter().filter_map(|a| if let ToDesktop::Show(t) = a { Some(t) } else { None }).collect()
    }

    #[test]
    fn one_arriving_without_focus_is_shown_with_its_source_and_links_and_none_is_with_focus() {
        let (mut app, fake, window) = shell(false);
        fill(&mut app, Snapshot { at: 1, ..default() });
        let links = json!({"body": "Look at the list",
                           "actions": [{"label": "Pull request", "url": "https://github.com/o/r/pull/1"}]});
        arrive(&mut app, 2, "n1", "notes", links);
        settle(&mut app);
        let asked = fake.take();
        let toasts = shown(&asked);
        assert_eq!(toasts.len(), 1, "{asked:?}");
        assert_eq!((toasts[0].key.as_str(), toasts[0].title.as_str()), (sid("n1").as_str(), "about n1"));
        assert_eq!((toasts[0].body.as_str(), toasts[0].sound), ("Notes: Look at the list", true));
        assert_eq!(toasts[0].actions, ["Pull request"]);

        app.world_mut().get_mut::<Window>(window).unwrap().focused = true;
        arrive(&mut app, 3, "n2", "server", json!({}));
        settle(&mut app);
        assert!(fake.take().is_empty(), "with focus the bell rings instead");
    }

    #[test]
    fn a_burst_is_one_notification_saying_how_many() {
        let (mut app, fake, _) = shell(false);
        fill(&mut app, Snapshot { at: 1, ..default() });
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(10)));
        for i in 0..6 {
            arrive(&mut app, 2 + i, &format!("n{i}"), "factory", json!({}));
        }
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(1)));
        settle(&mut app);
        let asked = fake.take();
        let toasts = shown(&asked);
        assert_eq!(toasts.len(), 1, "{asked:?}");
        assert_eq!((toasts[0].key.as_str(), toasts[0].title.as_str()), (DIGEST, "6 things need you"));
        assert_eq!(toasts[0].body.lines().last(), Some("and 2 more"));

        // It comes down once every one it stood for is read.
        for i in 0..6 {
            let n = format!("n{i}");
            event(
                &mut app,
                20 + i,
                &n,
                "notification.read",
                raised(&n, "factory", json!({"read_at": crate::fixture::at(9)})),
            );
            settle(&mut app);
            let closed = fake.take();
            assert_eq!(closed, if i == 5 { vec![ToDesktop::Close(DIGEST.into())] } else { vec![] });
        }
    }

    #[test]
    fn one_read_or_resolved_elsewhere_is_taken_down_and_one_read_before_it_shows_never_goes_up() {
        let (mut app, fake, _) = shell(false);
        fill(&mut app, Snapshot { at: 1, ..default() });
        arrive(&mut app, 2, "n1", "server", json!({}));
        settle(&mut app);
        assert_eq!(shown(&fake.take()).len(), 1);
        event(
            &mut app,
            3,
            "n1",
            "notification.resolved",
            raised("n1", "server", json!({"resolved_at": crate::fixture::at(9)})),
        );
        settle(&mut app);
        assert_eq!(fake.take(), vec![ToDesktop::Close(sid("n1"))]);

        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(10)));
        arrive(&mut app, 4, "n2", "server", json!({}));
        event(
            &mut app,
            5,
            "n2",
            "notification.read",
            raised("n2", "server", json!({"read_at": crate::fixture::at(9)})),
        );
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(1)));
        settle(&mut app);
        assert!(fake.take().is_empty(), "read within the wait, on another machine say");
    }

    #[test]
    fn pressing_one_brings_the_window_forward_with_the_panel_open() {
        let (mut app, fake, window) = shell(false);
        fill(&mut app, Snapshot { at: 1, ..default() });
        crate::testing::set_connection(&mut app, crate::Connection::Live);
        arrive(&mut app, 2, "n1", "server", json!({}));
        settle(&mut app);
        fake.take();
        fake.says.lock().unwrap().push_back(FromDesktop::Pressed { key: sid("n1") });
        settle(&mut app);
        assert!(app.world().get::<Window>(window).unwrap().focused);
        let mut panels = app.world_mut().query_filtered::<Entity, With<crate::NotificationsPanel>>();
        assert_eq!(panels.iter(app.world()).count(), 1);
    }

    #[test]
    fn a_launch_without_focus_says_what_was_raised_since_this_machine_last_announced_one() {
        let (mut app, fake, _) = shell(false);
        app.world_mut().resource_mut::<LocalStore>().set(SHELL, ANNOUNCED, sid("n2").parse::<Uuid>().unwrap());
        let notifications = vec![
            raised("n4", "server", json!({})),
            raised("n3", "server", json!({"read_at": crate::fixture::at(9)})),
            raised("n2", "server", json!({})),
            raised("n1", "server", json!({})),
        ];
        fill(&mut app, Snapshot { at: 1, notifications, ..default() });
        settle(&mut app);
        let asked = fake.take();
        let toasts = shown(&asked);
        assert_eq!(
            toasts.iter().map(|t| t.key.clone()).collect::<Vec<_>>(),
            [sid("n4")],
            "only the unread one raised since"
        );
        let kept: Option<Uuid> = app.world().resource::<LocalStore>().get(SHELL, ANNOUNCED);
        assert_eq!(kept, sid("n4").parse().ok());

        // A machine that has never announced one says nothing of what is already there.
        let (mut app, fake, _) = shell(false);
        fill(&mut app, Snapshot { at: 1, notifications: vec![raised("n1", "server", json!({}))], ..default() });
        settle(&mut app);
        assert!(fake.take().is_empty());
    }
}
