//! `trailhead app`: the launcher. It runs the newest shell the server's builder made (`trailhead_builder`), runs it
//! again when it exits asking to be restarted onto a newer build, and falls back to the build before when the
//! newest fails as it starts.

use std::ffi::OsString;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use trailhead_builder::{Builder, Step};
use trailhead_builder::{Layout, ShellConfig};
use trailhead_core::shell::{BUILD_ENV, LOCAL_STATE_ENV, RESTART_CODE, ShellBuild};
use trailhead_server::Paths;

use super::app_notice::Notice;
use super::app_remote::{FromServer, Link, only_launcher, shell_dir};
use super::print_json;
use crate::cli::AppCmd;
use crate::client;

/// A build that exits with an error sooner than this after it starts failed to start, and the one before is run.
const STARTUP: Duration = Duration::from_secs(10);

pub(crate) async fn app(paths: &Paths, url: &str, cmd: Option<&AppCmd>, args: &[OsString], json: bool) -> Result<()> {
    let done = start(paths, url, cmd, args, json).await;
    // Started from Trailhead.app, the error below goes to a log nobody is reading.
    if let (None, Err(e), Some(mut notice)) = (cmd, &done, Notice::from_env()) {
        notice.failed(&format!("Trailhead couldn't start: {e:#}"));
    }
    done
}

async fn start(paths: &Paths, url: &str, cmd: Option<&AppCmd>, args: &[OsString], json: bool) -> Result<()> {
    let config = ShellConfig::read(&paths.config_dir).map_err(anyhow::Error::msg)?;
    let remote = ShellConfig::read_remote(&paths.config_dir).map_err(anyhow::Error::msg)?;
    let layout = Layout::new(&paths.data_dir);
    let read = || ShellBuild { configured: config.is_some() || remote.is_some(), ..layout.read() };
    match cmd {
        Some(AppCmd::From { endpoint }) => build_from(paths, endpoint),
        Some(AppCmd::Install) => super::app_install::install(),
        None if remote.is_some() => {
            let _only = only_launcher(&shell_dir(paths))?;
            let (from, config) = remote.expect("checked");
            let code = launch_remote(paths, &from, config, &layout, args).await?;
            if code != 0 {
                std::process::exit(code);
            }
            Ok(())
        }
        Some(AppCmd::Status) if json => {
            print_json(&serde_json::to_value(read())?);
            Ok(())
        }
        Some(AppCmd::Status) => {
            println!("{}", status(&read(), &layout, &paths.config_dir));
            Ok(())
        }
        None => {
            let _only = only_launcher(&shell_dir(paths))?;
            ensure_server(url).await?;
            let code = launch(&layout, read, |binary, rev| run(binary, rev, args, None), &paths.config_dir)?;
            if code != 0 {
                std::process::exit(code);
            }
            Ok(())
        }
    }
}

/// Starts this machine's server if it isn't running, as other commands do: the builder lives in it.
async fn ensure_server(url: &str) -> Result<()> {
    let (http, base) = client::http_and_base(url)?;
    if client::is_local(&base) && client::health(&http, &base, None).await.is_none() {
        client::start_local_server(&http, &base).await?;
    }
    Ok(())
}

/// How a shell ended: its exit code (`None` when a signal ended it) and how long it ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ended {
    code: Option<i32>,
    ran: Duration,
}

/// Saves `[shell] from` in the user config, so `trailhead app` builds and runs that server's builds here.
fn build_from(paths: &Paths, endpoint: &str) -> Result<()> {
    trailhead_wire::parse_endpoint_id(endpoint).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut cfg = crate::config::load(paths)?;
    let table = cfg.shell.get_or_insert_with(Default::default);
    if table.contains_key("repo") {
        bail!(
            "{} has `[shell] repo`: this machine builds the window from its own apps repo. Remove it to build another server's builds instead",
            crate::config::path(paths).display()
        );
    }
    table.insert("from".into(), toml::Value::String(endpoint.trim().to_string()));
    crate::config::save(paths, &cfg)?;
    println!(
        "saved in {}. `trailhead app` now builds the window from that server's builds and runs it; the first build takes a while",
        crate::config::path(paths).display()
    );
    Ok(())
}

/// `trailhead app` with `[shell] from`: reaches the server, builds its newest build here when this machine has
/// none, keeps building its newer ones while the window runs, and runs the window as the launcher always does.
async fn launch_remote(
    paths: &Paths,
    from: &str,
    config: ShellConfig,
    layout: &Layout,
    args: &[OsString],
) -> Result<i32> {
    let link = Link::open(paths, from).await?;
    let builder = Builder::new(FromServer::new(link, from), config, std::path::PathBuf::new(), layout.clone());
    if layout.load().current.is_none() {
        eprintln!(
            "trailhead app: building the window for the first time, which takes a while (ten minutes or more); cargo's output is in {}",
            layout.log().display()
        );
        if let Step::Failed(rev) = builder.step().await.map_err(anyhow::Error::msg)? {
            let why = layout.load().error.map(|e| e.message).unwrap_or_default();
            bail!("the build of {} failed ({why}); its output is in {}", short(&rev), layout.log().display());
        }
    }
    let building = builder.spawn();
    // The window dials the server it is built from, unless told another.
    let dial = [OsString::from(from)];
    let args = if args.is_empty() { &dial[..] } else { args };
    let state = layout.state_file();
    let read = || ShellBuild { configured: true, ..layout.read() };
    // The window runs for hours on this thread; the builder's task carries on beside it.
    let code = tokio::task::block_in_place(|| {
        launch(layout, read, |binary, rev| run(binary, rev, args, Some(&state)), &paths.config_dir)
    });
    building.stop().await;
    code
}

/// Runs the build `rev` with the rest of the environment and `args`, until it exits. `built_here` is the state file
/// of a builder on this machine, for a window that should offer the builds it finds there.
fn run(binary: &Path, rev: &str, args: &[OsString], built_here: Option<&Path>) -> Result<Ended> {
    let started = Instant::now();
    let mut window = std::process::Command::new(binary);
    if let Some(state) = built_here {
        window.env(LOCAL_STATE_ENV, state);
    }
    let status = window
        .args(args)
        .env(BUILD_ENV, rev)
        .status()
        .map_err(|e| anyhow::anyhow!("starting {}: {e}", binary.display()))?;
    Ok(Ended { code: status.code(), ran: started.elapsed() })
}

/// Runs the newest build until a shell exits other than to restart, and returns its exit code. A newest build that
/// fails as it starts is marked bad and the one before it is run instead.
fn launch(
    layout: &Layout,
    read: impl Fn() -> ShellBuild,
    mut run: impl FnMut(&Path, &str) -> Result<Ended>,
    config_dir: &Path,
) -> Result<i32> {
    let mut rev = match read().newest() {
        Some(rev) => rev.to_string(),
        None => bail!("{}", no_build(&read(), layout, config_dir)),
    };
    loop {
        let ended = run(&layout.binary(&rev), &rev)?;
        let code = ended.code.unwrap_or(1);
        if code == i32::from(RESTART_CODE) {
            match read().newest() {
                Some(newest) => rev = newest.to_string(),
                None => bail!("{}", no_build(&read(), layout, config_dir)),
            }
            continue;
        }
        if code == 0 || ended.ran >= STARTUP {
            return Ok(code);
        }
        let build = read();
        let Some(fallback) = build.fallback_from(&rev).map(String::from) else { return Ok(code) };
        eprintln!(
            "trailhead app: the shell built from {} exited with code {code} as it started; marking it bad and starting the build before it, {}",
            short(&rev),
            short(&fallback)
        );
        if let Err(e) = layout.mark_bad(&rev) {
            eprintln!("trailhead app: couldn't mark {} bad: {e}", short(&rev));
        }
        rev = fallback;
    }
}

/// Why there is no build to run, and what to do about it.
fn no_build(build: &ShellBuild, layout: &Layout, config_dir: &Path) -> String {
    let log = layout.log();
    if !build.configured {
        return format!(
            "no shell to start: the server builds it from an apps repo, which the user config ({}) doesn't name. Add\n\n  [shell]\n  repo = \"~/Dev/trailhead-apps\"\n\nthen restart the server (`trailhead server restart`) and run `trailhead app` again once the first build is done",
            config_dir.join("config.toml").display()
        );
    }
    if let Some(rev) = &build.building {
        return format!(
            "the first build of the shell ({}) is still running; its output is in {}",
            short(rev),
            log.display()
        );
    }
    if let Some(error) = &build.error {
        return format!(
            "no shell to start: the build of {} failed ({}); its output is in {}",
            short(&error.rev),
            error.message,
            log.display()
        );
    }
    if !build.bad.is_empty() {
        return "no shell to start: every build kept failed as it started (`trailhead app status`)".into();
    }
    format!(
        "no shell built yet. The server looks at the apps repo every minute or so; if `[shell]` was added since it started, `trailhead server restart`. Build output goes to {}",
        log.display()
    )
}

/// `trailhead app status`.
fn status(build: &ShellBuild, layout: &Layout, config_dir: &Path) -> String {
    if !build.configured {
        return format!(
            "no builder: `[shell] repo` isn't set in {} (on a machine with no apps repo, `trailhead app from <the server's endpoint id>`)",
            config_dir.join("config.toml").display()
        );
    }
    let rev = |r: &Option<String>| match r {
        Some(r) if build.is_bad(r) => format!("{} (bad)", short(r)),
        Some(r) => short(r).to_string(),
        None => "none".into(),
    };
    let mut lines = vec![format!("current  {}", rev(&build.current)), format!("previous {}", rev(&build.previous))];
    if let Some(building) = &build.building {
        lines.push(format!("building {}", short(building)));
    }
    if let Some(error) = &build.error {
        lines.push(format!("failed   {} at {}: {}", short(&error.rev), error.at, error.message));
    }
    lines.push(format!("log      {}", layout.log().display()));
    lines.join("\n")
}

fn short(rev: &str) -> &str {
    rev.get(..12).unwrap_or(rev)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use trailhead_builder::{Built, State};

    use super::*;

    fn ended(code: i32, secs: u64) -> Ended {
        Ended { code: Some(code), ran: Duration::from_secs(secs) }
    }

    fn rig(current: &str, previous: Option<&str>) -> (tempfile::TempDir, Layout) {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        set(&layout, current, previous);
        (dir, layout)
    }

    fn set(layout: &Layout, current: &str, previous: Option<&str>) {
        let built = |rev: &str| Built { rev: rev.into(), at: jiff::Timestamp::UNIX_EPOCH };
        layout
            .save(&State { current: Some(built(current)), previous: previous.map(built), ..Default::default() })
            .unwrap();
    }

    /// Runs the launcher with the shell's exits given in turn, and returns what it ran and its exit code.
    fn launched(layout: &Layout, exits: Vec<Ended>, between: impl Fn(usize)) -> (Vec<String>, i32) {
        let ran = RefCell::new(Vec::new());
        let mut exits = exits.into_iter();
        let read = || ShellBuild { configured: true, ..layout.read() };
        let code = launch(
            layout,
            read,
            |binary, rev| {
                assert_eq!(binary, layout.binary(rev));
                ran.borrow_mut().push(rev.to_string());
                between(ran.borrow().len());
                Ok(exits.next().unwrap())
            },
            Path::new("/config"),
        )
        .unwrap();
        (ran.into_inner(), code)
    }

    #[test]
    fn a_shell_that_asks_to_restart_is_started_again_on_the_newest_build() {
        let (_dir, layout) = rig("a", None);
        let exits = vec![ended(i32::from(RESTART_CODE), 600), ended(0, 600)];
        let (ran, code) = launched(&layout, exits, |n| {
            if n == 1 {
                set(&layout, "b", Some("a"));
            }
        });
        assert_eq!((ran, code), (vec!["a".to_string(), "b".to_string()], 0));
    }

    #[test]
    fn a_newest_build_that_fails_as_it_starts_is_marked_bad_and_the_one_before_runs() {
        let (_dir, layout) = rig("b", Some("a"));
        let (ran, code) = launched(&layout, vec![ended(101, 1), ended(0, 600)], |_| {});
        assert_eq!((ran, code), (vec!["b".to_string(), "a".to_string()], 0));
        assert_eq!(layout.bad(), ["b"]);
        let (ran, _) = launched(&layout, vec![ended(0, 600)], |_| {});
        assert_eq!(ran, ["a"], "a bad build isn't run again");
    }

    #[test]
    fn a_failure_after_startup_or_with_nothing_to_fall_back_to_ends_the_launcher() {
        let (_dir, layout) = rig("b", Some("a"));
        assert_eq!(launched(&layout, vec![ended(3, 60)], |_| {}), (vec!["b".to_string()], 3));
        assert!(layout.bad().is_empty());
        let (_dir, layout) = rig("b", None);
        assert_eq!(launched(&layout, vec![ended(101, 1)], |_| {}), (vec!["b".to_string()], 101));
        assert!(layout.bad().is_empty());
    }

    #[test]
    fn with_no_build_it_says_what_to_set_or_that_the_first_build_is_running() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        let none = |configured| {
            launch(&layout, || ShellBuild { configured, ..layout.read() }, |_, _| unreachable!(), Path::new("/c"))
                .unwrap_err()
                .to_string()
        };
        assert!(none(false).contains("[shell]\n  repo ="), "{}", none(false));
        layout.save(&State { building: Some("abc".into()), ..Default::default() }).unwrap();
        let running = none(true);
        assert!(running.contains("still running") && running.contains("build.log"), "{running}");
    }

    #[test]
    fn status_shows_the_builds_and_marks_a_bad_one() {
        let (_dir, layout) = rig("b", Some("a"));
        layout.mark_bad("b").unwrap();
        let shown = status(&ShellBuild { configured: true, ..layout.read() }, &layout, Path::new("/c"));
        assert!(shown.starts_with("current  b (bad)\nprevious a\nlog "), "{shown}");
        let off = status(&ShellBuild::default(), &layout, Path::new("/c"));
        assert!(off.contains("/c/config.toml"), "{off}");
    }
}
