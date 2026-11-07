//! The Logs page of the settings modal (`docs/factory/ui.md#settings`): the control plane's log, the same lines
//! `trailhead-factory run` writes to `factory.log`, with newest at the bottom.
//!
//! The stream is opened only while the page is showing and closed when it isn't. [`LogState`] keeps at most
//! [`MAX_LINES`] lines, each with a number that never repeats, and the list under the page's scroll area is kept in
//! step with it through [`Keyed`], so a new line adds one entity and a dropped one removes one; nothing is rebuilt per
//! line (TRAIL-880). Follow mode keeps the list at its bottom until the person scrolls up.

use std::collections::{BTreeSet, VecDeque};

use bevy::feathers::controls::ButtonVariant;
use bevy::feathers::theme::ThemeToken;
use bevy::feathers::tokens;
use bevy::prelude::*;
use bevy::ui_widgets::{Activate, ControlOrientation};
use trailhead_factory::logs::{LogLevel, LogLine};

use crate::connection::SendNet;
use crate::net::{LogUpdate, NetCommand};
use crate::widgets::{
    Icon, Keyed, Picked, SelectEdit, button, extent, field, row, scroll_body, scroll_frame, scrollbar, select,
    set_display, set_text, size, space, text_mono, text_toned, token,
};

/// How many lines are kept; past this the oldest are dropped.
pub const MAX_LINES: usize = 3_000;

/// What the task dropdown says for no task filter.
const ALL_TASKS: &str = "All tasks";

/// The levels the dropdown offers as the least serious shown, in order.
const LEVELS: [LogLevel; 3] = [LogLevel::Info, LogLevel::Warn, LogLevel::Error];

/// A line kept, with the number it was given on arrival.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: u64,
    pub line: LogLine,
}

/// A [`LogUpdate`] on its way to the log.
#[derive(Message, Debug, Clone)]
pub struct LogItem(pub LogUpdate);

/// The lines heard while the page is open.
#[derive(Resource, Debug, Default)]
pub struct LogState {
    entries: VecDeque<Entry>,
    next: u64,
    /// Every task the lines named, for the task dropdown.
    tasks: BTreeSet<String>,
    /// Why there is nothing to read, when the factory can't send logs.
    note: Option<String>,
    /// Whether the net thread was last told to follow the log.
    streaming: bool,
}

impl LogState {
    /// The lines kept, oldest first.
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter()
    }

    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// The tasks seen in the lines, sorted.
    pub fn tasks(&self) -> impl Iterator<Item = &str> {
        self.tasks.iter().map(String::as_str)
    }

    fn push(&mut self, line: LogLine) {
        if let Some(task) = &line.task {
            self.tasks.insert(task.clone());
        }
        let id = self.next;
        self.next += 1;
        self.entries.push_back(Entry { id, line });
        while self.entries.len() > MAX_LINES {
            self.entries.pop_front();
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.tasks.clear();
        self.note = None;
    }

    fn apply(&mut self, update: LogUpdate) {
        match update {
            LogUpdate::Backlog(lines) => {
                self.clear();
                for line in lines {
                    self.push(line);
                }
            }
            LogUpdate::Line(line) => self.push(line),
            LogUpdate::Dropped => {}
            LogUpdate::TooOld(why) => self.note = Some(why),
        }
    }
}

/// Which lines the list shows.
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub struct LogFilter {
    /// The least serious level shown.
    pub level: LogLevel,
    pub task: Option<String>,
}

impl Default for LogFilter {
    fn default() -> Self {
        LogFilter { level: LogLevel::Info, task: None }
    }
}

impl LogFilter {
    pub fn allows(&self, line: &LogLine) -> bool {
        line.level >= self.level && self.task.as_ref().is_none_or(|t| line.task.as_ref() == Some(t))
    }
}

/// The lines the list shows: those the filter allows, oldest first.
pub fn visible<'a>(state: &'a LogState, filter: &LogFilter) -> Vec<&'a Entry> {
    state.entries().filter(|e| filter.allows(&e.line)).collect()
}

/// Whether the list sticks to its bottom.
#[derive(Resource, Debug)]
pub struct Follow {
    pub on: bool,
    /// How far the list could scroll when last looked at, to tell the person scrolling up from the list growing.
    last_max: f32,
    /// "Jump to latest" was pressed: go to the end whatever the list looked like.
    jump: bool,
}

impl Default for Follow {
    fn default() -> Self {
        Follow { on: true, last_max: 0.0, jump: false }
    }
}

/// How close to the end counts as being at it, in logical pixels.
const AT_BOTTOM: f32 = 2.0;

/// Where follow mode leaves a list scrolled to `y` that could scroll `max` (it could scroll `last_max` when last
/// looked at): `(following, y)`. Following, a list still at its old end moves to the new one, and one the person moved
/// up from it stops following. Not following, scrolling back to the end follows again.
pub fn follow_step(following: bool, y: f32, last_max: f32, max: f32) -> (bool, f32) {
    if following {
        if y < last_max - AT_BOTTOM { (false, y) } else { (true, max) }
    } else if y >= max - AT_BOTTOM {
        (true, max)
    } else {
        (false, y)
    }
}

/// The time of day a line happened, in the machine's time zone.
fn clock(ts_ms: i64) -> String {
    match jiff::Timestamp::from_millisecond(ts_ms) {
        Ok(at) => at.to_zoned(jiff::tz::TimeZone::system()).strftime("%H:%M:%S").to_string(),
        Err(_) => "--:--:--".into(),
    }
}

/// A line as the list shows it: the time, the level, the task if it has one, and the message.
pub fn line_text(line: &LogLine) -> String {
    let task = line.task.as_deref().map(|t| format!("{t} ")).unwrap_or_default();
    format!("{} {:<5} {task}{}", clock(line.ts_ms), line.level.label(), line.message)
}

/// The theme color a level is drawn in.
pub fn level_tone(level: LogLevel) -> ThemeToken {
    match level {
        LogLevel::Error => token::DANGER,
        LogLevel::Warn => token::WARN,
        LogLevel::Info => tokens::TEXT_MAIN,
        LogLevel::Debug | LogLevel::Trace => tokens::TEXT_DIM,
    }
}

/// The task dropdown's rows: no filter, then each task seen (and the one filtered to, if it has left the log).
pub fn task_options(state: &LogState, filter: &LogFilter) -> Vec<String> {
    let mut tasks: BTreeSet<&str> = state.tasks().collect();
    if let Some(task) = &filter.task {
        tasks.insert(task);
    }
    std::iter::once(ALL_TASKS.to_string()).chain(tasks.into_iter().map(String::from)).collect()
}

fn level_options() -> Vec<String> {
    LEVELS.iter().map(|l| l.label().to_string()).collect()
}

/// The state of the log: what it holds and how it is filtered. No drawing, so the connection can feed it.
pub struct LogsPlugin;

impl Plugin for LogsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LogState>()
            .init_resource::<LogFilter>()
            .init_resource::<Follow>()
            .add_message::<LogItem>()
            .add_message::<SendNet>()
            .add_systems(PreUpdate, apply_items);
    }
}

fn apply_items(mut items: MessageReader<LogItem>, mut state: ResMut<LogState>) {
    for LogItem(update) in items.read() {
        state.apply(update.clone());
    }
}

/// The Logs page's box in the settings modal.
#[derive(Component)]
pub struct LogsPage;

/// The scrolling list of lines.
#[derive(Component)]
struct LogList;

#[derive(Component, Clone, Default)]
struct LevelSelect;

#[derive(Component, Clone, Default)]
struct TaskSelect;

/// The "jump to latest" button, shown while not following.
#[derive(Component)]
struct JumpToLatest;

/// The line saying the factory can't send logs.
#[derive(Component)]
struct LogNote;

/// A line of the list.
#[derive(Component)]
struct LogRow;

/// The lines drawn.
#[derive(Resource, Default)]
struct LogRows(Keyed<Entry>);

/// Fills the Logs page's box: the filters, the note, and the list with its scrollbar.
pub fn logs_page(commands: &mut Commands, page: Entity) {
    commands.entity(page).insert(LogsPage);
    let filters = commands
        .spawn_scene(bsn! { @row() Node { align_items: AlignItems::FlexEnd, column_gap: { px(space::GROUPS) } } })
        .insert(ChildOf(page))
        .id();
    commands
        .spawn_scene(field("Level", bsn_list! { @select(level_options(), Some(0), LEVELS.len()) LevelSelect }))
        .insert(ChildOf(filters));
    commands
        .spawn_scene(field("Task", bsn_list! { @select(vec![ALL_TASKS.to_string()], Some(0), 8) TaskSelect }))
        .insert(ChildOf(filters));
    commands
        .spawn_scene(button("Jump to latest", Some(Icon::ChevronDown), ButtonVariant::Normal))
        .insert((JumpToLatest, ChildOf(filters)));
    commands.spawn_scene(text_toned("", size::BODY, token::WARN)).insert((
        LogNote,
        Node { display: Display::None, ..default() },
        ChildOf(page),
    ));
    let frame = commands.spawn(scroll_frame()).insert(ChildOf(page)).id();
    // A list of its own inside the modal, which scrolls too: the wheel goes to the nearest.
    commands.entity(frame).insert(Node {
        flex_direction: FlexDirection::Column,
        flex_grow: 0.0,
        min_height: px(0),
        height: px(extent::LIST_HEIGHT),
        ..default()
    });
    let list = commands.spawn((scroll_body(), LogList)).insert(ChildOf(frame)).id();
    commands.entity(list).insert(Node {
        flex_direction: FlexDirection::Column,
        min_height: px(0),
        overflow: Overflow::scroll_y(),
        ..default()
    });
    commands.queue_spawn_scene(scrollbar(list, ControlOrientation::Vertical)).insert(ChildOf(frame));
}

/// Tells the net thread to follow the log while the page is showing, and to stop when it isn't.
fn sync_stream(
    pages: Query<&Node, With<LogsPage>>,
    mut state: ResMut<LogState>,
    mut follow: ResMut<Follow>,
    mut send: MessageWriter<SendNet>,
) {
    let wanted = pages.iter().any(|n| n.display != Display::None);
    if wanted == state.streaming {
        return;
    }
    state.streaming = wanted;
    send.write(SendNet(NetCommand::Logs(wanted)));
    if !wanted {
        // Nothing is kept for a page nobody is reading; the next opening starts from the factory's backlog.
        state.clear();
    } else {
        *follow = Follow::default();
    }
}

/// A dropdown on the page picked: the least serious level shown, or the task.
fn on_pick(
    picked: On<Picked>,
    levels: Query<(), With<LevelSelect>>,
    tasks: Query<(), With<TaskSelect>>,
    state: Res<LogState>,
    mut filter: ResMut<LogFilter>,
) {
    if levels.contains(picked.entity) {
        if let Some(level) = LEVELS.get(picked.index)
            && filter.level != *level
        {
            filter.level = *level;
        }
    } else if tasks.contains(picked.entity) {
        let options = task_options(&state, &filter);
        let task = if picked.index == 0 { None } else { options.get(picked.index).cloned() };
        if filter.task != task {
            filter.task = task;
        }
    }
}

fn on_jump(activate: On<Activate>, jump: Query<(), With<JumpToLatest>>, mut follow: ResMut<Follow>) {
    if jump.contains(activate.entity) {
        follow.on = true;
        follow.jump = true;
    }
}

/// Keeps the page's dropdowns, note and button current, in place.
#[allow(clippy::too_many_arguments)]
fn refresh_controls(
    state: Res<LogState>,
    filter: Res<LogFilter>,
    follow: Res<Follow>,
    new: Query<(), Added<LogsPage>>,
    levels: Query<Entity, With<LevelSelect>>,
    tasks: Query<Entity, With<TaskSelect>>,
    jump: Query<Entity, With<JumpToLatest>>,
    notes: Query<(Entity, &mut Text), With<LogNote>>,
    mut nodes: Query<&mut Node>,
    mut selects: SelectEdit,
) {
    if !(state.is_changed() || filter.is_changed() || follow.is_changed() || !new.is_empty()) {
        return;
    }
    let mut notes = notes;
    for (note, text) in &mut notes {
        set_text(text, state.note().unwrap_or_default());
        set_display(&mut nodes, note, state.note().is_some());
    }
    for select in &jump {
        set_display(&mut nodes, select, !follow.on);
    }
    for select in &levels {
        let at = LEVELS.iter().position(|l| *l == filter.level);
        selects.set(select, &level_options(), at);
    }
    let options = task_options(&state, &filter);
    let at = filter.task.as_ref().and_then(|t| options.iter().position(|o| o == t)).unwrap_or(0);
    for select in &tasks {
        selects.set(select, &options, Some(at));
    }
}

/// Keeps the lines of the list in step with what the filter lets through: a new line spawns one entity, a dropped or
/// filtered one despawns, and the rest are left alone.
fn draw_rows(
    state: Res<LogState>,
    filter: Res<LogFilter>,
    new: Query<(), Added<LogList>>,
    list: Query<Entity, With<LogList>>,
    mut rows: ResMut<LogRows>,
    mut commands: Commands,
) {
    if !new.is_empty() {
        // The list is new, and so are its children: what was drawn went with the old one.
        rows.0.forget();
    } else if !(state.is_changed() || filter.is_changed()) {
        return;
    }
    let Ok(list) = list.single() else {
        rows.0.forget();
        return;
    };
    let wanted: Vec<(String, Entry)> =
        visible(&state, &filter).into_iter().map(|e| (e.id.to_string(), e.clone())).collect();
    rows.0.sync(
        &mut commands,
        &[(list, wanted)],
        |commands, _, entry| {
            let tone = level_tone(entry.line.level);
            let text = line_text(&entry.line);
            commands.spawn((Name::new("log line"), LogRow, text_mono(text, size::SECONDARY, tone))).id()
        },
        // A line never changes once it has arrived.
        |_, _, _| {},
    );
}

/// Holds the list at its end while following, and notices the person scrolling away from it and back.
fn follow_bottom(mut lists: Query<(&ComputedNode, &mut ScrollPosition), With<LogList>>, mut follow: ResMut<Follow>) {
    for (computed, mut position) in &mut lists {
        let scale = computed.inverse_scale_factor;
        let (size, content) = (computed.size().y * scale, computed.content_size().y * scale);
        if size <= 0.0 {
            // Not laid out: the page is hidden.
            continue;
        }
        let max = (content - size).max(0.0);
        let (on, y) = if follow.jump {
            follow.jump = false;
            (true, max)
        } else {
            follow_step(follow.on, position.y, follow.last_max, max)
        };
        if y != position.y {
            position.y = y;
        }
        if follow.on != on {
            follow.on = on;
        }
        if follow.last_max != max {
            follow.last_max = max;
        }
    }
}

/// The Logs page's drawing and input, on top of [`LogsPlugin`].
pub struct LogsUiPlugin;

impl Plugin for LogsUiPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<LogsPlugin>() {
            app.add_plugins(LogsPlugin);
        }
        app.init_resource::<LogRows>()
            .add_observer(on_pick)
            .add_observer(on_jump)
            .add_systems(Update, (sync_stream, refresh_controls, draw_rows, follow_bottom).chain());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(n: usize, level: LogLevel, task: Option<&str>) -> LogLine {
        LogLine {
            ts_ms: 1_700_000_000_000 + n as i64,
            level,
            target: "t".into(),
            repo: None,
            task: task.map(String::from),
            message: format!("line {n}"),
        }
    }

    fn open() -> LogState {
        LogState { streaming: true, ..default() }
    }

    #[test]
    fn the_cap_drops_the_oldest_and_numbers_never_repeat() {
        let mut state = open();
        for n in 0..MAX_LINES + 5 {
            state.push(line(n, LogLevel::Info, None));
        }
        assert_eq!(state.entries().count(), MAX_LINES);
        let first = state.entries().next().unwrap();
        assert_eq!((first.id, first.line.message.as_str()), (5, "line 5"));
        assert_eq!(state.entries().last().unwrap().id, (MAX_LINES + 4) as u64);
    }

    #[test]
    fn a_backlog_replaces_what_was_there_and_a_line_adds_to_it() {
        let mut state = open();
        state.apply(LogUpdate::Line(line(1, LogLevel::Info, Some("TRAIL-1"))));
        state.apply(LogUpdate::Backlog(vec![line(2, LogLevel::Warn, Some("TRAIL-2")), line(3, LogLevel::Info, None)]));
        state.apply(LogUpdate::Line(line(4, LogLevel::Error, Some("TRAIL-3"))));
        let messages: Vec<_> = state.entries().map(|e| e.line.message.as_str()).collect();
        assert_eq!(messages, ["line 2", "line 3", "line 4"]);
        assert_eq!(state.tasks().collect::<Vec<_>>(), ["TRAIL-2", "TRAIL-3"]);
        state.apply(LogUpdate::TooOld("this factory is too old to send logs".into()));
        assert_eq!(state.note(), Some("this factory is too old to send logs"));
    }

    #[test]
    fn the_filters_pick_by_level_and_task() {
        let mut state = open();
        state.push(line(1, LogLevel::Info, Some("TRAIL-1")));
        state.push(line(2, LogLevel::Warn, Some("TRAIL-1")));
        state.push(line(3, LogLevel::Error, Some("TRAIL-2")));
        state.push(line(4, LogLevel::Debug, None));
        let shown = |f: &LogFilter| visible(&state, f).iter().map(|e| e.line.ts_ms % 10).collect::<Vec<_>>();
        assert_eq!(shown(&LogFilter::default()), [1, 2, 3]);
        assert_eq!(shown(&LogFilter { level: LogLevel::Warn, task: None }), [2, 3]);
        assert_eq!(shown(&LogFilter { level: LogLevel::Error, task: None }), [3]);
        assert_eq!(shown(&LogFilter { level: LogLevel::Info, task: Some("TRAIL-1".into()) }), [1, 2]);
        assert_eq!(
            task_options(&state, &LogFilter { level: LogLevel::Info, task: Some("TRAIL-9".into()) }),
            ["All tasks", "TRAIL-1", "TRAIL-2", "TRAIL-9"]
        );
    }

    #[test]
    fn follow_sticks_to_the_bottom_and_lets_go_when_scrolled_up() {
        // At the end of a list that grew: follows it down.
        assert_eq!(follow_step(true, 100.0, 100.0, 140.0), (true, 140.0));
        // Moved up from the end: stops, and stays where the person put it.
        assert_eq!(follow_step(true, 60.0, 100.0, 140.0), (false, 60.0));
        assert_eq!(follow_step(false, 60.0, 140.0, 180.0), (false, 60.0));
        // Scrolled back to the end: follows again.
        assert_eq!(follow_step(false, 179.0, 180.0, 180.0), (true, 180.0));
    }

    #[test]
    fn lines_say_when_what_and_which_task() {
        let text = line_text(&line(1, LogLevel::Warn, Some("TRAIL-7")));
        assert!(text.ends_with(" WARN  TRAIL-7 line 1"), "{text}");
        assert!(line_text(&line(1, LogLevel::Info, None)).ends_with(" INFO  line 1"));
    }

    mod page {
        use super::*;

        fn app() -> App {
            let mut app = App::new();
            app.add_plugins(MinimalPlugins).add_plugins(LogsUiPlugin);
            app
        }

        fn rows(app: &mut App) -> Vec<(Entity, String)> {
            let world = app.world_mut();
            world.query_filtered::<(Entity, &Text), With<LogRow>>().iter(world).map(|(e, t)| (e, t.0.clone())).collect()
        }

        fn feed(app: &mut App, update: LogUpdate) {
            app.world_mut().write_message(LogItem(update));
        }

        /// A page with its list, as the settings modal spawns it, but with no dropdowns to draw.
        fn spawn_list(app: &mut App) -> Entity {
            let list = app.world_mut().spawn(LogList).id();
            app.world_mut().spawn((LogsPage, Node::default()));
            list
        }

        #[test]
        fn the_stream_is_asked_for_only_while_the_page_shows() {
            let mut app = app();
            app.add_systems(Update, |mut commands: Commands| {
                let _ = &mut commands;
            });
            let page = app.world_mut().spawn((LogsPage, Node { display: Display::None, ..default() })).id();
            app.update();
            let sent = |app: &mut App| -> Vec<bool> {
                app.world_mut()
                    .resource_mut::<Messages<SendNet>>()
                    .drain()
                    .filter_map(|SendNet(c)| match c {
                        NetCommand::Logs(on) => Some(on),
                        _ => None,
                    })
                    .collect()
            };
            assert!(sent(&mut app).is_empty());
            app.world_mut().get_mut::<Node>(page).unwrap().display = Display::Flex;
            app.update();
            assert_eq!(sent(&mut app), [true]);
            app.update();
            assert!(sent(&mut app).is_empty());
            app.world_mut().get_mut::<Node>(page).unwrap().display = Display::None;
            app.update();
            assert_eq!(sent(&mut app), [false]);
            // Closing the modal (the page is gone) stops it too.
            app.world_mut().get_mut::<Node>(page).unwrap().display = Display::Flex;
            app.update();
            app.world_mut().despawn(page);
            app.update();
            assert_eq!(sent(&mut app), [true, false]);
        }

        #[test]
        fn lines_append_without_respawning_earlier_ones_and_the_cap_drops_the_oldest() {
            let mut app = app();
            spawn_list(&mut app);
            app.update();
            feed(&mut app, LogUpdate::Backlog(vec![line(1, LogLevel::Info, None), line(2, LogLevel::Info, None)]));
            app.update();
            app.update();
            let before = rows(&mut app);
            assert_eq!(before.len(), 2);

            feed(&mut app, LogUpdate::Line(line(3, LogLevel::Warn, None)));
            app.update();
            app.update();
            let after = rows(&mut app);
            assert_eq!(after.len(), 3);
            for (entity, _) in &before {
                assert!(after.iter().any(|(e, _)| e == entity), "an earlier line was respawned");
            }

            for n in 4..MAX_LINES + 4 {
                feed(&mut app, LogUpdate::Line(line(n, LogLevel::Info, None)));
            }
            app.update();
            app.update();
            let capped = rows(&mut app);
            assert_eq!(capped.len(), MAX_LINES);
            assert!(!capped.iter().any(|(e, _)| before.iter().any(|(b, _)| b == e)), "the oldest should be gone");
            assert!(capped.iter().any(|(_, t)| t.ends_with(&format!("line {}", MAX_LINES + 3))));
        }

        #[test]
        fn the_filters_hide_lines_in_place() {
            let mut app = app();
            spawn_list(&mut app);
            app.update();
            feed(
                &mut app,
                LogUpdate::Backlog(vec![
                    line(1, LogLevel::Info, Some("TRAIL-1")),
                    line(2, LogLevel::Error, Some("TRAIL-2")),
                    line(3, LogLevel::Warn, Some("TRAIL-1")),
                ]),
            );
            app.update();
            app.update();
            let all = rows(&mut app);
            assert_eq!(all.len(), 3);

            app.world_mut().resource_mut::<LogFilter>().level = LogLevel::Warn;
            app.update();
            let warned = rows(&mut app);
            assert_eq!(warned.len(), 2);
            assert!(warned.iter().all(|(e, _)| all.iter().any(|(a, _)| a == e)));

            app.world_mut().resource_mut::<LogFilter>().task = Some("TRAIL-1".into());
            app.update();
            let one = rows(&mut app);
            assert_eq!(one.len(), 1);
            assert!(one[0].1.ends_with("line 3"));

            *app.world_mut().resource_mut::<LogFilter>() = LogFilter::default();
            app.update();
            assert_eq!(rows(&mut app).len(), 3);
        }

        #[test]
        fn a_rendered_list_gets_a_color_by_level() {
            let mut app = app();
            spawn_list(&mut app);
            app.update();
            feed(&mut app, LogUpdate::Backlog(vec![line(1, LogLevel::Error, None), line(2, LogLevel::Info, None)]));
            app.update();
            app.update();
            let world = app.world_mut();
            let tones: Vec<_> = world
                .query_filtered::<&bevy::feathers::theme::ThemeTextColor, With<LogRow>>()
                .iter(world)
                .map(|t| t.0.clone())
                .collect();
            assert_eq!(tones, [token::DANGER, tokens::TEXT_MAIN]);
        }
    }
}
