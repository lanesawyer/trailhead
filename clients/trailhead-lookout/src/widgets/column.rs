//! A board column: one status's cards on the status's own background, under a header with its name and count.
//!
//! The column is fixed-width and as tall as the board; its body scrolls down on its own, with a feathers scrollbar
//! laid over its right edge. [`column`] spawns the shell, header and body and returns the body, to put cards in.

use bevy::feathers::theme::ThemeBackgroundColor;
use bevy::prelude::*;
use bevy::ui_widgets::ControlOrientation;

use super::{ScrollDown, radius, scrollbar, size, space, status_token, text, text_dim};

/// How wide every column is.
pub const COLUMN_WIDTH: f32 = 260.0;

/// Spawns the column for `status` under `parent`: the header with `count`, the body scrolled to `scroll`, and the
/// body's scrollbar. Returns the body, to put cards in.
pub fn column(commands: &mut Commands, parent: Entity, status: &str, count: usize, scroll: f32) -> Entity {
    let shell = commands
        .spawn((
            Name::new(format!("column {status}")),
            Node {
                flex_direction: FlexDirection::Column,
                width: px(COLUMN_WIDTH),
                flex_shrink: 0.0,
                row_gap: px(space::CARDS),
                padding: UiRect::all(px(space::CONTROL_INSET)),
                border_radius: BorderRadius::all(px(radius::PANEL)),
                ..default()
            },
            ThemeBackgroundColor(status_token(status)),
            ChildOf(parent),
        ))
        .id();
    commands.queue_spawn_scene(column_header(status, count)).insert(ChildOf(shell));
    let body = commands
        .spawn((
            Name::new("column body"),
            Node {
                flex_direction: FlexDirection::Column,
                flex_grow: 1.0,
                min_height: px(0),
                row_gap: px(space::CARDS),
                overflow: Overflow::scroll_y(),
                ..default()
            },
            ScrollDown,
            ScrollPosition(Vec2::new(0.0, scroll)),
            ChildOf(shell),
        ))
        .id();
    commands.queue_spawn_scene(scrollbar(body, ControlOrientation::Vertical)).insert(ChildOf(shell));
    body
}

/// The header: the status's name and how many cards it holds.
fn column_header(status: impl Into<String>, count: usize) -> impl Scene {
    let status = status.into();
    bsn! {
        Node { column_gap: { px(space::CONTROLS) }, align_items: AlignItems::Baseline }
        Children [
            @text(status, size::HEADING)
            --
            @text_dim(count.to_string(), size::SMALL)
        ]
    }
}

/// The quiet line in a column with no cards.
pub fn column_empty() -> impl Scene {
    text_dim("Nothing here", size::SECONDARY)
}
