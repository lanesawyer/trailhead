use std::time::Duration;

use bevy::prelude::*;
use bevy::winit::{UpdateMode, WinitSettings};
use trailhead_lookout::LookoutPlugin;
use trailhead_lookout::author::{AuthorUiPlugin, Authoring};
use trailhead_lookout::board::BoardUiPlugin;
use trailhead_lookout::connect::ConnectUiPlugin;
use trailhead_lookout::connection::{ConnectionPlugin, SendNet};
use trailhead_lookout::icon::{IconUiPlugin, primary_window};
use trailhead_lookout::inspector::InspectorUiPlugin;
use trailhead_lookout::map::MapUiPlugin;
use trailhead_lookout::net::{NetCommand, NetConfig};
use trailhead_lookout::project::ProjectUiPlugin;
use trailhead_lookout::project_form::ProjectFormUiPlugin;
use trailhead_lookout::settings::{SettingsUiPlugin, ThemeFile};
use trailhead_lookout::sidebar::SidebarUiPlugin;
use trailhead_lookout::widgets::{ThemePlugin, theme};

/// `lookout [<control plane endpoint id>]`, with `LOOKOUT_TOKEN` to sign in with a pasted user token and
/// `LOOKOUT_QUEUE` for the task new tasks go under (default: the factory's own
/// queue, else `TRAIL-102`). Without an id it dials the one used last time.
fn main() {
    let Some(config) = NetConfig::from_env() else {
        eprintln!("lookout: no home directory to keep its endpoint key in");
        std::process::exit(1);
    };
    let target = std::env::args().nth(1);
    let token = std::env::var("LOOKOUT_TOKEN").ok().filter(|t| !t.is_empty());
    let theme_file = ThemeFile(config.config_dir.join("theme"));
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin { primary_window: Some(primary_window()), ..default() }))
        // Update on input, and often enough for the net thread's news to show, instead of redrawing every frame.
        .insert_resource(WinitSettings {
            focused_mode: UpdateMode::reactive(Duration::from_millis(100)),
            unfocused_mode: UpdateMode::reactive_low_power(Duration::from_secs(1)),
        })
        .add_plugins(ThemePlugin { theme: theme(), choice: theme_file.load() })
        .insert_resource(theme_file)
        .add_plugins(trailhead_lookout::widgets::UiPlugin)
        .add_plugins(ConnectionPlugin::new(config, target))
        .add_plugins((
            LookoutPlugin,
            BoardUiPlugin,
            MapUiPlugin,
            ProjectUiPlugin,
            ProjectFormUiPlugin,
            AuthorUiPlugin,
            InspectorUiPlugin,
            ConnectUiPlugin,
            SettingsUiPlugin,
            SidebarUiPlugin,
            IconUiPlugin,
        ))
        .add_systems(Startup, |mut authoring: ResMut<Authoring>| {
            if let Some(queue) = std::env::var("LOOKOUT_QUEUE").ok().filter(|q| !q.is_empty()) {
                authoring.queue = queue;
                authoring.pinned = true;
            }
        })
        .add_systems(Startup, move |mut send: MessageWriter<SendNet>| {
            if let Some(token) = &token {
                send.write(SendNet(NetCommand::Login(token.clone())));
            }
        })
        .run();
}
