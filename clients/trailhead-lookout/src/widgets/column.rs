//! A board column: one status's cards on the status's own background, under a header with its name and count.
//!
//! The column is fixed-width and as tall as the board; its body scrolls down on its own, with a feathers scrollbar
//! laid over its right edge. The parts are separate so a view can keep one column entity across redraws:
//! [`column_shell`] and [`column_body`] are bundles, and [`column_header`] and [`column_scrollbar`] add the rest.

use bevy::feathers::theme::ThemeBackgroundColor;
use bevy::prelude::*;
use bevy::ui_widgets::ControlOrientation;

use super::{ScrollDown, radius, scrollbar, size, space, status_token, text, text_dim};

/// How wide every column is.
pub const COLUMN_WIDTH: f32 = 260.0;

/// The shell of the column for `status`: fixed width, on the status's own background. Put the header in with
/// [`column_header`], the [`column_body`] under it and its scrollbar after.
pub fn column_shell(status: &str) -> impl Bundle {
    (
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
    )
}

/// The body of a column, which holds its cards and scrolls down on its own.
pub fn column_body() -> impl Bundle {
    (
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
    )
}

/// Puts the body's scrollbar in `shell`, after the body.
pub fn column_scrollbar(commands: &mut Commands, shell: Entity, body: Entity) {
    commands.queue_spawn_scene(scrollbar(body, ControlOrientation::Vertical)).insert(ChildOf(shell));
}

/// Marks the count in a column's header, and says whose it is, so the count can be changed in place.
#[derive(Component, Clone, Copy)]
pub struct ColumnCount(pub Entity);

/// Puts the header, with the status's name and `count`, first in `shell`.
pub fn column_header(commands: &mut Commands, shell: Entity, status: &str, count: usize) {
    let header =
        commands.spawn(Node { column_gap: px(space::CONTROLS), align_items: AlignItems::Baseline, ..default() }).id();
    commands.queue_spawn_scene(text(status.to_string(), size::HEADING)).insert(ChildOf(header));
    commands.queue_spawn_scene(text_dim(count.to_string(), size::SMALL)).insert((ColumnCount(shell), ChildOf(header)));
    commands.entity(shell).insert_children(0, &[header]);
}

/// The quiet line in a column with no cards.
pub fn column_empty() -> impl Scene {
    text_dim("Nothing here", size::SECONDARY)
}
