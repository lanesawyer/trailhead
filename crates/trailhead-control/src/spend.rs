//! What a paid target has cost today (`docs/factory/targets.md`). The cost is estimated from the time runs hold
//! sandboxes on the target, priced as if each ran flat out, and kept in a file so a restart doesn't reset the day.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tokio::time::Instant;
use trailhead_factory::{Role, TaskKey, Usd};

/// The day's spending by target, as kept in the file.
#[derive(Default, Serialize, Deserialize)]
struct Day {
    /// The machine's local day, `YYYY-MM-DD`.
    day: String,
    /// Microdollars by target name.
    spent: HashMap<String, u64>,
}

struct Live {
    target: String,
    /// What a sandbox here costs an hour, in microdollars.
    hourly: u64,
    /// The last moment its time was counted into the day.
    since: Instant,
}

/// The day so far, and the runs still adding to it.
#[derive(Default)]
struct State {
    day: Day,
    live: HashMap<(TaskKey, Role), Live>,
}

#[derive(Default)]
pub(crate) struct Ledger {
    path: Option<PathBuf>,
    state: Mutex<State>,
}

/// Today's date on the machine, which is what a day means for the cap.
pub(crate) fn today() -> String {
    jiff::Zoned::now().date().to_string()
}

impl Ledger {
    /// A ledger kept in `path`, starting from what it holds if that is from `today`. A file that can't be read
    /// starts the day over, which is logged.
    pub(crate) fn kept_in(path: PathBuf, today: &str) -> Ledger {
        let day = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str::<Day>(&text).unwrap_or_else(|e| {
                tracing::warn!(path = %path.display(), error = %e, "can't read the spend file; counting today from nothing");
                Day::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Day::default(),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "can't read the spend file; counting today from nothing");
                Day::default()
            }
        };
        let day = if day.day == today { day } else { Day { day: today.into(), ..Day::default() } };
        Ledger { path: Some(path), state: Mutex::new(State { day, live: HashMap::new() }) }
    }

    /// Starts counting `key`'s time on `target`, where a sandbox costs `hourly` an hour.
    pub(crate) fn begin(&self, key: (TaskKey, Role), target: &str, hourly: Usd, now: Instant) {
        let live = Live { target: target.into(), hourly: hourly.0, since: now };
        self.state.lock().unwrap().live.insert(key, live);
    }

    /// Stops counting `key`, after adding its time so far.
    pub(crate) fn end(&self, key: &(TaskKey, Role), now: Instant, today: &str) {
        let mut state = self.state.lock().unwrap();
        Self::accrue(&mut state, now, today);
        if state.live.remove(key).is_some() {
            self.save(&state.day);
        }
    }

    /// Counts the time live runs have held sandboxes so far and saves it, so a crash or restart mid-run forgets
    /// little. Called every pass of the run loop, whether or not a sandbox is free or a target has a cap.
    pub(crate) fn checkpoint(&self, now: Instant, today: &str) {
        let mut state = self.state.lock().unwrap();
        if Self::accrue(&mut state, now, today) {
            self.save(&state.day);
        }
    }

    /// What `target` has cost today so far, counting the runs still going.
    pub(crate) fn spent(&self, target: &str, now: Instant, today: &str) -> Usd {
        let mut state = self.state.lock().unwrap();
        if Self::accrue(&mut state, now, today) {
            self.save(&state.day);
        }
        Usd(state.day.spent.get(target).copied().unwrap_or(0))
    }

    /// Moves the time live runs have held sandboxes since last counted into the day, and starts a new day when the
    /// date has changed (time since the last count goes to the new day). Whether anything changed.
    fn accrue(state: &mut State, now: Instant, today: &str) -> bool {
        let mut changed = false;
        if state.day.day != today {
            state.day = Day { day: today.into(), ..Day::default() };
            changed = true;
        }
        let State { day, live } = state;
        for run in live.values_mut() {
            let ms = now.saturating_duration_since(run.since).as_millis();
            let cost = (ms * run.hourly as u128 / 3_600_000) as u64;
            if cost > 0 {
                // Keep the remainder of a microdollar: advance `since` only by the time the cost paid for.
                let paid = cost as u128 * 3_600_000 / run.hourly.max(1) as u128;
                run.since += std::time::Duration::from_millis(paid as u64);
                *day.spent.entry(run.target.clone()).or_default() += cost;
                changed = true;
            }
        }
        changed
    }

    fn save(&self, day: &Day) {
        let Some(path) = &self.path else { return };
        let write = || -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, serde_json::to_vec(day).map_err(std::io::Error::other)?)?;
            std::fs::rename(&tmp, path)
        };
        if let Err(e) = write() {
            tracing::warn!(path = %path.display(), error = %e, "can't save the spend file; a restart will forget today's spend");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn key(n: &str) -> (TaskKey, Role) {
        (TaskKey(n.into()), Role::Worker)
    }

    #[tokio::test]
    async fn live_runs_count_and_end_into_the_day() {
        let ledger = Ledger::default();
        let t0 = Instant::now();
        ledger.begin(key("T-1"), "sprites", Usd::dollars(0.4), t0);
        let later = t0 + Duration::from_secs(1800);
        assert_eq!(ledger.spent("sprites", later, "d1"), Usd::dollars(0.2));
        assert_eq!(ledger.spent("local", later, "d1"), Usd::ZERO);
        ledger.end(&key("T-1"), later + Duration::from_secs(1800), "d1");
        assert_eq!(ledger.spent("sprites", later + Duration::from_secs(7200), "d1"), Usd::dollars(0.4));
    }

    #[tokio::test]
    async fn a_new_day_starts_from_nothing() {
        let ledger = Ledger::default();
        let t0 = Instant::now();
        ledger.begin(key("T-1"), "sprites", Usd::dollars(1.0), t0);
        let later = t0 + Duration::from_secs(3600);
        assert_eq!(ledger.spent("sprites", later, "d1"), Usd::dollars(1.0));
        assert_eq!(ledger.spent("sprites", later, "d2"), Usd::ZERO);
    }

    #[tokio::test]
    async fn the_day_survives_a_restart_but_not_a_new_day() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("spend.json");
        let ledger = Ledger::kept_in(path.clone(), "d1");
        let t0 = Instant::now();
        ledger.begin(key("T-1"), "sprites", Usd::dollars(1.0), t0);
        ledger.end(&key("T-1"), t0 + Duration::from_secs(1800), "d1");
        let now = t0 + Duration::from_secs(1800);
        assert_eq!(Ledger::kept_in(path.clone(), "d1").spent("sprites", now, "d1"), Usd::dollars(0.5));
        assert_eq!(Ledger::kept_in(path, "d2").spent("sprites", now, "d2"), Usd::ZERO);
    }

    #[tokio::test]
    async fn a_checkpoint_saves_time_counted_so_far() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("spend.json");
        let ledger = Ledger::kept_in(path.clone(), "d1");
        let t0 = Instant::now();
        ledger.begin(key("T-1"), "sprites", Usd::dollars(1.0), t0);
        let now = t0 + Duration::from_secs(1800);
        ledger.checkpoint(now, "d1");
        // The run is still live, as after a crash: a fresh ledger reads what the checkpoint saved.
        assert_eq!(Ledger::kept_in(path, "d1").spent("sprites", now, "d1"), Usd::dollars(0.5));
    }

    #[tokio::test]
    async fn an_unreadable_file_starts_the_day_over() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("spend.json");
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(Ledger::kept_in(path, "d1").spent("sprites", Instant::now(), "d1"), Usd::ZERO);
    }
}
