//! The rail of apps down the left edge of the window: a button for each app, the one shown marked, and at its foot
//! the update notice ([`crate::update`]) while a newer build is ready. A shell with a single app has no rail, and the
//! app has the whole window.

use bevy::prelude::*;
use bevy::ui_widgets::Activate;
use trailhead_ui::{ButtonKind, Icon, NavItem, rail, rail_foot, rail_item, set_display, tool_button};

use crate::update::{Restart, UpdateReady};
use crate::{ActiveApp, AppId, Apps};

/// The rail's root.
#[derive(Component)]
pub struct Rail;

/// On an app's button on the rail: the app a press shows.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenApp(pub AppId);

/// The rail's foot, shown while a newer build is ready.
#[derive(Component)]
pub struct UpdateNotice;

/// The button in the [`UpdateNotice`] that restarts onto the newer build.
#[derive(Component)]
pub struct RestartButton;

pub(crate) struct RailUiPlugin;

impl Plugin for RailUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_rail)
            .add_observer(on_open)
            .add_observer(on_restart)
            .add_systems(Update, (mark_shown, show_update));
    }
}

/// The rail, its items in the order of the apps, then its foot. Each is put down at once (`spawn_scene`), so the
/// foot comes after the items and its node is there to hide before the first frame is drawn.
fn spawn_rail(mut commands: Commands, apps: Res<Apps>) {
    if apps.len() < 2 {
        return;
    }
    let root = commands.spawn_scene(rail()).insert((Name::new("rail"), Rail)).id();
    for manifest in apps.iter() {
        commands.spawn_scene(rail_item(manifest.icon, manifest.name)).insert((OpenApp(manifest.id), ChildOf(root)));
    }
    let foot = commands.spawn_scene(rail_foot()).insert((Name::new("update notice"), UpdateNotice, ChildOf(root))).id();
    commands
        .spawn_scene(tool_button(Icon::Refresh, "Update ready: restart", ButtonKind::Primary))
        .insert((RestartButton, ChildOf(foot)));
}

/// Shows the foot while a newer build is ready, in place. A shell the launcher didn't start never shows it.
fn show_update(
    ready: Option<Res<UpdateReady>>,
    notices: Query<Entity, With<UpdateNotice>>,
    mut nodes: Query<&mut Node>,
) {
    let shown = ready.is_some_and(|r| r.0.is_some());
    for notice in &notices {
        set_display(&mut nodes, notice, shown);
    }
}

fn on_restart(activate: On<Activate>, buttons: Query<(), With<RestartButton>>, mut commands: Commands) {
    if buttons.contains(activate.entity) {
        commands.trigger(Restart);
    }
}

fn on_open(activate: On<Activate>, items: Query<&OpenApp>, mut next: ResMut<NextState<ActiveApp>>) {
    if let Ok(OpenApp(id)) = items.get(activate.entity) {
        next.set_if_different(ActiveApp(*id));
    }
}

/// Marks the button of the app shown, in place.
fn mark_shown(active: Res<State<ActiveApp>>, mut items: Query<(&OpenApp, &mut NavItem)>) {
    for (open, mut item) in &mut items {
        item.set_if_neq(NavItem { current: open.0 == active.get().0 });
    }
}

#[cfg(test)]
mod tests {
    use bevy::state::app::StatesPlugin;

    use super::*;
    use crate::ShellPlugin;
    use crate::tests::{Fake, MAPS, NOTES, Seen};

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

    fn notice(app: &mut App) -> (Entity, Display) {
        let world = app.world_mut();
        world
            .query_filtered::<(Entity, &Node), With<UpdateNotice>>()
            .single(world)
            .map(|(e, n)| (e, n.display))
            .unwrap()
    }

    #[test]
    fn the_update_notice_sits_at_the_rails_foot_shows_in_place_when_a_build_is_ready_and_restarts() {
        let mut app = windowed(ShellPlugin::new().with(Fake(NOTES)).with(Fake(MAPS)).built_as(Some("a".into())));
        let (foot, display) = notice(&mut app);
        assert_eq!(display, Display::None, "hidden while this is the newest build");
        let rail = app.world_mut().query_filtered::<Entity, With<Rail>>().single(app.world()).unwrap();
        let children: Vec<Entity> = app.world().get::<Children>(rail).unwrap().iter().collect();
        assert_eq!(children.last(), Some(&foot), "the foot comes after the apps");

        app.world_mut().resource_mut::<UpdateReady>().0 = Some(crate::update::Newer::new("b", &[], 0));
        app.update();
        assert_eq!(notice(&mut app), (foot, Display::Flex), "the same entity, shown");

        let button = app.world_mut().query_filtered::<Entity, With<RestartButton>>().single(app.world()).unwrap();
        assert_eq!(app.world().get::<ChildOf>(button).map(ChildOf::parent), Some(foot));
        app.world_mut().trigger(Activate { entity: button });
        app.world_mut().flush();
        let exits: Vec<AppExit> = app.world_mut().resource_mut::<Messages<AppExit>>().drain().collect();
        assert_eq!(exits, [AppExit::from_code(trailhead_model::shell::RESTART_CODE)]);
    }

    #[test]
    fn a_shell_the_launcher_did_not_start_never_shows_the_notice() {
        let mut app = windowed(ShellPlugin::new().with(Fake(NOTES)).with(Fake(MAPS)));
        assert_eq!(notice(&mut app).1, Display::None);
    }
}
