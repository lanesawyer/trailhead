//! The factory half of the ECS mirror (`docs/factory/ui.md#factory-status`): what only the control plane knows, from
//! the snapshot and stream it serves over `trailhead/ui/0`. Sandboxes (the campsites, idle ones too) and runs (their
//! phase, steps, spend and last tool) are entities; the control plane's own state and a short log of live activity
//! are in [`FactoryStatus`].
//!
//! Like Trailhead's own mirror, a snapshot replaces what a repo had and an event updates it, and an event no newer
//! than the repo's last is a duplicate. The stream numbers from 1 on every control plane boot, but each connection
//! starts with a snapshot, so a restarted control plane never mixes its numbers with its predecessor's.
//!
//! When the server can't be reached everything here stays as it was, and [`FactoryStatus::link`] says
//! [`FactoryLink::Unreachable`]. The control plane runs inside the server, so one that isn't running in a live server
//! (no repos, disabled, starting) says so in the status frame's `control`, which is [`FactoryLink::Control`].

use std::collections::{BTreeMap, HashMap, VecDeque};

use bevy::prelude::*;
use trailhead_factory::{
    ControlState, FactoryEvent, FactoryState, Grade, RunPhase, RunStatus, RunUpdate, SandboxStatus, Sequenced, TaskKey,
    Usd,
};
use trailhead_wire::ui::RepoSnapshot;

/// How many lines of live activity are kept.
const ACTIVITY: usize = 200;

/// What the net thread reports about the factory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactoryUpdate {
    /// Every repo's state, replacing what the mirror had. Always the first thing on a connection.
    Snapshot(Vec<RepoSnapshot>),
    Event {
        repo: String,
        event: Sequenced,
    },
    /// The server's control plane state, from the status frame ahead of its snapshot; `None` from a server that
    /// doesn't send one.
    Control(Option<FactoryState>),
    /// The server can't be reached: the mirror keeps its last state and says so.
    Unreachable,
    /// The control plane answered a request with "I don't know that": it is older than this Lookout. Carries the
    /// message to show.
    TooOld(String),
    /// The control plane refused the status request (this Lookout isn't allowed, or it serves no status). Carries the
    /// message to show, which says when the next try is.
    Refused(String),
}

/// A [`FactoryUpdate`] on its way to the mirror.
#[derive(Message, Debug, Clone)]
pub struct FactoryItem(pub FactoryUpdate);

/// A pooled sandbox: a campsite on the map.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct Campsite {
    pub repo: String,
    pub status: SandboxStatus,
}

/// A run in flight.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub repo: String,
    pub status: RunStatus,
}

/// Whether the factory is being heard from.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum FactoryLink {
    /// Nothing heard yet.
    #[default]
    Unknown,
    /// Connected: the mirror is current.
    Running,
    /// Connected, but the server's control plane isn't running (disabled, no repos, still starting).
    Control(FactoryState),
    /// Not reachable. The mirror is the last state heard.
    Unreachable,
    /// Reachable, but older than this Lookout: it doesn't know the status request. The message says what to do.
    TooOld(String),
    /// The control plane refused the status request (this Lookout isn't allowed, or it serves no status). Carries the
    /// message to show, which says when the next try is.
    Refused(String),
}

/// One repo's control plane state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoState {
    pub control: ControlState,
    /// The repo's queue task keys, when the control plane reports them.
    pub queue: Vec<String>,
    /// Unix seconds and the message.
    pub last_error: Option<(i64, String)>,
    /// The grades whose gate wants a person.
    pub human_grades: Vec<Grade>,
    /// The last event applied, or the snapshot's.
    pub seq: u64,
}

/// A line of live activity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activity {
    pub repo: String,
    pub seq: u64,
    pub line: String,
}

#[derive(Resource, Debug, Default)]
pub struct FactoryStatus {
    pub link: FactoryLink,
    /// What the last status frame said of the server's control plane.
    pub control: Option<FactoryState>,
    pub repos: BTreeMap<String, RepoState>,
    /// Newest last, at most [`ACTIVITY`] of them.
    pub activity: VecDeque<Activity>,
}

impl FactoryStatus {
    /// What the map shows beside the factory: why it isn't running, or that the server can't be reached.
    pub fn label(&self) -> String {
        match &self.link {
            FactoryLink::Unknown => "factory: connecting".into(),
            FactoryLink::Running => "factory running".into(),
            FactoryLink::Control(state) => format!("factory {}", state.label()),
            FactoryLink::Unreachable => "unreachable".into(),
            FactoryLink::TooOld(why) | FactoryLink::Refused(why) => why.clone(),
        }
    }

    /// The grades whose gate wants a person, as the factory says: every repo's, or the default table's (3 and 4) until
    /// a snapshot has said.
    pub fn human_grades(&self) -> Vec<Grade> {
        if self.repos.is_empty() {
            return Grade::ALL.into_iter().filter(|g| g.get() >= 3).collect();
        }
        Grade::ALL.into_iter().filter(|g| self.repos.values().any(|r| r.human_grades.contains(g))).collect()
    }

    /// The queue tasks the factory works, as its repos report them, each once.
    pub fn queues(&self) -> Vec<String> {
        let mut keys: Vec<String> = Vec::new();
        for key in self.repos.values().flat_map(|r| &r.queue) {
            if !keys.contains(key) {
                keys.push(key.clone());
            }
        }
        keys
    }

    /// The control plane's state for the map, repo by repo.
    pub fn controls(&self) -> impl Iterator<Item = (&str, String)> {
        self.repos.iter().map(|(repo, s)| (repo.as_str(), trailhead_factory::status::control_label(&s.control)))
    }
}

/// Which entity holds what.
#[derive(Resource, Default, Debug)]
pub struct FactoryMirror {
    campsites: HashMap<(String, String), (Entity, SandboxStatus)>,
    runs: HashMap<(String, TaskKey), (Entity, RunStatus)>,
}

impl FactoryMirror {
    pub fn campsite(&self, repo: &str, name: &str) -> Option<Entity> {
        self.campsites.get(&(repo.to_string(), name.to_string())).map(|(e, _)| *e)
    }

    pub fn run(&self, repo: &str, task: &str) -> Option<Entity> {
        self.runs.get(&(repo.to_string(), TaskKey(task.to_string()))).map(|(e, _)| *e)
    }

    fn set_campsite(&mut self, commands: &mut Commands, repo: &str, status: SandboxStatus) {
        let key = (repo.to_string(), status.name.clone());
        let component = Campsite { repo: repo.to_string(), status: status.clone() };
        match self.campsites.get_mut(&key) {
            Some((_, have)) if *have == status => {}
            Some((entity, have)) => {
                commands.entity(*entity).insert(component);
                *have = status;
            }
            None => {
                let entity = commands.spawn(component).id();
                self.campsites.insert(key, (entity, status));
            }
        }
    }

    fn set_run(&mut self, commands: &mut Commands, repo: &str, status: RunStatus) {
        let key = (repo.to_string(), status.task.clone());
        let component = Run { repo: repo.to_string(), status: status.clone() };
        match self.runs.get_mut(&key) {
            Some((_, have)) if *have == status => {}
            Some((entity, have)) => {
                commands.entity(*entity).insert(component);
                *have = status;
            }
            None => {
                let entity = commands.spawn(component).id();
                self.runs.insert(key, (entity, status));
            }
        }
    }

    /// Changes a run we know of, if we do. Events about one we missed the start of wait for its next phase.
    fn update_run(&mut self, commands: &mut Commands, repo: &str, task: &TaskKey, change: impl FnOnce(&mut RunStatus)) {
        if let Some((_, have)) = self.runs.get(&(repo.to_string(), task.clone())) {
            let mut next = have.clone();
            change(&mut next);
            self.set_run(commands, repo, next);
        }
    }

    fn apply_snapshot(&mut self, commands: &mut Commands, status: &mut FactoryStatus, snapshot: RepoSnapshot) {
        let RepoSnapshot { repo, snapshot } = snapshot;
        let campsites: Vec<_> = self.campsites.keys().filter(|(r, _)| *r == repo).cloned().collect();
        for key in campsites {
            if !snapshot.sandboxes.iter().any(|s| s.name == key.1)
                && let Some((e, _)) = self.campsites.remove(&key)
            {
                commands.entity(e).despawn();
            }
        }
        let runs: Vec<_> = self.runs.keys().filter(|(r, _)| *r == repo).cloned().collect();
        for key in runs {
            if !snapshot.runs.iter().any(|r| r.task == key.1)
                && let Some((e, _)) = self.runs.remove(&key)
            {
                commands.entity(e).despawn();
            }
        }
        for sandbox in snapshot.sandboxes {
            self.set_campsite(commands, &repo, sandbox);
        }
        for run in snapshot.runs {
            self.set_run(commands, &repo, run);
        }
        status.repos.insert(
            repo,
            RepoState {
                control: snapshot.control,
                last_error: snapshot.last_error,
                human_grades: snapshot.human_grades,
                queue: snapshot.queue.into_iter().map(|k| k.0).collect(),
                seq: snapshot.seq,
            },
        );
    }

    fn apply_event(&mut self, commands: &mut Commands, status: &mut FactoryStatus, repo: &str, event: Sequenced) {
        // Without a snapshot there is nothing to apply an event to; the first thing on a connection is one.
        let Some(state) = status.repos.get_mut(repo) else { return };
        if event.seq <= state.seq {
            return;
        }
        state.seq = event.seq;
        let line = event.event.line();
        match event.event {
            FactoryEvent::Control { control } => state.control = control,
            FactoryEvent::Error { message } => state.last_error = Some((unix_now(), message)),
            // Shown in the activity log only for now.
            FactoryEvent::Holding { .. } | FactoryEvent::Unknown => {}
            FactoryEvent::SandboxChanged { sandbox } => self.set_campsite(commands, repo, sandbox),
            FactoryEvent::SandboxGone { name } => {
                if let Some((entity, _)) = self.campsites.remove(&(repo.to_string(), name)) {
                    commands.entity(entity).despawn();
                }
            }
            FactoryEvent::RunPhase { task, phase } => self.run_phase(commands, repo, task, phase),
            FactoryEvent::RunEnded { task } => {
                if let Some((entity, _)) = self.runs.remove(&(repo.to_string(), task)) {
                    commands.entity(entity).despawn();
                }
            }
            FactoryEvent::Step { task, step, spend } => self.update_run(commands, repo, &task, |r| {
                r.steps = step;
                r.spend = spend;
            }),
            FactoryEvent::ToolCall { task, name, .. } => self.update_run(commands, repo, &task, |r| {
                r.tool_calls += 1;
                r.last_tool = Some(name);
            }),
        }
        status.activity.push_back(Activity { repo: repo.to_string(), seq: event.seq, line });
        while status.activity.len() > ACTIVITY {
            status.activity.pop_front();
        }
    }

    fn run_phase(&mut self, commands: &mut Commands, repo: &str, task: TaskKey, phase: RunPhase) {
        let have = self.runs.get(&(repo.to_string(), task.clone())).map(|(_, r)| r.clone());
        let status = match have {
            Some(run) => RunStatus { phase, phase_started: unix_now(), ..run },
            None => RunStatus {
                task,
                phase,
                sandbox: None,
                steps: 0,
                tool_calls: 0,
                spend: Usd::ZERO,
                last_tool: None,
                started: unix_now(),
                phase_started: unix_now(),
            },
        };
        self.set_run(commands, repo, status);
    }
}

/// Whether the factory has taken `task` up: started a run on it (even one that failed before its agent started) or
/// opened its PR. Its metadata alone doesn't say so: the control plane also marks a queued task it skipped for having
/// no grade, which is how work done by hand is filed under a queue task (TRAIL-1159).
pub fn factory_ran(task: &trailhead_model::Task) -> bool {
    RunUpdate::of(task)
        .is_ok_and(|run| run.attempt.is_some() || run.sandbox.is_some() || run.retries.is_some() || run.pr.is_some())
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

pub fn apply_factory(
    mut items: MessageReader<FactoryItem>,
    mut mirror: ResMut<FactoryMirror>,
    mut status: ResMut<FactoryStatus>,
    mut commands: Commands,
) {
    for FactoryItem(update) in items.read() {
        match update.clone() {
            FactoryUpdate::Snapshot(repos) => {
                status.link = match status.control.clone() {
                    Some(state) if state != FactoryState::Running => FactoryLink::Control(state),
                    _ => FactoryLink::Running,
                };
                // A repo the control plane no longer serves goes with it.
                let gone: Vec<_> =
                    status.repos.keys().filter(|r| !repos.iter().any(|s| &s.repo == *r)).cloned().collect();
                for repo in gone {
                    status.repos.remove(&repo);
                    mirror.campsites.retain(|(r, _), (e, _)| {
                        let keep = *r != repo;
                        if !keep {
                            commands.entity(*e).despawn();
                        }
                        keep
                    });
                    mirror.runs.retain(|(r, _), (e, _)| {
                        let keep = *r != repo;
                        if !keep {
                            commands.entity(*e).despawn();
                        }
                        keep
                    });
                }
                for repo in repos {
                    mirror.apply_snapshot(&mut commands, &mut status, repo);
                }
            }
            FactoryUpdate::Event { repo, event } => mirror.apply_event(&mut commands, &mut status, &repo, event),
            FactoryUpdate::Control(state) => status.control = state,
            FactoryUpdate::Unreachable => status.link = FactoryLink::Unreachable,
            FactoryUpdate::TooOld(why) => status.link = FactoryLink::TooOld(why),
            FactoryUpdate::Refused(why) => status.link = FactoryLink::Refused(why),
        }
    }
}

/// Keeps the factory mirror from [`FactoryItem`] messages.
pub struct FactoryPlugin;

impl Plugin for FactoryPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FactoryMirror>()
            .init_resource::<FactoryStatus>()
            .add_message::<FactoryItem>()
            .add_systems(PreUpdate, apply_factory);
    }
}

#[cfg(test)]
mod tests {
    use trailhead_factory::{FactorySnapshot, SandboxState};

    use super::*;

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(FactoryPlugin);
        app
    }

    fn sandbox(name: &str, state: SandboxState) -> SandboxStatus {
        SandboxStatus { name: name.into(), target: None, state, task: None, role: None, warm: false, own: false }
    }

    fn snapshot(seq: u64, sandboxes: Vec<SandboxStatus>) -> FactoryUpdate {
        let snapshot = FactorySnapshot {
            control: ControlState::Up,
            last_error: None,
            sandboxes,
            runs: vec![],
            holding: None,
            targets: Vec::new(),
            human_grades: Vec::new(),
            queue: Vec::new(),
            seq,
        };
        FactoryUpdate::Snapshot(vec![RepoSnapshot { repo: "o/r".into(), snapshot }])
    }

    fn event(seq: u64, event: FactoryEvent) -> FactoryUpdate {
        FactoryUpdate::Event { repo: "o/r".into(), event: Sequenced { seq, event } }
    }

    fn send(app: &mut App, updates: impl IntoIterator<Item = FactoryUpdate>) {
        for update in updates {
            app.world_mut().write_message(FactoryItem(update));
        }
        app.update();
    }

    fn campsites(app: &mut App) -> Vec<(String, SandboxState)> {
        let world = app.world_mut();
        let mut all: Vec<_> =
            world.query::<&Campsite>().iter(world).map(|c| (c.status.name.clone(), c.status.state)).collect();
        all.sort_by(|a, b| a.0.cmp(&b.0));
        all
    }

    fn runs(app: &mut App) -> Vec<RunStatus> {
        let world = app.world_mut();
        world.query::<&Run>().iter(world).map(|r| r.status.clone()).collect()
    }

    #[test]
    fn campsites_include_idle_ones_and_follow_the_stream() {
        let mut app = app();
        send(&mut app, [snapshot(3, vec![sandbox("a", SandboxState::Idle), sandbox("b", SandboxState::Busy)])]);
        assert_eq!(campsites(&mut app), [("a".into(), SandboxState::Idle), ("b".into(), SandboxState::Busy)]);

        let changed = FactoryEvent::SandboxChanged { sandbox: sandbox("a", SandboxState::Busy) };
        send(&mut app, [event(4, changed), event(5, FactoryEvent::SandboxGone { name: "b".into() })]);
        assert_eq!(campsites(&mut app), [("a".into(), SandboxState::Busy)]);
        assert!(app.world().resource::<FactoryMirror>().campsite("o/r", "a").is_some());
        assert_eq!(app.world().resource::<FactoryStatus>().repos["o/r"].seq, 5);
    }

    #[test]
    fn duplicates_and_old_events_are_ignored() {
        let mut app = app();
        send(&mut app, [snapshot(3, vec![sandbox("a", SandboxState::Idle)])]);
        // At or before the snapshot's number: the snapshot already includes it.
        let stale = FactoryEvent::SandboxChanged { sandbox: sandbox("a", SandboxState::Broken) };
        send(&mut app, [event(3, stale)]);
        assert_eq!(campsites(&mut app), [("a".into(), SandboxState::Idle)]);
        assert!(app.world().resource::<FactoryStatus>().activity.is_empty());
    }

    #[test]
    fn a_run_goes_through_its_phases_and_shows_live_activity() {
        let mut app = app();
        let task = TaskKey("TRAIL-1".into());
        send(&mut app, [snapshot(0, vec![])]);
        let phase = |seq, phase| event(seq, FactoryEvent::RunPhase { task: task.clone(), phase });
        send(&mut app, [phase(1, RunPhase::Preparing { cold: true }), phase(2, RunPhase::Agent)]);
        let tool = FactoryEvent::ToolCall { task: task.clone(), name: "Bash".into(), detail: None };
        let step = FactoryEvent::Step { task: task.clone(), step: 1, spend: Usd::ZERO };
        send(&mut app, [event(3, tool), event(4, step)]);

        let [run] = &runs(&mut app)[..] else { panic!("one run") };
        assert_eq!((run.phase, run.steps, run.tool_calls), (RunPhase::Agent, 1, 1));
        assert_eq!(run.last_tool.as_deref(), Some("Bash"));
        assert_eq!(app.world().resource::<FactoryStatus>().activity.len(), 4);

        send(&mut app, [event(5, FactoryEvent::RunEnded { task })]);
        assert!(runs(&mut app).is_empty());
    }

    #[test]
    fn a_snapshot_replaces_a_repos_state_even_with_lower_numbers() {
        let mut app = app();
        send(&mut app, [snapshot(9, vec![sandbox("a", SandboxState::Idle), sandbox("b", SandboxState::Idle)])]);
        // The control plane restarted: the numbers start over, and the sandboxes are what its reconcile found.
        send(&mut app, [snapshot(1, vec![sandbox("b", SandboxState::Busy)])]);
        assert_eq!(campsites(&mut app), [("b".into(), SandboxState::Busy)]);
        let err = FactoryEvent::Error { message: "boom".into() };
        send(&mut app, [event(2, err)]);
        assert_eq!(app.world().resource::<FactoryStatus>().repos["o/r"].last_error.as_ref().unwrap().1, "boom");
    }

    #[test]
    fn human_grades_come_from_the_snapshots_and_default_to_the_table_before_one() {
        let mut app = app();
        let g = |n| Grade::new(n).unwrap();
        assert_eq!(app.world().resource::<FactoryStatus>().human_grades(), [g(3), g(4)]);
        let FactoryUpdate::Snapshot(mut repos) = snapshot(1, vec![]) else { unreachable!() };
        repos[0].snapshot.human_grades = vec![g(2), g(4)];
        send(&mut app, [FactoryUpdate::Snapshot(repos)]);
        assert_eq!(app.world().resource::<FactoryStatus>().human_grades(), [g(2), g(4)]);
    }

    #[test]
    fn a_control_plane_that_is_not_running_is_shown_until_it_is() {
        let mut app = app();
        let off = FactoryState::Starting { reason: Some("keyring locked".into()) };
        send(&mut app, [FactoryUpdate::Control(Some(off.clone())), snapshot(1, vec![])]);
        let status = app.world().resource::<FactoryStatus>();
        assert_eq!(status.link, FactoryLink::Control(off));
        assert_eq!(status.label(), "factory starting: keyring locked");
        send(&mut app, [FactoryUpdate::Control(Some(FactoryState::Running)), snapshot(2, vec![])]);
        assert_eq!(app.world().resource::<FactoryStatus>().link, FactoryLink::Running);
        // A server that sends no state is taken to be running.
        send(&mut app, [FactoryUpdate::Control(None), snapshot(3, vec![])]);
        assert_eq!(app.world().resource::<FactoryStatus>().link, FactoryLink::Running);
    }

    #[test]
    fn an_unreachable_server_keeps_the_last_known_state_and_says_so() {
        let mut app = app();
        send(&mut app, [snapshot(3, vec![sandbox("a", SandboxState::Idle)])]);
        assert_eq!(app.world().resource::<FactoryStatus>().label(), "factory running");
        send(&mut app, [FactoryUpdate::Unreachable]);
        assert_eq!(app.world().resource::<FactoryStatus>().label(), "unreachable");
        assert_eq!(campsites(&mut app), [("a".into(), SandboxState::Idle)]);
        // Back again: the new snapshot makes it current.
        send(&mut app, [snapshot(1, vec![])]);
        assert_eq!(app.world().resource::<FactoryStatus>().label(), "factory running");
        assert!(campsites(&mut app).is_empty());
    }

    #[test]
    fn a_factory_too_old_says_so_instead_of_stopped() {
        let mut app = app();
        send(&mut app, [FactoryUpdate::TooOld("reinstall it".into())]);
        assert_eq!(app.world().resource::<FactoryStatus>().label(), "reinstall it");
    }

    #[test]
    fn a_refused_request_shows_why_and_when_it_is_tried_again() {
        let mut app = app();
        send(&mut app, [FactoryUpdate::Refused("not allowed; trying again in 8 s".into())]);
        assert_eq!(app.world().resource::<FactoryStatus>().label(), "not allowed; trying again in 8 s");
    }
}
