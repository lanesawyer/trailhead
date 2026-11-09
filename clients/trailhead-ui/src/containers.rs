//! The surfaces controls sit in: a panel over the board, a section within it, and a field (a label over an input).

use crate::theme::{BorderTone, Surface};
use bevy::feathers::controls::{ButtonVariant, FeathersButton};
use bevy::prelude::*;
use bevy::text::LineBreak;

use super::{Truncated, button_text, layer, radius, size, space, stroke, text_dim, token};

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
        Surface::Panel
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

/// A list hanging under what holds it (a search field's suggestions), floating over what comes after, so showing and
/// hiding it moves nothing. Put it last in a node holding just the field, which it takes its width from.
pub fn dropdown_list() -> impl Scene {
    bsn! {
        Node {
            position_type: PositionType::Absolute,
            top: percent(100),
            left: px(0),
            right: px(0),
            margin: { UiRect::top(px(space::STACKED)) },
            flex_direction: FlexDirection::Column,
            padding: { UiRect::all(px(space::BADGE_INSET)) },
            border: { UiRect::all(px(stroke::HAIRLINE)) },
            border_radius: { BorderRadius::all(px(radius::CONTROL)) },
        }
        Surface::Floating
        BorderTone({ token::BORDER })
        GlobalZIndex({ layer::MENU })
    }
}

/// A row of a [`dropdown_list`]: a plain button the list's width, its words on one line and cut with an ellipsis when
/// too long for it. What a press does is the caller's, as for any button.
pub fn dropdown_item(caption: impl Into<String>) -> impl Scene {
    let caption = caption.into();
    let full = caption.clone();
    bsn! {
        @FeathersButton {
            @caption: { bsn_list! {
                @button_text(caption)
                Truncated { full: { full }, size: { size::BODY } }
                TextLayout { linebreak: LineBreak::NoWrap }
            } },
            @variant: { ButtonVariant::Plain },
        }
        Node {
            width: percent(100),
            min_width: px(0),
            justify_content: JustifyContent::Start,
            overflow: Overflow::clip_x(),
        }
    }
}
