//! The Notifications settings page ([`crate::settings::NOTIFICATIONS`]): how the person wants to be told, kept on the
//! server so every machine agrees. Listed in a shell with a connection.
//!
//! - A notice when the settings can't be shown as kept (not connected, not read yet, a read that failed).
//! - **Levels**: one setting per source, each app on the rail by name, then Factory, then Trailhead, each a choice of
//!   Silent, Bell and Interrupt, with a note saying what the one chosen does.
//! - **Quiet hours**: on or off, and when they start and end, as times of the day on this machine.
//! - **Sound and counts**: the sound on or off, the counts shown or hidden.
//! - The save's notice and Save.
//!
//! What the controls show is the settings last read with the page's edits made; the notice says when those edits are
//! not saved, and a save shows what the server says once it has taken them. Everything is spawned once when the page
//! opens and kept in step in place; the logic is [`crate::notification_settings`]'s.

use bevy::prelude::*;
use bevy::ui::{Checked, InteractionDisabled};
use bevy::ui_widgets::{Activate, ValueChange};
use trailhead_model::{NotificationLevel, NotificationSettings, QuietHours, SOURCE_FACTORY, SOURCE_SERVER};
use trailhead_ui::{
    ButtonGroup, ButtonKind, Notice, Picked, SelectEdit, SettingNote, Tone, UiPlace, button, button_group, notice, row,
    select, set_display, set_text, setting, setting_toggle, spawn_settings_section,
};

use crate::Apps;
use crate::ask::offline_reason;
use crate::connection::{Connection, ConnectionStatus};
use crate::notification_settings::{
    DEFAULT_QUIET, Gate, NotificationPrefs, NotificationsPage, ReadNotificationSettings, SaveNotificationsPage,
    time_options, time_words,
};
use crate::notifications::source_label;

/// The levels, in the order their buttons show them.
pub const LEVELS: [NotificationLevel; 3] =
    [NotificationLevel::Silent, NotificationLevel::Bell, NotificationLevel::Interrupt];

/// How many times a quiet-hours select shows before it scrolls.
const TIMES_SHOWN: usize = 8;

/// The sources the page sets a level for, as `(source, name)`: each app on the rail, then the factory, then the server.
pub fn sources(apps: &Apps) -> Vec<(String, String)> {
    let ids = apps.iter().map(|m| m.id.0).chain([SOURCE_FACTORY, SOURCE_SERVER]);
    ids.map(|id| (id.to_string(), source_label(id, apps).1)).collect()
}

/// What a level does, said under the source's choice.
pub fn level_note(level: NotificationLevel) -> &'static str {
    match level {
        NotificationLevel::Silent => "Listed in the bell's panel only: not counted, and it never rings.",
        NotificationLevel::Bell => "Counted, and the bell rings, with the sound if it is on.",
        NotificationLevel::Interrupt => {
            "Counted and the bell rings; while the window doesn't have focus, the desktop shows it too."
        }
    }
}

const QUIET_NOTE: &str = "A span of each day in which nothing rings or interrupts: notifications are still listed and \
                          counted, and when it ends one notice says what came. Focus, from the bell's panel, does the \
                          same until you end it.";
const SOUND_NOTE: &str = "The desktop's own message sound, with the bell's ring and with a desktop notification.";
const COUNTS_NOTE: &str = "How many are unread, on the bell and on each app on the rail.";

/// A source's choice of level.
#[derive(Component, Clone, Debug, Default, PartialEq, Eq)]
pub struct LevelPick(pub String);

/// A switch on the page.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NotifySwitch {
    #[default]
    Quiet,
    Sound,
    Counts,
}

/// The select of when quiet hours start or end.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum QuietTime {
    #[default]
    Start,
    End,
}

/// The minutes of the day a quiet-hours select offers, in its order.
#[derive(Component, Clone, Debug, Default, PartialEq, Eq)]
pub struct TimeChoices(pub Vec<u16>);

/// A part of the page that shows something.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NotifyPart {
    /// Why the settings can't be shown as kept.
    #[default]
    Status,
    /// What the save says.
    SaveNotice,
}

/// The page's Save.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct SaveNotifications;

/// On a source's setting: its note says what its level does.
#[derive(Component, Clone, Debug)]
struct LevelNote(String);

/// On the settings of when quiet hours start and end, hidden while they are off.
#[derive(Component, Clone, Copy, Debug)]
struct QuietRow;

fn level_index(level: NotificationLevel) -> usize {
    LEVELS.iter().position(|l| *l == level).unwrap_or(LEVELS.len() - 1)
}

/// The times a select offers and the one chosen, for quiet hours `quiet` (or the default ones while off).
fn times(quiet: Option<QuietHours>, which: QuietTime) -> (Vec<u16>, usize) {
    let quiet = quiet.unwrap_or(DEFAULT_QUIET);
    let at = match which {
        QuietTime::Start => quiet.start,
        QuietTime::End => quiet.end,
    };
    let options = time_options(&[at]);
    let chosen = options.iter().position(|m| *m == at).unwrap_or_default();
    (options, chosen)
}

/// Fills the page: run by the settings modal each time it is shown.
pub(crate) fn notifications_page(
    In(page): In<Entity>,
    apps: Res<Apps>,
    prefs: Res<NotificationPrefs>,
    state: Res<NotificationsPage>,
    mut commands: Commands,
) {
    commands.trigger(ReadNotificationSettings);
    let shown = state.shown(&prefs);
    commands.spawn_scene(notice()).insert((NotifyPart::Status, ChildOf(page)));

    let levels = spawn_settings_section(&mut commands, "Levels");
    commands.entity(levels.section).insert(ChildOf(page));
    for (source, name) in sources(&apps) {
        let level = shown.level_of(&source);
        let options = LEVELS.iter().map(|l| (level_word(*l).to_string(), None)).collect();
        let control = bsn_list! { @button_group(options, Some(level_index(level))) LevelPick({ source.clone() }) };
        let note = level_note(level);
        commands.spawn_scene(setting(name, note, control)).insert((LevelNote(source), ChildOf(levels.rows)));
    }

    let quiet = spawn_settings_section(&mut commands, "Quiet hours");
    commands.entity(quiet.section).insert(ChildOf(page));
    let on = shown.quiet_hours.is_some();
    let switch = setting_toggle("Quiet hours", QUIET_NOTE, on, bsn! { NotifySwitch::Quiet });
    commands.spawn_scene(switch).insert(ChildOf(quiet.rows));
    for (which, label, note) in [
        (QuietTime::Start, "From", "When they start, in this machine's time."),
        (QuietTime::End, "Until", "When they end. Earlier than the start runs through midnight."),
    ] {
        let (minutes, chosen) = times(shown.quiet_hours, which);
        let words = minutes.iter().map(|m| time_words(*m)).collect();
        let control = bsn_list! {
            @select(words, Some(chosen), TIMES_SHOWN) which TimeChoices({ minutes.clone() })
            Node { align_self: AlignSelf::FlexStart }
        };
        commands.spawn_scene(setting(label, note, control)).insert((QuietRow, ChildOf(quiet.rows)));
    }

    let other = spawn_settings_section(&mut commands, "Sound and counts");
    commands.entity(other.section).insert(ChildOf(page));
    let sound = setting_toggle("Sound", SOUND_NOTE, shown.sound, bsn! { NotifySwitch::Sound });
    commands.spawn_scene(sound).insert(ChildOf(other.rows));
    let counts = setting_toggle("Counts", COUNTS_NOTE, shown.counts, bsn! { NotifySwitch::Counts });
    commands.spawn_scene(counts).insert(ChildOf(other.rows));

    commands.spawn_scene(notice()).insert((NotifyPart::SaveNotice, ChildOf(page)));
    let bar = commands.spawn_scene(row()).insert(ChildOf(page)).id();
    commands.spawn_scene(button("Save", None, ButtonKind::Primary)).insert((SaveNotifications, ChildOf(bar)));
}

/// A level's name on its button.
pub fn level_word(level: NotificationLevel) -> &'static str {
    match level {
        NotificationLevel::Silent => "Silent",
        NotificationLevel::Bell => "Bell",
        NotificationLevel::Interrupt => "Interrupt",
    }
}

/// The page, for a windowed shell with a connection.
pub(crate) struct NotificationsPageUiPlugin;

impl Plugin for NotificationsPageUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(on_level)
            .add_observer(on_switch)
            .add_observer(on_time)
            .add_observer(on_save)
            .add_systems(Update, (show_choices, show_times, show_notices).chain().before(UiPlace));
    }
}

fn on_level(
    picked: On<Picked>,
    picks: Query<&LevelPick>,
    prefs: Res<NotificationPrefs>,
    mut page: ResMut<NotificationsPage>,
) {
    let (Ok(LevelPick(source)), Some(level)) = (picks.get(picked.entity), LEVELS.get(picked.index)) else { return };
    page.draft.set_level(source, *level, &prefs.settings);
    page.edited();
}

fn on_switch(
    change: On<ValueChange<bool>>,
    switches: Query<&NotifySwitch>,
    prefs: Res<NotificationPrefs>,
    mut page: ResMut<NotificationsPage>,
) {
    let Ok(switch) = switches.get(change.source) else { return };
    let kept = &prefs.settings;
    let shown = page.shown(&prefs);
    match switch {
        NotifySwitch::Quiet => {
            let quiet = change.value.then(|| shown.quiet_hours.unwrap_or(DEFAULT_QUIET));
            page.draft.set_quiet_hours(quiet, kept);
        }
        NotifySwitch::Sound => page.draft.set_sound(change.value, kept),
        NotifySwitch::Counts => page.draft.set_counts(change.value, kept),
    }
    page.edited();
}

fn on_time(
    picked: On<Picked>,
    selects: Query<(&QuietTime, &TimeChoices)>,
    prefs: Res<NotificationPrefs>,
    mut page: ResMut<NotificationsPage>,
) {
    let Ok((which, choices)) = selects.get(picked.entity) else { return };
    let Some(minute) = choices.0.get(picked.index).copied() else { return };
    let mut quiet = page.shown(&prefs).quiet_hours.unwrap_or(DEFAULT_QUIET);
    match which {
        QuietTime::Start => quiet.start = minute,
        QuietTime::End => quiet.end = minute,
    }
    page.draft.set_quiet_hours(Some(quiet), &prefs.settings);
    page.edited();
}

fn on_save(activate: On<Activate>, saves: Query<(), With<SaveNotifications>>, mut commands: Commands) {
    if saves.contains(activate.entity) {
        commands.trigger(SaveNotificationsPage { source: activate.entity });
    }
}

/// Shows the settings as read with the edits made: each source's level and its note, the switches, and the quiet
/// hours' times only while they are on.
#[allow(clippy::too_many_arguments)]
fn show_choices(
    prefs: Res<NotificationPrefs>,
    state: Res<NotificationsPage>,
    mut groups: Query<(&LevelPick, &mut ButtonGroup)>,
    roots: Query<(&LevelNote, &Children)>,
    mut notes: Query<&mut Text, With<SettingNote>>,
    switches: Query<(Entity, &NotifySwitch, Has<Checked>)>,
    quiet_rows: Query<Entity, With<QuietRow>>,
    mut nodes: Query<&mut Node>,
    mut commands: Commands,
) {
    let shown = state.shown(&prefs);
    for (LevelPick(source), mut group) in &mut groups {
        group.set_if_neq(ButtonGroup { chosen: Some(level_index(shown.level_of(source))) });
    }
    for (LevelNote(source), children) in &roots {
        for child in children {
            if let Ok(text) = notes.get_mut(*child) {
                set_text(text, level_note(shown.level_of(source)));
            }
        }
    }
    for (switch, which, checked) in &switches {
        let on = switched(&shown, *which);
        if on && !checked {
            commands.entity(switch).insert(Checked);
        } else if !on && checked {
            commands.entity(switch).remove::<Checked>();
        }
    }
    for row in &quiet_rows {
        set_display(&mut nodes, row, shown.quiet_hours.is_some());
    }
}

fn switched(shown: &NotificationSettings, which: NotifySwitch) -> bool {
    match which {
        NotifySwitch::Quiet => shown.quiet_hours.is_some(),
        NotifySwitch::Sound => shown.sound,
        NotifySwitch::Counts => shown.counts,
    }
}

/// Shows the quiet hours' times as read with the edits made, changing a select only when it shows something else.
fn show_times(
    prefs: Res<NotificationPrefs>,
    state: Res<NotificationsPage>,
    mut selects: Query<(Entity, &QuietTime, &mut TimeChoices)>,
    mut edit: SelectEdit,
) {
    let quiet = state.shown(&prefs).quiet_hours;
    for (select, which, mut choices) in &mut selects {
        let (minutes, chosen) = times(quiet, *which);
        if choices.0 == minutes && edit.selected_rows(select) == [chosen] {
            continue;
        }
        let words: Vec<String> = minutes.iter().map(|m| time_words(*m)).collect();
        if edit.set(select, &words, Some(chosen)) {
            choices.0 = minutes;
        }
    }
}

/// The page's notices, and its Save: enabled only with an edit to save over a live connection, once the settings are
/// read.
#[allow(clippy::too_many_arguments)]
fn show_notices(
    prefs: Res<NotificationPrefs>,
    state: Res<NotificationsPage>,
    connection: Res<State<Connection>>,
    status: Res<ConnectionStatus>,
    mut parts: Query<(&NotifyPart, &mut Notice)>,
    saves: Query<(Entity, Has<InteractionDisabled>), With<SaveNotifications>>,
    children: Query<&Children>,
    mut texts: Query<&mut Text>,
    mut commands: Commands,
) {
    let live = *connection.get() == Connection::Live;
    let offline = offline_reason(Some(&status));
    let gate = state.gate(&prefs, live, offline.clone());
    for (part, mut notice) in &mut parts {
        let says = match part {
            NotifyPart::Status => status_notice(&prefs, live, offline.clone()),
            NotifyPart::SaveNotice => save_notice(&state, &gate, live),
        };
        if *notice != says {
            *notice = says;
        }
    }
    for (save, disabled) in &saves {
        let ready = gate == Gate::Ready;
        if ready && disabled {
            commands.entity(save).remove::<InteractionDisabled>();
        } else if !ready && !disabled {
            commands.entity(save).insert(InteractionDisabled);
        }
        let caption = if gate == Gate::Saving { "Saving…" } else { "Save" };
        for inside in children.iter_descendants(save) {
            if let Ok(text) = texts.get_mut(inside) {
                set_text(text, caption);
            }
        }
    }
}

/// What the notice at the page's top says.
pub fn status_notice(prefs: &NotificationPrefs, live: bool, offline: Option<String>) -> Notice {
    if !live {
        let why = offline.map_or("Not connected to the server.".into(), |r| format!("Not connected: {r}."));
        return Notice::new(Tone::Info, format!("{why} Your settings are kept there, and show once it is."));
    }
    match (&prefs.error, prefs.answered) {
        (Some(why), _) => Notice::new(Tone::Error, format!("Couldn't read your settings: {why}")),
        (None, false) => Notice::new(Tone::Info, "Reading your settings…"),
        (None, true) => Notice::default(),
    }
}

/// What the notice over Save says: the server's refusal, that the edits aren't saved, or that they were.
pub fn save_notice(page: &NotificationsPage, gate: &Gate, live: bool) -> Notice {
    if let Some(refusal) = &page.refusal {
        return Notice::new(Tone::Error, format!("The server refused this: {refusal}"));
    }
    match gate {
        Gate::Ready => Notice::new(Tone::Info, "Not saved yet: the window goes on as before until you save."),
        Gate::Blocked(why) if live => Notice::new(Tone::Info, why.clone()),
        Gate::Unchanged if page.saved => Notice::new(Tone::Success, "Saved. Every window follows it within a minute."),
        _ => Notice::default(),
    }
}

#[cfg(test)]
mod tests {
    use trailhead_client::http::Call;
    use trailhead_ui::{Appearance, ModalPlugin, ThemeChoice};

    use super::*;
    use crate::commands::Outcome;
    use crate::settings::{NOTIFICATIONS, OpenSettings, SettingsPages};
    use crate::testing::{Other, answer, sent, set_connection, settle, windowed};
    use crate::{AppId, Manifest, ShellApp, ShellPlugin};

    struct Notes;

    impl ShellApp for Notes {
        fn manifest(&self) -> Manifest {
            Manifest { id: AppId("notes"), name: "Notes", icon: trailhead_ui::Icon::Flag }
        }

        fn build(&self, _: &mut App) {}
    }

    fn kept() -> NotificationSettings {
        let mut kept = NotificationSettings::default();
        kept.levels.insert("factory".into(), NotificationLevel::Bell);
        kept.focus = Some(trailhead_model::Focus { until: None });
        kept
    }

    fn app() -> App {
        let mut app = windowed(ShellPlugin::new().with(Notes).with(Other));
        app.insert_resource(Appearance::new(ThemeChoice::Desktop)).add_plugins(ModalPlugin);
        set_connection(&mut app, Connection::Live);
        app.world_mut().trigger(OpenSettings { page: Some(NOTIFICATIONS) });
        settle(&mut app);
        read(&mut app, kept());
        app
    }

    /// The settings as a read from the server leaves them.
    fn read(app: &mut App, settings: NotificationSettings) {
        *app.world_mut().resource_mut::<NotificationPrefs>() =
            NotificationPrefs { settings, answered: true, error: None };
        settle(app);
    }

    fn pick(app: &mut App, source: &str) -> (Entity, Option<usize>) {
        let world = app.world_mut();
        let found = world.query::<(Entity, &LevelPick, &ButtonGroup)>().iter(world).find(|(_, p, _)| p.0 == source);
        let (entity, _, group) = found.unwrap();
        (entity, group.chosen)
    }

    fn notice(app: &mut App, which: NotifyPart) -> Notice {
        let world = app.world_mut();
        world.query::<(&NotifyPart, &Notice)>().iter(world).find(|(p, _)| **p == which).unwrap().1.clone()
    }

    fn save_button(app: &mut App) -> (Entity, bool) {
        let world = app.world_mut();
        let (entity, disabled) = world
            .query_filtered::<(Entity, Has<InteractionDisabled>), With<SaveNotifications>>()
            .single(world)
            .unwrap();
        (entity, !disabled)
    }

    #[test]
    fn the_page_is_listed_with_a_level_for_each_app_then_factory_and_trailhead() {
        let mut app = app();
        let pages = app.world().resource::<SettingsPages>();
        assert!(pages.get(NOTIFICATIONS).is_some_and(|p| p.title == "Notifications"));
        let apps = app.world().resource::<Apps>();
        let names: Vec<String> = sources(apps).into_iter().map(|(_, name)| name).collect();
        assert_eq!(names, ["Notes", "Other", "Factory", "Trailhead"]);
        assert_eq!(pick(&mut app, "notes").1, Some(2), "a source with no level of its own interrupts");
        assert_eq!(pick(&mut app, "factory").1, Some(1), "as read");
        assert!(!save_button(&mut app).1, "nothing to save");
        let world = app.world_mut();
        let rows: Vec<Display> =
            world.query_filtered::<&Node, With<QuietRow>>().iter(world).map(|n| n.display).collect();
        assert_eq!(rows, [Display::None, Display::None], "the times hide while quiet hours are off");
    }

    #[test]
    fn an_edit_is_said_unsaved_and_a_save_sends_it_over_the_settings_read_then_shows_what_was_read_back() {
        let mut app = app();
        let (factory, _) = pick(&mut app, "factory");
        app.world_mut().trigger(Picked { entity: factory, index: 0 });
        let quiet = {
            let world = app.world_mut();
            world.query::<(Entity, &NotifySwitch)>().iter(world).find(|(_, s)| **s == NotifySwitch::Quiet).unwrap().0
        };
        app.world_mut().trigger(ValueChange { source: quiet, value: true, is_final: true });
        settle(&mut app);
        assert_eq!(pick(&mut app, "factory").1, Some(0));
        assert_eq!(notice(&mut app, NotifyPart::SaveNotice).tone, Tone::Info);
        let world = app.world_mut();
        let shown = world.query_filtered::<&Node, With<QuietRow>>().iter(world).all(|n| n.display != Display::None);
        assert!(shown, "the times show once quiet hours are on");

        let (save, enabled) = save_button(&mut app);
        assert!(enabled);
        app.world_mut().trigger(Activate { entity: save });
        app.update();
        let out = sent(&mut app);
        let Call::SetNotificationSettings(put) = &out[0].request.call else { panic!("{:?}", out[0].request.call) };
        assert_eq!(put.level_of("factory"), NotificationLevel::Silent);
        assert_eq!((put.quiet_hours, put.focus), (Some(DEFAULT_QUIET), kept().focus), "focus as read");
        settle(&mut app);
        assert!(!save_button(&mut app).1, "not while saving");

        answer(&mut app, out[0].source, Outcome::Failed("the server is busy".into()));
        assert_eq!(
            notice(&mut app, NotifyPart::SaveNotice),
            Notice::new(Tone::Error, "The server refused this: the server is busy")
        );
        assert_eq!(pick(&mut app, "factory").1, Some(0), "the edit stays");
    }

    #[test]
    fn a_read_that_changes_the_settings_is_shown_in_place() {
        let mut app = app();
        let (factory, _) = pick(&mut app, "factory");
        let mut changed = kept();
        changed.levels.insert("notes".into(), NotificationLevel::Silent);
        changed.sound = false;
        read(&mut app, changed);
        assert_eq!(pick(&mut app, "notes").1, Some(0));
        assert_eq!(pick(&mut app, "factory").0, factory, "the same controls");
        let world = app.world_mut();
        let sound =
            world.query::<(&NotifySwitch, Has<Checked>)>().iter(world).find(|(s, _)| **s == NotifySwitch::Sound);
        assert!(!sound.unwrap().1);
    }
}
