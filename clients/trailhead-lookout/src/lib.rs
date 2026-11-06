//! Lookout, the Trailhead factory's Bevy client (`docs/factory/ui.md`).

use bevy::prelude::*;

pub mod author;
pub mod board;
pub mod commands;
pub mod connect;
pub mod connection;
pub mod factory;
#[doc(hidden)]
pub mod fixture;
pub mod icon;
pub mod inspector;
pub mod map;
pub mod mirror;
pub mod net;
pub mod sync;
pub mod widgets;

/// The app's systems and resources, without a window or renderer, so tests can run it headless.
pub struct LookoutPlugin;

impl Plugin for LookoutPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup);
    }
}

fn setup(mut commands: Commands) {
    commands.spawn(Camera2d);
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
        assert_eq!(world.query::<&Camera2d>().iter(world).count(), 1);
        // An empty full-window node would sit over the board whenever it spawned after it, and take every click.
        assert_eq!(world.query::<&Node>().iter(world).count(), 0);
    }
}
