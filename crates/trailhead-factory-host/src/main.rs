//! The factory's control plane as its own process, beside a local Trailhead server (`docs/factory/state.md`). The
//! server runs it itself when the user config lists `[factory] repos`, and `trailhead factory <cmd>` takes the same
//! commands, so this binary goes away (TRAIL-1076):
//!
//! ```sh
//! trailhead-factory login sprites   # also `claude` and `github`; or set SPRITES_TOKEN, CLAUDE_CODE_OAUTH_TOKEN, GITHUB_TOKEN
//! trailhead-factory run
//! ```

use clap::Parser;
use trailhead_factory_server::Command;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    cmd: Command,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match trailhead_factory_server::command(Cli::parse().cmd).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("trailhead-factory: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
