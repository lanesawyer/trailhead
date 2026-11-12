//! A search field, and the matching every list searched through it shares.
//!
//! Bevy's text input has no placeholder, so the field carries a hint of its own, laid over it and shown only while it
//! is empty, and a button to clear it shown only while it is not. Whatever the field searches reads its
//! [`EditableText`] and decides for itself when to act on it.
//!
//! Ported from bevy-data-explorer's `widgets/search.rs`.

use bevy::feathers::controls::{ButtonVariant, FeathersTextInput, FeathersTextInputContainer, FeathersToolButton};
use bevy::feathers::display::label_dim;
use bevy::input::keyboard::KeyboardInput;
use bevy::input_focus::FocusedInput;
use bevy::prelude::*;
use bevy::text::EditableText;
use bevy::ui_widgets::Activate;

use crate::icons::{Icon, button_icon};
use crate::patch::patch_node;
use crate::spacing::extent;

/// The clear button's size: smaller than feathers' row, so it sits inside the field rather than filling it.
const CLEAR_SIZE: f32 = 18.0;
/// Where the hint starts from the field's left edge: where feathers' text input starts its text.
const HINT_LEFT: f32 = 6.0;

/// Stands in for the placeholder text Bevy's field doesn't have yet, shown only while the field is empty.
#[derive(Component, Clone)]
pub struct SearchHint {
    field: Entity,
}

impl Default for SearchHint {
    fn default() -> Self {
        SearchHint { field: Entity::PLACEHOLDER }
    }
}

/// The button that empties a search field.
#[derive(Component, Clone)]
pub struct ClearSearch {
    field: Entity,
}

impl Default for ClearSearch {
    fn default() -> Self {
        ClearSearch { field: Entity::PLACEHOLDER }
    }
}

/// The parts of a spawned search field.
pub struct SearchField {
    /// The box to place, holding the field and its hint.
    pub entry: Entity,
    /// The inner text entity, which holds the [`EditableText`]. Mark it with whatever finds it.
    pub field: Entity,
}

/// Spawn a search field reading `hint` while empty.
///
/// A field sitting straight in a pane or a menu needs setting in a [`field_well`](crate::field_well) to be seen; one
/// in a group doesn't.
pub fn spawn_search_field(commands: &mut Commands, hint: impl Into<String>) -> SearchField {
    spawn_search(commands, hint.into(), px(extent::SEARCH_WIDTH))
}

/// [`spawn_search_field`], as wide as what it sits in rather than [`extent::SEARCH_WIDTH`]: for a sidebar or a column.
pub fn spawn_search_field_filling(commands: &mut Commands, hint: impl Into<String>) -> SearchField {
    spawn_search(commands, hint.into(), percent(100))
}

fn spawn_search(commands: &mut Commands, hint: String, width: Val) -> SearchField {
    // Spawned apart and parented by hand: feathers' container is a scene of its own, and the hint has to know the
    // field.
    let field = commands.spawn_scene(bsn! { @FeathersTextInput on(on_escape_clears) }).id();
    let hint = commands
        .spawn_scene(bsn! {
            @label_dim(hint)
            SearchHint { field: { field } }
            TextLayout { linebreak: { LineBreak::NoWrap } }
            Node {
                position_type: { PositionType::Absolute },
                left: { px(HINT_LEFT) },
            }
            Pickable::IGNORE
        })
        .id();
    let clear = commands
        .spawn_scene(bsn! {
            @FeathersToolButton {
                @variant: { ButtonVariant::Plain },
                @caption: { bsn_list! {@button_icon(Icon::X)} }
            }
            ClearSearch { field: { field } }
            on(on_clear_search)
            Node {
                display: { Display::None },
                flex_shrink: { 0.0_f32 },
                width: { px(CLEAR_SIZE) },
                min_width: { px(CLEAR_SIZE) },
                height: { px(CLEAR_SIZE) },
                padding: { UiRect::ZERO },
            }
        })
        .id();
    let entry = commands
        .spawn_scene(bsn! {
            @FeathersTextInputContainer
            Node { flex_grow: { 0.0_f32 }, flex_shrink: { 0.0_f32 }, width: { width }, min_width: px(0) }
        })
        .id();
    commands.entity(entry).add_children(&[field, hint, clear]);
    SearchField { entry, field }
}

/// Show each hint only while its field is empty, and each clear button only while it isn't.
pub(crate) fn sync_search_hints(
    fields: Query<&EditableText>,
    mut hints: Query<(&SearchHint, &mut Node), Without<ClearSearch>>,
    mut clears: Query<(&ClearSearch, &mut Node), Without<SearchHint>>,
) {
    let empty = |field: Entity| fields.get(field).is_ok_and(|text| text.value().to_string().is_empty());
    for (hint, node) in &mut hints {
        let wanted = if empty(hint.field) { Display::Flex } else { Display::None };
        patch_node(node, |node| node.display = wanted);
    }
    for (clear, node) in &mut clears {
        let wanted = if empty(clear.field) { Display::None } else { Display::Flex };
        patch_node(node, |node| node.display = wanted);
    }
}

/// Empty the field whose clear button was pressed. Whatever it searches sees the text change as it would a keystroke.
fn on_clear_search(activate: On<Activate>, clears: Query<&ClearSearch>, mut fields: Query<&mut EditableText>) {
    let Ok(clear) = clears.get(activate.entity) else {
        return;
    };
    if let Ok(mut text) = fields.get_mut(clear.field) {
        text.clear();
    }
}

/// Escape in a focused search field empties it, as the clear button does. Only the field's own press counts, not one
/// bubbling up from something inside it.
fn on_escape_clears(key: On<FocusedInput<KeyboardInput>>, mut fields: Query<&mut EditableText>) {
    let pressed = &key.input;
    if key.event_target() != key.original_event_target()
        || pressed.key_code != KeyCode::Escape
        || !pressed.state.is_pressed()
    {
        return;
    }
    if let Ok(mut text) = fields.get_mut(key.event_target()) {
        text.clear();
    }
}

/// Whether every word of `query` appears somewhere in `fields`, ignoring case.
///
/// Words rather than the whole query, so "zarr mouse" finds an item whose kind says one and whose name says the
/// other.
pub fn matches_search(query: &str, fields: &[&str]) -> bool {
    let haystack = fields.join(" ").to_lowercase();
    query.split_whitespace().all(|word| haystack.contains(&word.to_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hint_shows_while_the_field_is_empty_and_the_clear_button_while_it_isnt() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .add_systems(Update, sync_search_hints);
        let parts = spawn_search_field(&mut app.world_mut().commands(), "Search");
        app.world_mut().flush();
        app.update();
        let shown = |app: &mut App| {
            let hint = app.world_mut().query_filtered::<&Node, With<SearchHint>>().single(app.world()).unwrap().display;
            let clear =
                app.world_mut().query_filtered::<&Node, With<ClearSearch>>().single(app.world()).unwrap().display;
            (hint, clear)
        };
        assert_eq!(shown(&mut app), (Display::Flex, Display::None));

        app.world_mut().entity_mut(parts.field).insert(EditableText::new("deep"));
        app.update();
        assert_eq!(shown(&mut app), (Display::None, Display::Flex));
    }

    #[test]
    fn escape_in_the_field_empties_it() {
        use bevy::input::keyboard::Key;
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>();
        let parts = spawn_search_field(&mut app.world_mut().commands(), "Search");
        app.world_mut().flush();
        app.update();
        app.world_mut().entity_mut(parts.field).insert(EditableText::new("deep"));
        let window = app.world_mut().spawn(Window::default()).id();
        let press = |key_code, logical_key| KeyboardInput {
            key_code,
            logical_key,
            state: bevy::input::ButtonState::Pressed,
            text: None,
            repeat: false,
            window,
        };
        let value = |app: &App| app.world().get::<EditableText>(parts.field).unwrap().value().to_string();
        let a = press(KeyCode::KeyA, Key::Character("a".into()));
        app.world_mut().trigger(FocusedInput::new(parts.field, a, window));
        assert_eq!(value(&app), "deep");
        app.world_mut().trigger(FocusedInput::new(parts.field, press(KeyCode::Escape, Key::Escape), window));
        assert_eq!(value(&app), "");
    }

    #[test]
    fn the_box_keeps_its_width_and_the_hint_stays_on_one_line() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>();
        let parts = spawn_search_field(&mut app.world_mut().commands(), "Search tasks");
        app.world_mut().flush();
        app.update();
        // The hint is laid over the field, so it gives the box no width: in a row of controls (Lookout's header) the
        // box shrank to a sliver and the hint wrapped onto the controls beside it.
        let node = app.world().get::<Node>(parts.entry).unwrap();
        assert_eq!((node.width, node.flex_shrink), (px(extent::SEARCH_WIDTH), 0.0));
        let layout = app.world_mut().query_filtered::<&TextLayout, With<SearchHint>>().single(app.world()).unwrap();
        assert_eq!(layout.linebreak, LineBreak::NoWrap);
    }

    #[test]
    fn an_empty_search_lists_everything() {
        assert!(matches_search("", &["Anything", "at all"]));
        assert!(matches_search("   ", &["Anything"]));
    }

    #[test]
    fn every_word_has_to_appear_but_not_in_the_same_field() {
        let fields = ["SEA-AD slide", "Deep Zoom image", "https://store/a.dzi"];
        assert!(matches_search("deep sea", &fields));
        assert!(matches_search("DZI", &fields));
        assert!(!matches_search("deep zarr", &fields));
    }
}
