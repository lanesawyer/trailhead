//! `status` and `logs`: read-only views of a running factory, for operating it without curl. They read the repo's
//! factory file and open no iroh endpoint, so they work beside the server's control plane.  They read tasks through the
//! server's API, so only the server opens the database.

use crate::error::{Error, Result};
use std::sync::Arc;
use std::time::Duration;

use trailhead_control::FACTORY_LABEL;
use trailhead_daemon::RestTracker;
use trailhead_factory::compute::label;
use trailhead_factory::status::control_label;
use trailhead_factory::{
    Credential, FactorySnapshot, Labels, Sandbox, SandboxState, SandboxStatus, Status, Task, TaskKey, Tracker,
};

use crate::{Opts, computes, load_config, resolve_opts, saved_endpoint_id, surface};

/// The program the control plane starts the daemon as, whose service log `logs` prints.
const DAEMON_PROGRAM: &str = "trailhead-daemon";
/// How often `logs --follow` asks for more.
const FOLLOW_EVERY: Duration = Duration::from_secs(2);

/// How many finished (done or cancelled) tasks `status` lists unless given `--all`.
const FINISHED_SHOWN: usize = 10;

#[derive(clap::Args, Clone)]
pub struct StatusOpts {
    /// List every finished task, not just the most recent few.
    #[arg(long)]
    all: bool,
    #[command(flatten)]
    opts: Opts,
}

#[derive(clap::Args, Clone)]
pub struct LogsOpts {
    /// The task whose sandbox's log to print, e.g. TRAIL-42.
    task: String,
    /// Keep printing as the log grows, until Ctrl-C.
    #[arg(long, short)]
    follow: bool,
    #[command(flatten)]
    opts: Opts,
}

pub async fn status(status: &StatusOpts) -> Result<()> {
    let opts = &status.opts;
    let paths = trailhead_client::paths::Paths::from_env().ok_or(Error::NoDirs)?;
    let (opts, fnox) = resolve_opts(opts, &paths, &std::env::current_dir().map_err(Error::backend)?)?;
    let (_, _, config) = load_config(&opts)?;
    let tracker = api_tracker(&opts, &paths)?;

    // A running control plane knows its sandboxes and runs itself, so it is asked first; the compute is only read when
    // there isn't one (or it isn't running this repo).
    let slug = config.repo.repo().slug();
    let found = match server(&opts, &paths) {
        Some(server) => surface::find(server).await,
        None => None,
    };
    if let Some((_, reply)) = &found
        && let Some(control) = &reply.control
    {
        println!("control: {}\n", control.label());
    }
    let live = found.and_then(|(_, reply)| reply.repos.into_iter().find(|r| r.repo == slug));
    let (rows, runs): (Vec<Row>, _) = match &live {
        Some(live) => (live.snapshot.sandboxes.iter().map(Row::from_status).collect(), Some(live)),
        None => {
            let computes = computes(&config, &opts, &fnox, &paths, Default::default(), true)?;
            let labels = Labels::from([(label::FACTORY.into(), FACTORY_LABEL.into()), (label::REPO.into(), slug)]);
            let (sandboxes, unreachable) = list_everywhere(&computes, &labels).await?;
            for name in &unreachable {
                eprintln!("target {name} can't be reached; its sandboxes aren't listed");
            }
            let sandboxes: Vec<Sandbox> = sandboxes.into_iter().map(|(_, s)| s).collect();
            (sandboxes.iter().map(Row::from_sandbox).collect(), None)
        }
    };
    let tasks = tracker
        .in_flight(&config.repo.project, &config.repo.queue.iter().cloned().map(TaskKey).collect::<Vec<_>>())
        .await
        .map_err(reach)?;
    let queue: Vec<TaskKey> = config.repo.queue.iter().cloned().map(TaskKey).collect();
    let mut waiting = tracker.ready(&config.repo.project, &queue, usize::MAX).await.map_err(reach)?;
    // Under a queue only graded tasks are the factory's to start.
    if !queue.is_empty() {
        waiting.retain(|t| t.grade.is_some());
    }
    if let Some(why) = runs.and_then(|live| live.stopped.as_ref()) {
        println!(
            "WARNING: this repo's loop has stopped and takes no new runs: {}\nRestart the server to start it again.\n",
            why.lines().next().unwrap_or_default()
        );
    }
    if let Some(live) = runs {
        print!("{}", render_live(&live.snapshot));
    }
    print!("{}", render_status(&rows, &tasks, &waiting, config.repo.max_sandboxes, status.all));
    println!("\nlog file: {} (written by the server's control plane)", crate::logfile::path(&paths.data_dir).display());
    match saved_endpoint_id(&paths.creds()) {
        Some(id) => println!("\nUI endpoint id: {id}  (allow a UI with `trailhead factory allow <its endpoint id>`)"),
        None => println!("\nUI endpoint id: none yet (the control plane makes it on its first run)"),
    }
    Ok(())
}

pub async fn logs(logs: &LogsOpts) -> Result<()> {
    let paths = trailhead_client::paths::Paths::from_env().ok_or(Error::NoDirs)?;
    let (opts, fnox) = resolve_opts(&logs.opts, &paths, &std::env::current_dir().map_err(Error::backend)?)?;
    let (_, _, config) = load_config(&opts)?;

    let tracker = api_tracker(&opts, &paths)?;
    let key = TaskKey(logs.task.to_ascii_uppercase());
    let task = tracker.show(&key).await.map_err(|e| Error::context(key.0.clone(), reach(e)))?;

    // With a control plane to ask, `--follow` follows its stream of the task's events rather than polling the log.
    let streaming = match (logs.follow, server(&opts, &paths)) {
        (true, Some(server)) => surface::find(server).await.map(|(remote, _)| remote),
        _ => None,
    };
    let source = log_source(&config, &opts, &fnox, &paths, &task).await;
    let (compute, sandbox) = match source {
        Ok(source) => source,
        Err(why) if streaming.is_some() => {
            eprintln!("{why}");
            return stream_events(streaming.as_ref().expect("checked"), &key).await;
        }
        Err(why) => return Err(why),
    };

    let mut printed = String::new();
    loop {
        let log =
            compute.logs(&sandbox.id, DAEMON_PROGRAM).await.map_err(|e| Error::context(sandbox.name.clone(), e))?;
        print!("{}", new_output(&printed, &log));
        printed = log;
        if !logs.follow {
            return Ok(());
        }
        if let Some(remote) = &streaming {
            println!("-- following the control plane's events for {} --", key.0);
            return stream_events(remote, &key).await;
        }
        tokio::select! {
            _ = tokio::time::sleep(FOLLOW_EVERY) => {}
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}

/// A tracker over the local server's API, with the user's token.
pub(crate) fn api_tracker(opts: &Opts, paths: &trailhead_client::paths::Paths) -> Result<RestTracker> {
    let (token, _) = paths.creds().read(trailhead_client::creds::Cred::User).ok_or(Error::NoUserToken)?;
    Ok(RestTracker::new(format!("http://{}", opts.server), Credential { agent: "user".into(), token }))
}

/// `e`, with a hint when the server didn't answer.
pub(crate) fn reach(e: trailhead_factory::Error) -> Error {
    if e.to_string().contains("error sending request") { Error::ServerDown(e.into()) } else { Error::backend(e) }
}

/// The local server's `/v1/factory`, when there is a user token to ask it with.
fn server(opts: &Opts, paths: &trailhead_client::paths::Paths) -> Option<surface::Remote> {
    let (token, _) = paths.creds().read(trailhead_client::creds::Cred::User)?;
    Some(surface::Remote::server(opts.server, token))
}

/// Prints the control plane's events about `key` as they happen, until Ctrl-C or it stops.
async fn stream_events(remote: &surface::Remote, key: &TaskKey) -> Result<()> {
    surface::follow(remote, Some(&key.0), |e| println!("{}", e.event.event.line()), async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}

/// The compute and sandbox whose daemon log belongs to `task`.
async fn log_source(
    config: &trailhead_factory::FactoryConfig,
    opts: &Opts,
    fnox: &crate::fnox::Fnox,
    paths: &trailhead_client::paths::Paths,
    task: &Task,
) -> Result<(Arc<dyn trailhead_factory::Compute>, Sandbox)> {
    let computes = computes(config, opts, fnox, paths, Default::default(), true)?;
    let wanted =
        Labels::from([(label::FACTORY.into(), FACTORY_LABEL.into()), (label::REPO.into(), config.repo.repo().slug())]);
    let (found, unreachable) = list_everywhere(&computes, &wanted).await?;
    let sandboxes: Vec<Sandbox> = found.iter().map(|(_, s)| s.clone()).collect();
    let sandbox = find_sandbox(task, &sandboxes).ok_or_else(|| {
        let unknown = if unreachable.is_empty() {
            String::new()
        } else {
            format!("; target(s) {} couldn't be reached, so it may be there", unreachable.join(", "))
        };
        Error::NoSandbox(format!(
            "{} has no sandbox right now (it isn't running, or its sandbox was released or destroyed){unknown}",
            task.key.0
        ))
    })?;
    let compute = computes[found.iter().find(|(_, s)| s.name == sandbox.name).map_or(0, |(i, _)| *i)].0.clone();
    Ok((compute, sandbox.clone()))
}

/// The sandbox a task is on: the one its run recorded (a pooled sandbox's name, else the run's own), else one labelled
/// with the task. A recorded name is ignored once the run released its sandbox, or when the sandbox is labelled for
/// another task: the pool has reused it since.
fn find_sandbox<'a>(task: &Task, sandboxes: &'a [Sandbox]) -> Option<&'a Sandbox> {
    let released = task.run.released.is_some_and(|at| at > 0);
    let recorded = task.run.host.as_ref().or(task.run.sandbox.as_ref()).filter(|_| !released);
    recorded
        .and_then(|name| sandboxes.iter().find(|s| &s.name == name))
        .filter(|s| s.labels.get(label::TASK).is_none_or(|t| t == &task.key.0))
        .or_else(|| sandboxes.iter().find(|s| s.labels.get(label::TASK) == Some(&task.key.0)))
}

/// What `log` adds to `printed`. The log is a window of its last events, so the start of what was printed may have
/// scrolled off, and the window can start partway through a line (an event is whatever one pipe read returned). If the
/// new log still begins with everything printed, the window hasn't scrolled and the rest is new, however repetitive the
/// lines. Otherwise take the smallest scroll that makes the rest of the printed log the start of the new one, so
/// repeated lines never make a whole copy of them look already printed. In a full window of identical lines the
/// smallest scroll reads as "nothing new": that can't be told from a real "nothing new" without a cursor, and
/// under-printing heartbeats beats duplicating them.
fn new_output<'a>(printed: &str, log: &'a str) -> &'a str {
    if let Some(rest) = log.strip_prefix(printed) {
        return rest;
    }
    for at in 1..printed.len() {
        if !printed.is_char_boundary(at) {
            continue;
        }
        if let Some(new) = log.strip_prefix(&printed[at..]) {
            return new;
        }
    }
    // Nothing in common (the log was restarted): print it all.
    log
}

/// A sandbox as `status` lists it, whether read from compute labels or from the control plane.
struct Row {
    name: String,
    state: String,
    role: String,
    task: String,
    warm: String,
}

impl Row {
    fn from_sandbox(s: &Sandbox) -> Row {
        let get = |k: &str| s.labels.get(k).map_or("-", String::as_str).to_string();
        // A run's own sandbox, not a pooled one, has no state label.
        let state = s.labels.get(label::STATE).map_or("run", String::as_str).to_string();
        Row { name: s.name.clone(), state, role: get(label::ROLE), task: get(label::TASK), warm: get(label::WARM) }
    }

    fn from_status(s: &SandboxStatus) -> Row {
        let state = if s.own && s.state == SandboxState::Busy { "run" } else { s.state.as_str() };
        Row {
            name: s.name.clone(),
            state: state.to_string(),
            role: s.role.clone().unwrap_or_else(|| "-".into()),
            task: s.task.as_ref().map_or("-", |t| t.0.as_str()).to_string(),
            warm: if s.warm { "yes" } else { "-" }.to_string(),
        }
    }
}

/// What only a running control plane knows: itself, its last error and each run's phase and activity.
fn render_live(snapshot: &FactorySnapshot) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(out, "Control plane: {}", control_label(&snapshot.control));
    if let Some((at, message)) = &snapshot.last_error {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        let ago = (now - at).max(0);
        let _ = writeln!(out, "  last error ({ago}s ago): {}", message.lines().next().unwrap_or_default());
    }
    if let Some(why) = &snapshot.holding {
        let _ = writeln!(out, "  holding merges: {why}");
    }
    for t in &snapshot.targets {
        let state = if t.capped { "capped, taking no new runs" } else { "under its cap" };
        let _ = writeln!(out, "  target {}: spent {} of {} today, {state}", t.target, t.spent, t.cap);
    }
    let _ = writeln!(out, "\nRuns in flight ({})", snapshot.runs.len());
    for r in &snapshot.runs {
        let last = r.last_tool.as_ref().map_or(String::new(), |t| format!(", last tool {t}"));
        let _ = writeln!(
            out,
            "  {} {:<16} for {} on {}, step {}, {} tool calls, spend {}{last}",
            r.task.0,
            r.phase.label(),
            since(r.phase_started),
            r.sandbox.as_deref().unwrap_or("-"),
            r.steps,
            r.tool_calls,
            r.spend,
        );
    }
    out.push('\n');
    out
}

/// How long ago `unix` was, as `42s`, `7m` or `1h05m`; `?` when the server didn't say (zero).
fn since(unix: i64) -> String {
    if unix <= 0 {
        return "?".into();
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let secs = (now - unix).max(0);
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        _ => format!("{}h{:02}m", secs / 3600, secs % 3600 / 60),
    }
}

/// `waiting` is the ready tasks the factory could start; `max_sandboxes` the repo's cap.
fn render_status(
    sandboxes: &[Row],
    tasks: &[Task],
    waiting: &[Task],
    max_sandboxes: usize,
    all_finished: bool,
) -> String {
    use std::fmt::Write;
    let mut out = String::new();

    let mut pool: Vec<&Row> = sandboxes.iter().collect();
    pool.sort_by(|a, b| a.name.cmp(&b.name));
    let _ = writeln!(out, "Sandboxes ({})", pool.len());
    if pool.is_empty() {
        let _ = writeln!(out, "  none");
    }
    for s in pool {
        let _ = writeln!(out, "  {:<20} {:<7} {:<9} task {:<10} warm {}", s.name, s.state, s.role, s.task, s.warm);
    }

    // A parked task keeps its sandbox in the cap, so enough of them leave ready tasks waiting on a person's answer.
    let parked = tasks
        .iter()
        .filter(|t| t.status == Status::NeedsInput)
        .filter(|t| sandboxes.iter().any(|s| s.task == t.key.0))
        .count();
    if !waiting.is_empty() && parked > 0 && parked >= max_sandboxes {
        let _ = writeln!(
            out,
            "\n{} ready tasks waiting; all {max_sandboxes} sandboxes held by parked tasks (answer or cancel them)",
            waiting.len()
        );
    }

    let running: Vec<&Task> = tasks.iter().filter(|t| t.status == Status::InProgress).collect();
    let _ = writeln!(out, "\nRunning ({})", running.len());
    for t in &running {
        let _ = writeln!(out, "  {}", task_line(t));
    }

    let (mut finished, mut others): (Vec<&Task>, Vec<&Task>) =
        tasks.iter().filter(|t| t.status != Status::InProgress).partition(|t| t.status.is_closed());
    others.sort_by(|a, b| a.key.cmp(&b.key));
    let _ = writeln!(out, "\nTasks ({})", others.len());
    for t in others {
        let _ = writeln!(out, "  {}", task_line(t));
    }

    // Finished tasks pile up, so only the newest (highest numbered) are listed unless asked for all.
    if !finished.is_empty() {
        finished.sort_by_key(|t| std::cmp::Reverse(key_number(&t.key)));
        let shown = if all_finished { finished.len() } else { finished.len().min(FINISHED_SHOWN) };
        let _ = writeln!(out, "\nFinished ({})", finished.len());
        for t in &finished[..shown] {
            let _ = writeln!(out, "  {}", task_line(t));
        }
        if shown < finished.len() {
            let _ = writeln!(out, "  ... and {} older (`--all` lists them)", finished.len() - shown);
        }
    }
    out
}

/// The number in a key like TRAIL-42, to order by age.
fn key_number(key: &TaskKey) -> u64 {
    key.0.rsplit('-').next().and_then(|n| n.parse().ok()).unwrap_or(0)
}

fn task_line(t: &Task) -> String {
    let run = &t.run;
    let mut line = format!("{} [{}]", t.key.0, t.status.as_str().replace('_', " "));
    line.push_str(&format!(" attempt {}", run.attempt.unwrap_or(0)));
    line.push_str(&format!(", spend {}", run.spend.unwrap_or_default()));
    if let Some(pr) = &run.pr {
        line.push_str(&format!(", PR #{} {}", pr.number, pr.url));
    }
    if let Some(sandbox) = run.host.as_ref().or(run.sandbox.as_ref()).filter(|_| t.status == Status::InProgress) {
        line.push_str(&format!(", on {sandbox}"));
    }
    if let Some(note) = run.failure.as_deref().or(run.merge.as_ref().and_then(|m| m.problem.as_deref())) {
        let note = note.lines().next().unwrap_or_default();
        line.push_str(&format!("\n      last note: {note}"));
    }
    line.push_str(&format!("\n      {}", t.title));
    line
}

/// The sandboxes matching `labels` on every target, each with the index of the target it is on, and the names of the
/// targets that couldn't be reached. An error only when none could.
async fn list_everywhere(computes: &[crate::Built], labels: &Labels) -> Result<(Vec<(usize, Sandbox)>, Vec<String>)> {
    let mut found = Vec::new();
    let mut unreachable = Vec::new();
    let mut last = None;
    for (i, (compute, target)) in computes.iter().enumerate() {
        match compute.list(labels).await {
            Ok(listed) => found.extend(listed.into_iter().map(|s| (i, s))),
            Err(e) => {
                last = Some(Error::context(format!("listing sandboxes on {}", target.name), e));
                unreachable.push(target.name.clone());
            }
        }
    }
    match last {
        Some(last) if unreachable.len() == computes.len() => Err(last),
        _ => Ok((found, unreachable)),
    }
}

#[cfg(test)]
mod tests {
    use trailhead_factory::{Grade, PrRef, Priority, Repo, RunUpdate, SandboxId, Usd};

    use super::*;

    fn sandbox(name: &str, labels: &[(&str, &str)]) -> Sandbox {
        Sandbox {
            id: SandboxId(name.into()),
            name: name.into(),
            labels: labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }

    fn rows(boxes: &[Sandbox]) -> Vec<Row> {
        boxes.iter().map(Row::from_sandbox).collect()
    }

    fn task(key: &str, status: Status, run: RunUpdate) -> Task {
        Task {
            key: TaskKey(key.into()),
            project: "TRAIL".into(),
            title: format!("title of {key}"),
            description: String::new(),
            status,
            grade: Grade::new(2),
            priority: Priority::default(),
            holder: None,
            changes_requested: false,
            run,
        }
    }

    #[test]
    fn follow_prints_only_new_lines_as_the_window_scrolls() {
        assert_eq!(new_output("", "a\nb\n"), "a\nb\n");
        assert_eq!(new_output("a\nb\n", "a\nb\n"), "");
        assert_eq!(new_output("a\nb\n", "a\nb\nc\n"), "c\n");
        // The window dropped its oldest lines.
        let printed = "1\n2\n3\n4\n5\n6\n";
        assert_eq!(new_output(printed, "3\n4\n5\n6\n7\n8\n"), "7\n8\n");
        // Nothing in common (the log was restarted): print it all.
        assert_eq!(new_output("old\n", "fresh\n"), "fresh\n");
        // Repeated lines: a growing window's new copies are new, and an unchanged one prints nothing.
        assert_eq!(new_output("hb\nhb\n", "hb\nhb\nhb\n"), "hb\n");
        assert_eq!(new_output("x\nhb\nhb\n", "x\nhb\nhb\nhb\nhb\n"), "hb\nhb\n");
        assert_eq!(new_output("x\nhb\nhb\n", "x\nhb\nhb\n"), "");
        // A scrolled window ending in repeats still finds the smallest scroll.
        assert_eq!(new_output("a\nb\nhb\n", "b\nhb\nhb\n"), "hb\n");
        // The window starts partway through a line.
        assert_eq!(new_output("abc\nd\n", "c\nd\ne\n"), "e\n");
    }

    #[test]
    fn a_task_is_found_by_its_run_then_by_label() {
        let boxes = [
            sandbox("trailhead-w1", &[("task", "TRAIL-1")]),
            sandbox("trailhead-w2", &[("task", "TRAIL-2")]),
            sandbox("trail-3-a1", &[]),
        ];
        let on_host =
            task("TRAIL-1", Status::InProgress, RunUpdate { host: Some("trailhead-w1".into()), ..Default::default() });
        assert_eq!(find_sandbox(&on_host, &boxes).unwrap().name, "trailhead-w1");
        // The pool reused it for another task, or the run released it: don't show that task's log.
        let reused =
            task("TRAIL-5", Status::InReview, RunUpdate { host: Some("trailhead-w1".into()), ..Default::default() });
        assert!(find_sandbox(&reused, &boxes).is_none());
        let released = task(
            "TRAIL-2",
            Status::InProgress,
            RunUpdate { host: Some("trailhead-w1".into()), released: Some(1), ..Default::default() },
        );
        assert_eq!(find_sandbox(&released, &boxes).unwrap().name, "trailhead-w2");
        let own =
            task("TRAIL-3", Status::InProgress, RunUpdate { sandbox: Some("trail-3-a1".into()), ..Default::default() });
        assert_eq!(find_sandbox(&own, &boxes).unwrap().name, "trail-3-a1");
        let labelled = task("TRAIL-2", Status::InReview, RunUpdate::default());
        assert_eq!(find_sandbox(&labelled, &boxes).unwrap().name, "trailhead-w2");
        assert!(find_sandbox(&task("TRAIL-9", Status::Open, RunUpdate::default()), &boxes).is_none());
    }

    #[test]
    fn status_lists_the_pool_and_each_tasks_outcome() {
        let boxes = [
            sandbox("trailhead-w2", &[("state", "idle"), ("warm", "cp-1")]),
            sandbox("trailhead-w1", &[("state", "busy"), ("task", "TRAIL-7"), ("role", "worker"), ("warm", "cp-2")]),
        ];
        let running = RunUpdate {
            attempt: Some(2),
            host: Some("trailhead-w1".into()),
            spend: Some(Usd::dollars(1.5)),
            ..Default::default()
        };
        let reviewed = RunUpdate {
            attempt: Some(1),
            spend: Some(Usd::dollars(0.25)),
            failure: Some("checks failed\nmore".into()),
            pr: Some(PrRef {
                repo: Repo { owner: "a".into(), name: "b".into() },
                number: 12,
                url: "https://x/12".into(),
            }),
            ..Default::default()
        };
        let out = render_status(
            &rows(&boxes),
            &[task("TRAIL-7", Status::InProgress, running), task("TRAIL-5", Status::InReview, reviewed)],
            &[],
            2,
            false,
        );
        let w1 = out.find("trailhead-w1 ").unwrap();
        assert!(w1 < out.find("trailhead-w2 ").unwrap(), "{out}");
        assert!(out.contains("busy"), "{out}");
        assert!(out.contains("warm cp-2"), "{out}");
        assert!(out.contains("Running (1)"), "{out}");
        assert!(out.contains("TRAIL-7 [in progress] attempt 2, spend $1.5000, on trailhead-w1"), "{out}");
        assert!(out.contains("TRAIL-5 [in review] attempt 1, spend $0.2500, PR #12 https://x/12"), "{out}");
        assert!(out.contains("last note: checks failed\n"), "{out}");
    }

    #[test]
    fn status_lists_only_the_latest_finished_tasks_unless_asked_for_all() {
        let tasks: Vec<Task> = (1..=12)
            .map(|n| {
                task(
                    &format!("TRAIL-{n}"),
                    if n % 2 == 0 { Status::Done } else { Status::Cancelled },
                    Default::default(),
                )
            })
            .chain([task("TRAIL-20", Status::Open, Default::default())])
            .collect();
        let out = render_status(&[], &tasks, &[], 2, false);
        assert!(out.contains("Tasks (1)"), "{out}");
        assert!(out.contains("Finished (12)"), "{out}");
        assert!(out.contains("TRAIL-12 ["), "{out}");
        assert!(out.contains("TRAIL-3 ["), "{out}");
        assert!(!out.contains("TRAIL-2 ["), "{out}");
        assert!(out.contains("... and 2 older"), "{out}");
        let all = render_status(&[], &tasks, &[], 2, true);
        assert!(all.contains("TRAIL-1 ["), "{all}");
        assert!(!all.contains("older"), "{all}");
    }

    #[test]
    fn status_warns_when_parked_tasks_hold_every_place_and_ready_tasks_wait() {
        let boxes = [
            sandbox("trailhead-w1", &[("state", "busy"), ("task", "TRAIL-1")]),
            sandbox("trailhead-w2", &[("state", "busy"), ("task", "TRAIL-2")]),
        ];
        let parked = [
            task("TRAIL-1", Status::NeedsInput, Default::default()),
            task("TRAIL-2", Status::NeedsInput, Default::default()),
        ];
        let waiting = [task("TRAIL-3", Status::Open, Default::default())];
        let out = render_status(&rows(&boxes), &parked, &waiting, 2, false);
        assert!(out.contains("1 ready tasks waiting; all 2 sandboxes held by parked tasks"), "{out}");
        assert!(!render_status(&rows(&boxes), &parked, &[], 2, false).contains("waiting;"));
        assert!(!render_status(&rows(&boxes), &parked, &waiting, 3, false).contains("waiting;"));
    }

    #[test]
    fn a_running_control_plane_adds_its_runs_and_sandbox_states() {
        use trailhead_factory::{ControlState, RunPhase, RunStatus, TargetSpend};
        let snapshot = FactorySnapshot {
            control: ControlState::Reattaching { task: TaskKey("TRAIL-7".into()) },
            last_error: Some((0, "reconcile failed: boom\nmore".into())),
            sandboxes: vec![SandboxStatus {
                name: "trailhead-w1".into(),
                target: None,
                state: SandboxState::Paused,
                task: Some(TaskKey("TRAIL-7".into())),
                role: Some("worker".into()),
                warm: true,
                own: false,
            }],
            runs: vec![RunStatus {
                task: TaskKey("TRAIL-7".into()),
                phase: RunPhase::Preparing { cold: true },
                sandbox: Some("trailhead-w1".into()),
                steps: 3,
                tool_calls: 9,
                spend: Usd::dollars(1.5),
                last_tool: Some("Bash".into()),
                started: 0,
                phase_started: 0,
            }],
            holding: Some("#306 has auto-merge on".into()),
            targets: vec![TargetSpend {
                target: "sprites".into(),
                spent: Usd::dollars(5.0),
                cap: Usd::dollars(5.0),
                capped: true,
            }],
            human_grades: Vec::new(),
            queue: Vec::new(),
            seq: 12,
        };
        let out = render_live(&snapshot);
        assert!(out.contains("Control plane: reattaching to TRAIL-7"), "{out}");
        assert!(out.contains("holding merges: #306 has auto-merge on"), "{out}");
        assert!(out.contains("target sprites: spent $5.0000 of $5.0000 today, capped"), "{out}");
        assert!(out.contains("last error ("), "{out}");
        assert!(out.contains(": reconcile failed: boom\n"), "{out}");
        assert!(out.contains("TRAIL-7 preparing (cold)"), "{out}");
        assert!(out.contains("for ? on trailhead-w1"), "{out}");
        assert!(out.contains("step 3, 9 tool calls, spend $1.5000, last tool Bash"), "{out}");
        let row = Row::from_status(&snapshot.sandboxes[0]);
        assert_eq!((row.state.as_str(), row.warm.as_str(), row.task.as_str()), ("paused", "yes", "TRAIL-7"));
    }

    #[test]
    fn since_reads_as_a_short_age() {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        assert_eq!(since(0), "?");
        assert_eq!(since(now + 5), "0s");
        assert_eq!(since(now - 7 * 60 - 3), "7m");
        assert_eq!(since(now - 3600 - 5 * 60), "1h05m");
    }

    #[test]
    fn an_unanswered_server_gets_a_hint() {
        let down = trailhead_factory::Error::Other("trailhead: error sending request for url (http://x)".into());
        assert!(reach(down).to_string().contains("is the server running?"));
        let refused = trailhead_factory::Error::NotFound("trailhead 404 not_found: no such task".into());
        assert!(!reach(refused).to_string().contains("is the server running?"));
    }
    #[test]
    fn an_empty_factory_says_so() {
        let out = render_status(&[], &[], &[], 2, false);
        assert!(out.contains("Sandboxes (0)\n  none"), "{out}");
    }
}
