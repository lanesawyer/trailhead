//! A box to set text fields in.
//!
//! Ported from bevy-data-explorer's `widgets/field_well.rs`.

use bevy::prelude::*;

use crate::spacing::{radius, space};
use crate::theme::{BorderTone, Surface, token};

/// How thick the well's border is.
const OUTLINE: f32 = 1.0;

/// A box to set text fields in.
///
/// Feathers fills a field with the same gray as a pane's body and a menu, so a field placed straight in either can't
/// be seen. It is drawn to sit on a raised surface, a step lighter, and this is that surface standing alone, so a field
/// in it is lifted a step lighter again rather than drawn in the well's own gray.
pub fn field_well() -> impl Scene {
    bsn! {
        Node {
            flex_direction: { FlexDirection::Column },
            width: { percent(100) },
            row_gap: { px(space::ROWS) },
            padding: { UiRect::all(px(space::CONTROL_INSET)) },
            border: { UiRect::all(px(OUTLINE)) },
            border_radius: { BorderRadius::all(px(radius::CONTROL)) },
        }
        Surface::Raised
        BorderTone({ token::SURFACE_RAISED })
    }
}
