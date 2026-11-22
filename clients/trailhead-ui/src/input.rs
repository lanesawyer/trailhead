//! A text input over feathers' `FeathersTextInput`, one line or several.
//!
//! Feathers' container is one row high, so it is let grow to hold the lines asked for, and it fills the width of
//! whatever holds it (a [`super::field`]). It never grows shorter than [`extent::CONTROL_HEIGHT`], so a one-line
//! input is as tall as the button or select beside it: grown to its text alone, it was a thin strip beside them
//! (TRAIL-1610). More than one line allows newlines and breaks at word boundaries.
//! [`text_input_sized`] types at another of [`super::size`]'s sizes, so an input can stand in for a text of that size.
//! Enter in a one-line input triggers [`TextEntered`] on its text entity, which is how a form submits from the
//! keyboard; the kit's own inline text saves on it too.

use bevy::feathers::controls::{FeathersTextInput, FeathersTextInputContainer};
use bevy::feathers::theme::{SurfaceLevel, ThemeContext, UiTheme};
use bevy::feathers::tokens;
use bevy::input::keyboard::KeyboardInput;
use bevy::input_focus::FocusedInput;
use bevy::prelude::*;
use bevy::text::{EditableText, FontSize, LineBreak, TextCursorStyle, TextFont, TextLayout};

use crate::spacing::extent;
use crate::text::{Size, size};

/// An input `lines` high. `own` is a scene applied to the text entity itself, where the caller puts the markers it
/// reads the text back by.
pub fn text_input(lines: f32, own: impl Scene) -> impl Scene {
    text_input_sized(lines, size::BODY, own)
}

/// A [`text_input`] that types at `font_size`.
pub fn text_input_sized(lines: f32, font_size: Size, own: impl Scene) -> impl Scene {
    // One line sits in the middle of the control's height; more start at its top and grow it.
    let align = if lines > 1.0 { AlignItems::FlexStart } else { AlignItems::Center };
    bsn! {
        @FeathersTextInputContainer
        Node {
            height: Val::Auto,
            min_height: { px(extent::CONTROL_HEIGHT) },
            width: percent(100),
            align_items: { align },
        }
        Children [
            @FeathersTextInput
            on(enter_pressed)
            @{own}
            EditableText { visible_lines: { Some(lines) }, allow_newlines: { lines > 1.0 } }
            TextLayout { linebreak: LineBreak::WordBoundary }
            TextFont { font_size: { FontSize::from(font_size) } }
        ]
    }
}

/// Enter was pressed in a one-line [`text_input`], triggered on its text entity (where the `own` markers are) with
/// what it holds. Several lines take Enter as a new line and never trigger it.
#[derive(EntityEvent, Clone, Debug, PartialEq, Eq)]
pub struct TextEntered {
    pub entity: Entity,
    pub value: String,
}

/// Turns Enter in a one-line input into [`TextEntered`]: once, for the input's own press, not one held down or still
/// being composed.
fn enter_pressed(key: On<FocusedInput<KeyboardInput>>, inputs: Query<&EditableText>, mut commands: Commands) {
    let pressed = &key.input;
    if key.event_target() != key.original_event_target()
        || !matches!(pressed.key_code, KeyCode::Enter | KeyCode::NumpadEnter)
        || !pressed.state.is_pressed()
        || pressed.repeat
    {
        return;
    }
    let entity = key.event_target();
    let Ok(text) = inputs.get(entity) else { return };
    if !text.allow_newlines && !text.is_composing() {
        commands.trigger(TextEntered { entity, value: text.value().to_string() });
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
#[expect(clippy::type_complexity)]
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
    use bevy::input::keyboard::Key;

    use super::*;
    use crate::Theme;
    use crate::test_support::press_key;

    #[derive(Resource, Default)]
    struct Entered(Vec<TextEntered>);

    /// The text entity of an input `lines` high, holding `typed`, in an app that notes what is entered.
    fn input(lines: f32, typed: &str) -> (App, Entity) {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .init_resource::<Entered>()
            .add_observer(|e: On<TextEntered>, mut seen: ResMut<Entered>| seen.0.push(e.clone()));
        app.world_mut().commands().spawn_scene(text_input(lines, bsn! {}));
        app.world_mut().flush();
        app.update();
        let field = app.world_mut().query_filtered::<Entity, With<FeathersTextInput>>().single(app.world()).unwrap();
        app.world_mut().get_mut::<EditableText>(field).unwrap().editor_mut().set_text(typed);
        (app, field)
    }

    /// The node of the container a scene spawns, or of the scene's root.
    fn root_node(scene: impl Scene) -> Node {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>();
        let root = app.world_mut().commands().spawn_scene(scene).id();
        app.world_mut().flush();
        app.update();
        app.world().get::<Node>(root).unwrap().clone()
    }

    #[test]
    fn a_one_line_input_is_as_tall_as_a_button_and_more_lines_grow_past_it() {
        let button = root_node(crate::button("Add", None, crate::ButtonKind::Primary));
        let one = root_node(text_input(1.0, bsn! {}));
        assert_eq!(button.height, px(extent::CONTROL_HEIGHT));
        assert_eq!((one.height, one.min_height), (Val::Auto, button.height));
        assert_eq!(one.align_items, AlignItems::Center, "its line in the middle, as a button's caption");
        let three = root_node(text_input(3.0, bsn! {}));
        assert_eq!((three.height, three.min_height), (Val::Auto, button.height), "grows by its lines");
        assert_eq!(three.align_items, AlignItems::FlexStart);
    }

    #[test]
    fn enter_in_a_one_line_input_triggers_text_entered_on_it_with_what_it_holds() {
        let (mut app, field) = input(1.0, "Buy chalk");
        press_key(app.world_mut(), field, KeyCode::KeyA, Key::Character("a".into()));
        press_key(app.world_mut(), field, KeyCode::Enter, Key::Enter);
        app.world_mut().flush();
        let wanted = TextEntered { entity: field, value: "Buy chalk".into() };
        assert_eq!(app.world().resource::<Entered>().0, [wanted]);
    }

    #[test]
    fn enter_in_an_input_of_several_lines_is_a_new_line_not_an_entry() {
        let (mut app, field) = input(3.0, "a spec");
        press_key(app.world_mut(), field, KeyCode::Enter, Key::Enter);
        app.world_mut().flush();
        assert!(app.world().resource::<Entered>().0.is_empty());
    }

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
