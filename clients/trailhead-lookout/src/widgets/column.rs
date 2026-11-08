//! A board column: one status's cards on the status's own background, under a header with its name and count.
//!
//! The column is fixed-width and as tall as its cards, up to the board's height; past that its body scrolls down on its
//! own. The body and its scrollbar sit in a [`column_frame`] under the header, so the bar runs along the body only,
//! in the shell's padding just outside the body, so the cards have the same margin on both sides. The parts are separate so a view can keep one
//! column entity across redraws: [`column_shell`], [`column_frame`] and [`column_body`] are bundles, and
//! [`column_header`] and [`column_scrollbar`] add the rest.

use bevy::prelude::*;
use bevy::ui_widgets::ControlOrientation;

use trailhead_model::Status;

use super::{BackgroundTone, ScrollDown, radius, scrollbar, size, space, status_label, status_token, text, text_dim};

/// How wide every column is.
pub const COLUMN_WIDTH: f32 = 260.0;

/// The shell of the column for `status`: fixed width, on the status's own background, as tall as what is in it but no
/// taller than the board. Put the header in with [`column_header`], the [`column_frame`] under it.
pub fn column_shell(status: Status) -> impl Bundle {
    (
        Name::new(format!("column {status}")),
        Node {
            flex_direction: FlexDirection::Column,
            width: px(COLUMN_WIDTH),
            flex_shrink: 0.0,
            align_self: AlignSelf::FlexStart,
            max_height: percent(100),
            row_gap: px(space::CARDS),
            padding: UiRect::all(px(space::PANEL_INSET)),
            border_radius: BorderRadius::all(px(radius::PANEL)),
            ..default()
        },
        BackgroundTone(status_token(status)),
    )
}

/// What holds a column's body and its scrollbar, so the bar's track is the body's and not the column's. It may shrink
/// below its content so the body scrolls.
pub fn column_frame() -> impl Bundle {
    (Name::new("column frame"), Node { flex_direction: FlexDirection::Column, min_height: px(0), ..default() })
}

/// The body of a column, which holds its cards and scrolls down on its own. Its scrollbar lies in the shell's padding.
pub fn column_body() -> impl Bundle {
    (
        Name::new("column body"),
        Node {
            flex_direction: FlexDirection::Column,
            min_height: px(0),
            row_gap: px(space::CARDS),
            overflow: Overflow::scroll_y(),
            ..default()
        },
        ScrollDown,
    )
}

/// Puts the body's scrollbar in `frame`, after the body.
pub fn column_scrollbar(commands: &mut Commands, frame: Entity, body: Entity) {
    commands.queue_spawn_scene(scrollbar(body, ControlOrientation::Vertical)).insert(ChildOf(frame));
}

/// Marks the count in a column's header, and says whose it is, so the count can be changed in place.
#[derive(Component, Clone, Copy)]
pub struct ColumnCount(pub Entity);

/// Puts the header, with the status's name and `count`, first in `shell`.
pub fn column_header(commands: &mut Commands, shell: Entity, status: Status, count: usize) {
    let header =
        commands.spawn(Node { column_gap: px(space::CONTROLS), align_items: AlignItems::Baseline, ..default() }).id();
    commands.queue_spawn_scene(text(status_label(status), size::HEADING)).insert(ChildOf(header));
    commands.queue_spawn_scene(text_dim(count.to_string(), size::SMALL)).insert((ColumnCount(shell), ChildOf(header)));
    commands.entity(shell).insert_children(0, &[header]);
}

/// The quiet line in a column with no cards.
pub fn column_empty() -> impl Scene {
    text_dim("Nothing here", size::SECONDARY)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::{SCROLLBAR_EDGE, SCROLLBAR_WIDTH};

    fn node_of(bundle: impl Bundle) -> Node {
        let mut world = World::new();
        let id = world.spawn(bundle).id();
        world.get::<Node>(id).unwrap().clone()
    }

    #[test]
    fn the_scrollbar_sits_centered_in_the_shell_padding_so_the_body_has_none_of_its_own() {
        const { assert!(SCROLLBAR_EDGE + SCROLLBAR_WIDTH + SCROLLBAR_EDGE == space::PANEL_INSET) };
        assert_eq!(node_of(column_shell(Status::Open)).padding.right, px(space::PANEL_INSET));
        assert_eq!(node_of(column_body()).padding, UiRect::ZERO);
    }

    #[test]
    fn a_column_ends_at_its_cards_and_never_passes_the_board() {
        let node = node_of(column_shell(Status::Open));
        assert_eq!(node.height, Val::Auto);
        assert_eq!(node.max_height, percent(100));
        assert_eq!(node.align_self, AlignSelf::FlexStart);
    }

    #[test]
    fn the_header_names_the_status_in_words() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>();
        let shell = app.world_mut().spawn(column_shell(Status::InProgress)).id();
        column_header(&mut app.world_mut().commands(), shell, Status::InProgress, 3);
        app.world_mut().flush();
        for _ in 0..3 {
            app.update();
        }
        let world = app.world_mut();
        let shown: Vec<String> = world.query::<&Text>().iter(world).map(|t| t.0.clone()).collect();
        assert!(shown.contains(&"In Progress".to_string()), "{shown:?}");
        assert!(!shown.contains(&"in_progress".to_string()), "{shown:?}");
    }

    #[test]
    fn the_frame_may_shrink_so_the_body_scrolls() {
        assert_eq!(node_of(column_frame()).min_height, px(0));
        assert_eq!(node_of(column_body()).min_height, px(0));
    }
}
