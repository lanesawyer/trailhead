//! A text input over feathers' `FeathersTextInput`, one line or several.
//!
//! Feathers' container is one row high, so it is let grow to hold the lines asked for, and it fills the width of
//! whatever holds it (a [`super::field`]). More than one line allows newlines and breaks at word boundaries.
//! [`text_input_sized`] types at another of [`super::size`]'s sizes, so an input can stand in for a text of that size.

use bevy::feathers::controls::{FeathersTextInput, FeathersTextInputContainer};
use bevy::feathers::theme::{SurfaceLevel, ThemeContext, UiTheme};
use bevy::feathers::tokens;
use bevy::prelude::*;
use bevy::text::{EditableText, FontSize, LineBreak, TextCursorStyle, TextFont, TextLayout};

use crate::text::{Size, size};

/// An input `lines` high. `own` is a scene applied to the text entity itself, where the caller puts the markers it
/// reads the text back by.
pub fn text_input(lines: f32, own: impl Scene) -> impl Scene {
    text_input_sized(lines, size::BODY, own)
}

/// A [`text_input`] that types at `font_size`.
pub fn text_input_sized(lines: f32, font_size: Size, own: impl Scene) -> impl Scene {
    bsn! {
        @FeathersTextInputContainer
        Node { height: Val::Auto, width: percent(100), align_items: AlignItems::FlexStart }
        Children [
            @FeathersTextInput
            @{own}
            EditableText { visible_lines: { Some(lines) }, allow_newlines: { lines > 1.0 } }
            TextLayout { linebreak: LineBreak::WordBoundary }
            TextFont { font_size: { FontSize::from(font_size) } }
        ]
    }
}

/// Text an input holds when it appears, with the cursor at its end: what a form opened on something that exists starts
/// with. Put it in the `own` scene of a [`text_input`].
#[derive(Component, Clone, Default, Debug)]
pub struct InitialText(pub String);

pub(crate) fn fill_initial_text(mut inputs: Query<(&InitialText, &mut EditableText), Added<InitialText>>) {
    for (InitialText(initial), mut text) in &mut inputs {
        text.editor_mut().set_text(initial);
        text.queue_edit(bevy::text::TextEdit::TextEnd(false));
    }
}

/// Colors the cursor and selection of each text input spawned after the theme was set, and of one moved onto another
/// surface. Feathers colors them only when the theme changes (bevy_feathers 0.20's `update_text_cursor_color`), so
/// any input but the ones there at startup kept bevy's dark default cursor, unseen on the field.
#[allow(clippy::type_complexity)]
pub(crate) fn color_new_cursors(
    theme: Res<UiTheme>,
    mut inputs: Query<
        (&mut TextCursorStyle, Option<&ThemeContext>),
        (With<FeathersTextInput>, Or<(Added<TextCursorStyle>, Changed<ThemeContext>)>),
    >,
) {
    for (mut style, context) in &mut inputs {
        let context = context.map_or(SurfaceLevel::Base, |c| c.0);
        style.color = theme.context_color(&tokens::TEXT_INPUT_CURSOR, context);
        style.selection_color = theme.context_color(&tokens::TEXT_INPUT_SELECTION, context);
        style.unfocused_selection_color = theme.context_color(&tokens::TEXT_INPUT_SELECTION_UNFOCUSED, context);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Theme;

    #[test]
    fn an_input_spawned_after_the_theme_gets_the_themes_cursor() {
        let mut app = App::new();
        app.insert_resource(Theme::new().resource()).add_systems(Update, color_new_cursors);
        app.update();
        let input = app.world_mut().spawn((FeathersTextInput, TextCursorStyle::default())).id();
        app.update();
        let theme = app.world().resource::<UiTheme>();
        let cursor = theme.context_color(&tokens::TEXT_INPUT_CURSOR, SurfaceLevel::Base);
        assert_ne!(cursor, TextCursorStyle::default().color, "the theme's cursor isn't bevy's default");
        assert_eq!(app.world().get::<TextCursorStyle>(input).unwrap().color, cursor);
    }
}
