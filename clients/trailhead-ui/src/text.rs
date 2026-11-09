//! How text is sized, in one place.
//!
//! Sizes are named for where the text sits, not how big it is, so the scale is one edit. Sizing goes on the text
//! itself: feathers' `label` carries `PropagateOver<TextFont>`, so a font set on a container above it is dropped.

use bevy::app::PropagateOver;
use bevy::feathers::display::{label, label_dim};
use bevy::prelude::*;
use bevy::text::FontSource;

use crate::theme::{TextTone, Token};

/// The scale text is sized from.
pub mod size {
    /// Badges, a card's key, counts beside a column's name.
    pub const SMALL: f32 = 11.0;
    /// A card's secondary line, a field's label.
    pub const SECONDARY: f32 = 12.0;
    /// Feathers' own size: body text, button captions.
    pub const BODY: f32 = 13.0;
    /// A card's title.
    pub const CARD_TITLE: f32 = 14.0;
    /// A column's name, a panel's heading.
    pub const HEADING: f32 = 15.0;
    /// The app's name in the header bar.
    pub const APP_TITLE: f32 = 18.0;
    /// The title of the one thing a detail modal shows.
    pub const DETAIL_TITLE: f32 = 18.0;
}

/// A label at `size`.
pub fn text(content: impl Into<String>, size: f32) -> impl Scene {
    let content = content.into();
    bsn! {
        @label(content)
        TextFont { font_size: { FontSize::Px(size) } }
    }
}

/// A dimmed label at `size`, for anything secondary to what's beside it.
pub fn text_dim(content: impl Into<String>, size: f32) -> impl Scene {
    let content = content.into();
    bsn! {
        @label_dim(content)
        TextFont { font_size: { FontSize::Px(size) } }
    }
}

/// A label at `size` in a theme color, such as [`super::token::STATUS_WARN`].
pub fn text_toned(content: impl Into<String>, size: f32, tone: Token) -> impl Scene {
    let content = content.into();
    bsn! {
        @label(content)
        TextFont { font_size: { FontSize::Px(size) } }
        TextTone({ tone })
    }
}

/// A label in the monospace face at `size`, in a theme color ([`super::token::TEXT`] for the default one), for text where
/// columns should line up and every character counts: log lines, ids, code. The face is Bevy's built-in one (Fira Mono,
/// the `default_font` feature), not the system's monospace: finding that needs `system_font_discovery`, which links
/// fontconfig, and without it the text has no font and lays out 0 tall (TRAIL-1144). The font is set on the text
/// itself, as the others are.
pub fn text_mono(content: impl Into<String>, size: f32, tone: Token) -> impl Bundle {
    (
        Text::new(content),
        TextFont { font: FontSource::default(), font_size: FontSize::Px(size), ..default() },
        TextTone(tone),
        PropagateOver::<TextFont>::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_text_is_in_the_built_in_face_which_needs_no_font_discovery() {
        let mut world = World::new();
        let line = world.spawn(text_mono("12:00:00 INFO  ready", size::SECONDARY, crate::token::TEXT)).id();
        assert_eq!(world.get::<TextFont>(line).unwrap().font, FontSource::default());
    }
}
