//! The rail of apps down the left edge of the window: a button for each app, the one shown marked. A shell with a
//! single app has no rail, and the app has the whole window.

use bevy::prelude::*;
use bevy::ui_widgets::Activate;
use trailhead_ui::{NavItem, rail, rail_item};

use crate::{ActiveApp, AppId, Apps};

/// The rail's root.
#[derive(Component)]
pub struct Rail;

/// On an app's button on the rail: the app a press shows.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenApp(pub AppId);

pub(crate) struct RailUiPlugin;

impl Plugin for RailUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_rail).add_observer(on_open).add_systems(Update, mark_shown);
    }
}

fn spawn_rail(mut commands: Commands, apps: Res<Apps>) {
    if apps.len() < 2 {
        return;
    }
    let root = commands.spawn_scene(rail()).insert((Name::new("rail"), Rail)).id();
    for manifest in apps.iter() {
        commands
            .queue_spawn_scene(rail_item(manifest.icon, manifest.name))
            .insert((OpenApp(manifest.id), ChildOf(root)));
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
}
