//! A small status chip: a colored dot and a few words, with the full explanation in a [`Hint`].
//!
//! For state that is always there and rarely needs reading, such as the connection in the header bar.

use bevy::feathers::theme::{ThemeBackgroundColor, ThemeToken};
use bevy::prelude::*;

use super::{Hint, radius, size, space, text};

/// A chip with a dot in `tone` and `words` beside it; hovering shows `hint`.
pub fn chip(tone: ThemeToken, words: impl Into<String>, hint: impl Into<String>) -> impl Scene {
    let words = words.into();
    let hint = hint.into();
    bsn! {
        Node {
            align_items: AlignItems::Center,
            column_gap: { px(space::ICON_LABEL) },
            padding: { UiRect::axes(px(space::CONTROL_INSET), px(space::STACKED)) },
        }
        Hint({ hint })
        Children [
                Node {
                    width: { px(space::DOT) },
                    height: { px(space::DOT) },
                    border_radius: { BorderRadius::all(px(radius::PANEL)) },
                }
                ThemeBackgroundColor({ tone })
            -- @text(words, size::SECONDARY)
        ]
    }
}
