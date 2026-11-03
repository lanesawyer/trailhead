use bevy::feathers::FeathersPlugins;
use bevy::prelude::*;
use trailhead_lookout::LookoutPlugin;

fn main() {
    App::new()
        .add_plugins(
            DefaultPlugins.set(WindowPlugin {
                primary_window: Some(Window { title: "Lookout".into(), ..default() }),
                ..default()
            }),
        )
        .add_plugins(FeathersPlugins)
        .add_plugins(LookoutPlugin)
        .run();
}
