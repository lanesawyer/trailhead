//! The surfaces controls sit in: a panel over the board, a section within it, and a field (a label over an input).

use bevy::feathers::theme::ThemeBackgroundColor;
use bevy::prelude::*;

use super::{radius, size, space, text_dim, token};

/// A panel over the board (the inspector, a form, the connect panel): a column on the panel background, rounded and
/// padded. Position it with a `Node` of the caller's, and give it a [`super::layer`].
pub fn panel() -> impl Scene {
    bsn! {
        Node {
            flex_direction: FlexDirection::Column,
            row_gap: { px(space::ROWS) },
            padding: { UiRect::all(px(space::PANEL_INSET)) },
            border_radius: { BorderRadius::all(px(radius::PANEL)) },
        }
        ThemeBackgroundColor({ token::PANEL_BG })
    }
}

/// A group of rows within a panel, under a dim heading.
pub fn section(heading: impl Into<String>) -> impl Scene {
    let heading = heading.into();
    bsn! {
        @section_frame()
        Children [ @text_dim(heading, size::SECONDARY) ]
    }
}

/// A section's box without its heading, for a caller that keeps the heading among the section's items and updates
/// them in place.
pub fn section_frame() -> impl Scene {
    bsn! {
        Node {
            flex_direction: FlexDirection::Column,
            row_gap: { px(space::ROWS) },
            margin: { UiRect::top(px(space::GROUPS - space::ROWS)) },
        }
    }
}

/// A row of controls side by side, centered on each other.
pub fn row() -> impl Scene {
    bsn! {
        Node { column_gap: { px(space::CONTROLS) }, align_items: AlignItems::Center }
    }
}

/// A field: `label` above `control` (an input, a select).
///
/// The control is part of the scene rather than a child added after it: the scene's own `Children` replace any added
/// before it spawns, which left a select orphaned at the window's top left (TRAIL-850).
pub fn field(label: impl Into<String>, control: impl SceneList) -> impl Scene {
    let label = label.into();
    bsn! {
        Node { flex_direction: FlexDirection::Column, row_gap: { px(space::STACKED) } }
        Children [ @text_dim(label, size::SECONDARY) -- {control} ]
    }
}
