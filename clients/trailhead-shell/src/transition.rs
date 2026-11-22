//! Switching between apps. An app's drawing is torn down when it is left and built when it is entered, so two apps are
//! never drawn at once: a switch is covered instead. The shell closes a cover over the app area (everything beside the
//! rail; the rail stays put, its mark moving to the app picked at once), switches [`ActiveApp`] once the app shown is
//! fully covered, and opens the cover again on the app picked. [`crate::cover`] draws it.
//!
//! How it covers is the person's [`AppTransition`], a [`TransitionKind`] kept in the shell's entry of the
//! [`LocalStore`](crate::LocalStore) as `transition` and picked on the Appearance page of the settings. `Random` picks
//! another drawn kind each switch, never the one before. `None`, or a desktop that asks for less motion
//! ([`trailhead_ui::Appearance::reduce_motion`]), switches at once. The first app shown at startup has no transition.
//!
//! [`SwitchApp`] asks for a switch, as the rail does. One asked for while a switch is under way retargets it: while the
//! cover is still closing, the switch goes to the newest app when it is closed; while it is opening, it closes again
//! from where it is and switches then. So there is never more than one cover, and it always opens in the end.
//!
//! Driven by real time, so a slow frame carries it further instead of stretching it: [`COVER`] to close, [`REVEAL`] to
//! open. While it runs the window is asked to redraw every frame, since it updates only on input otherwise.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bevy::prelude::*;
use bevy::window::RequestRedraw;
use trailhead_ui::Appearance;

use crate::{ActiveApp, AppId, Apps};

/// How long the cover takes to close over the app shown.
pub const COVER: Duration = Duration::from_millis(160);
/// How long the cover takes to open on the app picked.
pub const REVEAL: Duration = Duration::from_millis(160);

/// Where the shell keeps the kind picked, in its own entry of the `LocalStore`.
pub(crate) const TRANSITION: &str = "transition";

/// A way of switching between apps.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum TransitionKind {
    /// At once.
    None,
    /// The app area fades to the window's color and back.
    #[default]
    Fade,
    /// A panel sweeps across the app area in the direction the rail was travelled, then on off the far side.
    Wipe,
    /// Two halves close from the top and the bottom, and open again.
    Curtain,
    /// A ring closes on the middle of the app area, and opens from it.
    Iris,
    /// Bars close across the app area one after another, each from the other side, as a trail switches back up a
    /// slope, and leave the same way.
    Switchback,
    /// Another of the drawn kinds each switch, never the same one twice in a row.
    Random,
}

impl TransitionKind {
    /// Every kind, in the order the settings list them.
    pub const ALL: [TransitionKind; 7] = [
        TransitionKind::None,
        TransitionKind::Fade,
        TransitionKind::Wipe,
        TransitionKind::Curtain,
        TransitionKind::Iris,
        TransitionKind::Switchback,
        TransitionKind::Random,
    ];

    /// The kinds that draw a cover: what `Random` picks from.
    pub const DRAWN: [TransitionKind; 5] = [
        TransitionKind::Fade,
        TransitionKind::Wipe,
        TransitionKind::Curtain,
        TransitionKind::Iris,
        TransitionKind::Switchback,
    ];

    /// Its name in the settings.
    pub fn label(self) -> &'static str {
        match self {
            TransitionKind::None => "None",
            TransitionKind::Fade => "Fade",
            TransitionKind::Wipe => "Wipe",
            TransitionKind::Curtain => "Curtain",
            TransitionKind::Iris => "Iris",
            TransitionKind::Switchback => "Switchback",
            TransitionKind::Random => "Random",
        }
    }

    /// The word it is kept as.
    pub fn word(self) -> &'static str {
        match self {
            TransitionKind::None => "none",
            TransitionKind::Fade => "fade",
            TransitionKind::Wipe => "wipe",
            TransitionKind::Curtain => "curtain",
            TransitionKind::Iris => "iris",
            TransitionKind::Switchback => "switchback",
            TransitionKind::Random => "random",
        }
    }

    /// The kind a kept word means, if any.
    pub fn from_word(word: &str) -> Option<TransitionKind> {
        TransitionKind::ALL.into_iter().find(|k| k.word() == word.trim())
    }
}

/// How the shell switches between apps, as the person picked it.
#[derive(Resource, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct AppTransition(pub TransitionKind);

/// Shows the app named, as a press on the rail does: covered by the [`AppTransition`], or at once.
#[derive(Event, Clone, Copy, PartialEq, Eq, Debug)]
pub struct SwitchApp(pub AppId);

/// Which way down the rail a switch goes: to an app lower on it, or higher.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Direction {
    #[default]
    Down,
    Up,
}

/// Whether a cover is closing or opening.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Phase {
    Covering,
    #[default]
    Revealing,
}

/// What a cover shows at one moment: a drawn kind, which way the switch goes, closing or opening, and how far through
/// that, from 0 to 1, before easing. Closed is `Covering` at 1 (or `Revealing` at 0); open is `Revealing` at 1. On a
/// cover's area, which [`crate::cover`] draws.
#[derive(Component, Clone, Copy, PartialEq, Debug)]
pub struct CoverFrame {
    pub kind: TransitionKind,
    pub direction: Direction,
    pub phase: Phase,
    pub t: f32,
}

impl Default for CoverFrame {
    /// Open.
    fn default() -> Self {
        CoverFrame { kind: TransitionKind::None, direction: Direction::Down, phase: Phase::Revealing, t: 1.0 }
    }
}

/// Where a switch is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Step {
    /// Closing over the app shown.
    Cover,
    /// Closed, the switch asked for, until the app picked has been entered.
    Hold,
    /// Opening on the app picked.
    Reveal,
    /// Closing again from part open, for a newer app picked while it opened.
    Unreveal,
}

#[derive(Clone, Copy, Debug)]
struct Run {
    kind: TransitionKind,
    to: AppId,
    direction: Direction,
    step: Step,
    /// When the step began, in real time.
    since: Duration,
    /// How far through the step it began.
    from: f32,
    /// How far through the step it is.
    t: f32,
}

/// The switch under way, if one is.
#[derive(Resource, Debug)]
pub struct Switching {
    run: Option<Run>,
    /// The kind drawn last, which `Random` doesn't pick again.
    last: Option<TransitionKind>,
    /// `Random`'s state.
    seed: u64,
}

impl Default for Switching {
    fn default() -> Self {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        Switching { run: None, last: None, seed: (now.as_nanos() as u64) | 1 }
    }
}

impl Switching {
    /// The app a switch under way goes to.
    pub fn target(&self) -> Option<AppId> {
        self.run.map(|run| run.to)
    }

    /// What the cover shows now, while a switch is under way.
    pub fn frame(&self) -> Option<CoverFrame> {
        let run = self.run?;
        let (phase, t) = match run.step {
            Step::Cover => (Phase::Covering, run.t),
            Step::Hold => (Phase::Covering, 1.0),
            Step::Reveal | Step::Unreveal => (Phase::Revealing, run.t),
        };
        Some(CoverFrame { kind: run.kind, direction: run.direction, phase, t })
    }

    /// Ends a switch under way where it is, for a test that switches apps itself (`testing::show`).
    #[cfg(feature = "test-support")]
    pub(crate) fn cancel(&mut self) {
        self.run = None;
    }

    /// The kind to draw for `setting`: `Random` resolved to a drawn kind other than the last.
    fn pick(&mut self, setting: TransitionKind) -> TransitionKind {
        let kind = match setting {
            TransitionKind::Random => {
                let left: Vec<TransitionKind> =
                    TransitionKind::DRAWN.into_iter().filter(|k| Some(*k) != self.last).collect();
                left[(self.next_random() % left.len() as u64) as usize]
            }
            kind => kind,
        };
        if kind != TransitionKind::None {
            self.last = Some(kind);
        }
        kind
    }

    /// xorshift64: plenty to vary a transition, and no dependency for it.
    fn next_random(&mut self) -> u64 {
        let mut x = self.seed;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.seed = x;
        x
    }
}

/// Which way the rail is travelled from `from` to `to`.
fn direction(apps: &Apps, from: AppId, to: AppId) -> Direction {
    let at = |id: AppId| apps.iter().position(|m| m.id == id);
    if at(to) >= at(from) { Direction::Down } else { Direction::Up }
}

/// The switch's logic: headless, so a test steps it with time of its own.
pub(crate) struct TransitionPlugin;

impl Plugin for TransitionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Switching>().add_observer(on_switch).add_systems(Update, step_switch);
    }
}

/// A switch asked for: start one, retarget the one under way, or switch at once.
#[expect(clippy::too_many_arguments)]
fn on_switch(
    switch: On<SwitchApp>,
    apps: Res<Apps>,
    active: Res<State<ActiveApp>>,
    setting: Res<AppTransition>,
    appearance: Option<Res<Appearance>>,
    real: Res<Time<Real>>,
    mut switching: ResMut<Switching>,
    mut next: ResMut<NextState<ActiveApp>>,
) {
    let (to, shown, now) = (switch.0, active.get().0, real.elapsed());
    if !apps.iter().any(|m| m.id == to) {
        return;
    }
    if let Some(run) = &mut switching.run {
        match run.step {
            Step::Cover | Step::Unreveal => run.to = to,
            Step::Hold => {
                run.to = to;
                next.set_if_different(ActiveApp(to));
            }
            Step::Reveal if run.to != to => {
                *run = Run { step: Step::Unreveal, since: now, from: run.t, to, ..*run };
            }
            Step::Reveal => {}
        }
        return;
    }
    if to == shown {
        return;
    }
    let still = appearance.is_some_and(|a| a.reduce_motion());
    let kind = if still { TransitionKind::None } else { switching.pick(setting.0) };
    if kind == TransitionKind::None {
        next.set_if_different(ActiveApp(to));
        return;
    }
    let direction = direction(&apps, shown, to);
    switching.run = Some(Run { kind, to, direction, step: Step::Cover, since: now, from: 0.0, t: 0.0 });
}

/// Moves the switch under way on by the real time gone, switching the app once it is covered, and asks the window to
/// redraw while it runs.
pub(crate) fn step_switch(
    real: Res<Time<Real>>,
    active: Res<State<ActiveApp>>,
    mut switching: ResMut<Switching>,
    mut next: ResMut<NextState<ActiveApp>>,
    redraw: Option<ResMut<Messages<RequestRedraw>>>,
) {
    let Some(mut run) = switching.run else { return };
    if let Some(mut redraw) = redraw {
        redraw.write(RequestRedraw);
    }
    let now = real.elapsed();
    let gone = |length: Duration| now.saturating_sub(run.since).as_secs_f32() / length.as_secs_f32();
    match run.step {
        Step::Cover => {
            run.t = (run.from + gone(COVER)).min(1.0);
            if run.t >= 1.0 {
                next.set_if_different(ActiveApp(run.to));
                run.step = Step::Hold;
            }
        }
        Step::Hold => {
            if active.get().0 == run.to {
                run = Run { step: Step::Reveal, since: now, from: 0.0, t: 0.0, ..run };
            }
        }
        Step::Reveal => {
            run.t = gone(REVEAL).min(1.0);
            if run.t >= 1.0 {
                switching.run = None;
                return;
            }
        }
        Step::Unreveal => {
            run.t = (run.from - gone(REVEAL)).max(0.0);
            if run.t <= 0.0 {
                next.set_if_different(ActiveApp(run.to));
                run.step = Step::Hold;
            }
        }
    }
    switching.run = Some(run);
}

#[cfg(test)]
mod tests {
    use bevy::time::TimeUpdateStrategy;

    use super::*;
    use crate::ShellPlugin;
    use crate::testing::headless;
    use crate::tests::{Fake, MAPS, NOTES, Seen};

    const TREES: AppId = AppId("trees");
    const FRAME: Duration = Duration::from_millis(16);

    fn app(kind: TransitionKind) -> App {
        let mut app = headless(ShellPlugin::new().with(Fake(NOTES)).with(Fake(MAPS)).with(Fake(TREES)));
        app.insert_resource(TimeUpdateStrategy::ManualDuration(FRAME))
            .insert_resource(AppTransition(kind))
            .add_message::<RequestRedraw>();
        app.update();
        app.world_mut().resource_mut::<Seen>().0.clear();
        app
    }

    fn switch(app: &mut App, to: AppId) {
        app.world_mut().trigger(SwitchApp(to));
    }

    fn frame(app: &App) -> Option<CoverFrame> {
        app.world().resource::<Switching>().frame()
    }

    fn active(app: &App) -> AppId {
        app.world().resource::<State<ActiveApp>>().get().0
    }

    fn seen(app: &App) -> Vec<String> {
        app.world().resource::<Seen>().0.clone()
    }

    fn redraws(app: &mut App) -> usize {
        app.world_mut().resource_mut::<Messages<RequestRedraw>>().drain().count()
    }

    /// Runs frames until `done` holds, and says how many it took.
    fn until(app: &mut App, done: impl Fn(&App) -> bool) -> usize {
        for frames in 1..=100 {
            app.update();
            if done(app) {
                return frames;
            }
        }
        panic!("never got there: {:?}", frame(app));
    }

    fn covered(app: &App) -> bool {
        frame(app).is_some_and(|f| f.phase == Phase::Covering && f.t >= 1.0)
    }

    #[test]
    fn a_switch_covers_switches_the_app_once_covered_then_reveals_and_ends() {
        let mut app = app(TransitionKind::Wipe);
        switch(&mut app, MAPS);
        let start =
            CoverFrame { kind: TransitionKind::Wipe, direction: Direction::Down, phase: Phase::Covering, t: 0.0 };
        assert_eq!(frame(&app), Some(start), "it starts where it is, however long the window was idle");
        let frames = until(&mut app, covered);
        assert_eq!(frames, (COVER.as_millis() / FRAME.as_millis()) as usize, "closes in COVER of real time");
        assert_eq!(active(&app), NOTES, "not switched until the frame after it is covered");
        assert!(matches!(
            *app.world().resource::<NextState<ActiveApp>>(),
            NextState::PendingIfDifferent(ActiveApp(MAPS))
        ));
        assert!(redraws(&mut app) > 0, "the window redraws while it runs");

        app.update();
        assert_eq!(active(&app), MAPS);
        assert_eq!(seen(&app), ["exit notes", "enter maps"]);
        assert_eq!(frame(&app).map(|f| (f.phase, f.t)), Some((Phase::Revealing, 0.0)), "entered under a closed cover");
        let frames = until(&mut app, |app| frame(app).is_none());
        assert_eq!(frames, (REVEAL.as_millis() / FRAME.as_millis()) as usize, "opens in REVEAL");
        assert_eq!(app.world().resource::<Switching>().target(), None);
        redraws(&mut app);
        app.update();
        assert_eq!(redraws(&mut app), 0, "and stops asking for redraws once done");
        assert_eq!(seen(&app), ["exit notes", "enter maps"]);
    }

    #[test]
    fn a_slow_frame_carries_it_further_instead_of_stretching_it() {
        let mut app = app(TransitionKind::Fade);
        switch(&mut app, MAPS);
        app.insert_resource(TimeUpdateStrategy::ManualDuration(COVER / 2));
        app.update();
        assert_eq!(frame(&app).map(|f| f.t), Some(0.5));
        app.update();
        assert!(covered(&app));
    }

    #[test]
    fn none_switches_at_once_and_a_switch_to_the_app_shown_does_nothing() {
        let mut app = app(TransitionKind::None);
        switch(&mut app, MAPS);
        assert_eq!(frame(&app), None);
        app.update();
        assert_eq!(active(&app), MAPS);
        let mut app = self::app(TransitionKind::Fade);
        switch(&mut app, NOTES);
        assert_eq!(frame(&app), None);
        switch(&mut app, AppId("nowhere"));
        assert_eq!(frame(&app), None, "not an app in this shell");
    }

    #[test]
    fn random_never_picks_none_or_the_same_kind_twice_running() {
        let mut switching = Switching::default();
        let mut last = None;
        let mut picked = Vec::new();
        for _ in 0..200 {
            let kind = switching.pick(TransitionKind::Random);
            assert!(TransitionKind::DRAWN.contains(&kind), "{kind:?}");
            assert_ne!(Some(kind), last);
            last = Some(kind);
            picked.push(kind);
        }
        assert!(TransitionKind::DRAWN.iter().all(|k| picked.contains(k)), "every drawn kind comes up");
        assert_eq!(switching.pick(TransitionKind::Iris), TransitionKind::Iris);
        assert_ne!(switching.pick(TransitionKind::Random), TransitionKind::Iris);
    }

    #[test]
    fn a_switch_asked_for_while_closing_goes_to_the_newest_once_covered() {
        let mut app = app(TransitionKind::Curtain);
        switch(&mut app, MAPS);
        app.update();
        app.update();
        switch(&mut app, TREES);
        assert_eq!(app.world().resource::<Switching>().target(), Some(TREES));
        until(&mut app, covered);
        until(&mut app, |app| frame(app).is_none());
        assert_eq!(active(&app), TREES);
        assert_eq!(seen(&app), ["exit notes", "enter trees"], "the one in between is never built");
    }

    #[test]
    fn a_switch_asked_for_while_opening_closes_again_from_where_it_is() {
        let mut app = app(TransitionKind::Switchback);
        switch(&mut app, TREES);
        until(&mut app, |app| frame(app).is_some_and(|f| f.phase == Phase::Revealing && f.t > 0.4));
        let opened = frame(&app).unwrap().t;
        switch(&mut app, MAPS);
        app.update();
        let now = frame(&app).unwrap();
        assert_eq!(now.phase, Phase::Revealing);
        assert!(now.t < opened, "closing back from {opened}, not jumping: {now:?}");
        until(&mut app, |app| active(app) == MAPS);
        until(&mut app, |app| frame(app).is_none());
        assert_eq!(seen(&app), ["exit notes", "enter trees", "exit trees", "enter maps"]);
    }

    #[test]
    fn a_switch_runs_down_the_rail_to_an_app_lower_on_it_and_up_to_one_higher() {
        let mut app = app(TransitionKind::Wipe);
        switch(&mut app, TREES);
        assert_eq!(frame(&app).map(|f| f.direction), Some(Direction::Down));
        until(&mut app, |app| frame(app).is_none());
        switch(&mut app, MAPS);
        assert_eq!(frame(&app).map(|f| f.direction), Some(Direction::Up));
    }

    #[test]
    fn the_direction_follows_the_persons_order_of_the_rail() {
        let mut app = app(TransitionKind::Wipe);
        app.world_mut().trigger(crate::MoveApp { app: TREES, to: 0 });
        switch(&mut app, TREES);
        assert_eq!(frame(&app).map(|f| f.direction), Some(Direction::Up), "trees is above notes now");
        until(&mut app, |app| frame(app).is_none());
        switch(&mut app, MAPS);
        assert_eq!(frame(&app).map(|f| f.direction), Some(Direction::Down));
    }

    #[test]
    fn the_kind_picked_is_kept_as_a_word_and_read_back_after_a_restart() {
        for kind in TransitionKind::ALL {
            assert_eq!(TransitionKind::from_word(kind.word()), Some(kind));
        }
        assert_eq!(TransitionKind::from_word("spin"), None);
        let dir = tempfile::tempdir().unwrap();
        let shell = || ShellPlugin::new().with(Fake(NOTES)).with(Fake(MAPS)).store_in(dir.path().to_path_buf());
        let mut first = headless(shell());
        assert_eq!(*first.world().resource::<AppTransition>(), AppTransition(TransitionKind::Fade), "Fade at first");
        first.insert_resource(AppTransition(TransitionKind::Iris));
        first.update();
        drop(first);
        let again = headless(shell());
        assert_eq!(*again.world().resource::<AppTransition>(), AppTransition(TransitionKind::Iris));
        let kept = crate::LocalStore::open(Some(dir.path().to_path_buf()), &[crate::SHELL]);
        assert_eq!(kept.get::<String>(crate::SHELL, TRANSITION).as_deref(), Some("iris"));
    }
}
