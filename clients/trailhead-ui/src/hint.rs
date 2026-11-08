//! A short hint shown beside whatever the pointer is over, for icon-only buttons and truncated text.
//!
//! Feathers 0.20 has no tooltip. The pointer entering anything with a [`Hint`] spawns a hint as its child, placed by
//! bevy_ui_widgets' [`Popover`] the way feathers places a menu's popup, and leaving despawns it. Being a child, a hint
//! goes with what it describes: closing a modal from its Close button can't leave "Close" on screen.
//!
//! [`Popover`] only keeps a popup inside the window. A hint also stays inside the nearest [`HintBounds`] around what
//! it describes (a modal) when it fits there: it is centred under its target, else lined up with the target's end or
//! start edge, else put above it. Its size is known only after its first layout, so it is hidden until then.

use crate::theme::BackgroundTone;
use bevy::picking::events::{PointerEnter, PointerLeave};
use bevy::prelude::*;
use bevy::ui::{ComputedUiRenderTargetInfo, OverrideClip};
use bevy::ui_widgets::popover::{Popover, PopoverAlign, PopoverPlacement, PopoverSide};

use super::{layer, radius, size, space, text, token};

/// What the hint says over this entity.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct Hint(pub String);

/// A surface hints stay inside when they fit, such as a modal. Elsewhere they only stay inside the window.
#[derive(Component, Debug, Clone, Default)]
pub struct HintBounds;

/// A shown hint, a child of what it describes.
#[derive(Component)]
struct HintPopup;

/// Shows a hint over anything with a [`Hint`].
pub struct HintPlugin;

impl Plugin for HintPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, fit_hint).add_observer(show_hint).add_observer(hide_hint);
    }
}

/// Where a hint may go, best first.
const PLACEMENTS: [(PopoverSide, PopoverAlign); 6] = [
    (PopoverSide::Bottom, PopoverAlign::Center),
    (PopoverSide::Bottom, PopoverAlign::End),
    (PopoverSide::Bottom, PopoverAlign::Start),
    (PopoverSide::Top, PopoverAlign::Center),
    (PopoverSide::Top, PopoverAlign::End),
    (PopoverSide::Top, PopoverAlign::Start),
];

fn placement((side, align): (PopoverSide, PopoverAlign)) -> PopoverPlacement {
    PopoverPlacement { side, align, gap: space::CONTROLS }
}

fn show_hint(
    over: On<PointerEnter>,
    hints: Query<(&Hint, Option<&Children>)>,
    shown: Query<Entity, With<HintPopup>>,
    mut commands: Commands,
) {
    let target = over.event_target();
    let Ok((hint, children)) = hints.get(target) else { return };
    if hint.0.is_empty() || children.is_some_and(|c| c.iter().any(|e| shown.contains(e))) {
        return;
    }
    // One hint at a time, should a leave have been missed.
    for old in &shown {
        commands.entity(old).despawn();
    }
    let words = hint.0.clone();
    let popup = commands
        .spawn((
            Name::new("hint"),
            HintPopup,
            Node {
                position_type: PositionType::Absolute,
                padding: UiRect::axes(px(space::CONTROL_INSET), px(space::STACKED)),
                border_radius: BorderRadius::all(px(radius::SMALL)),
                ..default()
            },
            BackgroundTone(token::HINT_BG),
            GlobalZIndex(layer::HINT),
            Popover { positions: PLACEMENTS.map(placement).to_vec(), window_margin: space::HINT_MARGIN },
            // Not clipped by a scrolling list or card the target sits in.
            OverrideClip,
            Visibility::Hidden,
            // A hint under the pointer must not take the hover from what it describes.
            Pickable::IGNORE,
            ChildOf(target),
        ))
        .id();
    commands.spawn_scene(bsn! { @text(words, size::SECONDARY) Pickable::IGNORE }).insert(ChildOf(popup));
}

fn hide_hint(
    out: On<PointerLeave>,
    targets: Query<&Children, With<Hint>>,
    shown: Query<(), With<HintPopup>>,
    mut commands: Commands,
) {
    let Ok(children) = targets.get(out.event_target()) else { return };
    for child in children.iter().filter(|e| shown.contains(*e)) {
        commands.entity(child).despawn();
    }
}

/// Once a hint has been laid out, picks where it goes and shows it.
fn fit_hint(
    mut popups: Query<
        (&ChildOf, &ComputedNode, &ComputedUiRenderTargetInfo, &mut Popover, &mut Visibility),
        With<HintPopup>,
    >,
    nodes: Query<(&ComputedNode, &UiGlobalTransform)>,
    parents: Query<&ChildOf>,
    bounds: Query<(), With<HintBounds>>,
) {
    for (target, node, screen, mut popover, mut visibility) in &mut popups {
        let size = node.size() * node.inverse_scale_factor();
        if size == Vec2::ZERO {
            continue;
        }
        let Some(target_rect) = logical_rect(&nodes, target.parent()) else { continue };
        let window = Rect::from_corners(Vec2::ZERO, screen.logical_size());
        let surface = parents
            .iter_ancestors(target.parent())
            .find(|e| bounds.contains(*e))
            .and_then(|e| logical_rect(&nodes, e))
            .map(|r| r.intersect(window));
        let positions = fit(target_rect, size, surface, window);
        if popover.positions != positions {
            popover.positions = positions;
        }
        visibility.set_if_neq(Visibility::Inherited);
    }
}

fn logical_rect(nodes: &Query<(&ComputedNode, &UiGlobalTransform)>, entity: Entity) -> Option<Rect> {
    let (node, transform) = nodes.get(entity).ok()?;
    let scale = node.inverse_scale_factor();
    Some(Rect::from_center_size(transform.translation * scale, node.size() * scale))
}

/// The placements for a hint of `size` under `target`: the first that fits inside `surface`, else the first inside
/// the window less its margin, else all of them for [`Popover`] to pick the least clipped. A surface gets no margin:
/// it is a modal's content box, whose padding is outside it, and a button flush with its edge must still be able to
/// line a hint up with itself.
fn fit(target: Rect, size: Vec2, surface: Option<Rect>, window: Rect) -> Vec<PopoverPlacement> {
    let inside = |area: Rect| {
        PLACEMENTS.into_iter().find(|p| {
            let rect = place(target, size, *p);
            area.contains(rect.min) && area.contains(rect.max)
        })
    };
    match surface.and_then(inside).or_else(|| inside(window.inflate(-space::HINT_MARGIN))) {
        Some(p) => vec![placement(p)],
        None => PLACEMENTS.map(placement).to_vec(),
    }
}

/// Where a hint of `size` lands under or over `target`, as [`Popover`] places it.
fn place(target: Rect, size: Vec2, (side, align): (PopoverSide, PopoverAlign)) -> Rect {
    let x = match align {
        PopoverAlign::Start => target.min.x,
        PopoverAlign::Center => target.center().x - size.x / 2.0,
        PopoverAlign::End => target.max.x - size.x,
    };
    let y = match side {
        PopoverSide::Top => target.min.y - space::CONTROLS - size.y,
        _ => target.max.y + space::CONTROLS,
    };
    Rect::from_corners(Vec2::new(x, y), Vec2::new(x, y) + size)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Rect = Rect { min: Vec2::ZERO, max: Vec2::new(1000.0, 800.0) };
    const HINT: Vec2 = Vec2::new(60.0, 20.0);

    fn chosen(target: Rect, surface: Option<Rect>) -> (PopoverSide, PopoverAlign) {
        let positions = fit(target, HINT, surface, WINDOW);
        assert_eq!(positions.len(), 1, "one placement fits");
        (positions[0].side, positions[0].align)
    }

    #[test]
    fn a_hint_with_room_is_centred_under_its_target() {
        let target = Rect::new(400.0, 100.0, 424.0, 124.0);
        assert_eq!(chosen(target, None), (PopoverSide::Bottom, PopoverAlign::Center));
    }

    #[test]
    fn a_hint_at_the_windows_right_edge_lines_up_with_its_targets_end() {
        let cog = Rect::new(970.0, 8.0, 994.0, 32.0);
        assert_eq!(chosen(cog, None), (PopoverSide::Bottom, PopoverAlign::End));
    }

    #[test]
    fn a_hint_at_the_windows_left_edge_lines_up_with_its_targets_start() {
        let target = Rect::new(6.0, 8.0, 30.0, 32.0);
        assert_eq!(chosen(target, None), (PopoverSide::Bottom, PopoverAlign::Start));
    }

    #[test]
    fn a_hint_stays_inside_its_modal_though_the_window_has_room() {
        // Measured in Lookout's settings modal: Close is flush with the frame's right edge.
        let modal = Rect::new(320.0, 246.0, 960.0, 474.5);
        let close = Rect::new(936.0, 246.0, 960.0, 270.0);
        assert_eq!(chosen(close, None), (PopoverSide::Bottom, PopoverAlign::Center));
        assert_eq!(chosen(close, Some(modal)), (PopoverSide::Bottom, PopoverAlign::End));
    }

    #[test]
    fn a_hint_at_the_bottom_goes_above() {
        let target = Rect::new(400.0, 770.0, 424.0, 794.0);
        assert_eq!(chosen(target, None), (PopoverSide::Top, PopoverAlign::Center));
    }

    #[test]
    fn a_hint_too_big_for_its_modal_falls_back_to_the_window() {
        let modal = Rect::new(200.0, 100.0, 240.0, 140.0);
        let target = Rect::new(208.0, 108.0, 232.0, 132.0);
        assert_eq!(chosen(target, Some(modal)), (PopoverSide::Bottom, PopoverAlign::Center));
    }

    #[test]
    fn a_hint_that_fits_nowhere_leaves_popover_every_choice() {
        let huge = Vec2::new(2000.0, 20.0);
        assert_eq!(fit(Rect::new(400.0, 100.0, 424.0, 124.0), huge, None, WINDOW).len(), PLACEMENTS.len());
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .add_plugins(HintPlugin);
        app
    }

    fn hover(app: &mut App, target: Entity) {
        use bevy::picking::backend::HitData;
        use bevy::picking::events::Pointer;
        use bevy::picking::pointer::{Location, PointerId};
        let location = Location {
            target: bevy::camera::NormalizedRenderTarget::None { width: 1, height: 1 },
            position: Vec2::ZERO,
        };
        app.world_mut().trigger(PointerEnter {
            entity: target,
            pointer: Pointer::new(PointerId::Mouse, location),
            hit: HitData::new(Entity::PLACEHOLDER, 0.0, None, None),
            is_in_bounds: true,
        });
        app.update();
    }

    fn popups(app: &mut App) -> usize {
        app.world_mut().query_filtered::<(), With<HintPopup>>().iter(app.world()).count()
    }

    #[test]
    fn a_hint_goes_with_the_button_it_describes() {
        let mut app = app();
        let modal = app.world_mut().spawn(Node::default()).id();
        let close = app.world_mut().spawn((Node::default(), Hint("Close".into()), ChildOf(modal))).id();
        hover(&mut app, close);
        assert_eq!(popups(&mut app), 1);
        app.world_mut().entity_mut(modal).despawn();
        app.update();
        assert_eq!(popups(&mut app), 0);
    }

    #[test]
    fn hovering_again_keeps_one_hint() {
        let mut app = app();
        let button = app.world_mut().spawn((Node::default(), Hint("Settings".into()))).id();
        let other = app.world_mut().spawn((Node::default(), Hint("Close".into()))).id();
        hover(&mut app, button);
        hover(&mut app, button);
        hover(&mut app, other);
        assert_eq!(popups(&mut app), 1);
    }
}
