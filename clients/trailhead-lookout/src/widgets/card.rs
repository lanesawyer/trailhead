//! A task card: a pressable surface on the card background, holding a key chip, the title clamped to two lines, a
//! "under TRAIL-n" line, and rows of [`badge`](super::badge)s and tool buttons.
//!
//! The card is pressable as a whole (it sends `Activate`, which opens the inspector); its tool buttons are pressable
//! on their own and keep the press from reaching the card. A card never holds a handoff or a question body: those
//! belong to the inspector.
//!
//! Spawn the pieces in order as children of [`card`]: [`badge`](super::badge) for the key, [`card_title`],
//! [`card_under`] when there is a parent, then a [`card_row`] each for badges and for actions.

use bevy::feathers::theme::{ThemeBackgroundColor, ThemeBorderColor};
use bevy::prelude::*;
use bevy::ui_widgets::Button;

use super::{radius, size, space, text, text_dim, token};

/// How many lines of a title a card shows.
pub const TITLE_LINES: f32 = 2.0;

/// Bevy's default line height, as a multiple of the font size.
const LINE_HEIGHT: f32 = 1.2;

/// The card itself, `depth` steps in from the column's edge (a subtask sits in from its parent).
pub fn card(depth: usize) -> impl Scene {
    bsn! {
        Button
        Node {
            flex_direction: FlexDirection::Column,
            row_gap: { px(space::STACKED) },
            padding: { UiRect::all(px(space::CONTROL_INSET)) },
            margin: { UiRect::left(px(space::INDENT * depth as f32)) },
            border: { UiRect::all(px(1)) },
            border_radius: { BorderRadius::all(px(radius::CONTROL)) },
        }
        ThemeBackgroundColor({ token::CARD_BG })
        ThemeBorderColor({ token::DIVIDER })
    }
}

/// A card's title, clipped to [`TITLE_LINES`] lines.
pub fn card_title(title: impl Into<String>) -> impl Scene {
    let title = title.into();
    bsn! {
        Node {
            max_height: { px(size::CARD_TITLE * LINE_HEIGHT * TITLE_LINES) },
            overflow: { Overflow::clip() },
        }
        Children [ @text(title, size::CARD_TITLE) ]
    }
}

/// The dim line naming a subtask's parent.
pub fn card_under(parent: impl Into<String>) -> impl Scene {
    text_dim(format!("under {}", parent.into()), size::SMALL)
}

/// The dim line saying where a search matched a task that its key and title did not: "in description: …".
pub fn card_found(hint: impl Into<String>) -> impl Scene {
    text_dim(hint.into(), size::SMALL)
}

/// A row of badges or tool buttons on a card, wrapping when they don't fit.
pub fn card_row() -> impl Scene {
    bsn! {
        Node {
            flex_wrap: FlexWrap::Wrap,
            align_items: AlignItems::Center,
            column_gap: { px(space::CONTROLS) },
            row_gap: { px(space::CONTROLS) },
        }
    }
}
