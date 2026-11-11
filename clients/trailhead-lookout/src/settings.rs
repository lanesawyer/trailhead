//! The settings modal (`docs/factory/ui.md#settings`), opened from the gear at the right of the header: pages picked
//! from tabs across the top. General holds the appearance: light, dark or the desktop's theme, kept in [`ThemeFile`]
//! for the next launch. Connection holds where Lookout is connected and who it acts as, with Disconnect; Factory shows
//! what the factory status already says, read-only. A new page is a [`Page`] variant and the fields and texts
//! for it in [`fill`] and [`refresh`].
//!
//! The modal is spawned once, when it opens, and its texts are set in place as what they show changes.

use bevy::clipboard::Clipboard;
use bevy::prelude::*;
use bevy::ui_widgets::{Activate, ControlOrientation, SelectedTab, ValueChange};
use trailhead_model::Task;

use crate::author::Authoring;
use crate::connection::{Api, Connection, Dialed, LocalEndpoint, SendNet};
use crate::factory::FactoryStatus;
use crate::logs::{LogsUiPlugin, logs_page};
use crate::mirror::{Mirrored, rows};
use crate::net::NetCommand;
use crate::widgets::{
    Appearance, ButtonGroup, ButtonKind, Icon, Modal, ModalSlots, Picked, ThemeChoice, button, button_group, field,
    open_modal, row, set_text, size, space, tab, tab_strip, text, text_dim, tool_button,
};

/// A page of the modal, in tab order.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    #[default]
    General,
    Connection,
    Factory,
    Logs,
}

impl Page {
    pub const ALL: [Page; 4] = [Page::General, Page::Connection, Page::Factory, Page::Logs];

    pub fn label(self) -> &'static str {
        match self {
            Page::General => "General",
            Page::Connection => "Connection",
            Page::Factory => "Factory",
            Page::Logs => "Logs",
        }
    }
}

/// The gear in the header.
#[derive(Component)]
pub struct SettingsButton;

/// The settings modal.
#[derive(Component)]
pub struct SettingsModal;

impl Modal for SettingsModal {
    fn title(&self) -> String {
        "Settings".into()
    }
    // One size whichever tab is shown, so switching tabs doesn't resize the modal.
    fn tall(&self) -> bool {
        true
    }
    fn wide(&self) -> bool {
        true
    }
}

/// The strip of tabs in the modal.
#[derive(Component)]
struct SettingsStrip;

/// A tab, and the page it shows.
#[derive(Component)]
struct SettingsTab(Page);

/// A page's box, hidden unless its tab is picked.
#[derive(Component)]
struct SettingsPage(Page);

/// A button in the modal.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsAct {
    CopyEndpoint,
    Disconnect,
}

/// The theme's buttons, one per [`ThemeChoice::ALL`].
#[derive(Component, Clone, Default)]
struct ThemePick;

/// Where the theme picked is kept for the next launch: a file holding `light`, `dark` or `desktop`.
#[derive(Resource, Clone, Debug)]
pub struct ThemeFile(pub std::path::PathBuf);

impl ThemeFile {
    /// The choice kept last time, or the desktop's when there is none.
    pub fn load(&self) -> ThemeChoice {
        std::fs::read_to_string(&self.0).map_or(ThemeChoice::Desktop, |text| theme_from_word(&text))
    }
}

/// A theme choice as it is kept.
pub fn theme_word(choice: ThemeChoice) -> &'static str {
    match choice {
        ThemeChoice::Desktop => "desktop",
        ThemeChoice::Light => "light",
        ThemeChoice::Dark => "dark",
    }
}

/// The choice a kept word means; anything else is the desktop's.
pub fn theme_from_word(word: &str) -> ThemeChoice {
    ThemeChoice::ALL.into_iter().find(|c| theme_word(*c) == word.trim()).unwrap_or_default()
}

/// A text in the modal, and what it shows.
#[derive(Component, Clone, Default)]
struct Shows(Value);

/// What a text in the modal shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Value {
    #[default]
    Factory,
    Identity,
    Endpoint,
    Status,
    Repos,
    Queue,
}

/// What the Connection page says of the control plane dialed: the id, or that there is none.
pub fn dialed_text(dialed: &Dialed) -> String {
    dialed.0.clone().unwrap_or_else(|| "not connected".into())
}

/// What the Connection page says of who the token acts as, which is only known while signed in.
pub fn identity_text(connection: Connection, api: Option<&Api>) -> String {
    match api {
        Some(api) if connection == Connection::Live => api.0.user.clone(),
        _ => "not signed in".into(),
    }
}

/// The repos the factory serves, one per line with the state of its control plane.
pub fn repos_text(status: &FactoryStatus) -> String {
    let lines: Vec<String> = status.controls().map(|(repo, state)| format!("{repo}: {state}")).collect();
    if lines.is_empty() { "none heard of yet".into() } else { lines.join("\n") }
}

/// Every queue task the factory works, one per line, or the one new tasks go under until a status has listed them.
pub fn queues_text(authoring: &Authoring, status: &FactoryStatus, tasks: &[Task]) -> String {
    let mut queues = status.queues();
    if queues.is_empty() {
        queues.push(authoring.queue.clone());
    }
    let lines: Vec<String> = queues.iter().map(|q| queue_text(q, tasks)).collect();
    lines.join("\n")
}

/// One queue task, and how much is queued beneath it.
pub fn queue_text(queue: &str, tasks: &[Task]) -> String {
    let Some(task) = tasks.iter().find(|t| t.key == *queue) else {
        return format!("{queue} (not loaded)");
    };
    let queued: Vec<&Task> = tasks.iter().filter(|t| t.parent_id == Some(task.id)).collect();
    let open = queued.iter().filter(|t| !t.status.is_closed()).count();
    format!("{queue}: {open} not closed, {} in all", queued.len())
}

fn value(which: Value, label: &str) -> impl Scene {
    field(label.to_string(), bsn_list! { @text("", size::BODY) Shows({ which }) })
}

/// Opens the modal from the gear, unless one is open already.
fn on_gear(
    activate: On<Activate>,
    gear: Query<(), With<SettingsButton>>,
    open: Query<(), With<SettingsModal>>,
    mut commands: Commands,
) {
    if gear.contains(activate.entity) && open.is_empty() {
        open_modal(&mut commands, SettingsModal);
    }
}

/// Fills a new modal: the tabs, and a box for each page, of which only the first shows.
fn fill(
    new: Query<&ModalSlots, (With<SettingsModal>, Added<ModalSlots>)>,
    appearance: Res<Appearance>,
    mut commands: Commands,
) {
    for slots in &new {
        let strip = commands
            .spawn_scene(tab_strip(ControlOrientation::Horizontal))
            .insert((SettingsStrip, ChildOf(slots.body)))
            .id();
        let mut picked = None;
        for page in Page::ALL {
            let entity = commands
                .spawn_scene(tab(page.label(), ControlOrientation::Horizontal))
                .insert((SettingsTab(page), ChildOf(strip)))
                .id();
            picked.get_or_insert(entity);
        }
        commands.entity(strip).insert(SelectedTab(picked));

        for page in Page::ALL {
            let node = Node {
                flex_direction: FlexDirection::Column,
                row_gap: px(space::ROWS),
                display: if page == Page::ALL[0] { Display::Flex } else { Display::None },
                ..default()
            };
            let holder = commands.spawn((Name::new(page.label()), SettingsPage(page), node, ChildOf(slots.body))).id();
            match page {
                Page::General => general_page(&mut commands, holder, appearance.choice),
                Page::Connection => connection_page(&mut commands, holder),
                Page::Factory => factory_page(&mut commands, holder),
                Page::Logs => logs_page(&mut commands, holder),
            }
        }
    }
}

fn connection_page(commands: &mut Commands, page: Entity) {
    commands.spawn_scene(value(Value::Factory, "Factory endpoint")).insert(ChildOf(page));
    let line =
        commands.spawn_scene(bsn! { @row() Node { align_items: AlignItems::FlexEnd } }).insert(ChildOf(page)).id();
    commands.spawn_scene(value(Value::Endpoint, "This app's endpoint id")).insert(ChildOf(line));
    commands
        .spawn_scene(tool_button(Icon::Copy, "Copy the endpoint id", ButtonKind::Plain))
        .insert((SettingsAct::CopyEndpoint, ChildOf(line)));
    commands.spawn_scene(value(Value::Identity, "Signed in as")).insert(ChildOf(page));
    commands
        .spawn_scene(button("Disconnect", None, ButtonKind::Normal))
        .insert((SettingsAct::Disconnect, ChildOf(page)));
}

fn general_page(commands: &mut Commands, page: Entity, choice: ThemeChoice) {
    let options = ThemeChoice::ALL.iter().map(|c| (c.label().to_string(), Some(c.icon()))).collect();
    let at = ThemeChoice::ALL.iter().position(|c| *c == choice);
    commands.spawn_scene(field("Appearance", bsn_list! { @button_group(options, at) ThemePick })).insert(ChildOf(page));
    commands
        .spawn_scene(text_dim("Desktop follows your desktop's light or dark setting.", size::SECONDARY))
        .insert(ChildOf(page));
}

/// Shows a theme chosen elsewhere on the theme's buttons.
fn show_theme(appearance: Res<Appearance>, mut groups: Query<&mut ButtonGroup, With<ThemePick>>) {
    let at = ThemeChoice::ALL.iter().position(|c| *c == appearance.choice);
    for mut group in &mut groups {
        if group.chosen != at {
            group.chosen = at;
        }
    }
}

/// A theme was picked: wear it.
fn on_theme_pick(picked: On<Picked>, picks: Query<(), With<ThemePick>>, mut appearance: ResMut<Appearance>) {
    if !picks.contains(picked.entity) {
        return;
    }
    if let Some(choice) = ThemeChoice::ALL.get(picked.index) {
        appearance.choice = *choice;
    }
}

/// Keeps the theme picked for the next launch.
fn keep_theme(appearance: Res<Appearance>, file: Res<ThemeFile>, mut kept: Local<Option<ThemeChoice>>) {
    if *kept == Some(appearance.choice) {
        return;
    }
    // The first look is at what was loaded, which is already kept.
    if kept.replace(appearance.choice).is_none() {
        return;
    }
    if let Some(dir) = file.0.parent()
        && let Err(e) =
            std::fs::create_dir_all(dir).and_then(|()| std::fs::write(&file.0, theme_word(appearance.choice)))
    {
        warn!(error = %e, "couldn't keep the theme picked");
    }
}

fn factory_page(commands: &mut Commands, page: Entity) {
    commands.spawn_scene(value(Value::Status, "Status")).insert(ChildOf(page));
    commands.spawn_scene(value(Value::Repos, "Repos it serves")).insert(ChildOf(page));
    commands.spawn_scene(value(Value::Queue, "Queue task")).insert(ChildOf(page));
}

/// A tab was picked: select it and show its page.
fn on_tab(
    change: On<ValueChange<Option<Entity>>>,
    strips: Query<(), With<SettingsStrip>>,
    tabs: Query<&SettingsTab>,
    mut pages: Query<(&SettingsPage, &mut Node)>,
    mut commands: Commands,
) {
    if !strips.contains(change.source) {
        return;
    }
    commands.entity(change.source).insert(SelectedTab(change.value));
    let Some(shown) = change.value.and_then(|t| tabs.get(t).ok()).map(|t| t.0) else { return };
    for (SettingsPage(page), mut node) in &mut pages {
        let display = if *page == shown { Display::Flex } else { Display::None };
        if node.display != display {
            node.display = display;
        }
    }
}

/// Disconnect: let go of the factory and close the modal, so the connect panel under it is what's left.
fn on_disconnect(
    activate: On<Activate>,
    acts: Query<&SettingsAct>,
    modals: Query<Entity, With<SettingsModal>>,
    mut send: MessageWriter<SendNet>,
    mut commands: Commands,
) {
    if acts.get(activate.entity) != Ok(&SettingsAct::Disconnect) {
        return;
    }
    send.write(SendNet(NetCommand::Disconnect));
    for modal in &modals {
        commands.entity(modal).despawn();
    }
}

fn copy_endpoint(
    activate: On<Activate>,
    acts: Query<&SettingsAct>,
    endpoint: Res<LocalEndpoint>,
    mut clipboard: ResMut<Clipboard>,
) {
    if acts.get(activate.entity) != Ok(&SettingsAct::CopyEndpoint) {
        return;
    }
    if let Some(id) = &endpoint.0
        && let Err(e) = clipboard.set_text(id.clone())
    {
        warn!(error = %e, "couldn't copy the endpoint id");
    }
}

/// Keeps the texts of an open modal current, in place.
#[allow(clippy::too_many_arguments)]
fn refresh(
    mut texts: Query<(&Shows, &mut Text)>,
    dialed: Res<Dialed>,
    endpoint: Res<LocalEndpoint>,
    connection: Res<State<Connection>>,
    api: Option<Res<Api>>,
    status: Res<FactoryStatus>,
    authoring: Res<Authoring>,
    tasks: Query<&Mirrored<Task>>,
) {
    if texts.is_empty() {
        return;
    }
    let tasks = rows(&tasks);
    for (Shows(which), text) in &mut texts {
        let wanted = match which {
            Value::Factory => dialed_text(&dialed),
            Value::Identity => identity_text(*connection.get(), api.as_deref()),
            Value::Endpoint => endpoint.0.clone().unwrap_or_else(|| "starting".into()),
            Value::Status => status.label(),
            Value::Repos => repos_text(&status),
            Value::Queue => queues_text(&authoring, &status, &tasks),
        };
        set_text(text, &wanted);
    }
}

/// The settings modal's logic and drawing, without the clipboard.
pub struct SettingsPlugin;

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(on_gear)
            .add_observer(on_tab)
            .add_observer(on_disconnect)
            .add_observer(on_theme_pick)
            .add_plugins(LogsUiPlugin)
            .add_systems(Update, (fill, refresh, show_theme.run_if(resource_changed::<Appearance>)).chain());
    }
}

/// [`SettingsPlugin`] and its copy button, which needs the window's clipboard, and keeping the theme picked in the
/// [`ThemeFile`] the app inserts.
pub struct SettingsUiPlugin;

impl Plugin for SettingsUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(SettingsPlugin).add_observer(copy_endpoint).add_systems(Update, keep_theme);
    }
}

#[cfg(test)]
mod tests {
    use bevy::input::keyboard::{Key, KeyboardInput};
    use bevy::input_focus::FocusedInput;
    use bevy::state::app::StatesPlugin;
    use serde_json::json;

    use super::*;
    use crate::fixture::task_json;
    use crate::widgets::{ModalPlugin, ModalRoot};

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin, AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .init_state::<Connection>()
            .init_resource::<Dialed>()
            .init_resource::<LocalEndpoint>()
            .init_resource::<FactoryStatus>()
            .init_resource::<Authoring>()
            .init_resource::<Appearance>()
            .add_message::<SendNet>()
            .add_plugins((ModalPlugin, SettingsPlugin));
        app
    }

    fn settle(app: &mut App) {
        for _ in 0..4 {
            app.update();
        }
    }

    fn modals(app: &mut App) -> usize {
        app.world_mut().query_filtered::<(), With<SettingsModal>>().iter(app.world()).count()
    }

    fn open(app: &mut App) {
        let gear = app.world_mut().spawn(SettingsButton).id();
        app.world_mut().trigger(Activate { entity: gear });
        settle(app);
    }

    fn shown(app: &mut App) -> Vec<Page> {
        let world = app.world_mut();
        world
            .query::<(&SettingsPage, &Node)>()
            .iter(world)
            .filter(|(_, n)| n.display != Display::None)
            .map(|(p, _)| p.0)
            .collect()
    }

    type Texts = Vec<(Entity, Value, String)>;

    fn texts(app: &mut App) -> Texts {
        let world = app.world_mut();
        world.query::<(Entity, &Shows, &Text)>().iter(world).map(|(e, v, t)| (e, v.0, t.0.clone())).collect()
    }

    fn of(list: &Texts, which: Value) -> String {
        list.iter().find(|(_, v, _)| *v == which).unwrap().2.clone()
    }

    #[test]
    fn the_gear_opens_one_settings_modal_on_the_first_page() {
        let mut app = app();
        open(&mut app);
        assert_eq!(modals(&mut app), 1);
        assert_eq!(shown(&mut app), [Page::General]);
        // Pressing the gear again doesn't stack another.
        open(&mut app);
        assert_eq!(modals(&mut app), 1);
    }

    #[test]
    fn escape_closes_it() {
        let mut app = app();
        open(&mut app);
        // With nothing focused, the press goes to the window.
        let window = app.world_mut().spawn_empty().id();
        let input = KeyboardInput {
            key_code: KeyCode::Escape,
            logical_key: Key::Escape,
            state: bevy::input::ButtonState::Pressed,
            text: None,
            repeat: false,
            window,
        };
        app.world_mut().trigger(FocusedInput::new(window, input, window));
        settle(&mut app);
        assert_eq!(modals(&mut app), 0);
    }

    #[test]
    fn each_tab_shows_its_page_and_only_it() {
        let mut app = app();
        open(&mut app);
        let world = app.world_mut();
        let strip = world.query_filtered::<Entity, With<SettingsStrip>>().single(world).unwrap();
        let tabs: Vec<(Entity, Page)> =
            world.query::<(Entity, &SettingsTab)>().iter(world).map(|(e, t)| (e, t.0)).collect();
        assert_eq!(tabs.len(), Page::ALL.len());
        for page in [Page::Factory, Page::Connection] {
            let tab = tabs.iter().find(|(_, p)| *p == page).unwrap().0;
            app.world_mut().trigger(ValueChange { source: strip, value: Some(tab), is_final: true });
            settle(&mut app);
            assert_eq!(shown(&mut app), [page]);
            assert_eq!(app.world().get::<SelectedTab>(strip), Some(&SelectedTab(Some(tab))));
        }
    }

    #[test]
    fn the_pages_say_what_the_app_knows_and_update_in_place() {
        let mut app = app();
        open(&mut app);
        let before = texts(&mut app);
        assert_eq!(of(&before, Value::Factory), "not connected");
        assert_eq!(of(&before, Value::Identity), "not signed in");
        assert_eq!(of(&before, Value::Status), "factory: connecting");
        assert_eq!(of(&before, Value::Repos), "none heard of yet");
        assert_eq!(of(&before, Value::Queue), "TRAIL-102 (not loaded)");

        app.world_mut().resource_mut::<Dialed>().0 = Some("abc123".into());
        app.world_mut().resource_mut::<LocalEndpoint>().0 = Some("me456".into());
        settle(&mut app);
        let after = texts(&mut app);
        assert_eq!(of(&after, Value::Factory), "abc123");
        assert_eq!(of(&after, Value::Endpoint), "me456");
        let entities = |list: &Texts| list.iter().map(|(e, ..)| *e).collect::<Vec<_>>();
        assert_eq!(entities(&before), entities(&after));
    }

    #[test]
    fn disconnect_sends_it_and_closes_the_modal() {
        let mut app = app();
        open(&mut app);
        let world = app.world_mut();
        let button = world
            .query::<(Entity, &SettingsAct)>()
            .iter(world)
            .find(|(_, a)| **a == SettingsAct::Disconnect)
            .unwrap()
            .0;
        app.world_mut().trigger(Activate { entity: button });
        // Drain before settling: messages live two updates, and settle runs more than that.
        let sent: Vec<_> = app.world_mut().resource_mut::<Messages<SendNet>>().drain().collect();
        settle(&mut app);
        assert_eq!(modals(&mut app), 0);
        assert!(matches!(sent.as_slice(), [SendNet(NetCommand::Disconnect)]));
    }

    #[test]
    fn disconnect_leaves_other_modals_alone() {
        let mut app = app();
        open(&mut app);
        let other = app.world_mut().spawn(ModalRoot).id();
        let world = app.world_mut();
        let button = world
            .query::<(Entity, &SettingsAct)>()
            .iter(world)
            .find(|(_, a)| **a == SettingsAct::Disconnect)
            .unwrap()
            .0;
        app.world_mut().trigger(Activate { entity: button });
        settle(&mut app);
        assert_eq!(modals(&mut app), 0);
        assert!(app.world().get_entity(other).is_ok());
    }

    #[test]
    fn the_queue_line_counts_what_is_under_it() {
        let task = |id: &str, key: &str, status: &str, parent: Option<&str>| -> Task {
            serde_json::from_value(task_json(json!({"id": id, "key": key, "status": status, "parent_id": parent})))
                .unwrap()
        };
        let tasks = [
            task("t1", "TRAIL-102", "open", None),
            task("t2", "TRAIL-103", "open", Some("t1")),
            task("t3", "TRAIL-104", "done", Some("t1")),
            task("t4", "TRAIL-9", "open", None),
        ];
        assert_eq!(queue_text("TRAIL-102", &tasks), "TRAIL-102: 1 not closed, 2 in all");
        assert_eq!(queue_text("TRAIL-5", &tasks), "TRAIL-5 (not loaded)");

        let authoring = Authoring::default();
        let mut status = FactoryStatus::default();
        assert_eq!(queues_text(&authoring, &status, &tasks), "TRAIL-102: 1 not closed, 2 in all");
        status.repos.insert(
            "r".into(),
            crate::factory::RepoState {
                control: trailhead_factory::status::ControlState::Up,
                queue: vec!["TRAIL-102".into(), "TRAIL-9".into()],
                last_error: None,
                human_grades: vec![],
                holding: None,
                seq: 0,
            },
        );
        assert_eq!(
            queues_text(&authoring, &status, &tasks),
            "TRAIL-102: 1 not closed, 2 in all\nTRAIL-9: 0 not closed, 0 in all"
        );
    }

    #[test]
    fn picking_a_theme_wears_it() {
        let mut app = app();
        open(&mut app);
        let pick = app.world_mut().query_filtered::<Entity, With<ThemePick>>().single(app.world()).unwrap();
        let light = ThemeChoice::ALL.iter().position(|c| *c == ThemeChoice::Light).unwrap();
        app.world_mut().trigger(Picked { entity: pick, index: light });
        settle(&mut app);
        assert_eq!(app.world().resource::<Appearance>().choice, ThemeChoice::Light);
    }

    #[test]
    fn a_theme_is_kept_as_a_word_and_anything_else_is_the_desktops() {
        for choice in ThemeChoice::ALL {
            assert_eq!(theme_from_word(theme_word(choice)), choice);
        }
        assert_eq!(theme_from_word("light\n"), ThemeChoice::Light);
        assert_eq!(theme_from_word("sepia"), ThemeChoice::Desktop);
        let missing = ThemeFile(std::env::temp_dir().join("lookout-no-such-theme-file"));
        assert_eq!(missing.load(), ThemeChoice::Desktop);
    }
}
