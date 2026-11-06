//! A box to set text fields in.
//!
//! Ported from bevy-data-explorer's `widgets/field_well.rs`.

use bevy::app::Propagate;
use bevy::feathers::theme::{SurfaceLevel, ThemeBackgroundColor, ThemeBorderColor, ThemeContext};
use bevy::feathers::tokens;
use bevy::prelude::*;

use crate::spacing::{radius, space};

/// How thick the well's border is.
const OUTLINE: f32 = 1.0;

/// A box to set text fields in.
///
/// Feathers fills a field with the same gray as a pane's body and a menu, so a field placed straight in either can't
/// be seen. It is drawn to sit in a group, a step lighter, and this is that group's body standing alone, saying so
/// with its surface level, so a field in it is lifted a step lighter again rather than drawn in the well's own gray.
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
        ThemeBackgroundColor({ tokens::GROUP_BG })
        ThemeBorderColor({ tokens::GROUP_BORDER })
        Propagate::<ThemeContext>(ThemeContext(SurfaceLevel::Highest))
    }
}
