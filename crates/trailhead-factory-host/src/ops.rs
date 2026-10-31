//! `status` and `logs`: read-only views of a running factory, for operating it without curl. They read the same
//! factory file as `run`, take no control-plane lock and open no iroh endpoint, so they work beside a running one.

use std::sync::Arc;
use std::time::Duration;

use trailhead_control::{CoreTracker, FACTORY_LABEL};
use trailhead_core::Store;
use trailhead_factory::compute::label;
use trailhead_factory::{Labels, Sandbox, Status, Task, TaskKey, Tracker};

use crate::{AGENT, Opts, compute, factory_ctx, load_config, resolve_opts};

/// The program the control plane starts the daemon as, whose service log `logs` prints.
const DAEMON_PROGRAM: &str = "trailhead-daemon";
/// How often `logs --follow` asks for more.
const FOLLOW_EVERY: Duration = Duration::from_secs(2);
/// How many of a printed log's last lines `--follow` looks for again in the next fetch.
const OVERLAP_LINES: usize = 5;

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

pub async fn status(opts: &Opts) -> Result<(), String> {
    let paths = trailhead_server::Paths::from_env().ok_or("can't find Trailhead's data and config directories")?;
    let opts = resolve_opts(opts, &paths);
    let (_, _, config) = load_config(&opts)?;
    let (compute, _) = compute(&config.compute, &opts, &paths, Default::default(), true)?;
    let store = Arc::new(Store::open_local(&paths.db()).await.map_err(|e| e.to_string())?);
    let ctx = factory_ctx(&store, false).await?;
    let tracker = CoreTracker::new(store, ctx, AGENT);

    let labels =
        Labels::from([(label::FACTORY.into(), FACTORY_LABEL.into()), (label::REPO.into(), config.repo.repo().slug())]);
    let sandboxes = compute.list(&labels).await.map_err(|e| format!("listing sandboxes: {e}"))?;
    let tasks = tracker
        .in_flight(&config.repo.project, config.repo.queue.clone().map(TaskKey).as_ref())
        .await
        .map_err(|e| e.to_string())?;
    print!("{}", render_status(&sandboxes, &tasks));
    Ok(())
}

pub async fn logs(logs: &LogsOpts) -> Result<(), String> {
    let paths = trailhead_server::Paths::from_env().ok_or("can't find Trailhead's data and config directories")?;
    let opts = resolve_opts(&logs.opts, &paths);
    let (_, _, config) = load_config(&opts)?;
    let (compute, _) = compute(&config.compute, &opts, &paths, Default::default(), true)?;

    let store = Arc::new(Store::open_local(&paths.db()).await.map_err(|e| e.to_string())?);
    let ctx = factory_ctx(&store, false).await?;
    let tracker = CoreTracker::new(store, ctx, AGENT);
    let key = TaskKey(logs.task.to_ascii_uppercase());
    let task = tracker.show(&key).await.map_err(|e| format!("{}: {e}", key.0))?;

    let wanted =
        Labels::from([(label::FACTORY.into(), FACTORY_LABEL.into()), (label::REPO.into(), config.repo.repo().slug())]);
    let sandboxes = compute.list(&wanted).await.map_err(|e| format!("listing sandboxes: {e}"))?;
    let sandbox = find_sandbox(&task, &sandboxes).ok_or_else(|| {
        format!("{} has no sandbox right now (it isn't running, or its sandbox was released or destroyed)", key.0)
    })?;

    let mut printed = String::new();
    loop {
        let log = compute.logs(&sandbox.id, DAEMON_PROGRAM).await.map_err(|e| format!("{}: {e}", sandbox.name))?;
        print!("{}", new_output(&printed, &log));
        printed = log;
        if !logs.follow {
            return Ok(());
        }
        tokio::select! {
            _ = tokio::time::sleep(FOLLOW_EVERY) => {}
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}

/// The sandbox a task is on: the one its run recorded (a pooled sandbox's name, else the run's own), else one labelled
/// with the task.
fn find_sandbox<'a>(task: &Task, sandboxes: &'a [Sandbox]) -> Option<&'a Sandbox> {
    let recorded = task.run.host.as_ref().or(task.run.sandbox.as_ref());
    recorded
        .and_then(|name| sandboxes.iter().find(|s| &s.name == name))
        .or_else(|| sandboxes.iter().find(|s| s.labels.get(label::TASK) == Some(&task.key.0)))
}

/// What `log` adds to `printed`. The log is a window of its last lines, so the part already printed may have scrolled
/// off its start: find the printed log's last few lines in the new one and take what follows them.
fn new_output<'a>(printed: &str, log: &'a str) -> &'a str {
    if printed.is_empty() {
        return log;
    }
    let lines: Vec<&str> = printed.split_inclusive('\n').collect();
    // Fewer lines when the window has already scrolled past the older ones.
    for n in (1..=OVERLAP_LINES.min(lines.len())).rev() {
        let tail = lines[lines.len() - n..].concat();
        if let Some(at) = log.rfind(&tail) {
            return &log[at + tail.len()..];
        }
    }
    if log == printed { "" } else { log }
}

fn render_status(sandboxes: &[Sandbox], tasks: &[Task]) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let none = "-";

    let mut pool: Vec<&Sandbox> = sandboxes.iter().collect();
    pool.sort_by(|a, b| a.name.cmp(&b.name));
    let _ = writeln!(out, "Sandboxes ({})", pool.len());
    if pool.is_empty() {
        let _ = writeln!(out, "  none");
    }
    for s in pool {
        let get = |k: &str| s.labels.get(k).map_or(none, String::as_str);
        let state = match s.labels.get(label::STATE) {
            Some(state) => state.as_str(),
            None => "run", // a run's own sandbox, not a pooled one
        };
        let _ = writeln!(
            out,
            "  {:<20} {:<7} {:<9} task {:<10} warm {}",
            s.name,
            state,
            get(label::ROLE),
            get(label::TASK),
            get(label::WARM)
        );
    }

    let running: Vec<&Task> = tasks.iter().filter(|t| t.status == Status::InProgress).collect();
    let _ = writeln!(out, "\nRunning ({})", running.len());
    for t in &running {
        let _ = writeln!(out, "  {}", task_line(t));
    }

    let mut others: Vec<&Task> = tasks.iter().filter(|t| t.status != Status::InProgress).collect();
    others.sort_by(|a, b| a.key.cmp(&b.key));
    let _ = writeln!(out, "\nTasks ({})", others.len());
    for t in others {
        let _ = writeln!(out, "  {}", task_line(t));
    }
    out
}

fn task_line(t: &Task) -> String {
    let run = &t.run;
    let mut line = format!("{} [{}]", t.key.0, status_name(t.status));
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

fn status_name(s: Status) -> &'static str {
    match s {
        Status::Open => "open",
        Status::InProgress => "in progress",
        Status::NeedsInput => "needs input",
        Status::InReview => "in review",
        Status::Approved => "approved",
        Status::Done => "done",
        Status::Cancelled => "cancelled",
    }
}

#[cfg(test)]
mod tests {
    use trailhead_factory::{PrRef, Repo, RunUpdate, SandboxId, Usd};

    use super::*;

    fn sandbox(name: &str, labels: &[(&str, &str)]) -> Sandbox {
        Sandbox {
            id: SandboxId(name.into()),
            name: name.into(),
            labels: labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }

    fn task(key: &str, status: Status, run: RunUpdate) -> Task {
        Task {
            key: TaskKey(key.into()),
            project: "TRAIL".into(),
            title: format!("title of {key}"),
            description: String::new(),
            status,
            grade: Some(2),
            holder: None,
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
    }

    #[test]
    fn a_task_is_found_by_its_run_then_by_label() {
        let boxes = [
            sandbox("trailhead-w1", &[("task", "TRAIL-1")]),
            sandbox("trailhead-w2", &[("task", "TRAIL-2")]),
            sandbox("trail-3-a1", &[]),
        ];
        let on_host =
            task("TRAIL-2", Status::InProgress, RunUpdate { host: Some("trailhead-w1".into()), ..Default::default() });
        assert_eq!(find_sandbox(&on_host, &boxes).unwrap().name, "trailhead-w1");
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
            &boxes,
            &[task("TRAIL-7", Status::InProgress, running), task("TRAIL-5", Status::InReview, reviewed)],
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
    fn an_empty_factory_says_so() {
        let out = render_status(&[], &[]);
        assert!(out.contains("Sandboxes (0)\n  none"), "{out}");
    }
}
