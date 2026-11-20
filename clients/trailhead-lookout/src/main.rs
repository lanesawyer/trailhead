use trailhead_lookout::Lookout;
use trailhead_lookout::icon::primary_window;
use trailhead_shell::Shell;
use trailhead_shell::net::NetConfig;

/// `lookout [<control plane endpoint id>]`, with `LOOKOUT_TOKEN` to sign in with a pasted user token and
/// `LOOKOUT_QUEUE` for the queue task a task queued for the factory goes under in its project, ahead of the
/// factory's own queues. Without an id it dials the one used last time.
///
/// It is the Trailhead shell holding one app, Lookout. A shell with more apps shows a rail down the left edge.
fn main() {
    let Some(config) = NetConfig::from_env() else {
        eprintln!("lookout: no home directory to keep its endpoint key in");
        std::process::exit(1);
    };
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let kept = config.config_dir.clone();
    Shell::new(kept)
        .window(primary_window())
        .connecting(config, std::env::args().nth(1))
        .signing_in_with(var("LOOKOUT_TOKEN"))
        .with(Lookout::new().queueing_under(var("LOOKOUT_QUEUE")))
        .run();
}
