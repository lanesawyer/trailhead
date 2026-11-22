//! The rail of apps down the left edge of the window: a button for each app, the one shown marked, a press switching to
//! it ([`crate::transition`]), dragged (or moved with Alt and Up or Down) to put the apps in the person's order
//! ([`crate::MoveApp`]), under them a plus that asks for a new app, and at its foot the window's own buttons: Ask, which
//! asks for a change to the app shown. Both ([`crate::ask`]) show while connected to a server with an apps repo (Ask
//! only once there is an app to ask about) and open the form ([`crate::ask_panel`]); then the update notice ([`crate::update`]) while a newer
//! build is ready, whose button opens the panel saying what it brings ([`crate::update_panel`]); and last the gear,
//! which opens the window's settings ([`crate::settings`]). A shell with a single app has no rail, and the app has the
//! whole window.

use bevy::prelude::*;
use bevy::ui_widgets::Activate;
use trailhead_ui::{
    ButtonKind, Hint, Icon, NavItem, ReorderItem, ReorderList, Reordered, rail, rail_foot, rail_item, set_display,
    tool_button,
};

use crate::ask::Askable;
use crate::connection::Connection;
use crate::settings::SettingsGear;
use crate::transition::{SwitchApp, Switching};
use crate::update::UpdateReady;
use crate::{ActiveApp, AppId, Apps, MoveApp};

/// The rail's root.
#[derive(Component)]
pub struct Rail;

/// On an app's button on the rail: the app a press shows.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenApp(pub AppId);

/// The rail's foot, holding the window's own buttons.
#[derive(Component)]
pub struct RailFoot;

/// The button at the [`RailFoot`] that opens the form asking for a change to the app shown, shown while the connection
/// is live and the server has an apps repo with an app in it to ask about.
#[derive(Component)]
pub struct AskButton;

/// The plus under the apps on the rail that opens the form asking for a new app, shown while the connection is live and
/// the server has an apps repo.
#[derive(Component)]
pub struct NewAppButton;

/// The button at the [`RailFoot`] that opens the update's panel, shown while a newer build is ready, its hint saying
/// how many changes it brings.
#[derive(Component)]
pub struct UpdateButton;

/// The gear at the [`RailFoot`], under Ask and the update notice, that opens the settings.
#[derive(Component)]
pub struct SettingsButton;

/// What the Ask button's hint says.
pub const ASK_HINT: &str = "Ask for a change to this app";

/// What the plus's hint says.
pub const NEW_APP_HINT: &str = "Ask for a new app";

pub(crate) struct RailUiPlugin;

impl Plugin for RailUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_rail)
            .add_observer(on_open)
            .add_observer(on_reordered)
            .add_systems(Update, (order_rail, mark_shown, show_update, show_ask));
    }
}

/// The rail, its items in the order of the apps, a list the person can reorder, then the plus asking for a new app
/// and the foot with Ask, the update notice and the gear, which aren't part of the list. Each is put down at once (`spawn_scene`), so the foot comes after
/// the items and its buttons' nodes are there to hide before the first frame is drawn.
fn spawn_rail(mut commands: Commands, apps: Res<Apps>) {
    if !apps.has_rail() {
        return;
    }
    let list = ReorderList::vertical();
    let root = commands.spawn_scene(rail()).insert((Name::new("rail"), Rail, list)).id();
    for manifest in apps.iter() {
        let hint = format!("{} ({} moves it)", manifest.name, list.keys());
        let item = (OpenApp(manifest.id), ReorderItem(manifest.id.0.to_string()), ChildOf(root));
        commands.spawn_scene(rail_item(manifest.icon, hint)).insert(item);
    }
    let plus = bsn! {
        @tool_button(Icon::Plus, NEW_APP_HINT, ButtonKind::Plain) Node { flex_shrink: 0.0, display: Display::None }
    };
    commands.spawn_scene(plus).insert((Name::new("new app"), NewAppButton, ChildOf(root)));
    let foot = commands.spawn_scene(rail_foot()).insert((Name::new("rail foot"), RailFoot, ChildOf(root))).id();
    let ask = bsn! { @tool_button(Icon::Pencil, ASK_HINT, ButtonKind::Normal) Node { display: Display::None } };
    commands.spawn_scene(ask).insert((AskButton, ChildOf(foot)));
    let update =
        bsn! { @tool_button(Icon::Refresh, "Update ready", ButtonKind::Primary) Node { display: Display::None } };
    commands.spawn_scene(update).insert((UpdateButton, ChildOf(foot)));
    let gear = tool_button(Icon::Settings, "Settings", ButtonKind::Plain);
    commands.spawn_scene(gear).insert((SettingsButton, SettingsGear, ChildOf(foot)));
}

/// Shows the plus while the connection is live and the server has an apps repo, and Ask while it also has an app to
/// ask about.
fn show_ask(
    askable: Option<Res<Askable>>,
    connection: Option<Res<State<Connection>>>,
    changes: Query<Entity, With<AskButton>>,
    pluses: Query<Entity, With<NewAppButton>>,
    mut nodes: Query<&mut Node>,
) {
    let live = connection.is_some_and(|c| *c.get() == Connection::Live);
    let askable = askable.filter(|a| live && a.configured());
    let change = askable.as_ref().is_some_and(|a| !a.targets().is_empty());
    for (button, shown) in changes.iter().map(|b| (b, change)).chain(pluses.iter().map(|b| (b, askable.is_some()))) {
        set_display(&mut nodes, button, shown);
    }
}

/// Shows the update button while a newer build is ready, and says on it how many changes it brings, in place. A shell
/// the launcher didn't start never shows it.
fn show_update(
    ready: Option<Res<UpdateReady>>,
    buttons: Query<Entity, With<UpdateButton>>,
    mut nodes: Query<&mut Node>,
    mut hints: Query<&mut Hint, With<UpdateButton>>,
) {
    let newer = ready.as_ref().and_then(|r| r.0.as_ref());
    for button in &buttons {
        set_display(&mut nodes, button, newer.is_some());
    }
    if let Some(newer) = newer.filter(|_| ready.as_ref().is_some_and(|r| r.is_changed())) {
        let words = newer.hint();
        for mut hint in &mut hints {
            if hint.0 != words {
                hint.0.clone_from(&words);
            }
        }
    }
}

fn on_open(activate: On<Activate>, items: Query<&OpenApp>, mut commands: Commands) {
    if let Ok(OpenApp(id)) = items.get(activate.entity) {
        commands.trigger(SwitchApp(*id));
    }
}

/// An app dragged to another place on the rail, or moved there with Alt and an arrow.
fn on_reordered(moved: On<Reordered>, rails: Query<(), With<Rail>>, apps: Res<Apps>, mut commands: Commands) {
    if !rails.contains(moved.list) {
        return;
    }
    if let Some(manifest) = apps.iter().find(|m| m.id.0 == moved.key) {
        commands.trigger(MoveApp { app: manifest.id, to: moved.to });
    }
}

/// Puts the rail's buttons in the order of the apps, in place, the foot staying last, in the frame they are moved.
fn order_rail(
    apps: Res<Apps>,
    rails: Query<(Entity, &Children), With<Rail>>,
    items: Query<&OpenApp>,
    mut commands: Commands,
) {
    if !apps.is_changed() {
        return;
    }
    for (rail, children) in &rails {
        let mut wanted: Vec<Entity> = children.iter().collect();
        let place = |e: &Entity| items.get(*e).ok().and_then(|open| apps.position(open.0));
        wanted.sort_by_key(|e| place(e).unwrap_or(usize::MAX));
        if !wanted.iter().copied().eq(children.iter()) {
            commands.entity(rail).replace_children(&wanted);
        }
    }
}

/// Marks the button of the app shown, in place: the one being switched to as soon as the switch starts.
fn mark_shown(active: Res<State<ActiveApp>>, switching: Res<Switching>, mut items: Query<(&OpenApp, &mut NavItem)>) {
    let shown = switching.target().unwrap_or(active.get().0);
    for (open, mut item) in &mut items {
        item.set_if_neq(NavItem { current: open.0 == shown });
    }
}

#[cfg(test)]
mod tests {
    use bevy::state::app::StatesPlugin;

    use super::*;
    use crate::ShellPlugin;
    use crate::tests::{Fake, MAPS, NOTES, Seen};
    use crate::update::Newer;

    fn windowed(shell: ShellPlugin) -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin, AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .add_plugins(shell.windowed());
        for _ in 0..3 {
            app.update();
        }
        app
    }

    fn items(app: &mut App) -> Vec<(Entity, AppId, bool)> {
        let world = app.world_mut();
        world.query::<(Entity, &OpenApp, &NavItem)>().iter(world).map(|(e, o, n)| (e, o.0, n.current)).collect()
    }

    #[test]
    fn each_app_has_a_button_and_pressing_one_shows_that_app_and_marks_it() {
        let mut app = windowed(ShellPlugin::new().with(Fake(NOTES)).with(Fake(MAPS)));
        app.insert_resource(crate::transition::AppTransition(crate::transition::TransitionKind::None));
        let before = items(&mut app);
        assert_eq!(
            before.iter().map(|(_, id, current)| (*id, *current)).collect::<Vec<_>>(),
            [(NOTES, true), (MAPS, false)]
        );
        let maps = before[1].0;
        app.world_mut().trigger(Activate { entity: maps });
        app.update();
        app.update();
        assert_eq!(app.world().resource::<State<ActiveApp>>().get().0, MAPS);
        let after = items(&mut app);
        assert_eq!(
            after.iter().map(|(e, _, current)| (*e, *current)).collect::<Vec<_>>(),
            [(before[0].0, false), (maps, true)]
        );
        let seen = &app.world().resource::<Seen>().0;
        assert!(seen.contains(&"draws notes".to_string()) && seen.contains(&"draws maps".to_string()), "{seen:?}");
    }

    #[test]
    fn a_shell_with_one_app_has_no_rail() {
        let mut app = windowed(ShellPlugin::new().with(Fake(NOTES)));
        let world = app.world_mut();
        assert_eq!(world.query::<&Rail>().iter(world).count(), 0);
    }

    fn shown<M: Component>(app: &mut App) -> (Entity, Display) {
        let world = app.world_mut();
        world.query_filtered::<(Entity, &Node), With<M>>().single(world).map(|(e, n)| (e, n.display)).unwrap()
    }

    fn hint(app: &mut App) -> (Entity, String) {
        let world = app.world_mut();
        world
            .query_filtered::<(Entity, &Hint), With<UpdateButton>>()
            .single(world)
            .map(|(e, h)| (e, h.0.clone()))
            .unwrap()
    }

    fn foot(app: &mut App) -> (Entity, Vec<Entity>) {
        let world = app.world_mut();
        let foot = world.query_filtered::<Entity, With<RailFoot>>().single(world).unwrap();
        (foot, world.get::<Children>(foot).unwrap().iter().collect())
    }

    #[test]
    fn the_update_button_sits_at_the_rails_foot_and_shows_in_place_saying_how_many_changes() {
        let mut app = windowed(ShellPlugin::new().with(Fake(NOTES)).with(Fake(MAPS)).built_as(Some("a".into())));
        let (button, display) = shown::<UpdateButton>(&mut app);
        assert_eq!(display, Display::None, "hidden while this is the newest build");
        let rail = app.world_mut().query_filtered::<Entity, With<Rail>>().single(app.world()).unwrap();
        let children: Vec<Entity> = app.world().get::<Children>(rail).unwrap().iter().collect();
        let (foot, buttons) = foot(&mut app);
        let (plus, _) = shown::<NewAppButton>(&mut app);
        assert_eq!(children[children.len() - 2..], [plus, foot], "the plus under the apps, then the foot");
        let (ask, _) = shown::<AskButton>(&mut app);
        let (gear, display) = shown::<SettingsButton>(&mut app);
        assert_eq!(buttons, [ask, button, gear], "Ask above the update, and the gear last");
        assert_eq!(display, Display::Flex, "the gear always shows");

        let subjects = ["feat(a): one".to_string(), "fix(a): two".to_string(), "chore: three".to_string()];
        app.world_mut().resource_mut::<UpdateReady>().0 = Some(Newer::new("b", &subjects, 3));
        app.update();
        assert_eq!(shown::<UpdateButton>(&mut app), (button, Display::Flex), "the same entity, shown");
        assert_eq!(hint(&mut app), (button, "Update ready: 3 changes".into()));

        app.world_mut().resource_mut::<UpdateReady>().0 = Some(Newer::new("c", &[], 0));
        app.update();
        assert_eq!(hint(&mut app), (button, "Update ready".into()), "an older server says no changes");
    }

    #[test]
    fn the_plus_shows_while_live_with_an_apps_repo_and_ask_once_it_has_an_app() {
        use bevy::state::app::AppExtStates;
        use trailhead_model::shell::{AskTarget, AskTargets};

        let mut app = windowed(ShellPlugin::new().with(Fake(NOTES)).with(Fake(MAPS)));
        app.init_state::<Connection>();
        app.update();
        let (ask, display) = shown::<AskButton>(&mut app);
        let (plus, plus_display) = shown::<NewAppButton>(&mut app);
        assert_eq!((display, plus_display), (Display::None, Display::None));
        assert_eq!(app.world().get::<Hint>(ask).map(|h| h.0.as_str()), Some(ASK_HINT));
        assert_eq!(app.world().get::<Hint>(plus).map(|h| h.0.as_str()), Some(NEW_APP_HINT));
        let empty = AskTargets { configured: true, ..default() };
        app.world_mut().resource_mut::<Askable>().0 = Some(empty.clone());
        app.update();
        assert_eq!(shown::<NewAppButton>(&mut app).1, Display::None, "not while offline");
        app.world_mut().resource_mut::<NextState<Connection>>().set(Connection::Live);
        app.update();
        app.update();
        assert_eq!(shown::<NewAppButton>(&mut app), (plus, Display::Flex));
        assert_eq!(shown::<AskButton>(&mut app), (ask, Display::None), "no app to ask about yet");
        let target = AskTarget { project: "NOTE".into(), name: "Notes".into(), path: Some("apps/notes".into()) };
        app.world_mut().resource_mut::<Askable>().0 = Some(AskTargets { targets: vec![target], ..empty });
        app.update();
        assert_eq!(shown::<AskButton>(&mut app), (ask, Display::Flex));
        app.world_mut().resource_mut::<Askable>().0 = Some(AskTargets::default());
        app.update();
        assert_eq!(shown::<AskButton>(&mut app), (ask, Display::None), "not without an apps repo");
        assert_eq!(shown::<NewAppButton>(&mut app), (plus, Display::None));
    }

    mod reorder {
        use std::time::Duration;

        use bevy::camera::NormalizedRenderTarget;
        use bevy::input::ButtonState;
        use bevy::input::keyboard::{Key, KeyboardInput};
        use bevy::input_focus::FocusedInput;
        use bevy::picking::backend::HitData;
        use bevy::picking::events::{
            Pointer, PointerClick, PointerDrag, PointerDragEnd, PointerDragStart, PointerPress, PointerRelease,
        };
        use bevy::picking::pointer::{Location, PointerId};
        use trailhead_ui::{Lifted, ReorderPlugin, TextSize};

        use super::*;
        use crate::LocalStore;
        use crate::transition::{AppTransition, TransitionKind};

        const TREES: AppId = AppId("trees");
        const LAKES: AppId = AppId("lakes");
        /// The UI's scale: these tests drag at 150%.
        const UI_SCALE: f32 = 1.5;
        /// A rail item, the gap between them and the rail's inset at its top, in UI pixels.
        const ITEM: f32 = 24.0;
        const GAP: f32 = 8.0;
        const INSET: f32 = 12.0;

        fn shell(dir: &std::path::Path) -> ShellPlugin {
            ShellPlugin::new()
                .with(Fake(NOTES))
                .with(Fake(MAPS))
                .with(Fake(TREES))
                .with(Fake(LAKES))
                .store_in(dir.to_path_buf())
        }

        /// A windowed shell of four apps with the kit's reordering and buttons, switching at once, at 150%.
        fn rail_app(shell: ShellPlugin) -> App {
            let mut app = windowed(shell);
            app.add_plugins((ReorderPlugin, bevy::ui_widgets::ButtonPlugin))
                .init_resource::<ButtonInput<KeyCode>>()
                .insert_resource(AppTransition(TransitionKind::None))
                .insert_resource(TextSize::nearest(150));
            app.update();
            app
        }

        fn ids(app: &App) -> Vec<AppId> {
            app.world().resource::<Apps>().iter().map(|m| m.id).collect()
        }

        /// The rail's app buttons, top to bottom.
        fn buttons(app: &mut App) -> Vec<(Entity, AppId)> {
            let world = app.world_mut();
            let rail = world.query_filtered::<&Children, With<Rail>>().single(world).unwrap();
            let children: Vec<Entity> = rail.iter().collect();
            children.into_iter().filter_map(|e| world.get::<OpenApp>(e).map(|o| (e, o.0))).collect()
        }

        /// Lays the rail out as the window would, with no layout running: each node's place on screen.
        fn lay_out(app: &mut App) {
            let place = |world: &mut World, entity: Entity, rect: Rect| {
                let node =
                    ComputedNode { size: rect.size() * UI_SCALE, inverse_scale_factor: 1.0 / UI_SCALE, ..default() };
                world.entity_mut(entity).insert((node, UiGlobalTransform::from_translation(rect.center() * UI_SCALE)));
            };
            let items = buttons(app);
            let world = app.world_mut();
            let rail = world.query_filtered::<Entity, With<Rail>>().single(world).unwrap();
            place(world, rail, Rect::new(0.0, 0.0, 48.0, 480.0));
            for (at, (item, _)) in items.into_iter().enumerate() {
                let top = INSET + at as f32 * (ITEM + GAP);
                place(world, item, Rect::new(12.0, top, 12.0 + ITEM, top + ITEM));
            }
        }

        /// The middle of the button `at` from the top, in UI pixels.
        fn middle(at: usize) -> Vec2 {
            Vec2::new(24.0, INSET + ITEM / 2.0 + at as f32 * (ITEM + GAP))
        }

        fn pointer(ui: Vec2) -> Pointer {
            let target = NormalizedRenderTarget::None { width: 1, height: 1 };
            Pointer::new(PointerId::Mouse, Location { target, position: ui * UI_SCALE })
        }

        fn hit() -> HitData {
            HitData::new(Entity::PLACEHOLDER, 0.0, None, None)
        }

        /// Presses the button `at` and drags it `by` UI pixels, as picking reports it.
        fn drag(app: &mut App, at: usize, by: Vec2) -> Entity {
            lay_out(app);
            let item = buttons(app)[at].0;
            let (from, button) = (middle(at), PointerButton::Primary);
            let world = app.world_mut();
            world.trigger(PointerPress { entity: item, pointer: pointer(from), button, hit: hit(), count: 1 });
            world.flush();
            world.trigger(PointerDragStart { entity: item, pointer: pointer(from), button, hit: hit() });
            let distance = by * UI_SCALE;
            world.trigger(PointerDrag { entity: item, pointer: pointer(from + by), button, distance, delta: distance });
            world.flush();
            app.update();
            item
        }

        /// Lets the button go where it was dragged, over it still, as picking reports a release.
        fn release(app: &mut App, item: Entity, at: usize, by: Vec2) {
            let (to, button) = (pointer(middle(at) + by), PointerButton::Primary);
            let world = app.world_mut();
            let duration = Duration::from_millis(300);
            world.trigger(PointerClick { entity: item, pointer: to.clone(), button, hit: hit(), duration, count: 1 });
            world.trigger(PointerRelease { entity: item, pointer: to.clone(), button, hit: hit() });
            world.trigger(PointerDragEnd { entity: item, pointer: to, button, distance: by * UI_SCALE });
            world.flush();
            app.update();
            app.update();
        }

        fn press_key(app: &mut App, target: Entity, key_code: KeyCode, logical_key: Key) {
            let window = app.world_mut().spawn(Window::default()).id();
            let state = ButtonState::Pressed;
            let input = KeyboardInput { key_code, logical_key, state, text: None, repeat: false, window };
            app.world_mut().trigger(FocusedInput::new(target, input, window));
            app.update();
            app.update();
        }

        fn kept(dir: &std::path::Path) -> Option<Vec<String>> {
            LocalStore::open(Some(dir.to_path_buf()), &[crate::SHELL]).get(crate::SHELL, crate::RAIL_ORDER)
        }

        #[test]
        fn dragging_an_app_down_the_rail_moves_it_there_in_place_keeps_it_and_does_not_open_it() {
            let dir = tempfile::tempdir().unwrap();
            let mut app = rail_app(shell(dir.path()));
            let before = buttons(&mut app);
            assert_eq!(ids(&app), [NOTES, MAPS, TREES, LAKES]);
            let by = Vec2::new(0.0, (ITEM + GAP) * 2.0 + 2.0);
            let notes = drag(&mut app, 0, by);
            assert!(app.world().get::<Lifted>(notes).is_some());
            release(&mut app, notes, 0, by);

            assert_eq!(ids(&app), [MAPS, TREES, NOTES, LAKES]);
            let after = buttons(&mut app);
            let moved = [before[1], before[2], before[0], before[3]];
            assert_eq!(after, moved, "the same buttons, moved");
            assert_eq!(app.world().resource::<State<ActiveApp>>().get().0, NOTES, "the drag didn't open it");
            assert!(app.world().get::<NavItem>(notes).unwrap().current, "and its mark stayed with it");
            let rail = app.world_mut().query_filtered::<&Children, With<Rail>>().single(app.world()).unwrap();
            let last = rail.iter().last().unwrap();
            assert!(app.world().get::<RailFoot>(last).is_some(), "the foot stays last");
            assert_eq!(kept(dir.path()).unwrap(), ["maps", "trees", "notes", "lakes"]);
        }

        #[test]
        fn a_short_press_still_opens_the_app() {
            let dir = tempfile::tempdir().unwrap();
            let mut app = rail_app(shell(dir.path()));
            // Six pixels on screen: four of the UI's at 150%, under the threshold.
            let by = Vec2::new(0.0, 6.0 / UI_SCALE);
            let maps = drag(&mut app, 1, by);
            release(&mut app, maps, 1, by);
            assert_eq!(app.world().resource::<State<ActiveApp>>().get().0, MAPS);
            assert_eq!(ids(&app), [NOTES, MAPS, TREES, LAKES]);
            assert_eq!(kept(dir.path()), None, "nothing moved, nothing kept");
        }

        #[test]
        fn escape_puts_the_app_back() {
            let dir = tempfile::tempdir().unwrap();
            let mut app = rail_app(shell(dir.path()));
            let by = Vec2::new(0.0, 70.0);
            let notes = drag(&mut app, 0, by);
            press_key(&mut app, notes, KeyCode::Escape, Key::Escape);
            release(&mut app, notes, 0, by);
            assert_eq!(ids(&app), [NOTES, MAPS, TREES, LAKES]);
            assert_eq!(app.world().resource::<State<ActiveApp>>().get().0, NOTES, "and didn't open anything");
        }

        #[test]
        fn alt_and_down_moves_the_focused_app_one_place() {
            let dir = tempfile::tempdir().unwrap();
            let mut app = rail_app(shell(dir.path()));
            let trees = buttons(&mut app)[2].0;
            app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::AltLeft);
            press_key(&mut app, trees, KeyCode::ArrowDown, Key::ArrowDown);
            assert_eq!(ids(&app), [NOTES, MAPS, LAKES, TREES]);
            press_key(&mut app, trees, KeyCode::ArrowDown, Key::ArrowDown);
            assert_eq!(ids(&app), [NOTES, MAPS, LAKES, TREES], "the last goes no lower");
            assert_eq!(buttons(&mut app)[3], (trees, TREES));
            let hint = app.world().get::<Hint>(trees).unwrap().0.clone();
            assert_eq!(hint, "trees (Alt+Up or Down moves it)");
        }
    }

    #[test]
    fn a_shell_the_launcher_did_not_start_never_shows_the_notice() {
        let mut app = windowed(ShellPlugin::new().with(Fake(NOTES)).with(Fake(MAPS)));
        assert_eq!(shown::<UpdateButton>(&mut app).1, Display::None);
    }
}
