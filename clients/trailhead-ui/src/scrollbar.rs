//! A scrollbar along one edge of a scrolling area: [`scrollbar`] for the area and the way it scrolls.
//!
//! A vertical one lies just outside the area's right edge, in the padding of the node that holds the area, so the area is as wide with it as without and the spacing left and right stays equal; a horizontal one along the bottom (the board).
//! Spawn it as a child of the node that holds the area; that node needs at least `SCROLLBAR_WIDTH + SCROLLBAR_EDGE` of padding on the right.
//! It takes its color from feathers' scrollbar tokens and hides while the area fits, so there is nothing to scroll.

use bevy::ecs::template::EntityTemplate;
use bevy::feathers::controls::FeathersScrollbar;
use bevy::prelude::*;
use bevy::ui_widgets::{ControlOrientation, Scrollbar};

/// How thick a scrollbar is.
pub const SCROLLBAR_WIDTH: f32 = 6.0;
/// The gap between a vertical bar and the area it scrolls, which is also how far a horizontal bar sits in from the
/// bottom edge. A vertical bar takes this plus `SCROLLBAR_WIDTH` of room.
pub const SCROLLBAR_EDGE: f32 = 2.0;

/// The scrollbar of the scrolling node `target`, for scrolling along `orientation`. Spawn it into the target's
/// parent, after the target.
pub fn scrollbar(target: Entity, orientation: ControlOrientation) -> impl Scene {
    bsn! {
        @FeathersScrollbar {
            @target: { EntityTemplate::from(target) },
            @orientation: { orientation },
        }
        Node { ..{ placement(orientation) } }
    }
}

/// The node that puts the bar along its edge.
fn placement(orientation: ControlOrientation) -> Node {
    match orientation {
        ControlOrientation::Vertical => Node {
            position_type: PositionType::Absolute,
            right: px(-(SCROLLBAR_WIDTH + SCROLLBAR_EDGE)),
            top: px(0),
            bottom: px(0),
            width: px(SCROLLBAR_WIDTH),
            ..default()
        },
        ControlOrientation::Horizontal => Node {
            position_type: PositionType::Absolute,
            left: px(0),
            right: px(0),
            bottom: px(SCROLLBAR_EDGE),
            height: px(SCROLLBAR_WIDTH),
            ..default()
        },
    }
}

/// Whether content of `content` length in a node of `size` has anything to scroll along `orientation`.
fn overflows(orientation: ControlOrientation, size: Vec2, content: Vec2) -> bool {
    match orientation {
        ControlOrientation::Vertical => content.y > size.y + 0.5,
        ControlOrientation::Horizontal => content.x > size.x + 0.5,
    }
}

/// Hides each scrollbar whose target has no range to scroll.
pub(super) fn hide_when_fits(mut bars: Query<(&Scrollbar, &mut Node)>, targets: Query<&ComputedNode>) {
    for (bar, mut node) in &mut bars {
        let Ok(target) = targets.get(bar.target) else { continue };
        let display = if overflows(bar.orientation, target.size(), target.content_size()) {
            Display::Flex
        } else {
            Display::None
        };
        if node.display != display {
            node.display = display;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_is_placed_along_the_edge_it_scrolls() {
        let v = placement(ControlOrientation::Vertical);
        assert_eq!((v.width, v.top, v.bottom), (px(SCROLLBAR_WIDTH), px(0), px(0)));
        // Outside the area, in its holder's padding.
        assert_eq!(v.right, px(-(SCROLLBAR_WIDTH + SCROLLBAR_EDGE)));
        let h = placement(ControlOrientation::Horizontal);
        assert_eq!((h.height, h.left, h.right), (px(SCROLLBAR_WIDTH), px(0), px(0)));
    }

    #[test]
    fn it_shows_only_along_the_way_the_area_overflows() {
        let size = Vec2::new(100.0, 100.0);
        let wide = Vec2::new(300.0, 100.0);
        assert!(overflows(ControlOrientation::Horizontal, size, wide));
        assert!(!overflows(ControlOrientation::Vertical, size, wide));
        assert!(!overflows(ControlOrientation::Horizontal, size, size));
    }

    #[test]
    fn hiding_follows_the_target() {
        let mut app = App::new();
        app.add_systems(Update, hide_when_fits);
        let target = app.world_mut().spawn(ComputedNode::default()).id();
        let bar =
            app.world_mut().spawn((Scrollbar::new(target, ControlOrientation::Horizontal, 8.0), Node::default())).id();
        app.update();
        assert_eq!(app.world().get::<Node>(bar).unwrap().display, Display::None);
    }
}
