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
//! Not `bevy_ui_widgets`' `ScrollArea`, which scrolls only down and would move the area before Shift was noticed.
//!
//! Give a node the marker and the matching `overflow` (`Overflow::scroll()` or `scroll_x()` for [`ScrollBoth`],
//! `scroll_y()` for [`ScrollDown`]). A view that rebuilds its nodes keeps the [`ScrollPosition`] itself and puts it
//! back on the new node, or the area jumps to its start on every redraw. Better, keep the node (see `Keyed`).

use bevy::input::mouse::MouseScrollPixelsPerLine;
use bevy::picking::events::PointerScroll;
use bevy::prelude::*;

use super::{SCROLLBAR_WIDTH, space};

/// The node that holds a panel's scrolling body and its scrollbar, so the bar runs along the body only. It takes the
/// height the panel has left and may shrink below its content, so the body scrolls (TRAIL-836).
pub fn scroll_frame() -> impl Bundle {
    (
        Name::new("scroll frame"),
        Node { flex_direction: FlexDirection::Column, flex_grow: 1.0, min_height: px(0), ..default() },
    )
}

/// The body of a panel that scrolls down on its own, wheel and touchpad, for [`super::scrollbar`] to sit beside: a
/// column of rows with a gutter on its right for the bar. Put it in a [`scroll_frame`].
pub fn scroll_body() -> impl Bundle {
    (
        Name::new("scroll body"),
        Node {
            flex_direction: FlexDirection::Column,
            row_gap: px(space::ROWS),
            min_height: px(0),
            padding: UiRect::right(px(SCROLLBAR_WIDTH + space::SCROLLBAR_GAP)),
            overflow: Overflow::scroll_y(),
            ..default()
        },
        ScrollDown,
    )
}

/// Marks an area that scrolls both ways.
#[derive(Component, Clone, Default)]
#[require(ScrollPosition)]
pub struct ScrollBoth;

/// Marks an area that scrolls down only, inside a [`ScrollBoth`] that takes the sideways part.
#[derive(Component, Clone, Default)]
#[require(ScrollPosition)]
pub struct ScrollDown;

pub(super) fn on_scroll(
    mut scroll: On<PointerScroll>,
    keys: Res<ButtonInput<KeyCode>>,
    per_line: Res<MouseScrollPixelsPerLine>,
    parents: Query<&ChildOf>,
    downs: Query<(), With<ScrollDown>>,
    boths: Query<(), With<ScrollBoth>>,
    mut areas: Query<(&ComputedNode, &mut ScrollPosition)>,
) {
    let start = scroll.entity;
    let lineage = || std::iter::once(start).chain(parents.iter_ancestors(start));
    let down = lineage().find(|e| downs.contains(*e));
    let both = lineage().find(|e| boths.contains(*e));
    if down.is_none() && both.is_none() {
        return;
    }
    let pixels = scroll.to_pixels(&per_line);
    let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    let route = route(shift, Vec2::new(pixels.x, pixels.y), down.is_some());
    if let (Some(down), Some(dy)) = (down, route.down) {
        move_by(&mut areas, down, Vec2::new(0.0, dy));
    }
    if let Some(both) = both {
        move_by(&mut areas, both, route.both);
    }
    // Handled here, all of it, so nothing above scrolls it a second time.
    scroll.propagate(false);
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
    // Layout is in physical pixels and `ScrollPosition` in logical ones. Scrollbars take a lane out of what's visible.
    let scale = computed.inverse_scale_factor;
    let visible = (computed.size() - computed.scrollbar_size) * scale;
    let range = (computed.content_size() * scale - visible).max(Vec2::ZERO);
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
    fn over_a_column_the_wheel_scrolls_it_down_and_shift_scrolls_the_board_across() {
        assert_eq!(route(false, Vec2::new(0.0, -3.0), true), Route { down: Some(-3.0), both: Vec2::ZERO });
        assert_eq!(route(true, Vec2::new(0.0, -3.0), true), Route { down: None, both: Vec2::new(-3.0, 0.0) });
        // A touchpad's sideways swipe goes to the board, and a diagonal one is split.
        assert_eq!(route(false, Vec2::new(-5.0, 0.0), true), Route { down: None, both: Vec2::new(-5.0, 0.0) });
        assert_eq!(route(false, Vec2::new(-5.0, -2.0), true), Route { down: Some(-2.0), both: Vec2::new(-5.0, 0.0) });
    }

    #[test]
    fn with_no_column_the_board_takes_both_ways() {
        assert_eq!(route(false, Vec2::new(0.0, -3.0), false), Route { down: None, both: Vec2::new(0.0, -3.0) });
        assert_eq!(route(true, Vec2::new(0.0, -3.0), false), Route { down: None, both: Vec2::new(-3.0, 0.0) });
    }

    #[test]
    fn an_area_moves_as_far_as_its_end_and_no_further() {
        assert_eq!(scrolled(80.0, -40.0, 100.0), 100.0);
        assert_eq!(scrolled(0.0, 40.0, 100.0), 0.0);
        // An area that fits never moves.
        assert_eq!(scrolled(0.0, -40.0, -12.0), 0.0);
    }
}
