//! Lookout, the Trailhead factory's Bevy client (`docs/factory/ui.md`).

use bevy::prelude::*;

/// The app's systems and resources, without a window or renderer, so tests can run it headless.
pub struct LookoutPlugin;

impl Plugin for LookoutPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup);
    }
}

fn setup(mut commands: Commands) {
    commands.spawn(Camera2d);
    // An empty full-window view; the board and inspector go inside it.
    commands.spawn((Name::new("root"), Node { width: percent(100), height: percent(100), ..default() }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_headless_and_runs_one_update() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(LookoutPlugin);
        app.update();

        let world = app.world_mut();
        let roots = world.query::<&Name>().iter(world).filter(|n| n.as_str() == "root").count();
        assert_eq!(roots, 1);
    }
}
