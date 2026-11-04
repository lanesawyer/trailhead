use bevy::feathers::FeathersPlugins;
use bevy::prelude::*;
use trailhead_lookout::LookoutPlugin;
use trailhead_lookout::connection::{Connection, ConnectionPlugin, ConnectionStatus, LocalEndpoint, SendNet};
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
        .add_plugins(LookoutPlugin)
        .add_systems(Startup, move |mut send: MessageWriter<SendNet>| {
            if let Some(token) = &token {
                send.write(SendNet(NetCommand::Login(token.clone())));
            }
        })
        .add_systems(Update, show_connection)
        .run();
}

/// Until the board exists: the connection, why it isn't live, and this Lookout's id for `trailhead factory allow`.
fn show_connection(
    state: Res<State<Connection>>,
    status: Res<ConnectionStatus>,
    endpoint: Res<LocalEndpoint>,
    mut text: Local<Option<Entity>>,
    mut commands: Commands,
    mut texts: Query<&mut Text>,
) {
    let mut line = format!("{:?}", state.get());
    if let Some(reason) = &status.reason {
        line += &format!(": {reason}");
    }
    if let Some(id) = &endpoint.0 {
        line += &format!("\nthis Lookout: {id}");
    }
    match text.and_then(|e| texts.get_mut(e).ok()) {
        Some(mut t) => t.0 = line,
        None => {
            *text = Some(
                commands.spawn((Text::new(line), Node { position_type: PositionType::Absolute, ..default() })).id(),
            )
        }
    }
}
