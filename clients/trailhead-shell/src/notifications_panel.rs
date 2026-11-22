//! The bell and its panel, drawn ([`crate::notifications`] is the logic). Every [`NotificationBell`] (the rail's, or an
//! app's own in a shell with one app) gets a count of the unread notifications over its corner, rings when one arrives
//! while the window has focus ([`Ring`], not while the desktop asks for less motion), and opens the panel when pressed.
//! Each app's button on the rail gets the count of its own.
//!
//! At the panel's top, focus: whether it is on and until when, with buttons that start it for an hour or until it is
//! ended, and that end it, each a save of the person's settings with only focus changed
//! ([`crate::notification_settings`]).
//!
//! The panel lists them newest first, one card each: its source's icon and name, its age, its title and body, and its
//! buttons, Mark read (Mark unread on a read one) and one per action, which opens its link in the desktop's browser.
//! The unread ones carry a dot; the resolved ones say so, their title dim. A switch shows the unread ones or all of
//! them, and under all of them Show older reads further back. Mark all read is in its footer. Pressing a card does
//! nothing. The cards are kept in step with the mirror in place.

use std::time::Duration;

use bevy::prelude::*;
use bevy::ui::InteractionDisabled;
use bevy::ui_widgets::Activate;
use trailhead_client::http::Call;
use trailhead_model::{Notification, NotificationAction};
use trailhead_ui::{
    Appearance, ButtonGroup, ButtonKind, CountBadge, Hint, Icon, Keyed, Modal, ModalSlots, Notice, Picked, Ring, Tone,
    badge, button, button_group, count_badge, display, dot, icon_text, link_button, list_card, notice, open_modal,
    patch_node, row, set_display, set_text, size, space, text, text_dim, text_toned, token,
};
use uuid::Uuid;

use crate::Apps;
use crate::commands::{CommandDone, Outcome, Request, Submit};
use crate::connection::Connection;
use crate::mirror::Mirrored;
use crate::notification_settings::{
    Clock, NotificationPrefs, RingBells, SaveNotificationSettings, focus_text, with_focus,
};
use crate::notifications::{
    BELL_HINT, NotificationBell, NotificationCounts, NotificationHistory, ShowOlder, age, app_count, known,
    source_label, unread_counts,
};
use crate::rail::AppCount;

/// Opens the bell's panel, as pressing a bell does.
#[derive(Event, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OpenNotifications;

/// Which notifications the panel lists. It is kept between openings.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NotificationFilter {
    /// Those neither read nor resolved.
    #[default]
    Unread,
    /// Every one, newest first, with older ones read on asking.
    All,
}

/// The panel's modal.
#[derive(Component)]
pub struct NotificationsPanel;

impl Modal for NotificationsPanel {
    fn title(&self) -> String {
        "Notifications".into()
    }

    fn tall(&self) -> bool {
        true
    }
}

/// On a bell: its count.
#[derive(Component, Clone, Copy, Debug)]
pub struct BellCount(pub Entity);

/// The switch between the unread ones and all of them.
#[derive(Component, Clone, Copy, Default)]
pub struct FilterPick;

/// The column of cards, holding nothing else.
#[derive(Component)]
pub struct NotificationList;

/// The line said in place of the list when it is empty.
#[derive(Component)]
pub struct EmptyLine;

/// The panel's notice, saying why a mark or a read of older ones failed.
#[derive(Component)]
pub struct PanelNotice;

/// The line that says whether focus is on, and until when.
#[derive(Component)]
pub struct FocusLine;

/// A button that starts or ends focus.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FocusAct {
    /// Focus for an hour from now.
    #[default]
    Hour,
    /// Focus until it is ended.
    UntilEnded,
    End,
}

/// The button under the list that reads older ones.
#[derive(Component, Clone, Copy, Default)]
pub struct ShowOlderButton;

/// The footer's button that marks every one read.
#[derive(Component, Clone, Copy, Default)]
pub struct MarkAllRead;

/// A card's button that marks its notification read, or unread when `read` is false.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarkOne {
    pub id: Uuid,
    pub read: bool,
}

/// A card's notification.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotificationCard(pub Uuid);

/// Which part of a card this is, to update in place.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CardPart {
    /// The dot an unread one carries.
    #[default]
    Unread,
    Icon,
    Source,
    /// The badge a resolved one carries.
    Resolved,
    Age,
    Title,
    /// Hidden when it has none.
    Body,
}

/// What a card shows.
#[derive(Debug, Clone, PartialEq)]
struct Card {
    id: Uuid,
    icon: Icon,
    source: String,
    age: String,
    title: String,
    body: String,
    unread: bool,
    read: bool,
    resolved: bool,
    actions: Vec<NotificationAction>,
}

impl Card {
    fn of(n: &Notification, apps: &Apps, now: i64) -> Card {
        let (icon, source) = source_label(&n.source, apps);
        Card {
            id: n.id,
            icon,
            source,
            age: age(n, now),
            title: n.title.clone(),
            body: n.body.clone(),
            unread: n.is_unread(),
            read: n.read_at.is_some(),
            resolved: n.resolved_at.is_some(),
            actions: n.actions.clone(),
        }
    }

    fn mark_words(&self) -> &'static str {
        if self.read { "Mark unread" } else { "Mark read" }
    }

    fn title_tone(&self) -> trailhead_ui::Token {
        if self.resolved { token::TEXT_DIM } else { token::TEXT }
    }
}

/// The cards of the open panel, by id.
#[derive(Resource, Default)]
struct Cards(Keyed<Card>);

/// How often the cards' ages are worked out again while nothing else changes.
const AGES_EVERY: Duration = Duration::from_secs(30);

pub(crate) struct NotificationsUiPlugin;

impl Plugin for NotificationsUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Cards>()
            .init_resource::<NotificationFilter>()
            .add_observer(on_bell)
            .add_observer(on_open)
            .add_observer(on_filter)
            .add_observer(on_mark_one)
            .add_observer(on_mark_all)
            .add_observer(on_show_older)
            .add_observer(on_focus)
            .add_systems(
                Update,
                (
                    give_bells_counts,
                    show_counts,
                    ring_bells,
                    fill_panel,
                    (show_filter, show_focus, sync_cards, show_footer, show_answers).chain(),
                )
                    .chain(),
            );
    }
}

fn on_bell(activate: On<Activate>, bells: Query<(), With<NotificationBell>>, mut commands: Commands) {
    if bells.contains(activate.entity) {
        commands.trigger(OpenNotifications);
    }
}

fn on_open(
    _: On<OpenNotifications>,
    open: Query<(), With<NotificationsPanel>>,
    mut cards: ResMut<Cards>,
    mut commands: Commands,
) {
    if open.is_empty() {
        cards.0.forget();
        open_modal(&mut commands, NotificationsPanel);
    }
}

/// A bell whose own children are down and that has no count yet.
type BareBells = (With<NotificationBell>, With<Children>, Without<BellCount>);

/// Puts a count over each bell's corner, once the bell's own children are down (a bell spawned as a queued scene
/// gets them a frame later, and they would replace one put there first).
fn give_bells_counts(bells: Query<Entity, BareBells>, mut commands: Commands) {
    for bell in &bells {
        let count = commands.spawn_scene(count_badge(0)).insert(ChildOf(bell)).id();
        commands.entity(bell).insert(BellCount(count));
    }
}

/// A count put on a bell or an app's button since the last look.
type NewCounts = Or<(Added<BellCount>, Added<AppCount>)>;

/// Keeps the counts on the bells and on the apps' buttons, and the bells' hints, in step with what is unread, in place.
// A Bevy system takes what it reads and writes as parameters.
#[allow(clippy::too_many_arguments)]
fn show_counts(
    mirrored: Query<&Mirrored<Notification>>,
    changed: Query<(), Changed<Mirrored<Notification>>>,
    mut removed: RemovedComponents<Mirrored<Notification>>,
    (history, shown, prefs): (Res<NotificationHistory>, Res<NotificationCounts>, Res<NotificationPrefs>),
    new: Query<(), NewCounts>,
    bells: Query<(&BellCount, &mut Hint)>,
    mut badges: Query<(&mut CountBadge, Option<&AppCount>)>,
) {
    let gone = removed.read().count() > 0;
    let still = !history.is_changed() && !shown.is_changed() && !prefs.is_changed();
    if changed.is_empty() && !gone && still && new.is_empty() {
        return;
    }
    // A silent source is listed, never counted.
    let mut counted = known(mirrored.iter().map(|m| &m.0), &history);
    counted.retain(|n| prefs.settings.level_of(&n.source) != trailhead_model::NotificationLevel::Silent);
    let (total, by_source) = unread_counts(&counted);
    let hint = match total {
        0 => BELL_HINT.to_string(),
        n => format!("{BELL_HINT}: {n} unread"),
    };
    let shown_or_none = |n: usize| if shown.shown { n } else { 0 };
    for (bell, mut bell_hint) in bells {
        if bell_hint.0 != hint {
            bell_hint.0.clone_from(&hint);
        }
        if let Ok((mut badge, _)) = badges.get_mut(bell.0) {
            badge.set_if_neq(CountBadge { count: shown_or_none(total) });
        }
    }
    for (mut badge, app) in &mut badges {
        if let Some(app) = app {
            badge.set_if_neq(CountBadge { count: shown_or_none(app_count(&by_source, app.0)) });
        }
    }
}

/// Rings every bell when the settings say one is to be rung for ([`RingBells`]): an arrival while the window has
/// focus, from a source that rings, outside quiet hours and focus. A desktop that asks for less motion sees only the
/// count change.
fn ring_bells(
    mut rings: MessageReader<RingBells>,
    appearance: Option<Res<Appearance>>,
    bells: Query<Entity, With<NotificationBell>>,
    mut commands: Commands,
) {
    if rings.read().count() == 0 {
        return;
    }
    if appearance.is_some_and(|a| a.reduce_motion()) {
        return;
    }
    for bell in &bells {
        commands.entity(bell).insert(Ring::default());
    }
}

/// Fills a panel that has just spawned: the switch, the notice, the line for an empty list, the column for the cards
/// and Show older in its body, Mark all read in its footer. The cards themselves are [`sync_cards`]'s.
fn fill_panel(
    new: Query<&ModalSlots, (With<NotificationsPanel>, Added<ModalSlots>)>,
    filter: Res<NotificationFilter>,
    mut commands: Commands,
) {
    for slots in &new {
        commands.spawn_scene(text("", size::BODY)).insert((FocusLine, ChildOf(slots.body)));
        let acts = commands.spawn_scene(row()).insert(ChildOf(slots.body)).id();
        for (act, words, icon) in [
            (FocusAct::Hour, "Focus for an hour", Some(Icon::Moon)),
            (FocusAct::UntilEnded, "Focus until I end it", None),
            (FocusAct::End, "End focus", None),
        ] {
            commands.spawn_scene(button(words, icon, ButtonKind::Normal)).insert((act, ChildOf(acts)));
        }
        let options = vec![("Unread".to_string(), None), ("All".to_string(), None)];
        let chosen = Some(usize::from(*filter == NotificationFilter::All));
        let switch = bsn! { @button_group(options, chosen) FilterPick Node { align_self: AlignSelf::FlexStart } };
        commands.spawn_scene(switch).insert(ChildOf(slots.body));
        commands.spawn_scene(notice()).insert((PanelNotice, ChildOf(slots.body)));
        commands.spawn_scene(text_dim("", size::BODY)).insert((EmptyLine, ChildOf(slots.body)));
        commands.spawn((
            Node { flex_direction: FlexDirection::Column, row_gap: px(space::CARDS), ..default() },
            NotificationList,
            ChildOf(slots.body),
        ));
        let older = bsn! {
            @button("Show older", Some(Icon::History), ButtonKind::Normal)
            ShowOlderButton
            Node { align_self: AlignSelf::FlexStart }
        };
        commands.spawn_scene(older).insert(ChildOf(slots.body));
        let all = bsn! { @button("Mark all read", Some(Icon::CheckCheck), ButtonKind::Normal) MarkAllRead };
        commands.spawn_scene(all).insert(ChildOf(slots.footer));
    }
}

/// Says whether focus is on and until when, as the clock moves; the buttons are pressable over a live connection
/// once the settings are read, End only while focus is on.
fn show_focus(
    lines: Query<&mut Text, With<FocusLine>>,
    acts: Query<(Entity, &FocusAct, Has<InteractionDisabled>)>,
    prefs: Res<NotificationPrefs>,
    clock: Res<Clock>,
    connection: Res<State<Connection>>,
    mut commands: Commands,
) {
    if lines.is_empty() && acts.is_empty() {
        return;
    }
    let now = clock.now();
    let words = focus_text(&prefs.settings, &now);
    for line in lines {
        set_text(line, &words);
    }
    let ready = *connection.get() == Connection::Live && prefs.answered;
    let on = prefs.settings.focused(now.timestamp());
    for (button, act, disabled) in &acts {
        enable(&mut commands, button, disabled, ready && (*act != FocusAct::End || on));
    }
}

/// Starts or ends focus: the settings last read, with only focus changed.
fn on_focus(
    activate: On<Activate>,
    acts: Query<&FocusAct>,
    prefs: Res<NotificationPrefs>,
    clock: Res<Clock>,
    mut commands: Commands,
) {
    let Ok(act) = acts.get(activate.entity) else { return };
    let focus = match act {
        FocusAct::Hour => {
            let until = clock.now().timestamp().checked_add(jiff::SignedDuration::from_hours(1)).ok();
            Some(trailhead_model::Focus { until })
        }
        FocusAct::UntilEnded => Some(trailhead_model::Focus { until: None }),
        FocusAct::End => None,
    };
    let settings = with_focus(&prefs.settings, focus);
    commands.trigger(SaveNotificationSettings { source: activate.entity, settings });
}

fn on_filter(picked: On<Picked>, picks: Query<(), With<FilterPick>>, mut filter: ResMut<NotificationFilter>) {
    if picks.contains(picked.entity) {
        filter.set_if_neq(if picked.index == 1 { NotificationFilter::All } else { NotificationFilter::Unread });
    }
}

fn show_filter(filter: Res<NotificationFilter>, mut groups: Query<&mut ButtonGroup, With<FilterPick>>) {
    let chosen = Some(usize::from(*filter == NotificationFilter::All));
    for mut group in &mut groups {
        group.set_if_neq(ButtonGroup { chosen });
    }
}

/// The current time, in seconds since the epoch.
fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// A card for each notification the filter lists, kept in step with the mirror and the older ones read: cards are
/// added, moved, changed and removed in place, never redrawn. Their ages are worked out again every [`AGES_EVERY`].
// A Bevy system takes what it reads and writes as parameters.
#[allow(clippy::too_many_arguments)]
fn sync_cards(
    lists: Query<Entity, With<NotificationList>>,
    added: Query<(), Added<NotificationList>>,
    mirrored: Query<&Mirrored<Notification>>,
    changed: Query<(), Changed<Mirrored<Notification>>>,
    mut removed: RemovedComponents<Mirrored<Notification>>,
    (history, filter, apps, real): (Res<NotificationHistory>, Res<NotificationFilter>, Res<Apps>, Res<Time<Real>>),
    mut aged: Local<Duration>,
    mut cards: ResMut<Cards>,
    mut edit: CardEdit,
    mut commands: Commands,
) {
    let gone = removed.read().count() > 0;
    let stale = real.elapsed().saturating_sub(*aged) >= AGES_EVERY;
    let Ok(list) = lists.single() else { return };
    if added.is_empty() && changed.is_empty() && !gone && !history.is_changed() && !filter.is_changed() && !stale {
        return;
    }
    *aged = real.elapsed();
    let now = now();
    let listed: Vec<(String, Card)> = known(mirrored.iter().map(|m| &m.0), &history)
        .iter()
        .filter(|n| *filter == NotificationFilter::All || n.is_unread())
        .map(|n| (n.id.to_string(), Card::of(n, &apps, now)))
        .collect();
    let empty = listed.is_empty();
    cards.0.sync(
        &mut commands,
        &[(list, listed)],
        |commands, _, card| spawn_card(commands, card),
        |commands, entity, card| edit.update(commands, entity, card),
    );
    edit.say_empty(empty, *filter);
}

/// A card: its heading (the unread dot, the source's icon and name, the resolved badge, the age), its title, its body,
/// and its buttons. Spawned whole, since its actions' buttons are as many as it has.
fn spawn_card(commands: &mut Commands, card: &Card) -> Entity {
    let root = commands.spawn_scene(list_card()).insert(NotificationCard(card.id)).id();
    let head = commands.spawn_scene(row()).insert(ChildOf(root)).id();
    let unread = bsn! { @dot(token::FILL_ACCENT) CardPart::Unread Node { display: { display(card.unread) } } };
    commands.spawn_scene(unread).insert(ChildOf(head));
    commands.spawn_scene(icon_text(card.icon)).insert((
        CardPart::Icon,
        trailhead_ui::TextTone(token::TEXT_DIM),
        ChildOf(head),
    ));
    let source = bsn! { @text_dim(card.source.clone(), size::SECONDARY) Node { flex_grow: 1.0 } };
    commands.spawn_scene(source).insert((CardPart::Source, ChildOf(head)));
    let resolved = bsn! {
        @badge("Resolved", Some(Icon::CircleCheck), Some(token::STATUS_OK))
        Node { display: { display(card.resolved) } }
    };
    commands.spawn_scene(resolved).insert((CardPart::Resolved, ChildOf(head)));
    commands.spawn_scene(text_dim(card.age.clone(), size::SECONDARY)).insert((CardPart::Age, ChildOf(head)));
    let title = text_toned(card.title.clone(), size::CARD_TITLE, card.title_tone());
    commands.spawn_scene(title).insert((CardPart::Title, ChildOf(root)));
    let body = bsn! { @text_dim(card.body.clone(), size::BODY) Node { display: { display(!card.body.is_empty()) } } };
    commands.spawn_scene(body).insert((CardPart::Body, ChildOf(root)));
    let buttons = commands.spawn_scene(row()).insert(ChildOf(root)).id();
    let mark = button(card.mark_words(), None, ButtonKind::Normal);
    commands.spawn_scene(mark).insert((MarkOne { id: card.id, read: !card.read }, ChildOf(buttons)));
    for action in &card.actions {
        let link = link_button(Icon::ExternalLink, action.label.clone(), action.url.clone(), ButtonKind::Normal);
        commands.spawn_scene(link).insert(ChildOf(buttons));
    }
    root
}

/// Changes a card in place, and says when the list is empty.
#[derive(bevy::ecs::system::SystemParam)]
struct CardEdit<'w, 's> {
    children: Query<'w, 's, &'static Children>,
    parts: Query<'w, 's, &'static CardPart>,
    marks: Query<'w, 's, &'static mut MarkOne>,
    texts: Query<'w, 's, &'static mut Text>,
    tones: Query<'w, 's, &'static trailhead_ui::TextTone>,
    nodes: Query<'w, 's, &'static mut Node>,
    empty: Query<'w, 's, Entity, With<EmptyLine>>,
}

impl CardEdit<'_, '_> {
    fn update(&mut self, commands: &mut Commands, card_entity: Entity, card: &Card) {
        let parts: Vec<Entity> = self.children.iter_descendants(card_entity).collect();
        for part in parts {
            if let Ok(mut mark) = self.marks.get_mut(part) {
                mark.set_if_neq(MarkOne { id: card.id, read: !card.read });
                let captions: Vec<Entity> = self.children.iter_descendants(part).collect();
                for caption in captions {
                    if let Ok(text) = self.texts.get_mut(caption) {
                        set_text(text, card.mark_words());
                    }
                }
                continue;
            }
            let Ok(which) = self.parts.get(part).copied() else { continue };
            match which {
                CardPart::Unread => set_display(&mut self.nodes, part, card.unread),
                CardPart::Resolved => set_display(&mut self.nodes, part, card.resolved),
                CardPart::Icon => self.write(part, card.icon.glyph()),
                CardPart::Source => self.write(part, &card.source),
                CardPart::Age => self.write(part, &card.age),
                CardPart::Body => {
                    self.write(part, &card.body);
                    set_display(&mut self.nodes, part, !card.body.is_empty());
                }
                CardPart::Title => {
                    self.write(part, &card.title);
                    let tone = card.title_tone();
                    if self.tones.get(part).is_ok_and(|t| t.0 != tone) {
                        commands.entity(part).insert(trailhead_ui::TextTone(tone));
                    }
                }
            }
        }
    }

    fn write(&mut self, part: Entity, words: &str) {
        if let Ok(text) = self.texts.get_mut(part) {
            set_text(text, words);
        }
    }

    fn say_empty(&mut self, empty: bool, filter: NotificationFilter) {
        let words = match filter {
            NotificationFilter::Unread => "Nothing unread.",
            NotificationFilter::All => "No notifications yet.",
        };
        let lines: Vec<Entity> = self.empty.iter().collect();
        for line in lines {
            self.write(line, words);
            if let Ok(node) = self.nodes.get_mut(line) {
                patch_node(node, |n| n.display = display(empty));
            }
        }
    }
}

/// Shows Show older under all of them while there may be more, disabled while a page is read; and Mark all read and
/// each card's mark, disabled while not live and, for Mark all read, while nothing is unread.
// A Bevy system takes what it reads and writes as parameters.
#[allow(clippy::too_many_arguments)]
fn show_footer(
    filter: Res<NotificationFilter>,
    history: Res<NotificationHistory>,
    connection: Res<State<Connection>>,
    mirrored: Query<&Mirrored<Notification>>,
    older: Query<(Entity, Has<InteractionDisabled>), With<ShowOlderButton>>,
    all: Query<(Entity, Has<InteractionDisabled>), With<MarkAllRead>>,
    marks: Query<(Entity, Has<InteractionDisabled>), With<MarkOne>>,
    mut nodes: Query<&mut Node>,
    mut commands: Commands,
) {
    let live = *connection.get() == Connection::Live;
    let more = *filter == NotificationFilter::All && !history.at_end();
    let unread = mirrored.iter().any(|m| m.0.is_unread()) || history.older().iter().any(Notification::is_unread);
    for (button, disabled) in &older {
        set_display(&mut nodes, button, more);
        enable(&mut commands, button, disabled, live && !history.reading());
    }
    for (button, disabled) in &all {
        enable(&mut commands, button, disabled, live && unread);
    }
    for (button, disabled) in &marks {
        enable(&mut commands, button, disabled, live);
    }
}

/// Takes `InteractionDisabled` off a control that should be enabled, or puts it on one that shouldn't.
fn enable(commands: &mut Commands, control: Entity, disabled: bool, enabled: bool) {
    if disabled && enabled {
        commands.entity(control).remove::<InteractionDisabled>();
    } else if !disabled && !enabled {
        commands.entity(control).insert(InteractionDisabled);
    }
}

fn on_mark_one(activate: On<Activate>, marks: Query<&MarkOne>, mut submit: MessageWriter<Submit>) {
    let Ok(mark) = marks.get(activate.entity) else { return };
    let id = mark.id.to_string();
    let call = if mark.read { Call::ReadNotification(id) } else { Call::UnreadNotification(id) };
    submit.write(Submit { source: activate.entity, request: Request::new(call, None) });
}

fn on_mark_all(activate: On<Activate>, buttons: Query<(), With<MarkAllRead>>, mut submit: MessageWriter<Submit>) {
    if buttons.contains(activate.entity) {
        let request = Request::new(Call::ReadAllNotifications, None);
        submit.write(Submit { source: activate.entity, request });
    }
}

fn on_show_older(activate: On<Activate>, buttons: Query<(), With<ShowOlderButton>>, mut commands: Commands) {
    if buttons.contains(activate.entity) {
        commands.trigger(ShowOlder);
    }
}

/// A button that marks notifications read or unread, or starts or ends focus.
type MarkButtons = Or<(With<MarkOne>, With<MarkAllRead>, With<FocusAct>)>;

/// Says in the panel's notice why a mark or a change to focus failed, or a read of older ones; one that went through
/// clears it.
fn show_answers(
    mut done: MessageReader<CommandDone>,
    mine: Query<(), MarkButtons>,
    history: Res<NotificationHistory>,
    mut notices: Query<&mut Notice, With<PanelNotice>>,
) {
    let mut said: Option<Notice> = None;
    for d in done.read().filter(|d| mine.contains(d.source)) {
        said = Some(match &d.outcome {
            Outcome::Ok { .. } => Notice::new(Tone::Info, ""),
            Outcome::Conflict { message, .. } | Outcome::RateLimited { message, .. } => {
                Notice::new(Tone::Error, message.clone())
            }
            Outcome::Forbidden(message) => Notice::new(Tone::Error, format!("Not allowed: {message}")),
            Outcome::Failed(message) => Notice::new(Tone::Error, message.clone()),
        });
    }
    if history.is_changed()
        && let Some(why) = history.error()
    {
        said = Some(Notice::new(Tone::Error, format!("Couldn't read older ones: {why}")));
    }
    let Some(said) = said else { return };
    for mut notice in &mut notices {
        notice.set_if_neq(said.clone());
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use trailhead_ui::{ModalPlugin, ModalRoot};

    use super::*;
    use crate::commands::Method;
    use crate::fixture::{notification_json, sid, sync_event};
    use crate::sync::Snapshot;
    use crate::testing::{Other, answer, feed, fill, sent, set_connection, settle};
    use crate::{AppId, Manifest, ShellApp, ShellPlugin};

    const NOTES: AppId = AppId("notes");

    struct Notes;

    impl ShellApp for Notes {
        fn manifest(&self) -> Manifest {
            Manifest { id: NOTES, name: "Notes", icon: Icon::Flag }
        }

        fn build(&self, _: &mut App) {}
    }

    fn windowed() -> App {
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
        .add_plugins(ShellPlugin::new().with(Notes).with(Other).windowed());
        settle(&mut app);
        app
    }

    fn one<F: bevy::ecs::query::QueryFilter>(app: &mut App) -> Entity {
        app.world_mut().query_filtered::<Entity, F>().single(app.world()).unwrap()
    }

    fn raised(id: &str, source: &str, at: u8, extra: serde_json::Value) -> serde_json::Value {
        let mut fields = json!({"id": id, "source": source, "title": format!("about {id}"), "body": "",
                                "created_at": crate::fixture::at(at)});
        fields.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        notification_json(fields)
    }

    fn filled() -> App {
        let mut app = windowed();
        let notifications = vec![
            raised("n3", "notes", 3, json!({"body": "Look at the list"})),
            raised(
                "n2",
                "server",
                2,
                json!({"read_at": crate::fixture::at(4),
                "actions": [{"label": "Pull request", "url": "https://github.com/o/r/pull/1"}]}),
            ),
            raised("n1", "factory", 1, json!({"resolved_at": crate::fixture::at(5)})),
        ];
        fill(&mut app, Snapshot { at: 5, notifications, ..default() });
        set_connection(&mut app, Connection::Live);
        app
    }

    fn open(app: &mut App) -> Entity {
        let bell = one::<With<crate::rail::BellButton>>(app);
        app.world_mut().trigger(Activate { entity: bell });
        settle(app);
        one::<With<NotificationsPanel>>(app)
    }

    /// Each card's entity and title, top to bottom.
    fn cards(app: &mut App) -> Vec<(Entity, String)> {
        let list = one::<With<NotificationList>>(app);
        let world = app.world();
        let cards: Vec<Entity> = world.get::<Children>(list).map(|c| c.iter().collect()).unwrap_or_default();
        cards
            .into_iter()
            .map(|card| {
                let mut all = vec![card];
                let mut at = 0;
                while at < all.len() {
                    all.extend(world.get::<Children>(all[at]).into_iter().flatten());
                    at += 1;
                }
                let title = all.iter().find(|e| world.get::<CardPart>(**e) == Some(&CardPart::Title)).unwrap();
                (card, world.get::<Text>(*title).unwrap().0.clone())
            })
            .collect()
    }

    fn counts(app: &mut App) -> (usize, Vec<(AppId, usize)>) {
        let bell = one::<With<crate::rail::BellButton>>(app);
        let world = app.world_mut();
        let badge = world.get::<BellCount>(bell).unwrap().0;
        let total = world.get::<CountBadge>(badge).unwrap().count;
        let mut apps: Vec<(AppId, usize)> =
            world.query::<(&AppCount, &CountBadge)>().iter(world).map(|(a, c)| (a.0, c.count)).collect();
        apps.sort_by_key(|(a, _)| a.0);
        (total, apps)
    }

    #[test]
    fn the_bell_counts_the_unread_ones_and_each_app_its_own() {
        let mut app = filled();
        assert_eq!(counts(&mut app), (1, vec![(NOTES, 1), (crate::testing::OTHER, 0)]));
        let bell = one::<With<crate::rail::BellButton>>(&mut app);
        assert_eq!(app.world().get::<Hint>(bell).unwrap().0, "Notifications: 1 unread");
        app.world_mut().resource_mut::<NotificationCounts>().shown = false;
        settle(&mut app);
        assert_eq!(counts(&mut app), (0, vec![(NOTES, 0), (crate::testing::OTHER, 0)]), "hidden counts say nothing");
    }

    #[test]
    fn the_panel_lists_the_unread_ones_or_all_newest_first_and_updates_in_place() {
        let mut app = filled();
        let panel = open(&mut app);
        assert!(app.world().get::<ModalRoot>(panel).is_some());
        assert_eq!(cards(&mut app).iter().map(|c| c.1.as_str()).collect::<Vec<_>>(), ["about n3"]);
        let switch = one::<With<FilterPick>>(&mut app);
        app.world_mut().trigger(Picked { entity: switch, index: 1 });
        settle(&mut app);
        let all = cards(&mut app);
        assert_eq!(all.iter().map(|c| c.1.as_str()).collect::<Vec<_>>(), ["about n3", "about n2", "about n1"]);
        let mut links = app.world_mut().query::<&trailhead_ui::ExternalLink>();
        let urls: Vec<String> = links.iter(app.world()).map(|l| l.url.clone()).collect();
        assert_eq!(urls, ["https://github.com/o/r/pull/1"], "an action is a link button");

        let after = raised("n3", "notes", 3, json!({"body": "Look at the list", "read_at": crate::fixture::at(9)}));
        feed(
            &mut app,
            sync_event(json!({"id": 6, "entity": "notification", "entity_id": sid("n3"),
                                         "kind": "notification.read", "after": after})),
        );
        assert_eq!(cards(&mut app), all, "the same cards, changed in place");
        assert_eq!(counts(&mut app).0, 0);
    }

    #[test]
    fn the_buttons_send_their_commands() {
        let mut app = filled();
        open(&mut app);
        let switch = one::<With<FilterPick>>(&mut app);
        app.world_mut().trigger(Picked { entity: switch, index: 1 });
        settle(&mut app);
        let mut marks: Vec<(Entity, MarkOne)> =
            app.world_mut().query::<(Entity, &MarkOne)>().iter(app.world()).map(|(e, m)| (e, *m)).collect();
        marks.sort_by_key(|(_, m)| m.id);
        let n2 = marks.iter().find(|(_, m)| m.id == crate::fixture::id("n2")).unwrap();
        assert!(!n2.1.read, "a read one is marked unread");
        app.world_mut().trigger(Activate { entity: n2.0 });
        let all = one::<With<MarkAllRead>>(&mut app);
        app.world_mut().trigger(Activate { entity: all });
        app.update();
        let out = sent(&mut app);
        let calls: Vec<(Method, String)> = out.iter().map(|s| (s.request.method(), s.request.call.path())).collect();
        assert_eq!(
            calls,
            [
                (Method::Post, format!("/v1/notifications/{}/unread", sid("n2"))),
                (Method::Post, "/v1/notifications/read-all".into())
            ]
        );
        answer(&mut app, out[1].source, Outcome::Failed("the server is busy".into()));
        let notice = one::<With<PanelNotice>>(&mut app);
        assert_eq!(app.world().get::<Notice>(notice).unwrap().message, "the server is busy");
    }

    #[test]
    fn a_silent_source_is_listed_but_never_counted() {
        let mut app = filled();
        let mut prefs = app.world_mut().resource_mut::<NotificationPrefs>();
        prefs.settings.levels.insert("notes".into(), trailhead_model::NotificationLevel::Silent);
        settle(&mut app);
        assert_eq!(counts(&mut app), (0, vec![(NOTES, 0), (crate::testing::OTHER, 0)]));
        open(&mut app);
        assert_eq!(cards(&mut app).iter().map(|c| c.1.as_str()).collect::<Vec<_>>(), ["about n3"], "still listed");
    }

    fn focus_line(app: &mut App) -> String {
        let line = one::<With<FocusLine>>(app);
        app.world().get::<Text>(line).unwrap().0.clone()
    }

    fn focus_button(app: &mut App, act: FocusAct) -> (Entity, bool) {
        let world = app.world_mut();
        let mut buttons = world.query::<(Entity, &FocusAct, Has<InteractionDisabled>)>();
        let (entity, _, disabled) = buttons.iter(world).find(|(_, a, _)| **a == act).unwrap();
        (entity, !disabled)
    }

    #[test]
    fn the_panel_says_whether_focus_is_on_and_its_buttons_save_only_focus() {
        let mut app = filled();
        let now: jiff::Zoned = "2026-11-21T12:00:00-08:00[America/Los_Angeles]".parse().unwrap();
        let clock = now.clone();
        app.insert_resource(Clock::new(move || clock.clone()));
        open(&mut app);
        assert!(!focus_button(&mut app, FocusAct::Hour).1, "not until the settings are read");
        {
            let mut prefs = app.world_mut().resource_mut::<NotificationPrefs>();
            prefs.answered = true;
            prefs.settings.levels.insert("factory".into(), trailhead_model::NotificationLevel::Bell);
        }
        settle(&mut app);
        assert!(focus_line(&mut app).starts_with("Focus is off"));
        assert!(focus_button(&mut app, FocusAct::Hour).1 && !focus_button(&mut app, FocusAct::End).1);

        let hour = focus_button(&mut app, FocusAct::Hour).0;
        app.world_mut().trigger(Activate { entity: hour });
        settle(&mut app);
        let out = sent(&mut app);
        let Call::SetNotificationSettings(put) = &out[0].request.call else { panic!("{:?}", out[0].request.call) };
        assert_eq!(out[0].request.method(), Method::Put);
        let until = now.timestamp().checked_add(jiff::SignedDuration::from_hours(1)).unwrap();
        assert_eq!(put.focus, Some(trailhead_model::Focus { until: Some(until) }));
        assert_eq!(put.level_of("factory"), trailhead_model::NotificationLevel::Bell, "the rest as read");

        app.world_mut().resource_mut::<NotificationPrefs>().settings = put.clone();
        settle(&mut app);
        assert_eq!(focus_line(&mut app), "Focus is on until 13:00.");
        let end = focus_button(&mut app, FocusAct::End);
        assert!(end.1);
        app.world_mut().trigger(Activate { entity: end.0 });
        settle(&mut app);
        let out = sent(&mut app);
        let Call::SetNotificationSettings(put) = &out[0].request.call else { panic!() };
        assert_eq!(put.focus, None);
        answer(&mut app, out[0].source, Outcome::Failed("the server is busy".into()));
        let notice = one::<With<PanelNotice>>(&mut app);
        assert_eq!(app.world().get::<Notice>(notice).unwrap().message, "the server is busy");
    }

    fn arrive(app: &mut App, id: i64, n: &str) {
        let after = raised(n, "notes", 8, json!({}));
        feed(
            app,
            sync_event(json!({"id": id, "entity": "notification", "entity_id": sid(n),
                                    "kind": "notification.raised", "after": after})),
        );
    }

    fn rung(app: &mut App) -> bool {
        let bell = one::<With<crate::rail::BellButton>>(app);
        app.world().get::<Ring>(bell).is_some()
    }

    #[test]
    fn the_bell_rings_when_one_arrives_while_the_window_has_focus_and_not_for_less_motion() {
        let mut app = filled();
        arrive(&mut app, 6, "n4");
        assert!(!rung(&mut app), "no window with focus");
        let window = app.world_mut().spawn(Window::default()).id();
        arrive(&mut app, 7, "n5");
        assert!(rung(&mut app));
        assert_eq!(counts(&mut app).0, 3);
        let bell = one::<With<crate::rail::BellButton>>(&mut app);
        app.world_mut().entity_mut(bell).remove::<Ring>();
        app.insert_resource(Appearance::new(trailhead_ui::ThemeChoice::Dark).with_reduce_motion(true));
        arrive(&mut app, 8, "n6");
        assert!(!rung(&mut app), "less motion: only the count changes");
        assert_eq!(counts(&mut app).0, 4);
        app.insert_resource(Appearance::new(trailhead_ui::ThemeChoice::Dark));
        app.world_mut().get_mut::<Window>(window).unwrap().focused = false;
        arrive(&mut app, 9, "n7");
        assert!(!rung(&mut app), "not without focus");
    }
}
