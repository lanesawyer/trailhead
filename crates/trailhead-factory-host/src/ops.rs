//! `status` and `logs`: read-only views of a running factory, for operating it without curl. They read the same
//! factory file as `run`, take no control-plane lock and open no iroh endpoint, so they work beside a running one.

use std::sync::Arc;
use std::time::Duration;

use trailhead_control::{CoreTracker, FACTORY_LABEL};
use trailhead_core::Store;
use trailhead_factory::compute::label;
use trailhead_factory::status::control_label;
use trailhead_factory::{
    FactorySnapshot, Labels, Sandbox, SandboxState, SandboxStatus, Status, Task, TaskKey, Tracker,
};

use crate::{AGENT, Opts, computes, factory_ctx, load_config, resolve_opts, saved_endpoint_id, surface};

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
    let (opts, fnox) = resolve_opts(opts, &paths)?;
    let (_, _, config) = load_config(&opts)?;
    let store = Arc::new(Store::open_local(&paths.db()).await.map_err(|e| e.to_string())?);
    let ctx = factory_ctx(&store, false).await?;
    let tracker = CoreTracker::new(store, ctx, AGENT);

    // A running control plane knows its sandboxes and runs itself, so it is asked first; the compute is only read when
    // there isn't one (or it isn't running this repo).
    let slug = config.repo.repo().slug();
    let live = surface::fetch(&paths.data_dir.join("factory"))
        .await
        .and_then(|reply| reply.repos.into_iter().find(|r| r.repo == slug));
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
        .map_err(|e| e.to_string())?;
    let queue: Vec<TaskKey> = config.repo.queue.iter().cloned().map(TaskKey).collect();
    let mut waiting = tracker.ready(&config.repo.project, &queue, usize::MAX).await.map_err(|e| e.to_string())?;
    // Under a queue only graded tasks are the factory's to start.
    if !queue.is_empty() {
        waiting.retain(|t| t.grade.is_some());
    }
    if let Some(live) = runs {
        print!("{}", render_live(&live.snapshot));
    }
    print!("{}", render_status(&rows, &tasks, &waiting, config.repo.max_sandboxes));
    println!("\nlog file: {} (written by `run`)", crate::logfile::path(&paths.data_dir).display());
    match saved_endpoint_id(&paths.creds()) {
        Some(id) => println!("\nUI endpoint id: {id}  (allow a UI with `trailhead factory allow <its endpoint id>`)"),
        None => println!("\nUI endpoint id: none yet (the control plane makes it on its first run)"),
    }
    Ok(())
}

pub async fn logs(logs: &LogsOpts) -> Result<(), String> {
    let paths = trailhead_server::Paths::from_env().ok_or("can't find Trailhead's data and config directories")?;
    let (opts, fnox) = resolve_opts(&logs.opts, &paths)?;
    let (_, _, config) = load_config(&opts)?;

    let store = Arc::new(Store::open_local(&paths.db()).await.map_err(|e| e.to_string())?);
    let ctx = factory_ctx(&store, false).await?;
    let tracker = CoreTracker::new(store, ctx, AGENT);
    let key = TaskKey(logs.task.to_ascii_uppercase());
    let task = tracker.show(&key).await.map_err(|e| format!("{}: {e}", key.0))?;

    // With a control plane to ask, `--follow` follows its stream of the task's events rather than polling the log.
    let dir = paths.data_dir.join("factory");
    let streaming = logs.follow && surface::fetch(&dir).await.is_some();
    let source = log_source(&config, &opts, &fnox, &paths, &task).await;
    let (compute, sandbox) = match source {
        Ok(source) => source,
        Err(why) if streaming => {
            eprintln!("{why}");
            return stream_events(&dir, &key).await;
        }
        Err(why) => return Err(why),
    };

    let mut printed = String::new();
    loop {
        let log = compute.logs(&sandbox.id, DAEMON_PROGRAM).await.map_err(|e| format!("{}: {e}", sandbox.name))?;
        print!("{}", new_output(&printed, &log));
        printed = log;
        if !logs.follow {
            return Ok(());
        }
        if streaming {
            println!("-- following the control plane's events for {} --", key.0);
            return stream_events(&dir, &key).await;
        }
        tokio::select! {
            _ = tokio::time::sleep(FOLLOW_EVERY) => {}
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}

/// Prints the control plane's events about `key` as they happen, until Ctrl-C or it stops.
async fn stream_events(dir: &std::path::Path, key: &TaskKey) -> Result<(), String> {
    surface::follow(dir, Some(&key.0), |e| println!("{}", e.event.event.line()), async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}

/// The compute and sandbox whose daemon log belongs to `task`.
async fn log_source(
    config: &trailhead_factory::FactoryConfig,
    opts: &Opts,
    fnox: &crate::fnox::Fnox,
    paths: &trailhead_server::Paths,
    task: &Task,
) -> Result<(Arc<dyn trailhead_factory::Compute>, Sandbox), String> {
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
        format!(
            "{} has no sandbox right now (it isn't running, or its sandbox was released or destroyed){unknown}",
            task.key.0
        )
    })?;
    let compute = computes[found.iter().find(|(_, s)| s.name == sandbox.name).map_or(0, |(i, _)| *i)].0.clone();
    Ok((compute, sandbox.clone()))
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
            "  {} {:<16} on {}, step {}, {} tool calls, spend {}{last}",
            r.task.0,
            r.phase.label(),
            r.sandbox.as_deref().unwrap_or("-"),
            r.steps,
            r.tool_calls,
            r.spend,
        );
    }
    out.push('\n');
    out
}

/// `waiting` is the ready tasks the factory could start; `max_sandboxes` the repo's cap.
fn render_status(sandboxes: &[Row], tasks: &[Task], waiting: &[Task], max_sandboxes: usize) -> String {
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
async fn list_everywhere(
    computes: &[crate::Built],
    labels: &Labels,
) -> Result<(Vec<(usize, Sandbox)>, Vec<String>), String> {
    let mut found = Vec::new();
    let mut unreachable = Vec::new();
    let mut last = String::new();
    for (i, (compute, target)) in computes.iter().enumerate() {
        match compute.list(labels).await {
            Ok(listed) => found.extend(listed.into_iter().map(|s| (i, s))),
            Err(e) => {
                last = format!("listing sandboxes on {}: {e}", target.name);
                unreachable.push(target.name.clone());
            }
        }
    }
    if unreachable.len() == computes.len() { Err(last) } else { Ok((found, unreachable)) }
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
            &rows(&boxes),
            &[task("TRAIL-7", Status::InProgress, running), task("TRAIL-5", Status::InReview, reviewed)],
            &[],
            2,
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
        let out = render_status(&rows(&boxes), &parked, &waiting, 2);
        assert!(out.contains("1 ready tasks waiting; all 2 sandboxes held by parked tasks"), "{out}");
        assert!(!render_status(&rows(&boxes), &parked, &[], 2).contains("waiting;"));
        assert!(!render_status(&rows(&boxes), &parked, &waiting, 3).contains("waiting;"));
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
            }],
            holding: Some("#306 has auto-merge on".into()),
            targets: vec![TargetSpend {
                target: "sprites".into(),
                spent: Usd::dollars(5.0),
                cap: Usd::dollars(5.0),
                capped: true,
            }],
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
        assert!(out.contains("step 3, 9 tool calls, spend $1.5000, last tool Bash"), "{out}");
        let row = Row::from_status(&snapshot.sandboxes[0]);
        assert_eq!((row.state.as_str(), row.warm.as_str(), row.task.as_str()), ("paused", "yes", "TRAIL-7"));
    }

    #[test]
    fn an_empty_factory_says_so() {
        let out = render_status(&[], &[], &[], 2);
        assert!(out.contains("Sandboxes (0)\n  none"), "{out}");
    }
}
