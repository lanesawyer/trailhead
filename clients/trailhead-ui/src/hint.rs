//! A short hint shown under whatever the pointer is over, for icon-only buttons and truncated text.
//!
//! Feathers 0.20 has no tooltip. One hint node lives at the top layer; the pointer entering anything with a [`Hint`] fills it and
//! puts it under that thing, and leaving hides it.

use bevy::feathers::theme::ThemeBackgroundColor;
use bevy::picking::events::{PointerEnter, PointerLeave};
use bevy::prelude::*;

use super::{layer, radius, size, space, text, token};

/// What the hint says over this entity.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct Hint(pub String);

/// The one node hints are shown in.
#[derive(Component)]
struct HintPopup;

/// Spawns the hint node and shows it over anything with a [`Hint`].
pub struct HintPlugin;

impl Plugin for HintPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_hint).add_observer(show_hint).add_observer(hide_hint);
    }
}

#[derive(Component, Clone, Default)]
struct HintText;

fn spawn_hint(mut commands: Commands) {
    let popup = commands
        .spawn((
            Name::new("hint"),
            HintPopup,
            Node {
                position_type: PositionType::Absolute,
                padding: UiRect::axes(px(space::CONTROL_INSET), px(space::STACKED)),
                border_radius: BorderRadius::all(px(radius::SMALL)),
                display: Display::None,
                ..default()
            },
            ThemeBackgroundColor(token::HINT_BG),
            GlobalZIndex(layer::HINT),
            // A hint under the pointer must not take the hover from what it describes.
            Pickable::IGNORE,
        ))
        .id();
    commands.spawn_scene(bsn! { @text("", size::SECONDARY) HintText Pickable::IGNORE }).insert(ChildOf(popup));
}

fn show_hint(
    over: On<PointerEnter>,
    hints: Query<(&Hint, &ComputedNode, &UiGlobalTransform)>,
    mut popup: Single<&mut Node, With<HintPopup>>,
    mut words: Single<&mut Text, With<HintText>>,
) {
    let Ok((hint, node, transform)) = hints.get(over.event_target()) else { return };
    if hint.0.is_empty() {
        return;
    }
    // Layout is in physical pixels; `Node` positions are logical.
    let scale = node.inverse_scale_factor();
    let center = transform.translation * scale;
    let size = node.size() * scale;
    popup.left = px(center.x - size.x / 2.0);
    popup.top = px(center.y + size.y / 2.0 + space::CONTROLS);
    popup.display = Display::Flex;
    words.0.clone_from(&hint.0);
}

fn hide_hint(out: On<PointerLeave>, hints: Query<(), With<Hint>>, mut popup: Single<&mut Node, With<HintPopup>>) {
    if hints.contains(out.event_target()) {
        popup.display = Display::None;
    }
}
