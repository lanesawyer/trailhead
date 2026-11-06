//! A text input over feathers' `FeathersTextInput`, one line or several.
//!
//! Feathers' container is one row high, so it is let grow to hold the lines asked for, and it fills the width of
//! whatever holds it (a [`super::field`]). More than one line allows newlines and breaks at word boundaries.

use bevy::feathers::controls::{FeathersTextInput, FeathersTextInputContainer};
use bevy::prelude::*;
use bevy::text::{EditableText, LineBreak, TextLayout};

/// An input `lines` high. `own` is a scene applied to the text entity itself, where the caller puts the markers it
/// reads the text back by.
pub fn text_input(lines: f32, own: impl Scene) -> impl Scene {
    bsn! {
        @FeathersTextInputContainer
        Node { height: Val::Auto, width: percent(100), align_items: AlignItems::FlexStart }
        Children [
            @FeathersTextInput
            @{own}
            EditableText { visible_lines: { Some(lines) }, allow_newlines: { lines > 1.0 } }
            TextLayout { linebreak: LineBreak::WordBoundary }
        ]
    }
}
