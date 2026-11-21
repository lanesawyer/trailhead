//! Where a run stands on the trail: its phase as a share of the climb ([`super::terrain::Trail::at`]), so the higher a
//! hiker is, the further its run has got.

use trailhead_factory::RunPhase;

/// How far up the trail a run in `phase` stands, `0` the foot and `1` the summit. The summit is left for the merge
/// and the stretch below it for the gate, which the spike doesn't place yet.
pub fn distance(phase: RunPhase) -> f32 {
    match phase {
        RunPhase::Preparing { .. } => 0.06,
        RunPhase::Agent => 0.28,
        RunPhase::Checks => 0.5,
        RunPhase::Publishing => 0.66,
        RunPhase::Reviewing => 0.8,
    }
}

/// How far behind the one ahead of it a run in the same phase stands, so two never overlap.
const BEHIND: f32 = 0.035;

/// A hiker on the trail: its task, and how far along it stands.
#[derive(Clone, Debug, PartialEq)]
pub struct Hiker {
    pub key: String,
    pub distance: f32,
}

/// Each run as a hiker at its phase's distance; runs in the same phase stand one behind another in the order of their
/// keys.
pub fn hikers<'a>(runs: impl IntoIterator<Item = (&'a str, RunPhase)>) -> Vec<Hiker> {
    let mut runs: Vec<(&str, RunPhase)> = runs.into_iter().collect();
    runs.sort_by(|a, b| distance(a.1).total_cmp(&distance(b.1)).then(a.0.cmp(b.0)));
    let mut out: Vec<Hiker> = Vec::with_capacity(runs.len());
    let mut behind = 0;
    for (i, (key, phase)) in runs.iter().enumerate() {
        behind = if i > 0 && distance(runs[i - 1].1) == distance(*phase) { behind + 1 } else { 0 };
        let distance = (distance(*phase) - BEHIND * behind as f32).max(0.0);
        out.push(Hiker { key: key.to_string(), distance });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peak::terrain::{Peak, SEED, Trail};

    const PHASES: [RunPhase; 6] = [
        RunPhase::Preparing { cold: true },
        RunPhase::Preparing { cold: false },
        RunPhase::Agent,
        RunPhase::Checks,
        RunPhase::Publishing,
        RunPhase::Reviewing,
    ];

    #[test]
    fn each_phase_stands_higher_than_the_one_before_and_below_the_summit() {
        let trail = Trail::new(&Peak::new(SEED));
        let heights: Vec<f32> = PHASES.iter().map(|p| trail.at(distance(*p)).y).collect();
        assert_eq!(heights[0], heights[1], "a cold start prepares where a warm one does");
        assert!(heights[1..].windows(2).all(|w| w[1] > w[0]), "{heights:?}");
        assert!(PHASES.iter().all(|p| (0.0..1.0).contains(&distance(*p))));
        assert!(heights[5] < trail.at(1.0).y);
    }

    #[test]
    fn runs_in_one_phase_stand_in_line_behind_each_other() {
        let hikers = hikers([("T-9", RunPhase::Agent), ("T-2", RunPhase::Checks), ("T-1", RunPhase::Agent)]);
        let at = |key: &str| hikers.iter().find(|h| h.key == key).unwrap().distance;
        assert_eq!(at("T-1"), distance(RunPhase::Agent));
        assert!(at("T-9") < at("T-1") && at("T-9") > distance(RunPhase::Preparing { cold: false }));
        assert_eq!(at("T-2"), distance(RunPhase::Checks));
        let keys: Vec<&str> = hikers.iter().map(|h| h.key.as_str()).collect();
        assert_eq!(keys, ["T-1", "T-9", "T-2"]);
    }

    #[test]
    fn a_long_line_stops_at_the_foot() {
        let many: Vec<String> = (0..10).map(|i| format!("T-{i}")).collect();
        let hikers = hikers(many.iter().map(|k| (k.as_str(), RunPhase::Preparing { cold: false })));
        assert!(hikers.iter().all(|h| h.distance >= 0.0));
    }
}
