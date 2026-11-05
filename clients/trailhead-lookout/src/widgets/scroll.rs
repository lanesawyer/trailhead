//! An area that scrolls both ways, where holding Shift turns the wheel sideways (ported from bevy-data-explorer's
//! `widgets/scroll.rs`).
//!
//! Shift is what a browser and a spreadsheet use, and the only way across for a wheel with no sideways axis of its
//! own, which is most of them. A touchpad's sideways swipe moves it across directly.
//!
//! Not `bevy_ui_widgets`' `ScrollArea`, which scrolls only down: its observer runs first and would already have moved
//! the area down by the time this one noticed Shift, so an area that wants Shift reads the wheel itself.
//!
//! Give the node [`ScrollBoth`] and `overflow: Overflow::scroll()`. A view that rebuilds its nodes keeps the
//! [`ScrollPosition`] itself and puts it back on the new node, or the area jumps to its start on every redraw.

use bevy::input::mouse::MouseScrollPixelsPerLine;
use bevy::picking::events::PointerScroll;
use bevy::prelude::*;

/// Marks an area that scrolls both ways.
#[derive(Component, Clone, Default)]
#[require(ScrollPosition)]
pub struct ScrollBoth;

pub(super) fn on_both_scroll(
    mut scroll: On<PointerScroll>,
    keys: Res<ButtonInput<KeyCode>>,
    per_line: Res<MouseScrollPixelsPerLine>,
    mut areas: Query<(&ComputedNode, &mut ScrollPosition), With<ScrollBoth>>,
) {
    let Ok((computed, mut position)) = areas.get_mut(scroll.entity) else { return };
    let pixels = scroll.to_pixels(&per_line);
    let delta = wheel(keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]), Vec2::new(pixels.x, pixels.y));
    // Layout is in physical pixels and `ScrollPosition` in logical ones. Scrollbars take a lane out of what's visible.
    let scale = computed.inverse_scale_factor;
    let visible = (computed.size() - computed.scrollbar_size) * scale;
    let range = (computed.content_size() * scale - visible).max(Vec2::ZERO);
    position.x = scrolled(position.x, delta.x, range.x);
    position.y = scrolled(position.y, delta.y, range.y);
    scroll.propagate(false);
}

/// Which way a wheel turn moves the area: with Shift held, a vertical turn goes sideways.
fn wheel(shift: bool, wheel: Vec2) -> Vec2 {
    if shift && wheel.x == 0.0 { Vec2::new(wheel.y, 0.0) } else { wheel }
}

/// Where a wheel turn of `delta` leaves an area at `from` that can move as far as `range`.
fn scrolled(from: f32, delta: f32, range: f32) -> f32 {
    (from - delta).clamp(0.0, range.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_turns_the_wheel_sideways() {
        assert_eq!(wheel(true, Vec2::new(0.0, -3.0)), Vec2::new(-3.0, 0.0));
        assert_eq!(wheel(false, Vec2::new(0.0, -3.0)), Vec2::new(0.0, -3.0));
        // A touchpad's own sideways swipe isn't turned again.
        assert_eq!(wheel(true, Vec2::new(-5.0, 0.0)), Vec2::new(-5.0, 0.0));
    }

    #[test]
    fn an_area_moves_as_far_as_its_end_and_no_further() {
        assert_eq!(scrolled(80.0, -40.0, 100.0), 100.0);
        assert_eq!(scrolled(0.0, 40.0, 100.0), 0.0);
        // An area that fits never moves.
        assert_eq!(scrolled(0.0, -40.0, -12.0), 0.0);
    }
}
