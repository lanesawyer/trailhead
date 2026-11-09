//! A scrollbar along one edge of a scrolling area: [`scrollbar`] for the area and the way it scrolls.
//!
//! A vertical one lies just outside the area's right edge, in the padding of the node that holds the area, so the area is as wide with it as without and the spacing left and right stays equal; a horizontal one along the bottom (the board).
//! Spawn it as a child of the node that holds the area; that node needs `space::PANEL_INSET` of padding on the right,
//! which the bar sits in the middle of, the same gap on either side of it.
//! It takes its color from feathers' scrollbar tokens. It shows only while the area overflows and the pointer is over
//! the area or the bar (anywhere in the node holding both), or while its thumb is being dragged, which carries on past
//! the holder's edge (TRAIL-1145). It starts hidden: shown from the start, it was drawn for a frame before its area's
//! height was known, a flash on every page that opens (TRAIL-1139).

use bevy::ecs::template::EntityTemplate;
use bevy::feathers::controls::FeathersScrollbar;
use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::ui_widgets::{ControlOrientation, Scrollbar, ScrollbarDragState};

use crate::space;

/// How thick a scrollbar is.
pub const SCROLLBAR_WIDTH: f32 = 6.0;
/// The gap either side of a scrollbar: between it and the area, and between it and the holder's edge.
pub const SCROLLBAR_EDGE: f32 = (space::PANEL_INSET - SCROLLBAR_WIDTH) / 2.0;

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

/// The node that puts the bar along its edge, hidden until [`show_scrollbars`] shows it.
fn placement(orientation: ControlOrientation) -> Node {
    let node = match orientation {
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
    };
    Node { display: Display::None, ..node }
}

/// Whether content of `content` length in a node of `size` has anything to scroll along `orientation`.
fn overflows(orientation: ControlOrientation, size: Vec2, content: Vec2) -> bool {
    match orientation {
        ControlOrientation::Vertical => content.y > size.y + 0.5,
        ControlOrientation::Horizontal => content.x > size.x + 0.5,
    }
}

/// Shows each scrollbar while its target has range to scroll and the pointer is over the node holding the two, or its
/// thumb is being dragged; hides it otherwise. A holder is given [`Hovered`] the first time it is seen.
pub(super) fn show_scrollbars(
    mut bars: Query<(Entity, &Scrollbar, &ChildOf, &mut Node)>,
    targets: Query<&ComputedNode>,
    holders: Query<&Hovered>,
    children: Query<&Children>,
    drags: Query<&ScrollbarDragState>,
    mut commands: Commands,
) {
    for (entity, bar, holder, mut node) in &mut bars {
        let Ok(target) = targets.get(bar.target) else { continue };
        let hovered = match holders.get(holder.parent()) {
            Ok(hovered) => hovered.0,
            Err(_) => {
                commands.entity(holder.parent()).insert(Hovered::default());
                false
            }
        };
        let dragging = children.iter_descendants(entity).any(|e| drags.get(e).is_ok_and(|d| d.dragging));
        let overflowing = overflows(bar.orientation, target.size(), target.content_size());
        let display = if overflowing && (hovered || dragging) { Display::Flex } else { Display::None };
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
    fn it_starts_hidden_until_the_area_is_known_to_overflow() {
        for orientation in [ControlOrientation::Vertical, ControlOrientation::Horizontal] {
            assert_eq!(placement(orientation).display, Display::None);
        }
    }

    #[test]
    fn it_shows_only_along_the_way_the_area_overflows() {
        let size = Vec2::new(100.0, 100.0);
        let wide = Vec2::new(300.0, 100.0);
        assert!(overflows(ControlOrientation::Horizontal, size, wide));
        assert!(!overflows(ControlOrientation::Vertical, size, wide));
        assert!(!overflows(ControlOrientation::Horizontal, size, size));
    }

    /// A holder with an area of `content` height in 100 and a vertical bar, in an app that runs [`show_scrollbars`].
    fn holder(content: f32) -> (App, Entity, Entity) {
        let mut app = App::new();
        app.add_systems(Update, show_scrollbars);
        let holder = app.world_mut().spawn_empty().id();
        let area = ComputedNode { size: Vec2::new(100.0, 100.0), content_size: Vec2::new(100.0, content), ..default() };
        let target = app.world_mut().spawn((area, ChildOf(holder))).id();
        let bar = app
            .world_mut()
            .spawn((Scrollbar::new(target, ControlOrientation::Vertical, 8.0), placement(ControlOrientation::Vertical)))
            .insert(ChildOf(holder))
            .id();
        app.update();
        (app, holder, bar)
    }

    fn shown(app: &App, bar: Entity) -> bool {
        app.world().get::<Node>(bar).unwrap().display == Display::Flex
    }

    #[test]
    fn an_overflowing_area_shows_its_bar_only_under_the_pointer() {
        let (mut app, holder, bar) = holder(300.0);
        assert_eq!(app.world().get::<Hovered>(holder), Some(&Hovered(false)), "the holder is watched for the pointer");
        assert!(!shown(&app, bar));
        app.world_mut().entity_mut(holder).insert(Hovered(true));
        app.update();
        assert!(shown(&app, bar));
        app.world_mut().entity_mut(holder).insert(Hovered(false));
        app.update();
        assert!(!shown(&app, bar));
    }

    #[test]
    fn an_area_that_fits_shows_no_bar_even_under_the_pointer() {
        let (mut app, holder, bar) = holder(100.0);
        app.world_mut().entity_mut(holder).insert(Hovered(true));
        app.update();
        assert!(!shown(&app, bar));
    }

    #[test]
    fn a_bar_stays_while_its_thumb_is_dragged_past_the_holder() {
        let (mut app, _, bar) = holder(300.0);
        let thumb = app.world_mut().spawn((ScrollbarDragState::default(), ChildOf(bar))).id();
        app.world_mut().get_mut::<ScrollbarDragState>(thumb).unwrap().dragging = true;
        app.update();
        assert!(shown(&app, bar));
        app.world_mut().get_mut::<ScrollbarDragState>(thumb).unwrap().dragging = false;
        app.update();
        assert!(!shown(&app, bar));
    }
}
