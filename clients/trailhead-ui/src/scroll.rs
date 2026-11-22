//! Areas that scroll: [`ScrollBoth`] (both ways, with Shift turning the wheel sideways) and [`ScrollDown`] (down only,
//! nested in one that scrolls across, such as the board's columns inside the board). Ported in part from
//! bevy-data-explorer's `widgets/scroll.rs`.
//!
//! One observer routes every wheel turn. It starts from what the pointer is over and walks up its parents for the
//! nearest [`ScrollDown`] and the nearest [`ScrollBoth`], then splits the turn between them: down to the column,
//! across (a touchpad swipe, or Shift + wheel) to the board. Each area handling the event as it bubbled didn't work:
//! the event names the entity it was fired on, not the ancestor it has reached, so the board never saw its own
//! (TRAIL-836).
//!
//! Shift is what a browser and a spreadsheet use, and the only way across for a wheel with no sideways axis of its
//! own, which is most of them.
//!
//! A [`Canvas`](crate::Canvas) takes the wheel too, to zoom: whichever of a canvas and the two areas is nearest the
//! pointer takes the whole turn, so a list inside a canvas scrolls and a canvas inside a page zooms (TRAIL-1668).
//!
//! Not `bevy_ui_widgets`' `ScrollArea`, which scrolls only down and would move the area before Shift was noticed.
//!
//! Give a node the marker and the matching `overflow` (`Overflow::scroll()` or `scroll_x()` for [`ScrollBoth`],
//! `scroll_y()` for [`ScrollDown`]). A view that rebuilds its nodes keeps the [`ScrollPosition`] itself and puts it
//! back on the new node, or the area jumps to its start on every redraw. Better, keep the node (see `Keyed`).
//!
//! How far an area scrolls is [`scroll_range`], which the wheel, the scrollbar and [`clamp_scroll_positions`] share:
//! content overflowing by no more than layout's rounding is no range at all.

use bevy::input::mouse::MouseScrollPixelsPerLine;
use bevy::picking::events::PointerScroll;
use bevy::prelude::*;

use super::space;
use crate::canvas::{Canvas, wheel_canvas};

/// The node that holds a panel's scrolling body and its scrollbar, so the bar runs along the body only. It takes the
/// height the panel has left and may shrink below its content, so the body scrolls (TRAIL-836).
pub fn scroll_frame() -> impl Bundle {
    (Name::new("scroll frame"), scroll_frame_node(1.0))
}

/// The [`Node`] of a [`scroll_frame`] that grows by `grow` into the height its parent has left, for a frame that sets
/// its own growth (a board column's, whose height is its cards') or is spawned in a scene (the modal's).
pub fn scroll_frame_node(grow: f32) -> Node {
    Node { flex_direction: FlexDirection::Column, flex_grow: grow, min_height: px(0), ..default() }
}

/// The body of a panel that scrolls down on its own, wheel and touchpad, for [`super::scrollbar`] to sit beside: a
/// column of rows. The bar lies in the padding of the frame's parent, outside the body. Put it in a [`scroll_frame`].
pub fn scroll_body() -> impl Bundle {
    (Name::new("scroll body"), scroll_body_node(space::ROWS, 0.0), ScrollDown)
}

/// The [`Node`] of a [`scroll_body`] with `gap` between its rows, growing by `grow` into its frame. The gutter and the
/// minimum size, which lets a body shrink below its content and scroll, live here for every scrolling body.
pub fn scroll_body_node(gap: f32, grow: f32) -> Node {
    Node {
        flex_direction: FlexDirection::Column,
        flex_grow: grow,
        row_gap: px(gap),
        min_height: px(0),
        overflow: Overflow::scroll_y(),
        ..default()
    }
}

/// Marks an area that scrolls both ways.
#[derive(Component, Clone, Default)]
#[require(ScrollPosition)]
pub struct ScrollBoth;

/// Marks an area that scrolls down only, inside a [`ScrollBoth`] that takes the sideways part.
#[derive(Component, Clone, Default)]
#[require(ScrollPosition)]
pub struct ScrollDown;

/// What a wheel turn may move above what the pointer is over: areas that scroll, and canvases, which zoom.
#[derive(bevy::ecs::system::SystemParam)]
pub(super) struct Wheeled<'w, 's> {
    parents: Query<'w, 's, &'static ChildOf>,
    kinds: Query<'w, 's, (Has<ScrollDown>, Has<ScrollBoth>)>,
    canvases: Query<'w, 's, (&'static mut Canvas, &'static ComputedNode, &'static UiGlobalTransform)>,
}

pub(super) fn on_scroll(
    mut scroll: On<PointerScroll>,
    keys: Res<ButtonInput<KeyCode>>,
    per_line: Res<MouseScrollPixelsPerLine>,
    ui_scale: Option<Res<UiScale>>,
    mut wheeled: Wheeled,
    mut areas: Query<(&ComputedNode, &mut ScrollPosition)>,
) {
    let start = scroll.entity;
    let Wheeled { parents, kinds, canvases } = &mut wheeled;
    let lineage = || std::iter::once(start).chain(parents.iter_ancestors(start));
    let down = lineage().position(|e| kinds.get(e).is_ok_and(|(down, _)| down));
    let both = lineage().position(|e| kinds.get(e).is_ok_and(|(_, both)| both));
    let canvas = lineage().position(|e| canvases.contains(e));
    let scale = ui_scale.map_or(1.0, |s| s.0);
    let pixels = scroll.to_pixels(&per_line);
    let wheel = in_ui_pixels(Vec2::new(pixels.x, pixels.y), scale);
    let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    if let Some(at) = canvas.filter(|at| canvas_is_nearest(*at, down, both)) {
        let Some(root) = lineage().nth(at) else { return };
        if let Ok((mut canvas, computed, place)) = canvases.get_mut(root) {
            let inverse = computed.inverse_scale_factor();
            let top_left = (place.translation - computed.size() / 2.0) * inverse;
            let pointer = scroll.pointer.position / scale.max(f32::EPSILON) - top_left;
            // With Shift held a turn pans across, as it scrolls an area across.
            let wheel = if shift && wheel.x == 0.0 { Vec2::new(wheel.y, 0.0) } else { wheel };
            wheel_canvas(&mut canvas, wheel, pointer);
        }
        scroll.propagate(false);
        return;
    }
    let down = down.and_then(|at| lineage().nth(at));
    let both = both.and_then(|at| lineage().nth(at));
    if down.is_none() && both.is_none() {
        return;
    }
    let route = route(shift, wheel, down.is_some());
    if let (Some(down), Some(dy)) = (down, route.down) {
        move_by(&mut areas, down, Vec2::new(0.0, dy));
    }
    if let Some(both) = both {
        move_by(&mut areas, both, route.both);
    }
    // Handled here, all of it, so nothing above scrolls it a second time.
    scroll.propagate(false);
}

/// Whether a canvas `at` steps above what the pointer is over is nearer than the nearest area that scrolls each way,
/// and so takes the wheel turn.
fn canvas_is_nearest(at: usize, down: Option<usize>, both: Option<usize>) -> bool {
    [down, both].into_iter().flatten().all(|other| at < other)
}

/// A wheel turn or a touchpad swipe of `logical` pixels on screen, in the UI pixels a [`ScrollPosition`] is in: under a
/// larger [`TextSize`](crate::TextSize) the content follows the fingers on a touchpad, and a notch of the wheel moves
/// as far on screen as at 100%, as a browser zoomed in does.
fn in_ui_pixels(logical: Vec2, ui_scale: f32) -> Vec2 {
    logical / ui_scale.max(f32::EPSILON)
}

/// How a wheel turn splits between the nearest down-only area and the nearest both-ways one.
#[derive(Debug, PartialEq)]
struct Route {
    down: Option<f32>,
    both: Vec2,
}

fn route(shift: bool, wheel: Vec2, has_down: bool) -> Route {
    // With Shift held, a vertical turn goes across; a touchpad's own sideways swipe isn't turned again.
    let wheel = if shift && wheel.x == 0.0 { Vec2::new(wheel.y, 0.0) } else { wheel };
    if has_down {
        // The column takes the vertical part; the board only the sideways one.
        Route { down: (wheel.y != 0.0).then_some(wheel.y), both: Vec2::new(wheel.x, 0.0) }
    } else {
        Route { down: None, both: wheel }
    }
}

fn move_by(areas: &mut Query<(&ComputedNode, &mut ScrollPosition)>, area: Entity, delta: Vec2) {
    let Ok((computed, mut position)) = areas.get_mut(area) else { return };
    let range = scroll_range(computed);
    let next = Vec2::new(scrolled(position.x, delta.x, range.x), scrolled(position.y, delta.y, range.y));
    // Only write a change, so a turn at the end doesn't mark the position changed.
    if next != position.0 {
        position.0 = next;
    }
}

/// Where a wheel turn of `delta` leaves an area at `from` that can move as far as `range`.
fn scrolled(from: f32, delta: f32, range: f32) -> f32 {
    (from - delta).clamp(0.0, range.max(0.0))
}

/// How far past its area content may reach, in physical pixels, and still count as fitting. Layout rounds each edge
/// to a whole physical pixel, so content that fits can come out a pixel past the area holding it (at any scale
/// factor, since the rounding is in physical pixels); scrolling that pixel moved a list that fit (TRAIL-1610).
const ROUNDING_SLACK: f32 = 1.0;

/// How far an area scrolls each way, in UI pixels as [`ScrollPosition`] and `Val::Px` are (larger than logical ones
/// under a larger [`TextSize`](crate::TextSize)): nothing along an axis its content
/// overflows by no more than [`ROUNDING_SLACK`]. The wheel, the scrollbar and the clamp below all ask this, so an
/// area that has no range shows no bar and does not move.
pub(crate) fn scroll_range(computed: &ComputedNode) -> Vec2 {
    // Layout is in physical pixels. Scrollbars take a lane out of what's visible.
    let over = computed.content_size() - (computed.size() - computed.scrollbar_size);
    let real = over.cmpgt(Vec2::splat(ROUNDING_SLACK));
    Vec2::select(real, over, Vec2::ZERO) * computed.inverse_scale_factor
}

/// Brings an area's [`ScrollPosition`] back within its range once layout has found it shorter: content that shrank
/// to fit leaves no position behind for the next growth to jump to. Layout draws within the range anyway; this keeps
/// the position the wheel starts from honest.
#[expect(clippy::type_complexity)]
pub(super) fn clamp_scroll_positions(
    mut areas: Query<
        (&ComputedNode, &mut ScrollPosition),
        (Or<(With<ScrollDown>, With<ScrollBoth>)>, Changed<ComputedNode>),
    >,
) {
    for (computed, mut position) in &mut areas {
        let clamped = position.0.clamp(Vec2::ZERO, scroll_range(computed));
        if clamped != position.0 {
            position.0 = clamped;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scroll_body_is_held_to_its_frame_and_scrolls_down() {
        let mut world = World::new();
        let (frame, body) = (world.spawn(scroll_frame()).id(), world.spawn(scroll_body()).id());
        assert_eq!(world.get::<Node>(frame).unwrap().min_height, px(0));
        let node = world.get::<Node>(body).unwrap();
        assert_eq!((node.min_height, node.overflow), (px(0), Overflow::scroll_y()));
        assert!(world.get::<ScrollDown>(body).is_some());
    }

    #[test]
    fn the_nodes_take_the_gap_and_growth_and_keep_the_minimum_size() {
        let body = scroll_body_node(7.0, 1.0);
        assert_eq!((body.row_gap, body.flex_grow, body.min_height), (px(7.0), 1.0, px(0)));
        assert_eq!(body.overflow, Overflow::scroll_y());
        let frame = scroll_frame_node(0.0);
        assert_eq!((frame.flex_grow, frame.min_height), (0.0, px(0)));
    }

    #[test]
    fn over_a_column_the_wheel_scrolls_it_down_and_shift_scrolls_the_board_across() {
        assert_eq!(route(false, Vec2::new(0.0, -3.0), true), Route { down: Some(-3.0), both: Vec2::ZERO });
        assert_eq!(route(true, Vec2::new(0.0, -3.0), true), Route { down: None, both: Vec2::new(-3.0, 0.0) });
        // A touchpad's sideways swipe goes to the board, and a diagonal one is split.
        assert_eq!(route(false, Vec2::new(-5.0, 0.0), true), Route { down: None, both: Vec2::new(-5.0, 0.0) });
        assert_eq!(route(false, Vec2::new(-5.0, -2.0), true), Route { down: Some(-2.0), both: Vec2::new(-5.0, 0.0) });
    }

    #[test]
    fn the_nearest_of_a_canvas_and_the_areas_takes_the_wheel() {
        assert!(canvas_is_nearest(2, None, None), "a canvas on its own zooms");
        assert!(canvas_is_nearest(1, None, Some(4)), "a canvas inside a page that scrolls zooms");
        assert!(!canvas_is_nearest(3, Some(0), None), "a list that scrolls inside a canvas scrolls");
        assert!(!canvas_is_nearest(3, Some(5), Some(1)));
    }

    #[test]
    fn with_no_column_the_board_takes_both_ways() {
        assert_eq!(route(false, Vec2::new(0.0, -3.0), false), Route { down: None, both: Vec2::new(0.0, -3.0) });
        assert_eq!(route(true, Vec2::new(0.0, -3.0), false), Route { down: None, both: Vec2::new(-3.0, 0.0) });
    }

    /// A body of three rows in a sidebar-like column `height` high, laid out at `scale`, the last row holding
    /// something `poke` taller than itself (as a text whose measured box overhangs its node does). Returns its range.
    fn laid_out_range(scale: f32, height: f32, poke: f32) -> Vec2 {
        laid_out_range_at(scale, 1.0, height, poke)
    }

    /// [`laid_out_range`] on a screen of `scale` with the UI drawn at `ui_scale`.
    fn laid_out_range_at(scale: f32, ui_scale: f32, height: f32, poke: f32) -> Vec2 {
        let mut app = crate::layout_test::layout_app(scale, ui_scale);
        let world = app.world_mut();
        let root = world
            .spawn(Node { flex_direction: FlexDirection::Column, width: px(220), height: px(height), ..default() })
            .id();
        // A title of a text's fractional height, so the body starts off the pixel grid.
        world.spawn((Node { height: px(21.6), flex_shrink: 0.0, ..default() }, ChildOf(root)));
        let frame = world.spawn((scroll_frame(), ChildOf(root))).id();
        let body = world.spawn((scroll_body(), ChildOf(frame))).id();
        let mut last = body;
        for _ in 0..3 {
            last = world.spawn((Node { height: px(24), flex_shrink: 0.0, ..default() }, ChildOf(body))).id();
        }
        world.spawn((Node { width: px(24), height: px(24.0 + poke), ..default() }, ChildOf(last)));
        app.update();
        scroll_range(app.world().get::<ComputedNode>(body).unwrap())
    }

    #[test]
    fn a_body_that_fits_has_no_range_at_any_scale_even_a_rounded_pixel_over() {
        for scale in [1.0, 1.25, 1.5, 1.75, 2.0] {
            assert_eq!(laid_out_range(scale, 400.0, 0.0), Vec2::ZERO, "fits at {scale}");
            // Under a physical pixel over in layout's own sums comes out a whole one over at some of these once
            // rounded: still no range.
            for tenths in 1..10 {
                let poke = tenths as f32 / 10.0 / scale;
                assert_eq!(laid_out_range(scale, 400.0, poke), Vec2::ZERO, "{poke} over at {scale}");
            }
        }
    }

    #[test]
    fn a_body_taller_than_its_frame_scrolls_as_far_as_it_overflows() {
        for scale in [1.0, 1.5, 2.0] {
            // 21.6 of title, then 88 of rows in the 60 left over.
            let range = laid_out_range(scale, 81.6, 0.0);
            assert_eq!(range.x, 0.0);
            assert!((range.y - 28.0).abs() <= 1.0, "{range} at {scale}");
        }
    }

    #[test]
    fn under_a_larger_text_size_the_range_is_in_the_ui_pixels_a_scroll_position_is_in() {
        for (scale, ui_scale) in [(1.0, 1.5), (2.0, 1.5), (1.0, 0.8)] {
            assert_eq!(laid_out_range_at(scale, ui_scale, 400.0, 0.0), Vec2::ZERO, "fits at {scale} x {ui_scale}");
            let range = laid_out_range_at(scale, ui_scale, 81.6, 0.0);
            assert!((range.y - 28.0).abs() <= 1.0, "{range} at {scale} x {ui_scale}");
        }
    }

    #[test]
    fn a_wheel_turn_moves_as_far_on_screen_whatever_the_text_size() {
        assert_eq!(in_ui_pixels(Vec2::new(0.0, -60.0), 1.0), Vec2::new(0.0, -60.0));
        // At 150% a UI pixel is a pixel and a half on screen, so 60 on screen is 40 of the content's.
        assert_eq!(in_ui_pixels(Vec2::new(-30.0, -60.0), 1.5), Vec2::new(-20.0, -40.0));
    }

    fn area(content: Vec2, inverse_scale_factor: f32) -> ComputedNode {
        ComputedNode { size: Vec2::splat(100.0), content_size: content, inverse_scale_factor, ..default() }
    }

    #[test]
    fn a_physical_pixel_of_overflow_is_rounding_not_range_both_ways() {
        assert_eq!(scroll_range(&area(Vec2::new(101.0, 101.0), 1.0)), Vec2::ZERO);
        assert_eq!(scroll_range(&area(Vec2::new(101.0, 140.0), 1.0)), Vec2::new(0.0, 40.0));
        assert_eq!(scroll_range(&area(Vec2::new(140.0, 101.0), 1.0)), Vec2::new(40.0, 0.0));
        // At twice the scale a physical pixel is half a logical one, and the range comes out logical.
        assert_eq!(scroll_range(&area(Vec2::new(101.0, 101.0), 0.5)), Vec2::ZERO);
        assert_eq!(scroll_range(&area(Vec2::new(140.0, 140.0), 0.5)), Vec2::new(20.0, 20.0));
    }

    /// Where a wheel turn of `delta` leaves an area at the top with `content` in its 100 by 100.
    fn wheeled(content: Vec2, delta: Vec2) -> Vec2 {
        use bevy::ecs::system::RunSystemOnce;
        let mut world = World::new();
        let area = world.spawn((ScrollBoth, area(content, 1.0))).id();
        world
            .run_system_once(move |mut areas: Query<(&ComputedNode, &mut ScrollPosition)>| {
                move_by(&mut areas, area, delta);
            })
            .unwrap();
        world.get::<ScrollPosition>(area).unwrap().0
    }

    #[test]
    fn the_wheel_does_not_move_an_area_whose_content_fits_to_a_pixel() {
        assert_eq!(wheeled(Vec2::new(101.0, 101.0), Vec2::new(-40.0, -40.0)), Vec2::ZERO);
        assert_eq!(wheeled(Vec2::new(101.0, 300.0), Vec2::new(-40.0, -40.0)), Vec2::new(0.0, 40.0));
        assert_eq!(wheeled(Vec2::new(300.0, 101.0), Vec2::new(-40.0, -40.0)), Vec2::new(40.0, 0.0));
    }

    #[test]
    fn a_position_left_from_taller_content_comes_back_when_it_shrinks() {
        let mut app = App::new();
        app.add_systems(Update, clamp_scroll_positions);
        let down = app.world_mut().spawn((ScrollDown, area(Vec2::new(100.0, 300.0), 1.0))).id();
        let both = app.world_mut().spawn((ScrollBoth, area(Vec2::new(300.0, 300.0), 1.0))).id();
        app.world_mut().get_mut::<ScrollPosition>(down).unwrap().0 = Vec2::new(0.0, 150.0);
        app.world_mut().get_mut::<ScrollPosition>(both).unwrap().0 = Vec2::new(150.0, 150.0);
        app.update();
        let at = |app: &App, e| app.world().get::<ScrollPosition>(e).unwrap().0;
        assert_eq!((at(&app, down), at(&app, both)), (Vec2::new(0.0, 150.0), Vec2::new(150.0, 150.0)));
        // Shorter, but still taller than the area: back to its new end.
        app.world_mut().get_mut::<ComputedNode>(down).unwrap().content_size = Vec2::new(100.0, 120.0);
        app.world_mut().get_mut::<ComputedNode>(both).unwrap().content_size = Vec2::new(130.0, 300.0);
        app.update();
        assert_eq!((at(&app, down), at(&app, both)), (Vec2::new(0.0, 20.0), Vec2::new(30.0, 150.0)));
        // Fits, to within a rounded pixel: back to the start.
        app.world_mut().get_mut::<ComputedNode>(down).unwrap().content_size = Vec2::new(100.0, 101.0);
        app.world_mut().get_mut::<ComputedNode>(both).unwrap().content_size = Vec2::new(101.0, 100.0);
        app.update();
        assert_eq!((at(&app, down), at(&app, both)), (Vec2::ZERO, Vec2::new(0.0, 0.0)));
    }

    #[test]
    fn an_area_moves_as_far_as_its_end_and_no_further() {
        assert_eq!(scrolled(80.0, -40.0, 100.0), 100.0);
        assert_eq!(scrolled(0.0, 40.0, 100.0), 0.0);
        // An area that fits never moves.
        assert_eq!(scrolled(0.0, -40.0, -12.0), 0.0);
    }
}
