use std::time::Duration;

use bevy::prelude::*;
use bevy::winit::{UpdateMode, WinitSettings};
use trailhead_app_example::Example;
use trailhead_lookout::Lookout;
use trailhead_lookout::icon::primary_window;
use trailhead_lookout::net::NetConfig;
use trailhead_lookout::settings::ThemeFile;
use trailhead_lookout::widgets::ThemePlugin;
use trailhead_shell::ShellPlugin;

/// `lookout [<control plane endpoint id>]`, with `LOOKOUT_TOKEN` to sign in with a pasted user token and
/// `LOOKOUT_QUEUE` for the queue task a task queued for the factory goes under in its project, ahead of the
/// factory's own queues. Without an id it dials the one used last time.
///
/// It is the Trailhead shell with its apps: Lookout and the example app, on the rail down the left edge.
fn main() {
    let Some(config) = NetConfig::from_env() else {
        eprintln!("lookout: no home directory to keep its endpoint key in");
        std::process::exit(1);
    };
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let theme_file = ThemeFile(config.config_dir.join("theme"));
    let kept = config.config_dir.join("apps");
    let lookout = Lookout::new(config)
        .dialing(std::env::args().nth(1))
        .signing_in_with(var("LOOKOUT_TOKEN"))
        .queueing_under(var("LOOKOUT_QUEUE"));
    let shell = ShellPlugin::new().with(lookout).with(Example).store_in(kept).windowed();
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin { primary_window: Some(primary_window()), ..default() }))
        // Update on input, and often enough for the net thread's news to show, instead of redrawing every frame.
        .insert_resource(WinitSettings {
            focused_mode: UpdateMode::reactive(Duration::from_millis(100)),
            unfocused_mode: UpdateMode::reactive_low_power(Duration::from_secs(1)),
        })
        .add_plugins(ThemePlugin { theme: shell.theme(), choice: theme_file.load() })
        .insert_resource(theme_file)
        .add_plugins(trailhead_lookout::widgets::UiPlugin)
        .add_plugins(shell)
        .run();
}
