//! The cover drawn while the shell switches apps ([`crate::transition`]), and how each kind looks.
//!
//! A cover is an area carrying a [`CoverFrame`], holding a fixed set of pieces spawned once with it
//! ([`spawn_cover_pieces`]): [`BARS`] bars and a ring. [`cover_shape`] works out where each piece is for a frame, as
//! shares of the area, and [`draw_covers`] puts them there in place each frame it changes. The real cover is an area
//! beside the rail, over the apps and under the shell's modals, that blocks the pointer while it is up; the
//! settings' previews ([`crate::previews`]) are small areas drawn by the same code.
//!
//! - **Fade**: one bar over the whole area, in the window's color, its opacity rising and falling.
//! - **Wipe**: one bar whose leading edge sweeps down the area (up, travelling up the rail), then whose trailing edge
//!   follows it off the far side.
//! - **Curtain**: two bars closing from the top and the bottom to meet in the middle, and opening again.
//! - **Iris**: the ring, a circle wider than the area's diagonal whose border closes on the middle until no hole is
//!   left, then opens from it.
//! - **Switchback**: [`BARS`] rows, each sweeping across from the other side to the row before it, one after another
//!   down the area (up, travelling up), and leaving the same way.
//!
//! The cover is in the panel's color, as the rail is, except Fade's, which is the window's.

use bevy::prelude::*;
use trailhead_ui::{
    BackgroundFade, BackgroundTone, BesideRail, BorderTone, Token, UiPlace, beside_rail, ease_in, ease_out, layer,
    patch_node, token,
};

use crate::transition::{CoverFrame, Direction, Phase, Switching, TransitionKind, step_switch};

/// How many bars a cover has: Switchback's rows, and the most any kind uses.
pub const BARS: usize = 5;
/// How far into one Switchback row's sweep the next one starts.
const STAGGER: f32 = 0.5;
/// The area's width over its height before it is laid out.
const FALLBACK_ASPECT: f32 = 1.6;
/// How much wider than the area's diagonal the iris is, in hundredths of the area's width, so its edge never shows a
/// corner.
const IRIS_SPARE: f32 = 2.0;

/// A bar's place in its area, each a share of the area's width or height from 0 to 1.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Piece {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
}

impl Piece {
    fn rows(top: f32, height: f32) -> Piece {
        Piece { left: 0.0, top, width: 1.0, height }
    }
}

/// What a cover shows for one frame.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct CoverShape {
    /// The bars drawn, in order; `None` is hidden.
    pub bars: [Option<Piece>; BARS],
    /// The bars' opacity.
    pub opacity: f32,
    /// The ring, when drawn: how open its hole is, 0 closed to 1 gone.
    pub iris: Option<f32>,
    /// The bars' and the ring's color.
    pub tone: Token,
}

/// Where each piece of a cover is for `frame`.
pub fn cover_shape(frame: CoverFrame) -> CoverShape {
    // How much is covered while closing, and how much has gone while opening, eased.
    let (closing, shut, gone) = match frame.phase {
        Phase::Covering => (true, ease_in(frame.t), 0.0),
        Phase::Revealing => (false, 1.0, ease_out(frame.t)),
    };
    let mut shape = CoverShape { opacity: 1.0, tone: token::SURFACE_PANEL, ..default() };
    let down = frame.direction == Direction::Down;
    match frame.kind {
        TransitionKind::None | TransitionKind::Random => {}
        TransitionKind::Fade => {
            shape.tone = token::SURFACE_WINDOW;
            shape.bars[0] = Some(Piece::rows(0.0, 1.0));
            shape.opacity = if closing { shut } else { 1.0 - gone };
        }
        TransitionKind::Wipe => {
            shape.bars[0] = Some(match (closing, down) {
                (true, true) => Piece::rows(0.0, shut),
                (true, false) => Piece::rows(1.0 - shut, shut),
                (false, true) => Piece::rows(gone, 1.0 - gone),
                (false, false) => Piece::rows(0.0, 1.0 - gone),
            });
        }
        TransitionKind::Curtain => {
            let half = if closing { shut } else { 1.0 - gone } / 2.0;
            shape.bars[0] = Some(Piece::rows(0.0, half));
            shape.bars[1] = Some(Piece::rows(1.0 - half, half));
        }
        TransitionKind::Iris => shape.iris = Some(if closing { 1.0 - shut } else { gone }),
        TransitionKind::Switchback => {
            let reach = 1.0 + (BARS - 1) as f32 * STAGGER;
            for (order, bar) in shape.bars.iter_mut().enumerate() {
                let row = if down { order } else { BARS - 1 - order };
                let (top, height) = (row as f32 / BARS as f32, 1.0 / BARS as f32);
                let at = if closing { shut } else { gone };
                let p = (at * reach - order as f32 * STAGGER).clamp(0.0, 1.0);
                let from_left = order % 2 == 0;
                let (left, width) = match (closing, from_left) {
                    (true, true) => (0.0, p),
                    (true, false) => (1.0 - p, p),
                    (false, true) => (p, 1.0 - p),
                    (false, false) => (0.0, 1.0 - p),
                };
                *bar = (width > 0.0).then_some(Piece { left, top, width, height });
            }
        }
    }
    shape
}

/// One of a cover's bars, by its place in [`CoverShape::bars`].
#[derive(Component, Clone, Copy, Debug)]
pub struct CoverBar(pub usize);

/// A cover's ring.
#[derive(Component, Clone, Copy, Debug)]
pub struct CoverIris;

/// The real cover, over the app area while the shell switches apps.
#[derive(Component, Clone, Copy, Debug)]
pub struct SwitchCover;

/// Spawns a cover's pieces in `area`, hidden until a frame draws them.
pub(crate) fn spawn_cover_pieces(commands: &mut Commands, area: Entity) {
    let hidden = Node { position_type: PositionType::Absolute, display: Display::None, ..default() };
    for at in 0..BARS {
        commands.spawn((
            CoverBar(at),
            hidden.clone(),
            BackgroundTone(token::SURFACE_PANEL),
            BackgroundFade::default(),
            Pickable::IGNORE,
            ChildOf(area),
        ));
    }
    commands.spawn((
        CoverIris,
        Node { aspect_ratio: Some(1.0), border_radius: BorderRadius::MAX, ..hidden },
        BorderTone(token::SURFACE_PANEL),
        Pickable::IGNORE,
        ChildOf(area),
    ));
}

/// Puts each cover's pieces where its frame says, in place, when the frame or the area's size changes.
#[expect(clippy::type_complexity)]
pub(crate) fn draw_covers(
    areas: Query<(&CoverFrame, &Children, &ComputedNode), Or<(Changed<CoverFrame>, Changed<ComputedNode>)>>,
    mut bars: Query<(&CoverBar, &mut Node, &mut BackgroundFade, &BackgroundTone), Without<CoverIris>>,
    mut irises: Query<(&mut Node, &BorderTone), (With<CoverIris>, Without<CoverBar>)>,
    mut commands: Commands,
) {
    for (frame, children, computed) in &areas {
        let shape = cover_shape(*frame);
        let size = computed.size();
        let aspect = if size.x > 0.0 && size.y > 0.0 { size.x / size.y } else { FALLBACK_ASPECT };
        for &piece in children {
            if let Ok((CoverBar(at), node, mut fade, tone)) = bars.get_mut(piece) {
                patch_node(node, |node| place_bar(node, shape.bars[*at]));
                fade.set_if_neq(BackgroundFade(shape.opacity));
                if tone.0 != shape.tone {
                    commands.entity(piece).insert(BackgroundTone(shape.tone.clone()));
                }
            } else if let Ok((node, tone)) = irises.get_mut(piece) {
                patch_node(node, |node| place_iris(node, shape.iris, aspect));
                if tone.0 != shape.tone {
                    commands.entity(piece).insert(BorderTone(shape.tone.clone()));
                }
            }
        }
    }
}

fn place_bar(node: &mut Node, piece: Option<Piece>) {
    let Some(piece) = piece else {
        node.display = Display::None;
        return;
    };
    node.display = Display::Flex;
    node.left = percent(piece.left * 100.0);
    node.top = percent(piece.top * 100.0);
    node.width = percent(piece.width * 100.0);
    node.height = percent(piece.height * 100.0);
}

/// The ring: a circle centered on the area, wider than its diagonal, whose border is all of it when closed. Sizes are
/// shares of the area's width (a border's share is of the width too), and its top a share of the height.
fn place_iris(node: &mut Node, open: Option<f32>, aspect: f32) {
    let Some(open) = open else {
        node.display = Display::None;
        return;
    };
    let across = 100.0 * (1.0 + 1.0 / (aspect * aspect)).sqrt() + IRIS_SPARE;
    node.display = Display::Flex;
    node.width = percent(across);
    node.left = percent(50.0 - across / 2.0);
    node.top = percent(50.0 - across * aspect / 2.0);
    node.border = UiRect::all(percent(across / 2.0 * (1.0 - open)));
}

/// Draws the real cover while a switch is under way.
pub(crate) struct CoverUiPlugin;

impl Plugin for CoverUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (open_cover, follow_switch, close_cover).chain().after(step_switch).before(UiPlace))
            .add_systems(Update, draw_covers.after(follow_switch).before(UiPlace));
    }
}

/// Puts the cover up when a switch starts.
fn open_cover(switching: Res<Switching>, covers: Query<(), With<SwitchCover>>, mut commands: Commands) {
    let Some(frame) = switching.frame() else { return };
    if !covers.is_empty() {
        return;
    }
    let node = Node { overflow: Overflow::clip(), ..beside_rail() };
    let area = commands
        .spawn((Name::new("switch cover"), SwitchCover, frame, node, BesideRail, GlobalZIndex(layer::SWITCH)))
        .insert(Pickable { should_block_lower: true, is_hoverable: true })
        .id();
    spawn_cover_pieces(&mut commands, area);
}

/// Keeps the cover's frame at the switch's.
fn follow_switch(switching: Res<Switching>, mut covers: Query<&mut CoverFrame, With<SwitchCover>>) {
    let Some(frame) = switching.frame() else { return };
    for mut shown in &mut covers {
        shown.set_if_neq(frame);
    }
}

/// Takes the cover down when the switch is done.
fn close_cover(switching: Res<Switching>, covers: Query<Entity, With<SwitchCover>>, mut commands: Commands) {
    if switching.frame().is_some() {
        return;
    }
    for cover in &covers {
        commands.entity(cover).despawn();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bevy::time::TimeUpdateStrategy;
    use trailhead_ui::NavItem;

    use super::*;
    use crate::testing::{settle, windowed};
    use crate::tests::{Fake, MAPS, NOTES};
    use crate::transition::{AppTransition, SwitchApp};
    use crate::{ActiveApp, AppId, OpenApp, ShellPlugin};

    fn frame(kind: TransitionKind, direction: Direction, phase: Phase, t: f32) -> CoverFrame {
        CoverFrame { kind, direction, phase, t }
    }

    /// Whether the point at `x`, `y` (shares of the area) is hidden by `shape`.
    fn hides(shape: &CoverShape, x: f32, y: f32) -> bool {
        let in_bar = shape.bars.iter().flatten().any(|p| {
            (p.left..=p.left + p.width).contains(&x) && (p.top..=p.top + p.height).contains(&y) && p.width > 0.0
        });
        let in_ring = shape.iris.is_some_and(|open| open <= 0.0);
        (in_bar && shape.opacity >= 1.0) || in_ring
    }

    fn grid() -> impl Iterator<Item = (f32, f32)> {
        (0..=10).flat_map(|x| (0..=10).map(move |y| (x as f32 / 10.0, y as f32 / 10.0)))
    }

    fn hidden(shape: &CoverShape) -> usize {
        grid().filter(|(x, y)| hides(shape, *x, *y)).count()
    }

    fn drawn(shape: &CoverShape) -> bool {
        let bars = shape.bars.iter().flatten().any(|p| p.width > 0.0 && p.height > 0.0) && shape.opacity > 0.0;
        bars || shape.iris.is_some_and(|open| open < 1.0)
    }

    #[test]
    fn every_kind_hides_the_whole_area_when_covered_and_nothing_when_open() {
        for kind in TransitionKind::DRAWN {
            for direction in [Direction::Down, Direction::Up] {
                let covered = cover_shape(frame(kind, direction, Phase::Covering, 1.0));
                assert_eq!(hidden(&covered), 121, "{kind:?} {direction:?} covered: {covered:?}");
                let closed_again = cover_shape(frame(kind, direction, Phase::Revealing, 0.0));
                assert_eq!(hidden(&closed_again), 121, "{kind:?} {direction:?} starting to open");
                for open in
                    [frame(kind, direction, Phase::Covering, 0.0), frame(kind, direction, Phase::Revealing, 1.0)]
                {
                    assert!(!drawn(&cover_shape(open)), "{kind:?} {open:?} draws nothing");
                }
                let midway = cover_shape(frame(kind, direction, Phase::Covering, 0.6));
                assert!(drawn(&midway), "{kind:?} midway");
                assert!(hidden(&midway) < 121, "{kind:?} midway leaves some to see");
            }
        }
        assert!(!drawn(&cover_shape(frame(TransitionKind::None, Direction::Down, Phase::Covering, 0.5))));
    }

    #[test]
    fn the_kinds_look_different_from_each_other_midway() {
        let shapes: Vec<CoverShape> = TransitionKind::DRAWN
            .iter()
            .map(|k| cover_shape(frame(*k, Direction::Down, Phase::Covering, 0.6)))
            .collect();
        for (at, shape) in shapes.iter().enumerate() {
            assert!(shapes[..at].iter().all(|other| other != shape), "{:?}", TransitionKind::DRAWN[at]);
        }
        assert_eq!(shapes[0].tone, token::SURFACE_WINDOW, "Fade goes to the window's color");
    }

    #[test]
    fn a_wipe_runs_the_way_the_rail_was_travelled_and_leaves_off_the_far_side() {
        let wipe = |direction, phase, t| cover_shape(frame(TransitionKind::Wipe, direction, phase, t));
        let down = wipe(Direction::Down, Phase::Covering, 0.6);
        assert!(hides(&down, 0.5, 0.1) && !hides(&down, 0.5, 0.9), "down: the top first");
        let up = wipe(Direction::Up, Phase::Covering, 0.6);
        assert!(!hides(&up, 0.5, 0.1) && hides(&up, 0.5, 0.9), "up: the bottom first");
        let leaving = wipe(Direction::Down, Phase::Revealing, 0.4);
        assert!(!hides(&leaving, 0.5, 0.1) && hides(&leaving, 0.5, 0.9), "and on down to show the top first");
    }

    fn app() -> App {
        let mut app = windowed(ShellPlugin::new().with(Fake(NOTES)).with(Fake(MAPS)));
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(16)))
            .insert_resource(AppTransition(TransitionKind::Iris));
        settle(&mut app);
        app
    }

    fn covers(app: &mut App) -> Vec<Entity> {
        let world = app.world_mut();
        world.query_filtered::<Entity, With<SwitchCover>>().iter(world).collect()
    }

    fn marked(app: &mut App) -> Vec<AppId> {
        let world = app.world_mut();
        world.query::<(&OpenApp, &NavItem)>().iter(world).filter(|(_, n)| n.current).map(|(o, _)| o.0).collect()
    }

    #[test]
    fn the_cover_is_up_over_the_apps_and_under_the_modals_while_switching_then_gone() {
        let mut app = app();
        assert!(covers(&mut app).is_empty());
        app.world_mut().trigger(SwitchApp(MAPS));
        app.update();
        let cover = covers(&mut app);
        assert_eq!(cover.len(), 1);
        let world = app.world();
        let z = world.get::<GlobalZIndex>(cover[0]).unwrap().0;
        assert!(layer::INSPECTOR < z && z < layer::MODAL, "over an app's panels, under the shell's modals");
        assert!(world.get::<BesideRail>(cover[0]).is_some(), "beside the rail, which it leaves alone");
        assert!(world.get::<Pickable>(cover[0]).is_some_and(|p| p.should_block_lower), "the app gets no pointer");
        assert_eq!(world.get::<Children>(cover[0]).map(|c| c.len()), Some(BARS + 1));
        assert_eq!(marked(&mut app), [MAPS], "the rail marks the app picked at once");
        assert_eq!(app.world().resource::<State<ActiveApp>>().get().0, NOTES);

        app.update();
        app.world_mut().trigger(SwitchApp(NOTES));
        app.world_mut().trigger(SwitchApp(MAPS));
        for _ in 0..40 {
            app.update();
            assert!(covers(&mut app).len() <= 1, "never two covers");
        }
        assert!(covers(&mut app).is_empty(), "and none once it is done");
        assert_eq!(app.world().resource::<State<ActiveApp>>().get().0, MAPS);
    }

    #[test]
    fn the_iris_is_a_ring_wider_than_the_areas_diagonal_that_closes_to_nothing() {
        let mut node = Node::default();
        place_iris(&mut node, Some(0.0), 2.0);
        let Val::Percent(across) = node.width else { panic!("{:?}", node.width) };
        assert!(across >= 100.0 * (1.0f32 + 0.25).sqrt(), "{across}");
        assert_eq!(node.border.left, percent(across / 2.0), "closed: all border");
        assert_eq!(node.left, percent(50.0 - across / 2.0));
        place_iris(&mut node, Some(1.0), 2.0);
        assert_eq!(node.border.left, percent(0.0), "open: no border left");
    }
}
