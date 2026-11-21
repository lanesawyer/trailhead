//! The rail of apps down the left edge of the window: a button for each app, the one shown marked, a press switching to
//! it ([`crate::transition`]), and at its foot
//! the window's own buttons: Ask ([`crate::ask`]), while connected to a server with an apps repo, which opens the form
//! for asking for a change or a new app ([`crate::ask_panel`]); and the update notice ([`crate::update`]) while a newer
//! build is ready, whose button opens the panel saying what it brings ([`crate::update_panel`]); and last the gear,
//! which opens the window's settings ([`crate::settings`]). A shell with a single app has no rail, and the app has the
//! whole window.

use bevy::prelude::*;
use bevy::ui_widgets::Activate;
use trailhead_ui::{ButtonKind, Hint, Icon, NavItem, rail, rail_foot, rail_item, set_display, tool_button};

use crate::ask::Askable;
use crate::connection::Connection;
use crate::settings::SettingsGear;
use crate::transition::{SwitchApp, Switching};
use crate::update::UpdateReady;
use crate::{ActiveApp, AppId, Apps};

/// The rail's root.
#[derive(Component)]
pub struct Rail;

/// On an app's button on the rail: the app a press shows.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenApp(pub AppId);

/// The rail's foot, holding the window's own buttons.
#[derive(Component)]
pub struct RailFoot;

/// The button at the [`RailFoot`] that opens the Ask form, shown while the connection is live and the server has an
/// apps repo to ask about.
#[derive(Component)]
pub struct AskButton;

/// The button at the [`RailFoot`] that opens the update's panel, shown while a newer build is ready, its hint saying
/// how many changes it brings.
#[derive(Component)]
pub struct UpdateButton;

/// The gear at the [`RailFoot`], under Ask and the update notice, that opens the settings.
#[derive(Component)]
pub struct SettingsButton;

/// What the Ask button's hint says.
pub const ASK_HINT: &str = "Ask for a change or a new app";

pub(crate) struct RailUiPlugin;

impl Plugin for RailUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_rail)
            .add_observer(on_open)
            .add_systems(Update, (mark_shown, show_update, show_ask));
    }
}

/// The rail, its items in the order of the apps, then its foot with Ask, the update notice and the gear. Each is put down at
/// once (`spawn_scene`), so the foot comes after the items and its buttons' nodes are there to hide before the first
/// frame is drawn.
fn spawn_rail(mut commands: Commands, apps: Res<Apps>) {
    if !apps.has_rail() {
        return;
    }
    let root = commands.spawn_scene(rail()).insert((Name::new("rail"), Rail)).id();
    for manifest in apps.iter() {
        commands.spawn_scene(rail_item(manifest.icon, manifest.name)).insert((OpenApp(manifest.id), ChildOf(root)));
    }
    let foot = commands.spawn_scene(rail_foot()).insert((Name::new("rail foot"), RailFoot, ChildOf(root))).id();
    let ask = bsn! { @tool_button(Icon::Plus, ASK_HINT, ButtonKind::Normal) Node { display: Display::None } };
    commands.spawn_scene(ask).insert((AskButton, ChildOf(foot)));
    let update =
        bsn! { @tool_button(Icon::Refresh, "Update ready", ButtonKind::Primary) Node { display: Display::None } };
    commands.spawn_scene(update).insert((UpdateButton, ChildOf(foot)));
    let gear = tool_button(Icon::Settings, "Settings", ButtonKind::Plain);
    commands.spawn_scene(gear).insert((SettingsButton, SettingsGear, ChildOf(foot)));
}

/// Shows Ask while the connection is live and the server has an apps repo to ask about.
fn show_ask(
    askable: Option<Res<Askable>>,
    connection: Option<Res<State<Connection>>>,
    buttons: Query<Entity, With<AskButton>>,
    mut nodes: Query<&mut Node>,
) {
    let live = connection.is_some_and(|c| *c.get() == Connection::Live);
    let shown = live && askable.is_some_and(|a| a.configured());
    for button in &buttons {
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
        assert_eq!(children.last(), Some(&foot), "the foot comes after the apps");
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
    fn ask_shows_only_while_live_with_an_apps_repo_to_ask_about() {
        use bevy::state::app::AppExtStates;

        let mut app = windowed(ShellPlugin::new().with(Fake(NOTES)).with(Fake(MAPS)));
        app.init_state::<Connection>();
        app.update();
        let (ask, display) = shown::<AskButton>(&mut app);
        assert_eq!(display, Display::None);
        assert_eq!(app.world().get::<Hint>(ask).map(|h| h.0.as_str()), Some(ASK_HINT));
        let configured = trailhead_model::shell::AskTargets { configured: true, ..default() };
        app.world_mut().resource_mut::<Askable>().0 = Some(configured.clone());
        app.update();
        assert_eq!(shown::<AskButton>(&mut app).1, Display::None, "not while offline");
        app.world_mut().resource_mut::<NextState<Connection>>().set(Connection::Live);
        app.update();
        app.update();
        assert_eq!(shown::<AskButton>(&mut app), (ask, Display::Flex));
        app.world_mut().resource_mut::<Askable>().0 = Some(trailhead_model::shell::AskTargets::default());
        app.update();
        assert_eq!(shown::<AskButton>(&mut app), (ask, Display::None), "not without an apps repo");
    }

    #[test]
    fn a_shell_the_launcher_did_not_start_never_shows_the_notice() {
        let mut app = windowed(ShellPlugin::new().with(Fake(NOTES)).with(Fake(MAPS)));
        assert_eq!(shown::<UpdateButton>(&mut app).1, Display::None);
    }
}
