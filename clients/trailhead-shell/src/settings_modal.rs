//! The settings modal ([`crate::settings`]) drawn: the kit's settings tabs down its side, a heading over each group of
//! pages, and the page shown beside them. The modal follows [`SettingsShown`]: opened when it names a page, closed
//! when it names none. The page shown is spawned when its tab is picked, filled by its [`PageEntry`]'s system, and
//! despawned when another is picked; the tabs and the modal's frame stay.

use bevy::prelude::*;
use bevy::ui_widgets::{ControlOrientation, SelectedTab, ValueChange};
use trailhead_ui::{Modal, ModalSlots, UiPlace, open_modal, settings_page, spawn_settings_tabs, tab, tab_heading};

use crate::settings::{CloseSettings, OpenSettings, PageId, SettingsPages, SettingsShown};
use crate::settings_pages::SettingsPagesUiPlugin;

/// The settings modal.
#[derive(Component)]
pub struct SettingsModal;

impl Modal for SettingsModal {
    fn title(&self) -> String {
        "Settings".into()
    }
    // One size whichever page is shown, so turning a page doesn't resize the modal.
    fn tall(&self) -> bool {
        true
    }
    fn wide(&self) -> bool {
        true
    }
}

/// The strip of tabs down the modal's side.
#[derive(Component)]
pub struct SettingsStrip;

/// A page's tab.
#[derive(Component, Debug, Clone, Copy)]
pub struct SettingsTab(pub PageId);

/// Where the page shown goes.
#[derive(Component)]
pub struct SettingsPane;

/// The box of the page shown, and which page it is.
#[derive(Component, Debug, Clone, Copy)]
pub struct ShownPage(pub PageId);

pub(crate) struct SettingsModalUiPlugin;

impl Plugin for SettingsModalUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(SettingsPagesUiPlugin)
            .add_observer(on_tab)
            .add_observer(on_modal_gone)
            .add_systems(Update, (sync_modal, fill_frame, leave_page, enter_page, mark_tab).chain().before(UiPlace));
    }
}

/// Opens the modal when a page is to be shown, and closes it when none is.
fn sync_modal(shown: Res<SettingsShown>, modals: Query<Entity, With<SettingsModal>>, mut commands: Commands) {
    match (shown.page(), modals.iter().next()) {
        (Some(_), None) => {
            open_modal(&mut commands, SettingsModal);
        }
        (None, Some(modal)) => commands.entity(modal).despawn(),
        _ => {}
    }
}

/// However the modal went (its X, Escape, a click outside, a despawn), the settings are closed.
fn on_modal_gone(_: On<Remove<SettingsModal>>, mut commands: Commands) {
    commands.trigger(CloseSettings);
}

/// Fills a new modal's frame: the tabs, under a heading per group, and the pane the page goes in.
fn fill_frame(
    new: Query<&ModalSlots, (With<SettingsModal>, Added<ModalSlots>)>,
    pages: Res<SettingsPages>,
    mut commands: Commands,
) {
    for slots in &new {
        let tabs = spawn_settings_tabs(&mut commands, slots.body);
        commands.entity(tabs.strip).insert(SettingsStrip);
        commands.entity(tabs.pane).insert(SettingsPane);
        let mut group = None;
        for page in pages.iter() {
            if group != Some(page.group) {
                group = Some(page.group);
                commands.spawn_scene(tab_heading(page.group)).insert(ChildOf(tabs.strip));
            }
            commands
                .spawn_scene(tab(page.title, ControlOrientation::Vertical))
                .insert((SettingsTab(page.id), ChildOf(tabs.strip)));
        }
    }
}

/// Takes down the page that was shown once another is picked.
fn leave_page(shown: Res<SettingsShown>, holders: Query<(Entity, &ShownPage)>, mut commands: Commands) {
    for (holder, ShownPage(page)) in &holders {
        if shown.page() != Some(*page) {
            commands.entity(holder).despawn();
        }
    }
}

/// Puts up the page picked, once the modal's pane is there: its box, filled by the page's own system.
fn enter_page(
    shown: Res<SettingsShown>,
    panes: Query<Entity, With<SettingsPane>>,
    holders: Query<(), With<ShownPage>>,
    pages: Res<SettingsPages>,
    mut commands: Commands,
) {
    let (Some(page), Ok(pane)) = (shown.page(), panes.single()) else { return };
    if !holders.is_empty() {
        return;
    }
    let Some(entry) = pages.get(page) else { return };
    let holder =
        commands.spawn_scene(settings_page()).insert((Name::new(entry.title), ShownPage(page), ChildOf(pane))).id();
    commands.run_system_with(entry.fill, holder);
}

/// Marks the tab of the page shown, in place.
fn mark_tab(
    shown: Res<SettingsShown>,
    mut strips: Query<(Entity, &SelectedTab), With<SettingsStrip>>,
    tabs: Query<(Entity, &SettingsTab)>,
    mut commands: Commands,
) {
    let Ok((strip, selected)) = strips.single_mut() else { return };
    let wanted = tabs.iter().find(|(_, t)| Some(t.0) == shown.page()).map(|(e, _)| e);
    if selected.0 != wanted {
        commands.entity(strip).insert(SelectedTab(wanted));
    }
}

/// A tab picked turns to its page.
fn on_tab(
    change: On<ValueChange<Option<Entity>>>,
    strips: Query<(), With<SettingsStrip>>,
    tabs: Query<&SettingsTab>,
    mut commands: Commands,
) {
    if !strips.contains(change.source) {
        return;
    }
    if let Some(SettingsTab(page)) = change.value.and_then(|t| tabs.get(t).ok()) {
        commands.trigger(OpenSettings { page: Some(*page) });
    }
}

#[cfg(test)]
mod tests {
    use bevy::input::ButtonState;
    use bevy::input::keyboard::{Key, KeyboardInput};
    use bevy::input_focus::FocusedInput;
    use bevy::ui_widgets::Activate;
    use trailhead_ui::{Appearance, ButtonGroup, ModalPlugin, ModalRoot, Picked, TextSize, ThemeChoice, button};

    use super::*;
    use crate::connection::{Connection, ConnectionStatus, Dialed, LocalEndpoint, SendNet};
    use crate::net::{NetCommand, Reason};
    use crate::settings::{
        ABOUT, APPEARANCE, CONNECTION, ConnectionAct, SettingsGear, SettingsPage, Shows, TextSizePick, ThemePick,
    };
    use crate::testing::{settle, windowed};
    use crate::{ActiveApp, AppId, Manifest, SettingsButton, ShellApp, ShellPlugin};

    const NOTES: AppId = AppId("notes");
    const TIDY: PageId = PageId::new(NOTES, "tidy");

    /// An app with a settings page holding one button.
    struct Notes;

    #[derive(Component, Clone, Default)]
    struct TidyButton;

    impl ShellApp for Notes {
        fn manifest(&self) -> Manifest {
            Manifest { id: NOTES, name: "Notes", icon: trailhead_ui::Icon::Flag }
        }

        fn build(&self, _: &mut App) {}

        fn settings_pages(&self) -> Vec<SettingsPage> {
            vec![SettingsPage::new(TIDY.key, "Tidying", |In(page): In<Entity>, mut commands: Commands| {
                commands
                    .spawn_scene(bsn! { @button("Tidy now", None, trailhead_ui::ButtonKind::Normal) TidyButton })
                    .insert(ChildOf(page));
            })]
        }
    }

    fn app() -> App {
        let mut app = windowed(ShellPlugin::new().with(Notes).with(crate::testing::Other));
        app.insert_resource(Appearance::new(ThemeChoice::Desktop)).add_plugins(ModalPlugin);
        settle(&mut app);
        app
    }

    fn open(app: &mut App, page: Option<PageId>) {
        app.world_mut().trigger(OpenSettings { page });
        settle(app);
    }

    fn one<F: bevy::ecs::query::QueryFilter>(app: &mut App) -> Entity {
        app.world_mut().query_filtered::<Entity, F>().single(app.world()).unwrap()
    }

    fn count<F: bevy::ecs::query::QueryFilter>(app: &mut App) -> usize {
        app.world_mut().query_filtered::<(), F>().iter(app.world()).count()
    }

    fn shown(app: &mut App) -> Vec<PageId> {
        app.world_mut().query::<&ShownPage>().iter(app.world()).map(|p| p.0).collect()
    }

    fn picked(app: &mut App) -> Option<PageId> {
        let strip = one::<With<SettingsStrip>>(app);
        let tab = app.world().get::<SelectedTab>(strip).unwrap().0?;
        app.world().get::<SettingsTab>(tab).map(|t| t.0)
    }

    fn entities(app: &mut App) -> Vec<Entity> {
        let mut all: Vec<Entity> = app.world_mut().query::<Entity>().iter(app.world()).collect();
        all.sort();
        all
    }

    fn pick<M: Component>(app: &mut App, index: usize) {
        let group = one::<With<M>>(app);
        app.world_mut().trigger(Picked { entity: group, index });
        settle(app);
    }

    fn chosen<M: Component>(app: &mut App) -> Option<usize> {
        let group = one::<With<M>>(app);
        app.world().get::<ButtonGroup>(group).unwrap().chosen
    }

    fn act(app: &mut App, wanted: ConnectionAct) -> Entity {
        let mut acts = app.world_mut().query::<(Entity, &ConnectionAct)>();
        acts.iter(app.world()).find(|(_, a)| **a == wanted).map(|(e, _)| e).unwrap()
    }

    fn text_of(app: &mut App, which: Shows) -> String {
        let mut texts = app.world_mut().query::<(&Shows, &Text)>();
        texts.iter(app.world()).find(|(s, _)| **s == which).map(|(_, t)| t.0.clone()).unwrap()
    }

    #[test]
    fn the_shells_pages_come_first_then_each_apps_under_its_name() {
        let mut app = app();
        let pages = app.world().resource::<SettingsPages>();
        let listed: Vec<(&str, &str)> = pages.iter().map(|p| (p.group, p.title)).collect();
        assert_eq!(
            listed,
            [
                ("Trailhead", "Appearance"),
                ("Trailhead", "Connection"),
                ("Trailhead", "Server"),
                ("Trailhead", "About"),
                ("Notes", "Tidying")
            ]
        );
        open(&mut app, None);
        let strip = one::<With<SettingsStrip>>(&mut app);
        let words: Vec<String> = app
            .world()
            .get::<Children>(strip)
            .unwrap()
            .iter()
            .map(|child| {
                let text = |e: Entity| app.world().get::<Text>(e).map(|t| t.0.clone());
                text(child).or_else(|| {
                    let tab = app.world().get::<SettingsTab>(child)?;
                    Some(format!("tab {}", tab.0.key))
                })
            })
            .map(Option::unwrap)
            .collect();
        assert_eq!(
            words,
            ["Trailhead", "tab appearance", "tab connection", "tab server", "tab about", "Notes", "tab tidy"]
        );
    }

    #[test]
    fn it_opens_at_the_page_asked_for_with_that_tab_picked_and_one_modal() {
        let mut app = app();
        assert_eq!(count::<With<SettingsModal>>(&mut app), 0);
        open(&mut app, Some(ABOUT));
        assert_eq!((count::<With<SettingsModal>>(&mut app), shown(&mut app)), (1, vec![ABOUT]));
        assert_eq!(picked(&mut app), Some(ABOUT));
        assert_eq!(text_of(&mut app, Shows::Build), "Run directly, not by the launcher");
        assert_eq!(text_of(&mut app, Shows::Apps), "Notes (notes)\nOther (other)");
        open(&mut app, Some(CONNECTION));
        assert_eq!((count::<With<SettingsModal>>(&mut app), shown(&mut app)), (1, vec![CONNECTION]), "no second modal");
    }

    #[test]
    fn a_page_nobody_added_opens_the_first_and_no_page_opens_the_last_one_shown() {
        let mut app = app();
        open(&mut app, Some(PageId::new(NOTES, "gone")));
        assert_eq!(shown(&mut app), [APPEARANCE]);
        open(&mut app, Some(TIDY));
        app.world_mut().trigger(CloseSettings);
        settle(&mut app);
        assert_eq!(count::<With<SettingsModal>>(&mut app), 0);
        open(&mut app, None);
        assert_eq!(shown(&mut app), [TIDY]);
    }

    #[test]
    fn the_gear_and_ctrl_comma_open_it() {
        let mut app = app();
        let gear = one::<With<SettingsButton>>(&mut app);
        assert!(app.world().get::<SettingsGear>(gear).is_some());
        app.world_mut().trigger(Activate { entity: gear });
        settle(&mut app);
        assert_eq!(shown(&mut app), [APPEARANCE]);
        app.world_mut().trigger(CloseSettings);
        settle(&mut app);

        app.init_resource::<ButtonInput<KeyCode>>();
        let window = app.world_mut().spawn(Window::default()).id();
        let press = |app: &mut App| {
            let input = KeyboardInput {
                key_code: KeyCode::Comma,
                logical_key: Key::Character(",".into()),
                state: ButtonState::Pressed,
                text: None,
                repeat: false,
                window,
            };
            app.world_mut().trigger(FocusedInput::new(window, input, window));
            settle(app);
        };
        press(&mut app);
        assert_eq!(count::<With<SettingsModal>>(&mut app), 0, "not without the modifier");
        let command = if cfg!(target_os = "macos") { KeyCode::SuperLeft } else { KeyCode::ControlLeft };
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(command);
        press(&mut app);
        assert_eq!(count::<With<SettingsModal>>(&mut app), 1);
    }

    #[test]
    fn turning_a_page_swaps_only_the_page_and_keeps_the_frame_and_the_tabs() {
        let mut app = app();
        open(&mut app, Some(APPEARANCE));
        let frame = |app: &mut App| {
            let mut kept = vec![one::<With<SettingsModal>>(app), one::<With<SettingsStrip>>(app)];
            kept.push(one::<With<SettingsPane>>(app));
            kept.extend(app.world_mut().query_filtered::<Entity, With<SettingsTab>>().iter(app.world()));
            kept
        };
        let before = frame(&mut app);
        let page = one::<With<ShownPage>>(&mut app);
        let strip = one::<With<SettingsStrip>>(&mut app);
        let about = app.world_mut().query::<(Entity, &SettingsTab)>().iter(app.world()).find(|(_, t)| t.0 == ABOUT);
        let about = about.unwrap().0;
        app.world_mut().trigger(ValueChange { source: strip, value: Some(about), is_final: true });
        settle(&mut app);
        assert_eq!(frame(&mut app), before);
        assert_eq!((shown(&mut app), picked(&mut app)), (vec![ABOUT], Some(ABOUT)));
        assert!(app.world().get_entity(page).is_err(), "the page left is despawned");
        assert_eq!(count::<With<ThemePick>>(&mut app), 0, "and what was on it");
    }

    #[test]
    fn an_apps_page_is_listed_and_filled_by_its_own_system_when_shown() {
        let mut app = app();
        open(&mut app, Some(APPEARANCE));
        assert_eq!(count::<With<TidyButton>>(&mut app), 0, "filled only when shown");
        open(&mut app, Some(TIDY));
        let button = one::<With<TidyButton>>(&mut app);
        let holder = one::<With<ShownPage>>(&mut app);
        let parent = app.world().get::<ChildOf>(button).unwrap().parent();
        assert_eq!(parent, holder);
        assert_eq!(picked(&mut app), Some(TIDY));
    }

    #[test]
    fn the_text_size_buttons_show_the_size_and_set_it() {
        let mut app = app();
        open(&mut app, Some(APPEARANCE));
        assert_eq!(chosen::<TextSizePick>(&mut app), Some(TextSize::DEFAULT.step()));
        let at_150 = TextSize::nearest(150).step();
        pick::<TextSizePick>(&mut app, at_150);
        assert_eq!(app.world().resource::<TextSize>().label(), "150%");
        // The keys, elsewhere, show on the buttons in place.
        let group = one::<With<TextSizePick>>(&mut app);
        app.world_mut().trigger(trailhead_ui::ChangeTextSize::Reset);
        settle(&mut app);
        assert_eq!((one::<With<TextSizePick>>(&mut app), chosen::<TextSizePick>(&mut app)), (group, Some(2)));
        assert_eq!(
            app.world().get::<ButtonGroup>(group).unwrap().chosen.and_then(TextSize::at),
            Some(TextSize::DEFAULT)
        );
    }

    #[test]
    fn picking_a_theme_wears_it_and_the_window_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = crate::ThemeFile(dir.path().join("theme"));
        let mut app = app();
        app.insert_resource(file.clone()).add_systems(Update, crate::window::keep_theme);
        open(&mut app, Some(APPEARANCE));
        assert_eq!(chosen::<ThemePick>(&mut app), ThemeChoice::ALL.iter().position(|c| *c == ThemeChoice::Desktop));
        let light = ThemeChoice::ALL.iter().position(|c| *c == ThemeChoice::Light).unwrap();
        pick::<ThemePick>(&mut app, light);
        assert_eq!(app.world().resource::<Appearance>().choice, ThemeChoice::Light);
        assert_eq!(file.load(), ThemeChoice::Light);
    }

    #[test]
    fn the_connection_page_says_what_the_shell_knows_in_place_and_redials() {
        let mut app = app();
        open(&mut app, Some(CONNECTION));
        assert_eq!(text_of(&mut app, Shows::Server), "none dialed");
        assert_eq!(text_of(&mut app, Shows::State), "Offline");
        assert_eq!(text_of(&mut app, Shows::Endpoint), "starting");
        let redial = act(&mut app, ConnectionAct::Redial);
        assert!(app.world().get::<bevy::ui::InteractionDisabled>(redial).is_some(), "nothing to re-dial");
        let page = entities(&mut app);

        let world = app.world_mut();
        world.resource_mut::<Dialed>().0 = Some("7204d50f".into());
        world.resource_mut::<LocalEndpoint>().0 = Some("me456".into());
        world.resource_mut::<ConnectionStatus>().reason = Some(Reason::NotAllowed);
        settle(&mut app);
        assert_eq!(text_of(&mut app, Shows::Server), "7204d50f");
        assert!(text_of(&mut app, Shows::State).starts_with("Offline: your Trailhead server doesn't allow this app"));
        assert_eq!(text_of(&mut app, Shows::Allow), "trailhead factory allow me456");
        assert_eq!(entities(&mut app), page, "updated in place");
        assert!(app.world().get::<bevy::ui::InteractionDisabled>(redial).is_none());

        app.world_mut().trigger(Activate { entity: redial });
        let sent: Vec<_> = app.world_mut().resource_mut::<Messages<SendNet>>().drain().collect();
        assert!(matches!(sent.as_slice(), [SendNet(NetCommand::Connect(t))] if t == "7204d50f"));
        crate::testing::set_connection(&mut app, Connection::Live);
        assert_eq!(text_of(&mut app, Shows::State), "Live");
    }

    #[test]
    fn disconnect_sends_it_and_closes_the_settings() {
        let mut app = app();
        open(&mut app, Some(CONNECTION));
        let disconnect = act(&mut app, ConnectionAct::Disconnect);
        app.world_mut().trigger(Activate { entity: disconnect });
        let sent: Vec<_> = app.world_mut().resource_mut::<Messages<SendNet>>().drain().collect();
        settle(&mut app);
        assert!(matches!(sent.as_slice(), [SendNet(NetCommand::Disconnect)]));
        assert_eq!(count::<With<SettingsModal>>(&mut app), 0);
    }

    #[test]
    fn closing_leaves_the_window_as_it_was_before_opening() {
        let mut app = app();
        let before = entities(&mut app);
        open(&mut app, Some(TIDY));
        open(&mut app, Some(CONNECTION));
        let modal = one::<With<ModalRoot>>(&mut app);
        app.world_mut().trigger(bevy::ui_widgets::RequestClose { source: modal });
        settle(&mut app);
        assert_eq!(app.world().resource::<SettingsShown>().page(), None);
        assert_eq!(entities(&mut app), before);
        // And it opens again after a close it didn't make itself.
        open(&mut app, None);
        assert_eq!(shown(&mut app), [CONNECTION]);
        let modal = one::<With<SettingsModal>>(&mut app);
        app.world_mut().despawn(modal);
        settle(&mut app);
        assert_eq!(entities(&mut app), before);
    }

    #[test]
    fn switching_apps_leaves_the_settings_open_at_their_page() {
        let mut app = app();
        open(&mut app, Some(TIDY));
        crate::testing::show(&mut app, crate::testing::OTHER);
        assert_eq!(app.world().resource::<State<ActiveApp>>().get().0, crate::testing::OTHER);
        assert_eq!(shown(&mut app), [TIDY]);
    }
}
