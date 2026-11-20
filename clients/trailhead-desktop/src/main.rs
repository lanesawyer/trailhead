use trailhead_lookout::Lookout;
use trailhead_shell::Shell;
use trailhead_shell::net::NetConfig;

/// `trailhead-shell [<control plane endpoint id>]`, with `TRAILHEAD_SHELL_TOKEN` to sign in with a pasted user token
/// and `LOOKOUT_QUEUE` for the queue task a task queued for the factory goes under in its project, ahead of the
/// factory's own queues. Without an id it dials the one used last time. `TRAILHEAD_SHELL_DIR` moves what the window
/// keeps (its endpoint key, the control plane dialed last, the theme and each app's store) out of
/// `<Trailhead's config directory>/shell`.
///
/// It is the Trailhead shell holding the apps it ships with: Lookout. A shell with more apps shows a rail down the
/// left edge.
fn main() {
    let Some(config) = NetConfig::from_env() else {
        eprintln!("trailhead-shell: no home directory to keep its endpoint key in");
        std::process::exit(1);
    };
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let kept = config.config_dir.clone();
    Shell::new(kept)
        .connecting(config, std::env::args().nth(1))
        .signing_in_with(var("TRAILHEAD_SHELL_TOKEN"))
        .with(Lookout::new().queueing_under(var("LOOKOUT_QUEUE")))
        .run();
}
