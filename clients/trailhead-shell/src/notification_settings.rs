//! How the person wants to be told (`docs/app-platform.md#notifications`, "Levels" and "Quiet hours and focus"),
//! headless: the settings the server keeps, held in [`NotificationPrefs`], and what an arriving notification does under
//! them.
//!
//! - **Read** when the connection goes live, every [`POLL`] while it is, after a save, and when the Notifications page
//!   opens (`GET /v1/notifications/settings`). Until the server has said them, the defaults hold.
//! - **Saved** whole, as a command (`PUT /v1/notifications/settings`, [`SaveNotificationSettings`]), from the settings
//!   last read with only the edited part changed: the Notifications page's draft ([`NotificationsPage`]) or the bell
//!   panel's focus. What the page shows as kept is what a read after the save says, never the edit.
//! - **What an arrival does** is [`NotificationSettings::alert`] at the [`Clock`]'s time and minute of the day: a silent
//!   source is listed only (and left out of every count); during quiet hours or focus it is counted and held; a bell
//!   source rings the bell; an interrupting one rings it while the window has focus and goes to the desktop while it
//!   has none. A ring plays the [`Sound`] when the settings say so.
//! - **When quiet hours or focus end**, what was held and is still unread is said once: one ring of the bell with focus,
//!   one desktop notification for those from interrupting sources without.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use jiff::Zoned;
use trailhead_client::http::Call;
use trailhead_model::{Alert, Focus, Notification, NotificationLevel, NotificationSettings, QuietHours};
use uuid::Uuid;

use crate::commands::{CommandDone, Outcome, Request, Submit};
use crate::connection::{Connection, SendNet};
use crate::mirror::{Mirrored, NotificationArrived};
use crate::net::{Fetch, NetCommand};
use crate::notifications::NotificationCounts;
use crate::sound::Sound;

/// How often a live shell reads the settings again, for a change made on another machine.
pub const POLL: Duration = Duration::from_secs(60);

/// The quiet hours a person starts from when switching them on: ten at night to seven in the morning.
pub const DEFAULT_QUIET: QuietHours = QuietHours { start: 22 * 60, end: 7 * 60 };

/// How far apart the times offered for quiet hours are, in minutes.
pub const TIME_STEP: u16 = 30;

/// The person's settings, as the server last said them.
#[derive(Resource, Debug, Clone, Default, PartialEq, Eq)]
pub struct NotificationPrefs {
    /// The defaults until the server has said them.
    pub settings: NotificationSettings,
    /// The server has said them since the shell started.
    pub answered: bool,
    /// Why the last read failed.
    pub error: Option<String>,
}

impl NotificationPrefs {
    /// Whether the shell has heard back about them, either way: what waits on them (the desktop's notice of what a
    /// launch missed) need wait no longer.
    pub fn heard(&self) -> bool {
        self.answered || self.error.is_some()
    }
}

/// The time where the window is: the machine's own clock and zone, or a test's.
#[derive(Resource, Clone)]
pub struct Clock(Arc<dyn Fn() -> Zoned + Send + Sync>);

impl Default for Clock {
    fn default() -> Self {
        Clock(Arc::new(Zoned::now))
    }
}

impl Clock {
    /// A clock that tells the time `now` says.
    pub fn new(now: impl Fn() -> Zoned + Send + Sync + 'static) -> Self {
        Clock(Arc::new(now))
    }

    pub fn now(&self) -> Zoned {
        (self.0)()
    }
}

/// Minutes past midnight at `at`, where it is.
pub fn minute_of_day(at: &Zoned) -> u16 {
    (at.hour() as u16) * 60 + at.minute() as u16
}

/// A minute of the day as a time: `07:30`.
pub fn time_words(minute: u16) -> String {
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

/// The times quiet hours can start or end at: every [`TIME_STEP`] minutes, and the ones `kept` names between them.
pub fn time_options(kept: &[u16]) -> Vec<u16> {
    let mut times: Vec<u16> = (0..24 * 60).step_by(TIME_STEP as usize).chain(kept.iter().copied()).collect();
    times.sort_unstable();
    times.dedup();
    times
}

/// What the bell's panel says of focus at `now`.
pub fn focus_text(settings: &NotificationSettings, now: &Zoned) -> String {
    match settings.focus {
        Some(focus) if settings.focused(now.timestamp()) => match focus.until {
            None => "Focus is on until you end it.".into(),
            Some(until) => {
                let local = until.to_zoned(now.time_zone().clone());
                let format = if local.date() == now.date() { "%H:%M" } else { "%a %H:%M" };
                format!("Focus is on until {}.", local.strftime(format))
            }
        },
        _ => "Focus is off. Notifications interrupt as their levels say.".into(),
    }
}

/// The settings last read with focus set to `focus`, and nothing else changed.
pub fn with_focus(kept: &NotificationSettings, focus: Option<Focus>) -> NotificationSettings {
    NotificationSettings { focus, ..kept.clone() }
}

/// What the person has changed on the Notifications page and not saved: only the parts edited, each to apply over the
/// settings last read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotificationDraft {
    pub levels: BTreeMap<String, NotificationLevel>,
    pub quiet_hours: Option<Option<QuietHours>>,
    pub sound: Option<bool>,
    pub counts: Option<bool>,
}

impl NotificationDraft {
    /// The settings `kept` with these edits made.
    pub fn apply(&self, kept: &NotificationSettings) -> NotificationSettings {
        let mut settings = kept.clone();
        settings.levels.extend(self.levels.iter().map(|(source, level)| (source.clone(), *level)));
        if let Some(quiet) = self.quiet_hours {
            settings.quiet_hours = quiet;
        }
        settings.sound = self.sound.unwrap_or(settings.sound);
        settings.counts = self.counts.unwrap_or(settings.counts);
        settings
    }

    /// Sets `source`'s level, or drops the edit when it is what `kept` says.
    pub fn set_level(&mut self, source: &str, level: NotificationLevel, kept: &NotificationSettings) {
        if kept.level_of(source) == level {
            self.levels.remove(source);
        } else {
            self.levels.insert(source.to_string(), level);
        }
    }

    /// Sets the quiet hours, or drops the edit when they are what `kept` says.
    pub fn set_quiet_hours(&mut self, quiet: Option<QuietHours>, kept: &NotificationSettings) {
        self.quiet_hours = (kept.quiet_hours != quiet).then_some(quiet);
    }

    pub fn set_sound(&mut self, on: bool, kept: &NotificationSettings) {
        self.sound = (kept.sound != on).then_some(on);
    }

    pub fn set_counts(&mut self, shown: bool, kept: &NotificationSettings) {
        self.counts = (kept.counts != shown).then_some(shown);
    }
}

/// The Notifications page: its draft and its save.
#[derive(Resource, Debug, Clone, Default, PartialEq, Eq)]
pub struct NotificationsPage {
    pub draft: NotificationDraft,
    /// The save is with the server, or the read that shows it is.
    pub saving: bool,
    /// What the server said when it refused the last save.
    pub refusal: Option<String>,
    /// The last save was kept, and the page shows it as read back.
    pub saved: bool,
    /// The read whose answer shows the save.
    confirm: Option<u64>,
}

/// Whether the page's Save can be pressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    Ready,
    /// Nothing differs from what the server keeps.
    Unchanged,
    Saving,
    /// Not now, and why.
    Blocked(String),
}

impl NotificationsPage {
    /// What the page shows: the settings last read with its edits made.
    pub fn shown(&self, prefs: &NotificationPrefs) -> NotificationSettings {
        self.draft.apply(&prefs.settings)
    }

    /// Whether Save can be pressed now, and why not.
    pub fn gate(&self, prefs: &NotificationPrefs, live: bool, offline: Option<String>) -> Gate {
        if !live {
            return Gate::Blocked(
                offline.map_or("Not connected to the server.".into(), |r| format!("Not connected: {r}.")),
            );
        }
        if self.saving {
            return Gate::Saving;
        }
        if !prefs.answered {
            return Gate::Blocked("Your settings haven't been read from the server yet.".into());
        }
        if self.shown(prefs) == prefs.settings { Gate::Unchanged } else { Gate::Ready }
    }

    /// An edit was made: what was said of the last save no longer holds.
    pub fn edited(&mut self) {
        self.saved = false;
        self.refusal = None;
    }
}

/// Reads the settings from the server now.
#[derive(Event, Debug, Clone, Copy, Default)]
pub struct ReadNotificationSettings;

/// Saves `settings` whole, as a command whose answer goes back to `source` (the control pressed). The settings are
/// read again once the server has taken them.
#[derive(Event, Debug, Clone, PartialEq, Eq)]
pub struct SaveNotificationSettings {
    pub source: Entity,
    pub settings: NotificationSettings,
}

/// Saves the Notifications page's draft over the settings last read, if [`NotificationsPage::gate`] lets it.
#[derive(Event, Debug, Clone, Copy)]
pub struct SaveNotificationsPage {
    pub source: Entity,
}

/// The server's answer to the read sent as `seq`.
#[derive(Message, Debug, Clone)]
pub struct NotificationSettingsRead {
    pub seq: u64,
    pub result: Result<NotificationSettings, String>,
}

/// Rings every bell: one notification or more is to be rung for now.
#[derive(Message, Debug, Clone, Copy, Default)]
pub struct RingBells;

/// For the desktop: one that arrived to interrupt while the window has no focus.
#[derive(Message, Debug, Clone)]
pub(crate) struct Interrupt(pub Notification);

/// For the desktop: quiet hours or focus ended without window focus, and these arrived meanwhile, newest first.
#[derive(Message, Debug, Clone)]
pub(crate) struct QuietEnded(pub Vec<Notification>);

/// Whether it was quiet when last looked, and what arrived to be counted while it was.
#[derive(Resource, Debug, Default)]
pub(crate) struct Quiet {
    was: bool,
    held: Vec<Uuid>,
}

impl Quiet {
    /// Holds these for the notice when quiet ends.
    pub(crate) fn hold(&mut self, ids: impl IntoIterator<Item = Uuid>) {
        self.held.extend(ids);
    }
}

/// The reads sent, and when the last one was.
#[derive(Resource, Debug, Default)]
struct Reads {
    seq: u64,
    asked: Option<Duration>,
}

/// The saves sent, by the control each answers: whether each is the page's.
#[derive(Resource, Debug, Default)]
struct Saves(Vec<(Entity, bool)>);

/// The settings' logic, for a shell with a connection.
pub(crate) struct NotificationSettingsPlugin;

impl Plugin for NotificationSettingsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NotificationPrefs>()
            .init_resource::<NotificationsPage>()
            .init_resource::<Clock>()
            .init_resource::<Quiet>()
            .init_resource::<Reads>()
            .init_resource::<Saves>()
            .add_message::<NotificationSettingsRead>()
            .add_message::<RingBells>()
            .add_message::<Interrupt>()
            .add_message::<QuietEnded>()
            .add_message::<SendNet>()
            .add_observer(on_read)
            .add_observer(on_save)
            .add_observer(on_save_page)
            .add_systems(OnEnter(Connection::Live), read_now)
            .add_systems(PreUpdate, decide.after(crate::mirror::apply_sync))
            .add_systems(
                Update,
                (poll.run_if(in_state(Connection::Live)), hear_done, hear_read, follow_counts).chain(),
            );
    }
}

fn read(reads: &mut Reads, now: Duration, net: &mut MessageWriter<SendNet>) -> u64 {
    reads.seq += 1;
    reads.asked = Some(now);
    let seq = reads.seq;
    net.write(SendNet(NetCommand::Fetch(Fetch::new(move |api| async move {
        let result = match api {
            Some(api) => api.notification_settings().await.map_err(|e| e.chain()),
            None => Err("not connected".into()),
        };
        NotificationSettingsRead { seq, result }
    }))));
    seq
}

/// Reads them as the connection goes live, before anything else asks the server.
fn read_now(time: Res<Time<Real>>, mut reads: ResMut<Reads>, mut net: MessageWriter<SendNet>) {
    read(&mut reads, time.elapsed(), &mut net);
}

fn poll(time: Res<Time<Real>>, mut reads: ResMut<Reads>, mut net: MessageWriter<SendNet>) {
    let now = time.elapsed();
    if reads.asked.is_none_or(|at| now.saturating_sub(at) >= POLL) {
        read(&mut reads, now, &mut net);
    }
}

fn on_read(
    _: On<ReadNotificationSettings>,
    connection: Res<State<Connection>>,
    time: Res<Time<Real>>,
    mut reads: ResMut<Reads>,
    mut net: MessageWriter<SendNet>,
) {
    if *connection.get() == Connection::Live {
        read(&mut reads, time.elapsed(), &mut net);
    }
}

fn submit(submit: &mut MessageWriter<Submit>, source: Entity, settings: NotificationSettings) {
    submit.write(Submit { source, request: Request::new(Call::SetNotificationSettings(settings), None) });
}

fn on_save(
    save: On<SaveNotificationSettings>,
    connection: Res<State<Connection>>,
    mut saves: ResMut<Saves>,
    mut out: MessageWriter<Submit>,
) {
    if *connection.get() == Connection::Live {
        saves.0.push((save.source, false));
        submit(&mut out, save.source, save.settings.clone());
    }
}

fn on_save_page(
    save: On<SaveNotificationsPage>,
    connection: Res<State<Connection>>,
    prefs: Res<NotificationPrefs>,
    mut page: ResMut<NotificationsPage>,
    mut saves: ResMut<Saves>,
    mut out: MessageWriter<Submit>,
) {
    if page.gate(&prefs, *connection.get() == Connection::Live, None) != Gate::Ready {
        return;
    }
    let settings = page.shown(&prefs);
    *page = NotificationsPage { draft: page.draft.clone(), saving: true, ..default() };
    saves.0.push((save.source, true));
    submit(&mut out, save.source, settings);
}

/// A save's answer: taken, and the settings are read again to show it; or refused, said on the page if it was the
/// page's, and by the control's own owner otherwise.
fn hear_done(
    mut done: MessageReader<CommandDone>,
    time: Res<Time<Real>>,
    mut saves: ResMut<Saves>,
    mut reads: ResMut<Reads>,
    mut page: ResMut<NotificationsPage>,
    mut net: MessageWriter<SendNet>,
) {
    for d in done.read() {
        let Some(at) = saves.0.iter().position(|(source, _)| *source == d.source) else { continue };
        let (_, from_page) = saves.0.remove(at);
        match &d.outcome {
            Outcome::Ok { .. } => {
                let seq = read(&mut reads, time.elapsed(), &mut net);
                if from_page {
                    page.confirm = Some(seq);
                }
            }
            refused if from_page => {
                page.saving = false;
                page.refusal = Some(describe(refused));
            }
            _ => {}
        }
    }
}

/// An outcome that isn't `Ok`, in words.
pub fn describe(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Ok { .. } => String::new(),
        Outcome::Conflict { message, .. } | Outcome::RateLimited { message, .. } | Outcome::Failed(message) => {
            message.clone()
        }
        Outcome::Forbidden(message) => format!("Not allowed: {message}"),
    }
}

/// A reading: the settings now held. Only the latest read counts, so one sent before a save never undoes it. The read
/// that follows the page's save ends it: the draft is dropped, and the page shows what was read.
fn hear_read(
    mut replies: MessageReader<NotificationSettingsRead>,
    reads: Res<Reads>,
    mut prefs: ResMut<NotificationPrefs>,
    mut page: ResMut<NotificationsPage>,
) {
    for NotificationSettingsRead { seq, result } in replies.read() {
        if *seq != reads.seq {
            continue;
        }
        match result {
            Ok(settings) => {
                prefs.set_if_neq(NotificationPrefs { settings: settings.clone(), answered: true, error: None });
            }
            Err(why) => prefs.error = Some(why.clone()),
        }
        if page.confirm.is_some_and(|c| *seq >= c) {
            *page = NotificationsPage { saved: true, ..default() };
        }
    }
}

/// Hides the counts when the settings say so.
fn follow_counts(prefs: Res<NotificationPrefs>, mut counts: ResMut<NotificationCounts>) {
    if prefs.is_changed() {
        counts.set_if_neq(NotificationCounts { shown: prefs.settings.counts });
    }
}

/// Where what an arrival does goes.
#[derive(SystemParam)]
struct Alerts<'w> {
    ring: MessageWriter<'w, RingBells>,
    interrupt: MessageWriter<'w, Interrupt>,
    ended: MessageWriter<'w, QuietEnded>,
    sound: Option<Res<'w, Sound>>,
}

/// Decides what each arrival does, and says what arrived while it was quiet once it no longer is.
fn decide(
    mut arrived: MessageReader<NotificationArrived>,
    prefs: Res<NotificationPrefs>,
    clock: Res<Clock>,
    windows: Query<&Window>,
    mirrored: Query<&Mirrored<Notification>>,
    mut quiet: ResMut<Quiet>,
    mut out: Alerts,
) {
    let now = clock.now();
    let (at, minute) = (now.timestamp(), minute_of_day(&now));
    let settings = &prefs.settings;
    let focused = windows.iter().any(|w| w.focused);
    let mut ring = false;
    for NotificationArrived(n) in arrived.read() {
        match settings.alert(&n.source, at, minute) {
            Alert::None => {}
            Alert::Count => quiet.held.push(n.id),
            Alert::Ring => ring = true,
            Alert::Interrupt if focused => ring = true,
            Alert::Interrupt => {
                out.interrupt.write(Interrupt(n.clone()));
            }
        }
    }
    let is_quiet = settings.is_quiet(at, minute);
    if quiet.was && !is_quiet {
        let held = std::mem::take(&mut quiet.held);
        let mut meanwhile: Vec<Notification> = mirrored
            .iter()
            .map(|m| &m.0)
            .filter(|n| n.is_unread() && held.contains(&n.id))
            .filter(|n| settings.level_of(&n.source) != NotificationLevel::Silent)
            .cloned()
            .collect();
        meanwhile.sort_by_key(|n| std::cmp::Reverse((n.created_at, n.id)));
        if focused {
            ring |= !meanwhile.is_empty();
        } else {
            meanwhile.retain(|n| settings.level_of(&n.source) == NotificationLevel::Interrupt);
            if !meanwhile.is_empty() {
                out.ended.write(QuietEnded(meanwhile));
            }
        }
    }
    quiet.was = is_quiet;
    if ring {
        out.ring.write(RingBells);
        if settings.sound
            && let Some(sound) = &out.sound
        {
            sound.0.play();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::ShellPlugin;
    use crate::fixture::{notification_json, sid, sync_event};
    use crate::sound::Speaker;
    use crate::sync::Snapshot;
    use crate::testing::{Other, answer, feed, fill, headless, sent, set_connection};

    /// A speaker that counts what it played.
    #[derive(Clone, Default)]
    struct Counted(Arc<Mutex<usize>>);

    impl Speaker for Counted {
        fn play(&self) {
            *self.0.lock().unwrap() += 1;
        }
    }

    /// What the logic told the bells and the desktop, gathered each frame.
    #[derive(Resource, Default)]
    struct Told {
        rings: usize,
        interrupts: Vec<String>,
        ended: Vec<Vec<String>>,
        fetches: Vec<Fetch>,
    }

    fn gather(
        mut rings: MessageReader<RingBells>,
        mut interrupts: MessageReader<Interrupt>,
        mut ended: MessageReader<QuietEnded>,
        mut net: MessageReader<SendNet>,
        mut told: ResMut<Told>,
    ) {
        told.rings += rings.read().count();
        told.interrupts.extend(interrupts.read().map(|i| i.0.title.clone()));
        told.ended.extend(ended.read().map(|e| e.0.iter().map(|n| n.title.clone()).collect()));
        for SendNet(command) in net.read() {
            if let NetCommand::Fetch(fetch) = command {
                told.fetches.push(fetch.clone());
            }
        }
    }

    /// The time the test sets, in a zone of its own, so the minute of the day doesn't follow the machine's.
    #[derive(Clone)]
    struct SetTime(Arc<Mutex<Zoned>>);

    impl SetTime {
        fn set(&self, at: &str) {
            *self.0.lock().unwrap() = at.parse().unwrap();
        }
    }

    struct Shell {
        app: App,
        time: SetTime,
        played: Counted,
    }

    impl Shell {
        fn new(focused: bool) -> Shell {
            let time = SetTime(Arc::new(Mutex::new("2026-11-21T12:00:00-08:00[America/Los_Angeles]".parse().unwrap())));
            let played = Counted::default();
            let mut app = headless(ShellPlugin::new().with(Other));
            let clock = time.clone();
            app.insert_resource(Clock::new(move || clock.0.lock().unwrap().clone()))
                .insert_resource(Sound(Box::new(played.clone())))
                .init_resource::<Told>()
                .add_systems(PostUpdate, gather);
            app.world_mut().spawn(Window { focused, ..default() });
            fill(&mut app, Snapshot { at: 1, ..default() });
            Shell { app, time, played }
        }

        fn settings(&mut self, settings: NotificationSettings) {
            let mut prefs = self.app.world_mut().resource_mut::<NotificationPrefs>();
            prefs.settings = settings;
            prefs.answered = true;
            self.app.update();
        }

        fn arrive(&mut self, event: i64, n: &str, source: &str) {
            let after = notification_json(json!({"id": n, "source": source, "title": n}));
            let item = json!({"id": event, "entity": "notification", "entity_id": sid(n),
                              "kind": "notification.raised", "after": after});
            feed(&mut self.app, sync_event(item));
        }

        fn read(&mut self, event: i64, n: &str, source: &str) {
            let after = notification_json(json!({"id": n, "source": source, "title": n,
                                                 "read_at": crate::fixture::at(9)}));
            let item = json!({"id": event, "entity": "notification", "entity_id": sid(n),
                              "kind": "notification.read", "after": after});
            feed(&mut self.app, sync_event(item));
        }

        fn told(&mut self) -> Told {
            std::mem::take(&mut *self.app.world_mut().resource_mut::<Told>())
        }

        fn played(&self) -> usize {
            std::mem::take(&mut *self.played.0.lock().unwrap())
        }
    }

    fn levels(pairs: &[(&str, NotificationLevel)]) -> NotificationSettings {
        let levels = pairs.iter().map(|(s, l)| (s.to_string(), *l)).collect();
        NotificationSettings { levels, ..Default::default() }
    }

    #[test]
    fn a_silent_source_only_lists_a_bell_one_rings_and_an_interrupting_one_rings_with_focus() {
        let mut shell = Shell::new(true);
        shell.settings(levels(&[("quiet", NotificationLevel::Silent), ("ticklist", NotificationLevel::Bell)]));
        shell.told();
        shell.arrive(2, "n1", "quiet");
        let told = shell.told();
        assert_eq!((told.rings, shell.played()), (0, 0), "a silent source is listed only");
        shell.arrive(3, "n2", "ticklist");
        assert_eq!((shell.told().rings, shell.played()), (1, 1), "a bell source rings, with the sound");
        shell.arrive(4, "n3", "server");
        let told = shell.told();
        assert_eq!((told.rings, told.interrupts.len(), shell.played()), (1, 0, 1), "with focus the bell is the notice");
    }

    #[test]
    fn without_focus_a_bell_source_rings_and_an_interrupting_one_goes_to_the_desktop() {
        let mut shell = Shell::new(false);
        shell.settings(levels(&[("ticklist", NotificationLevel::Bell)]));
        shell.told();
        shell.arrive(2, "n1", "ticklist");
        let told = shell.told();
        assert_eq!((told.rings, told.interrupts.len()), (1, 0), "a bell source never reaches the desktop");
        shell.arrive(3, "n2", "server");
        let told = shell.told();
        assert_eq!((told.rings, told.interrupts.as_slice(), shell.played()), (0, ["n2".to_string()].as_slice(), 1));
    }

    #[test]
    fn the_sound_setting_silences_the_ring() {
        let mut shell = Shell::new(true);
        shell.settings(NotificationSettings { sound: false, ..Default::default() });
        shell.arrive(2, "n1", "server");
        assert_eq!((shell.told().rings, shell.played()), (1, 0));
    }

    #[test]
    fn quiet_hours_count_without_a_ring_and_their_end_rings_once_for_what_is_still_unread() {
        let mut shell = Shell::new(true);
        let mut night = levels(&[("quiet", NotificationLevel::Silent)]);
        night.quiet_hours = Some(DEFAULT_QUIET);
        shell.settings(night);
        shell.time.set("2026-11-21T23:00:00-08:00[America/Los_Angeles]");
        shell.app.update();
        shell.told();
        shell.arrive(2, "n1", "server");
        shell.arrive(3, "n2", "server");
        shell.arrive(4, "n3", "quiet");
        let told = shell.told();
        assert_eq!((told.rings, told.interrupts.len(), shell.played()), (0, 0, 0), "counted only");
        shell.read(5, "n2", "server");
        shell.time.set("2026-11-22T07:00:00-08:00[America/Los_Angeles]");
        shell.app.update();
        assert_eq!((shell.told().rings, shell.played()), (1, 1), "one ring for n1");
        shell.app.update();
        assert_eq!(shell.told().rings, 0, "once");
    }

    #[test]
    fn focus_ending_without_window_focus_is_one_desktop_notice_of_the_interrupting_ones() {
        let mut shell = Shell::new(false);
        let mut focused = levels(&[("ticklist", NotificationLevel::Bell)]);
        focused.focus = Some(Focus { until: Some("2026-11-21T21:00:00Z".parse().unwrap()) });
        shell.settings(focused);
        shell.told();
        shell.arrive(2, "n1", "server");
        shell.arrive(3, "n2", "ticklist");
        shell.arrive(4, "n3", "factory");
        let told = shell.told();
        assert_eq!((told.rings, told.interrupts.len(), told.ended.len()), (0, 0, 0));
        shell.time.set("2026-11-21T13:30:00-08:00[America/Los_Angeles]");
        shell.app.update();
        let told = shell.told();
        assert_eq!(told.ended.len(), 1, "one notice");
        let mut said = told.ended[0].clone();
        said.sort();
        assert_eq!(said, ["n1", "n3"], "the bell source's waits on the count");
        assert_eq!((told.rings, shell.played()), (0, 0), "the desktop plays its own sound");

        // Nothing arrived meanwhile: nothing is said.
        let mut again = levels(&[]);
        again.focus = Some(Focus { until: None });
        shell.settings(again);
        shell.settings(levels(&[]));
        let told = shell.told();
        assert!(told.ended.is_empty() && told.rings == 0);
    }

    #[test]
    fn the_counts_follow_the_setting() {
        let mut shell = Shell::new(true);
        assert!(shell.app.world().resource::<NotificationCounts>().shown);
        shell.settings(NotificationSettings { counts: false, ..Default::default() });
        assert!(!shell.app.world().resource::<NotificationCounts>().shown);
    }

    /// Runs a read against a server answering `body`, and delivers its answer.
    fn run(app: &mut App, fetch: &Fetch, status: &str, body: String) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let status = status.to_string();
        let served = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let n = stream.read(&mut request).unwrap();
            let head =
                format!("HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n", body.len());
            write!(stream, "{head}connection: close\r\n\r\n{body}").unwrap();
            String::from_utf8_lossy(&request[..n]).lines().next().unwrap_or_default().to_string()
        });
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let fetched = runtime.block_on(fetch.run(Some(trailhead_client::http::Client::new(base, "token"))));
        fetched.deliver(app.world_mut());
        app.update();
        served.join().unwrap()
    }

    #[test]
    fn the_settings_are_read_when_live_and_every_poll_and_the_defaults_hold_until_then() {
        let mut shell = Shell::new(true);
        assert_eq!(*shell.app.world().resource::<NotificationPrefs>(), NotificationPrefs::default());
        shell.told();
        set_connection(&mut shell.app, Connection::Live);
        assert!(!shell.told().fetches.is_empty(), "a read on going live");
        shell.app.world_mut().trigger(ReadNotificationSettings);
        shell.app.update();
        let fetch = shell.told().fetches.remove(0);
        let body = json!({"levels": {"factory": "bell"}, "sound": false}).to_string();
        let line = run(&mut shell.app, &fetch, "200 OK", body);
        assert_eq!(line, "GET /v1/notifications/settings HTTP/1.1");
        let prefs = shell.app.world().resource::<NotificationPrefs>().clone();
        assert!(prefs.answered && !prefs.settings.sound);
        assert_eq!(prefs.settings.level_of("factory"), NotificationLevel::Bell);

        shell.app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(Duration::from_secs(1)));
        shell.told();
        for _ in 0..POLL.as_secs() + 1 {
            shell.app.update();
        }
        let reads = shell.told().fetches.len();
        assert!(reads >= 1, "read again after a poll");
    }

    #[test]
    fn focus_saved_from_elsewhere_keeps_the_rest_and_a_save_reads_the_settings_again() {
        let mut shell = Shell::new(true);
        set_connection(&mut shell.app, Connection::Live);
        shell.settings(levels(&[("factory", NotificationLevel::Silent)]));
        let button = shell.app.world_mut().spawn_empty().id();
        let kept = shell.app.world().resource::<NotificationPrefs>().settings.clone();
        let settings = with_focus(&kept, Some(Focus { until: None }));
        shell.app.world_mut().trigger(SaveNotificationSettings { source: button, settings });
        shell.app.update();
        let out = sent(&mut shell.app);
        assert_eq!(out.len(), 1);
        let Call::SetNotificationSettings(put) = &out[0].request.call else { panic!("{:?}", out[0].request.call) };
        assert_eq!(put.level_of("factory"), NotificationLevel::Silent, "the rest as read");
        assert_eq!(put.focus, Some(Focus { until: None }));
        shell.told();
        answer(&mut shell.app, button, Outcome::Ok { version: None });
        assert_eq!(shell.told().fetches.len(), 1, "read again to show it");
    }

    #[test]
    fn the_page_saves_its_draft_over_the_settings_read_and_shows_the_read_back_not_the_edit() {
        let mut shell = Shell::new(true);
        set_connection(&mut shell.app, Connection::Live);
        let mut kept = levels(&[("factory", NotificationLevel::Bell)]);
        kept.focus = Some(Focus { until: None });
        shell.settings(kept.clone());
        let button = shell.app.world_mut().spawn_empty().id();
        let gate = |shell: &Shell| {
            let world = shell.app.world();
            world.resource::<NotificationsPage>().gate(world.resource::<NotificationPrefs>(), true, None)
        };
        assert_eq!(gate(&shell), Gate::Unchanged);
        {
            let mut page = shell.app.world_mut().resource_mut::<NotificationsPage>();
            page.draft.set_level("factory", NotificationLevel::Bell, &kept);
            assert!(page.draft.levels.is_empty(), "the kept level is no edit");
            page.draft.set_sound(false, &kept);
            page.draft.set_quiet_hours(Some(DEFAULT_QUIET), &kept);
        }
        assert_eq!(gate(&shell), Gate::Ready);
        shell.app.world_mut().trigger(SaveNotificationsPage { source: button });
        shell.app.update();
        let out = sent(&mut shell.app);
        let Call::SetNotificationSettings(put) = &out[0].request.call else { panic!() };
        assert_eq!(out[0].request.method(), crate::commands::Method::Put);
        assert_eq!((put.sound, put.quiet_hours, put.focus), (false, Some(DEFAULT_QUIET), kept.focus));
        assert_eq!(gate(&shell), Gate::Saving);

        shell.told();
        answer(&mut shell.app, button, Outcome::Ok { version: None });
        let page = shell.app.world().resource::<NotificationsPage>().clone();
        assert!(page.saving && !page.saved, "until it is read back");
        let fetch = shell.told().fetches.remove(0);
        run(&mut shell.app, &fetch, "200 OK", serde_json::to_string(put).unwrap());
        let page = shell.app.world().resource::<NotificationsPage>().clone();
        assert!(page.saved && !page.saving && page.draft == NotificationDraft::default());
        assert_eq!(shell.app.world().resource::<NotificationPrefs>().settings, *put);
    }

    #[test]
    fn a_refused_save_keeps_the_draft_and_says_why() {
        let mut shell = Shell::new(true);
        set_connection(&mut shell.app, Connection::Live);
        shell.settings(NotificationSettings::default());
        shell.app.world_mut().resource_mut::<NotificationsPage>().draft.counts = Some(false);
        let button = shell.app.world_mut().spawn_empty().id();
        shell.app.world_mut().trigger(SaveNotificationsPage { source: button });
        shell.app.update();
        sent(&mut shell.app);
        answer(&mut shell.app, button, Outcome::Forbidden("only a person may".into()));
        let page = shell.app.world().resource::<NotificationsPage>().clone();
        assert_eq!(page.refusal.as_deref(), Some("Not allowed: only a person may"));
        assert_eq!((page.saving, page.draft.counts), (false, Some(false)));
    }

    #[test]
    fn times_and_focus_are_said_in_the_windows_time() {
        assert_eq!((time_words(0), time_words(22 * 60 + 5)), ("00:00".into(), "22:05".into()));
        let times = time_options(&[7 * 60 + 15]);
        assert_eq!((times.len(), times[15]), (49, 7 * 60 + 15));
        let now: Zoned = "2026-11-21T12:00:00-08:00[America/Los_Angeles]".parse().unwrap();
        let mut s = NotificationSettings::default();
        assert!(focus_text(&s, &now).starts_with("Focus is off"));
        s.focus = Some(Focus { until: None });
        assert_eq!(focus_text(&s, &now), "Focus is on until you end it.");
        s.focus = Some(Focus { until: Some("2026-11-21T21:30:00Z".parse().unwrap()) });
        assert_eq!(focus_text(&s, &now), "Focus is on until 13:30.");
        s.focus = Some(Focus { until: Some("2026-11-22T17:00:00Z".parse().unwrap()) });
        assert_eq!(focus_text(&s, &now), "Focus is on until Sun 09:00.");
        s.focus = Some(Focus { until: Some("2026-11-21T19:00:00Z".parse().unwrap()) });
        assert!(focus_text(&s, &now).starts_with("Focus is off"), "it has ended");
        assert_eq!(minute_of_day(&now), 12 * 60);
    }
}
