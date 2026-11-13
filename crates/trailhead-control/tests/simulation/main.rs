//! The factory's lifecycle under random histories (TRAIL-1467).
//!
//! Each seed plays a list of events against the real task engine and the fake sandboxes, daemon and code host: the
//! factory's own passes and restarts, what agents report, what a person does to a task or its PR, what the code host
//! says, and infrastructure trouble. Then the trouble stops, and two things have to hold:
//!
//! 1. **The factory settles.** Given fair passes, it reaches a state where another pass writes nothing.
//! 2. **Someone owes the next move.** Every task is then closed, parked with a question a person can answer, or in
//!    review with only a person's approval missing. Anything else is a task nobody will ever pick up.
//!
//! The seeds are fixed, so a run is the same everywhere and a failure is never luck. `SIM_SEEDS=5000` plays more
//! (worth doing after a change to the lifecycle), and `SIM_SEED=17` plays one. A failure is shrunk to the shortest
//! list of events that still fails in the same way, and printed with what each event did and as code to paste into
//! `regressions.rs`. Fix the factory, not the history: an event the factory can't cope with is a bug, unless what it
//! should do is undecided, and then the event's own guard in `world.rs` says so with the task that decides it.

mod regressions;
mod rng;
mod world;

use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;

use futures_util::FutureExt;
use rng::Rng;
use trailhead_factory::{GateKind, Status, TaskKey, label};
use world::{Ci, Event, Fault, World};

/// What went wrong, and the log of what each event did.
pub struct Failure {
    /// The kind of failure, the same for every history that fails this way: shrinking keeps to it, so a history
    /// isn't shrunk into a different bug.
    pub kind: String,
    pub what: String,
    pub log: Vec<String>,
}

fn events(seed: u64) -> (Vec<u8>, Vec<Event>) {
    let mut rng = Rng::new(seed);
    let grades: Vec<u8> = (0..1 + rng.below(2)).map(|_| 1 + rng.below(3) as u8).collect();
    let len = 6 + rng.below(30);
    let events = (0..len).map(|_| event(&mut rng)).collect();
    (grades, events)
}

fn event(rng: &mut Rng) -> Event {
    let task = rng.below(2) as usize;
    match rng.below(100) {
        0..22 => Event::Tick(rng.next()),
        22..44 => Event::Reconcile(rng.next()),
        44..48 => Event::Restart,
        48..52 => Event::Wait([30, 600, 7200][rng.below(3) as usize]),
        52..60 => Event::Ci(task, [Ci::Pending, Ci::Passed, Ci::Passed, Ci::Failed][rng.below(4) as usize]),
        60..66 => Event::Mergeable(task, [None, Some(true), Some(false), Some(false)][rng.below(4) as usize]),
        66..68 => Event::Behind(task),
        68..71 => Event::Push(task),
        71..74 => Event::PersonMerges(task),
        74..76 => Event::PersonCloses(task),
        76..81 => Event::PersonApproves(task),
        81..85 => Event::PersonRequestsChanges(task),
        85..89 => Event::PersonAnswers(task),
        89..91 => Event::PersonReopens(task),
        91..92 => Event::PersonFinishes(task),
        92..93 => Event::PersonCancels(task),
        _ => Event::Fault(
            [
                Fault::RunFails,
                Fault::RunFailsMidway,
                Fault::RunUnavailable,
                Fault::RunCancelled,
                Fault::PrepareFails,
                Fault::PublishFails,
                Fault::PublishFailsAfterApplying,
                Fault::OpenPrFails,
                Fault::CommentFails,
                Fault::MergeRefused,
                Fault::ComputeDown,
                Fault::ComputeUp,
            ][rng.below(12) as usize],
        ),
    }
}

/// Plays one history on its own paused clock and checks where it ends.
pub fn play(grades: &[u8], events: &[Event]) -> Result<(), Failure> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().start_paused(true).build().unwrap();
    runtime.block_on(async {
        let mut world = World::new(grades).await;
        let played = AssertUnwindSafe(async {
            for event in events {
                world.apply(event).await;
            }
            world.settle().await.map_err(|what| ("the factory never settles".to_string(), what))?;
            check(&world).await
        })
        .catch_unwind()
        .await;
        let (kind, what) = match played {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(failed)) => failed,
            Err(panic) => {
                let text = panic.downcast_ref::<String>().cloned();
                let text = text.or(panic.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
                ("the factory panicked".to_string(), text)
            }
        };
        Err(Failure { kind, what, log: std::mem::take(&mut world.log) })
    })
}

/// Every task has to be somewhere a person or nobody has the next move, and no sandbox may be left working for a
/// run that is over.
async fn check(world: &World) -> Result<(), (String, String)> {
    for sandbox in world.compute.sandboxes() {
        let Some(task) = sandbox
            .labels
            .get(label::TASK)
            .filter(|_| sandbox.labels.get(label::STATE).map(String::as_str) == Some(label::BUSY))
        else {
            continue;
        };
        let status = world.show(&TaskKey(task.clone())).await.status;
        // A parked task keeps its sandbox for the answer.
        if status != Status::NeedsInput {
            let why = "a sandbox is still busy for a task that isn't running or parked";
            return Err((
                why.to_string(),
                format!("{why}: {} for {task}, which is {status:?}\n  {:?}", sandbox.name, sandbox.labels),
            ));
        }
    }
    for key in &world.tasks {
        let task = world.show(key).await;
        let stuck = |why: &str| {
            let what = format!(
                "{} is {:?} and nobody owes it a move: {why}\n  holder {:?}, changes_requested {}, run {:?}",
                key.0, task.status, task.holder, task.changes_requested, task.run
            );
            Err((why.to_string(), what))
        };
        match task.status {
            Status::Done | Status::Cancelled => {}
            Status::NeedsInput if world.asked(key).await => {}
            Status::NeedsInput => return stuck("it is parked without an open question"),
            Status::Open if task.run.sandbox.is_none() => return stuck("the factory never started it"),
            Status::Open => return stuck("it is back in the queue and the factory doesn't start it"),
            Status::InProgress => return stuck("it is in progress with no run going"),
            Status::Approved => return stuck("it is approved and the merger doesn't merge it"),
            Status::InReview => {
                let Some(pr) = &task.run.pr else { return stuck("it is in review without a PR") };
                let open = world.codehost.prs().iter().any(|p| p.pr == *pr && !p.merged && !p.closed);
                if !open {
                    return stuck("its PR is merged or closed");
                }
                if task.changes_requested {
                    return stuck("changes were requested and no rework starts");
                }
                let Some(decision) = &task.run.merge else { return stuck("the merger never decided on it") };
                if decision.waiting != [GateKind::Human] {
                    return stuck("it is in review and the merger waits on more than a person");
                }
            }
        }
    }
    Ok(())
}

/// Drops events, in halves then one by one, for as long as the history still fails.
fn shrink(grades: &[u8], events: Vec<Event>) -> (Vec<Event>, Failure) {
    let Err(mut failure) = play(grades, &events) else { panic!("only a failing history is shrunk") };
    let mut events = events;
    let mut chunk = events.len().div_ceil(2);
    while chunk > 0 {
        let mut at = 0;
        while at < events.len() {
            let mut fewer = events.clone();
            fewer.drain(at..(at + chunk).min(events.len()));
            match play(grades, &fewer) {
                Err(f) if f.kind == failure.kind => (events, failure) = (fewer, f),
                _ => at += chunk,
            }
        }
        chunk = if chunk == 1 { 0 } else { chunk.div_ceil(2) };
    }
    (events, failure)
}

fn report(grades: &[u8], events: &[Event], failure: &Failure) -> String {
    let list: Vec<String> = events.iter().map(|e| format!("        {e:?},")).collect();
    format!(
        "{}\n\nas a regression:\n    replay(&{grades:?}, &[\n{}\n    ]);\n\nwhat happened:\n  {}",
        failure.what,
        list.join("\n"),
        failure.log.join("\n  ")
    )
}

/// Plays a pinned history, for `regressions.rs`.
pub fn replay(grades: &[u8], events: &[Event]) {
    if let Err(failure) = play(grades, events) {
        panic!("{}", report(grades, events, &failure));
    }
}

fn env(name: &str) -> Option<u64> {
    std::env::var(name).ok().map(|v| v.parse().unwrap_or_else(|_| panic!("{name} must be a number, got {v}")))
}

#[test]
fn every_history_settles_with_someone_owing_the_next_move() {
    let seeds = match env("SIM_SEED") {
        Some(seed) => seed..seed + 1,
        None => 0..env("SIM_SEEDS").unwrap_or(300),
    };
    // One shrunk history per kind of failure is enough to act on; the rest are counted.
    let mut kinds: BTreeMap<String, (usize, String)> = BTreeMap::new();
    for seed in seeds.clone() {
        let (grades, events) = events(seed);
        let Err(failure) = play(&grades, &events) else { continue };
        if let Some((count, _)) = kinds.get_mut(&failure.kind) {
            *count += 1;
            continue;
        }
        let (events, failure) = shrink(&grades, events);
        kinds.insert(failure.kind.clone(), (1, format!("seed {seed}: {}", report(&grades, &events, &failure))));
    }
    let reports: Vec<String> =
        kinds.iter().map(|(kind, (count, report))| format!("{kind} ({count} seeds), first at {report}")).collect();
    assert!(kinds.is_empty(), "seeds {seeds:?} failed in {} ways.\n\n{}", kinds.len(), reports.join("\n\n----\n\n"));
}
