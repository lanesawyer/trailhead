use bevy::feathers::FeathersPlugins;
use bevy::prelude::*;
use trailhead_lookout::LookoutPlugin;
use trailhead_lookout::board::BoardUiPlugin;
use trailhead_lookout::connection::{ConnectionPlugin, SendNet};
use trailhead_lookout::inspector::InspectorUiPlugin;
use trailhead_lookout::net::{NetCommand, NetConfig};

/// `lookout [<control plane endpoint id>]`, with `LOOKOUT_TOKEN` to sign in with a pasted user token. Without an
/// id it dials the one used last time.
fn main() {
    let Some(config) = NetConfig::from_env() else {
        eprintln!("lookout: no home directory to keep its endpoint key in");
        std::process::exit(1);
    };
    let target = std::env::args().nth(1);
    let token = std::env::var("LOOKOUT_TOKEN").ok().filter(|t| !t.is_empty());
    App::new()
        .add_plugins(
            DefaultPlugins.set(WindowPlugin {
                primary_window: Some(Window { title: "Lookout".into(), ..default() }),
                ..default()
            }),
        )
        .add_plugins(FeathersPlugins)
        .add_plugins(ConnectionPlugin::new(config, target))
        .add_plugins((LookoutPlugin, BoardUiPlugin, InspectorUiPlugin))
        .add_systems(Startup, move |mut send: MessageWriter<SendNet>| {
            if let Some(token) = &token {
                send.write(SendNet(NetCommand::Login(token.clone())));
            }
        })
        .run();
}
