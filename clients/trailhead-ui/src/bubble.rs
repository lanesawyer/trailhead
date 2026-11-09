//! A message in a conversation, as a messaging app shows one: a rounded box against the end edge for what the reader
//! wrote and the start edge for everyone else's, each side in its own color, so whose is whose reads at a glance.
//!
//! Put it in a column (its parent lays its children out top to bottom) and spawn the message's lines as its children,
//! a [`bubble_head`] first.

use bevy::prelude::*;

use super::{BackgroundTone, Icon, TextTone, icon_text, radius, size, space, text_dim, token};

/// Whose a [`bubble`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Someone else's: against the start edge.
    Theirs,
    /// The reader's own: against the end edge.
    Mine,
}

/// The widest a bubble grows, as a share of its column, so the two sides stay apart.
const MAX_WIDTH_PERCENT: f32 = 80.0;

/// A message box on `side`.
pub fn bubble(side: Side) -> impl Scene {
    let (align, tone) = match side {
        Side::Theirs => (AlignSelf::FlexStart, token::SURFACE_RAISED),
        Side::Mine => (AlignSelf::FlexEnd, token::FILL_ACCENT),
    };
    bsn! {
        Node {
            flex_direction: FlexDirection::Column,
            row_gap: { px(space::STACKED) },
            padding: { UiRect::all(px(space::CONTROL_INSET)) },
            border_radius: { BorderRadius::all(px(radius::PANEL)) },
            max_width: { percent(MAX_WIDTH_PERCENT) },
            align_self: { align },
        }
        BackgroundTone({ tone })
    }
}

/// A bubble's first line, dim and small: `icon` for what kind of message it is, then `words` about it (who wrote it,
/// when).
pub fn bubble_head(icon: Icon, words: impl Into<String>) -> impl Scene {
    let words = words.into();
    bsn! {
        Node { align_items: AlignItems::Center, column_gap: { px(space::ICON_LABEL) } }
        Children [
                @icon_text(icon) TextTone({ token::TEXT_DIM })
            -- @text_dim(words, size::SECONDARY)
        ]
    }
}
