use bevy::feathers::FeathersPlugins;
use bevy::feathers::dark_theme::create_dark_theme;
use bevy::feathers::theme::UiTheme;
use bevy::prelude::*;
use trailhead_lookout::LookoutPlugin;
use trailhead_lookout::author::{AuthorUiPlugin, Authoring};
use trailhead_lookout::board::BoardUiPlugin;
use trailhead_lookout::connect::ConnectUiPlugin;
use trailhead_lookout::connection::{ConnectionPlugin, SendNet};
use trailhead_lookout::map::MapUiPlugin;
use trailhead_lookout::net::{NetCommand, NetConfig};

/// `lookout [<control plane endpoint id>]`, with `LOOKOUT_TOKEN` to sign in with a pasted user token and
/// `LOOKOUT_QUEUE` for the task new tasks go under (default `TRAIL-102`). Without an
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
        // Without a theme every feathers color is missing, and feathers draws missing ones in fuchsia.
        .insert_resource(UiTheme(create_dark_theme()))
        .add_plugins(ConnectionPlugin::new(config, target))
        .add_plugins((LookoutPlugin, BoardUiPlugin, MapUiPlugin, AuthorUiPlugin, ConnectUiPlugin))
        .add_systems(Startup, |mut authoring: ResMut<Authoring>| {
            if let Some(queue) = std::env::var("LOOKOUT_QUEUE").ok().filter(|q| !q.is_empty()) {
                authoring.queue = queue;
            }
        })
        .add_systems(Startup, move |mut send: MessageWriter<SendNet>| {
            if let Some(token) = &token {
                send.write(SendNet(NetCommand::Login(token.clone())));
            }
        })
        .run();
}
