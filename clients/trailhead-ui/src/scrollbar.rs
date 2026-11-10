//! A scrollbar along one edge of a scrolling area: [`scrollbar`] for the area and the way it scrolls.
//!
//! A vertical one lies just outside the area's right edge, in the padding of the node that holds the area, so the area is as wide with it as without and the spacing left and right stays equal; a horizontal one along the bottom (the board).
//! Spawn it as a child of the node that holds the area; that node needs `space::PANEL_INSET` of padding on the right,
//! which the bar sits in the middle of, the same gap on either side of it.
//! It takes its color from feathers' scrollbar tokens. It shows only while the area overflows and the pointer is over
//! the area or the bar (anywhere in the node holding both) and inside the window, or while its thumb is being dragged,
//! which carries on past the holder's edge (TRAIL-1145). It fades in and out over [`FADE_IN`] and [`FADE_OUT`]: its
//! colors are the theme's with their alpha scaled. It starts hidden: shown from the start, it was drawn for a frame before its area's
//! height was known, a flash on every page that opens (TRAIL-1139).

use bevy::ecs::template::EntityTemplate;
use bevy::feathers::controls::FeathersScrollbar;
use bevy::feathers::theme::SurfaceLevel;
use bevy::feathers::theme::{ThemeBackgroundColor, ThemeContext, UiTheme};
use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::ui_widgets::{ControlOrientation, Scrollbar, ScrollbarDragState};
use bevy::window::RequestRedraw;

use crate::space;

/// How thick a scrollbar is.
pub const SCROLLBAR_WIDTH: f32 = 6.0;
/// The gap either side of a scrollbar: between it and the area, and between it and the holder's edge.
pub const SCROLLBAR_EDGE: f32 = (space::PANEL_INSET - SCROLLBAR_WIDTH) / 2.0;

/// How long a scrollbar takes to fade in, in seconds.
pub const FADE_IN: f32 = 0.12;
/// How long a scrollbar takes to fade out, in seconds: a little slower, so it doesn't snap away.
pub const FADE_OUT: f32 = 0.18;

/// How far a scrollbar has faded in: 0 is gone (and not displayed), 1 is the theme's colors.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
pub struct ScrollbarFade(pub f32);

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

/// `fade` moved toward shown (1) or gone (0) by `seconds`.
fn step(fade: f32, shown: bool, seconds: f32) -> f32 {
    if shown { (fade + seconds / FADE_IN).min(1.0) } else { (fade - seconds / FADE_OUT).max(0.0) }
}

/// Fades each scrollbar in while its target has range to scroll and the pointer is over the node holding the two and
/// inside the window, or its thumb is being dragged; fades it out otherwise. A holder is given [`Hovered`] the first
/// time it is seen. While a bar is fading, the app is asked to redraw, since it otherwise only does on input.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub(super) fn show_scrollbars(
    mut bars: Query<(Entity, &Scrollbar, &ChildOf, &mut Node, Option<&mut ScrollbarFade>)>,
    targets: Query<&ComputedNode>,
    holders: Query<&Hovered>,
    children: Query<&Children>,
    drags: Query<&ScrollbarDragState>,
    windows: Query<&Window>,
    mut colored: Query<(&ThemeBackgroundColor, Option<&ThemeContext>, &mut BackgroundColor)>,
    theme: Option<Res<UiTheme>>,
    time: Res<Time>,
    mut redraw: MessageWriter<RequestRedraw>,
    mut commands: Commands,
) {
    // The pointer's last place in a window is kept after it leaves, so its hover never ends without this.
    let in_window = windows.iter().any(|w| w.cursor_position().is_some());
    for (entity, bar, holder, mut node, fade) in &mut bars {
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
        let shown = overflowing && ((hovered && in_window) || dragging);
        let Some(mut fade) = fade else {
            commands.entity(entity).insert(ScrollbarFade::default());
            continue;
        };
        let next = step(fade.0, shown, time.delta_secs());
        if fade.0 != next {
            fade.0 = next;
        }
        if next != f32::from(u8::from(shown)) {
            redraw.write(RequestRedraw);
        }
        let display = if next > 0.0 { Display::Flex } else { Display::None };
        if node.display != display {
            node.display = display;
        }
        let Some(theme) = theme.as_ref() else { continue };
        // The track and the thumb, in the theme's colors at the bar's fade.
        for part in std::iter::once(entity).chain(children.iter_descendants(entity)) {
            let Ok((token, context, mut color)) = colored.get_mut(part) else { continue };
            let full = theme.context_color(&token.0, context.map_or(SurfaceLevel::Base, |c| c.0));
            let faded = full.with_alpha(full.alpha() * next);
            if color.0 != faded {
                color.0 = faded;
            }
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
    fn the_scene_spawns_a_scrollbar_for_its_target_and_orientation() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>();
        let target = app.world_mut().spawn_empty().id();
        let bar = app.world_mut().commands().spawn_scene(scrollbar(target, ControlOrientation::Horizontal)).id();
        app.world_mut().flush();
        app.update();
        let bar = app.world().get::<Scrollbar>(bar).expect("the scene is a scrollbar");
        assert_eq!((bar.target, bar.orientation), (target, ControlOrientation::Horizontal));
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

    /// A holder with an area of `content` height in 100 and a vertical bar, in an app that runs [`show_scrollbars`],
    /// with the pointer in its window. Each update advances the clock by [`Tick`], a second unless a test changes it,
    /// so a fade runs through in one.
    fn holder(content: f32) -> (App, Entity, Entity) {
        let mut app = App::new();
        app.add_message::<RequestRedraw>()
            .insert_resource(Time::<()>::default())
            .insert_resource(Tick(1.0))
            .add_systems(First, |tick: Res<Tick>, mut time: ResMut<Time>| {
                time.advance_by(std::time::Duration::from_secs_f32(tick.0))
            })
            .add_systems(Update, show_scrollbars);
        let mut window = Window::default();
        window.set_cursor_position(Some(Vec2::new(10.0, 10.0)));
        app.world_mut().spawn(window);
        let holder = app.world_mut().spawn_empty().id();
        let area = ComputedNode { size: Vec2::new(100.0, 100.0), content_size: Vec2::new(100.0, content), ..default() };
        let target = app.world_mut().spawn((area, ChildOf(holder))).id();
        let bar = app
            .world_mut()
            .spawn((Scrollbar::new(target, ControlOrientation::Vertical, 8.0), placement(ControlOrientation::Vertical)))
            .insert(ChildOf(holder))
            .id();
        app.update();
        app.update();
        (app, holder, bar)
    }

    #[derive(Resource)]
    struct Tick(f32);

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
    fn the_pointer_leaving_the_window_hides_the_bar() {
        let (mut app, holder, bar) = holder(300.0);
        app.world_mut().entity_mut(holder).insert(Hovered(true));
        app.update();
        assert!(shown(&app, bar));
        // Leaving the window doesn't end the hover.
        let world = app.world_mut();
        world.query::<&mut Window>().single_mut(world).unwrap().set_cursor_position(None);
        app.update();
        assert!(!shown(&app, bar));
    }

    #[test]
    fn a_bar_fades_in_quickly_and_out_more_slowly() {
        assert_eq!(step(0.0, true, FADE_IN / 2.0), 0.5);
        assert_eq!(step(0.5, true, FADE_IN), 1.0);
        assert_eq!(step(1.0, false, FADE_OUT / 2.0), 0.5);
        assert_eq!(step(0.5, false, FADE_OUT), 0.0);
        const { assert!(FADE_OUT > FADE_IN) };
    }

    #[test]
    fn a_fading_bar_is_drawn_in_the_themes_colors_at_its_fade() {
        let (mut app, holder, bar) = holder(300.0);
        let theme = crate::theme::Theme::new().resource();
        let full = theme.context_color(&bevy::feathers::tokens::SCROLLBAR_BG, SurfaceLevel::Base);
        app.insert_resource(theme);
        app.world_mut()
            .entity_mut(bar)
            .insert((ThemeBackgroundColor(bevy::feathers::tokens::SCROLLBAR_BG), BackgroundColor::default()));
        app.world_mut().entity_mut(holder).insert(Hovered(true));
        app.world_mut().get_mut::<ScrollbarFade>(bar).unwrap().0 = 0.0;
        app.insert_resource(Tick(FADE_IN / 2.0));
        app.update();
        let color = app.world().get::<BackgroundColor>(bar).unwrap().0;
        assert_eq!(color, full.with_alpha(full.alpha() * 0.5));
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
