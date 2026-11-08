//! The factory's control plane, run next to a local Trailhead server (`docs/factory/state.md`):
//!
//! ```sh
//! trailhead-factory login sprites   # also `claude` and `github`; or set SPRITES_TOKEN, CLAUDE_CODE_OAUTH_TOKEN, GITHUB_TOKEN
//! trailhead-factory run             # or `trailhead factory run`
//! ```
//!
//! The control plane itself is the `trailhead-factory-server` library; this binary is its command line until the
//! Trailhead server runs it (`docs/factory/server-control-plane.md`).

use clap::{Parser, Subcommand};
use trailhead_factory_server::{Opts, RunOpts, login, ops};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Set everything up and report, without running anything.
    Check(RunOpts),
    /// Run the control plane for every repo until Ctrl-C.
    Run(RunOpts),
    /// Show the sandbox pool, what is running and how queued tasks have gone. Read-only; runs beside `run`.
    Status(Opts),
    /// Print the daemon's service log from a task's sandbox.
    Logs(ops::LogsOpts),
    /// Let a UI (Lookout) connect: add its endpoint id to the allowlist. Takes effect on a running control plane at once.
    Allow { endpoint_id: String },
    /// Stop allowing a UI endpoint.
    Disallow { endpoint_id: String },
    /// List the UI endpoint ids allowed to connect.
    Allowed,
    /// Save a secret in the OS keyring (read from stdin), so `run` needs no environment variables.
    #[command(subcommand)]
    Login(login::Login),
}

async fn main_inner(cli: Cli) -> Result<(), String> {
    match cli.cmd {
        Cmd::Check(run) => trailhead_factory_server::check(&run).await,
        Cmd::Run(run) => trailhead_factory_server::run(&run).await,
        Cmd::Status(opts) => ops::status(&opts).await,
        Cmd::Logs(logs) => ops::logs(&logs).await,
        Cmd::Allow { endpoint_id } => trailhead_factory_server::allow(&endpoint_id, true),
        Cmd::Disallow { endpoint_id } => trailhead_factory_server::allow(&endpoint_id, false),
        Cmd::Allowed => trailhead_factory_server::allowed(),
        Cmd::Login(cmd) => trailhead_factory_server::login(cmd).await,
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let file = if matches!(cli.cmd, Cmd::Run(_)) { trailhead_factory_server::open_log_file() } else { None };
    trailhead_factory_server::init_logging(file);
    match main_inner(cli).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("trailhead-factory: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
