//! Words a person copies (an id, a command, a path), set apart in a box of their own with the copy button at its
//! end, so it is plain what lands on the clipboard and what is only said about it.

use bevy::prelude::*;

use crate::buttons::{ButtonKind, tool_button};
use crate::icons::Icon;
use crate::spacing::{radius, space, stroke};
use crate::text::{size, text};
use crate::theme::{BorderTone, Surface, token};

/// A box holding words to copy and the button that copies them, as wide as what holds it, the words wrapping when
/// they are longer than that and the button at its end. `words` goes on the text (your marker, to `set_text` it) and `copy` on the button (your marker, to
/// observe its `Activate` and put the words on the clipboard); `hint` says what the button copies.
pub fn copy_block(words: impl Scene, hint: impl Into<String>, copy: impl Scene) -> impl Scene {
    let hint = hint.into();
    bsn! {
        Node {
            align_items: AlignItems::Center,
            column_gap: { px(space::CONTROLS) },
            padding: { UiRect::new(px(space::CONTROL_INSET), px(space::LABEL_INSET), px(space::LABEL_INSET), px(space::LABEL_INSET)) },
            border: { UiRect::all(px(stroke::HAIRLINE)) },
            border_radius: { BorderRadius::all(px(radius::CONTROL)) },
        }
        Surface::Raised
        BorderTone({ token::BORDER })
        Children [
            @text("", size::BODY) @{words} Node { flex_grow: 1.0, flex_shrink: 1.0, min_width: px(0) }
            -- @tool_button(Icon::Copy, hint, ButtonKind::Plain) @{copy} Node { flex_shrink: 0.0 }
        ]
    }
}
