//! How text is sized, in one place.
//!
//! Sizes are named for where the text sits, not how big it is, so the scale is one edit. A [`Size`] is made only here,
//! so a size typed as a number doesn't compile: a size with a new meaning gets a new name in [`size`]. Sizing goes on
//! the text itself: feathers' `label` carries `PropagateOver<TextFont>`, so a font set on a container above it is
//! dropped.

use bevy::app::PropagateOver;
use bevy::feathers::display::{label, label_dim};
use bevy::prelude::*;
use bevy::text::{FontSource, LineBreak};

use crate::theme::{TextTone, Token};
use crate::truncate::Truncated;

/// A text size from the kit's scale, one of [`size`]'s.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Size(f32);

impl Size {
    /// The size in logical pixels, for measuring: how tall a line is, how wide a glyph comes out.
    pub const fn px(self) -> f32 {
        self.0
    }
}

impl From<Size> for FontSize {
    fn from(size: Size) -> Self {
        FontSize::Px(size.0)
    }
}

/// The scale text is sized from.
pub mod size {
    use super::Size;

    /// Badges, a card's key, counts beside a column's name.
    pub const SMALL: Size = Size(11.0);
    /// A card's secondary line, a field's label.
    pub const SECONDARY: Size = Size(12.0);
    /// Feathers' own size: body text, button captions.
    pub const BODY: Size = Size(13.0);
    /// A card's title.
    pub const CARD_TITLE: Size = Size(14.0);
    /// A column's name, a panel's heading, an icon.
    pub const HEADING: Size = Size(15.0);
    /// The app's name in the header bar.
    pub const APP_TITLE: Size = Size(18.0);
    /// The title of the one thing a detail modal shows.
    pub const DETAIL_TITLE: Size = Size(18.0);
}

/// A label at `size`.
pub fn text(content: impl Into<String>, size: Size) -> impl Scene {
    let content = content.into();
    bsn! {
        @label(content)
        TextFont { font_size: { FontSize::from(size) } }
    }
}

/// A dimmed label at `size`, for anything secondary to what's beside it.
pub fn text_dim(content: impl Into<String>, size: Size) -> impl Scene {
    let content = content.into();
    bsn! {
        @label_dim(content)
        TextFont { font_size: { FontSize::from(size) } }
    }
}

/// A label at `size` in a theme color, such as [`super::token::STATUS_WARN`].
pub fn text_toned(content: impl Into<String>, size: Size, tone: Token) -> impl Scene {
    let content = content.into();
    bsn! {
        @label(content)
        TextFont { font_size: { FontSize::from(size) } }
        TextTone({ tone })
    }
}

/// One line of text at `size`, cut with an ellipsis to the room its row leaves it, and growing to take that room: a
/// list row's title beside its buttons. The row's own width has to come from outside it (see [`Truncated`]). Change
/// what it says through its [`Truncated`]'s `full`, never its `Text`, which the kit rewrites to fit.
pub fn text_truncated(content: impl Into<String>, size: Size) -> impl Scene {
    let content = content.into();
    let full = content.clone();
    bsn! {
        @text(content, size)
        Truncated { full: { full }, size: { size } }
        TextLayout { linebreak: LineBreak::NoWrap }
        Node { flex_grow: 1.0, min_width: px(0) }
    }
}

/// A label in the monospace face at `size`, in a theme color ([`super::token::TEXT`] for the default one), for text where
/// columns should line up and every character counts: log lines, ids, code. The face is Bevy's built-in one (Fira Mono,
/// the `default_font` feature), not the system's monospace: finding that needs `system_font_discovery`, which links
/// fontconfig, and without it the text has no font and lays out 0 tall (TRAIL-1144). The font is set on the text
/// itself, as the others are.
pub fn text_mono(content: impl Into<String>, size: Size, tone: Token) -> impl Bundle {
    (
        Text::new(content),
        TextFont { font: FontSource::default(), font_size: FontSize::from(size), ..default() },
        TextTone(tone),
        PropagateOver::<TextFont>::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_truncated_line_holds_its_whole_text_stays_on_one_line_and_takes_the_room_left() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>();
        let line = app.world_mut().commands().spawn_scene(text_truncated("Send the crux", size::BODY)).id();
        app.world_mut().flush();
        app.update();
        let world = app.world();
        assert_eq!(world.get::<Truncated>(line), Some(&Truncated { full: "Send the crux".into(), size: size::BODY }));
        assert_eq!(world.get::<TextLayout>(line).unwrap().linebreak, LineBreak::NoWrap);
        let node = world.get::<Node>(line).unwrap();
        assert_eq!((node.flex_grow, node.min_width), (1.0, px(0)));
    }

    #[test]
    fn mono_text_is_in_the_built_in_face_which_needs_no_font_discovery() {
        let mut world = World::new();
        let line = world.spawn(text_mono("12:00:00 INFO  ready", size::SECONDARY, crate::token::TEXT)).id();
        assert_eq!(world.get::<TextFont>(line).unwrap().font, FontSource::default());
    }
}
