//! A choice shown as a small picture with its caption under it: one of a row of them, for a pick a person should see
//! rather than read (the Trailhead shell's ways of switching apps, each a little preview that plays). Over
//! `bevy_ui_widgets`' headless `Button`, so a press, Enter or Space sends `Activate` from the tile; it answers the pointer
//! as a plain button does, and the one chosen is ringed in the accent with its caption at full strength.
//!
//! It does not choose itself: the caller answers `Activate` and sets [`ChoiceTile::chosen`] on every tile from what it
//! keeps, so a choice made elsewhere shows too. The caller fills the picture.

use bevy::feathers::focus::FocusIndicator;
use bevy::input_focus::tab_navigation::TabIndex;
use bevy::picking::cursor::EntityCursor;
use bevy::prelude::*;
use bevy::ui_widgets::Button;
use bevy::window::SystemCursorIcon;

use crate::hover::HoverTone;
use crate::spacing::{extent, radius, space, stroke};
use crate::text::{size, text_dim};
use crate::theme::{BorderTone, TextTone, Token, token};

/// Whether a tile is the one chosen. Set it with `set_if_neq`; the kit repaints the tile.
#[derive(Component, Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct ChoiceTile {
    pub chosen: bool,
}

/// A tile's picture, `extent::TILE_PICTURE_WIDTH` by `TILE_PICTURE_HEIGHT`, clipped to its rounded corners: the caller
/// fills it.
#[derive(Component, Clone, Copy, Default, Debug)]
pub struct ChoicePicture;

/// A tile's caption, which the kit tones.
#[derive(Component, Clone, Copy, Default, Debug)]
pub struct ChoiceCaption;

/// A spawned tile: put `root` in the row with `ChildOf` and fill `picture`.
#[derive(Clone, Copy, Debug)]
pub struct ChoiceTileParts {
    pub root: Entity,
    pub picture: Entity,
}

/// A tile captioned `caption`, `chosen` or not, spawned unparented. Lay several out in a row that wraps, `space::ROWS`
/// apart.
pub fn spawn_choice_tile(commands: &mut Commands, caption: impl Into<String>, chosen: bool) -> ChoiceTileParts {
    let root = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: px(space::STACKED),
                padding: UiRect::all(px(space::LABEL_INSET)),
                border: UiRect::all(px(stroke::GRIP)),
                border_radius: BorderRadius::all(px(radius::CONTROL)),
                ..default()
            },
            Button,
            ChoiceTile { chosen },
            HoverTone { rest: token::FILL_ITEM, hover: token::FILL_ITEM_HOVER },
            BorderTone(ring(chosen)),
            TabIndex(0),
            FocusIndicator,
            EntityCursor::System(SystemCursorIcon::Pointer),
        ))
        .id();
    let picture = commands
        .spawn((
            Node {
                width: px(extent::TILE_PICTURE_WIDTH),
                height: px(extent::TILE_PICTURE_HEIGHT),
                border: UiRect::all(px(stroke::HAIRLINE)),
                border_radius: BorderRadius::all(px(radius::SMALL)),
                overflow: Overflow::clip(),
                ..default()
            },
            BorderTone(token::BORDER),
            ChoicePicture,
            ChildOf(root),
        ))
        .id();
    commands.spawn_scene(text_dim(caption, size::SMALL)).insert((
        ChoiceCaption,
        TextTone(words(chosen)),
        ChildOf(root),
    ));
    ChoiceTileParts { root, picture }
}

fn ring(chosen: bool) -> Token {
    if chosen { token::FILL_ACCENT } else { token::FILL_ITEM }
}

fn words(chosen: bool) -> Token {
    if chosen { token::TEXT } else { token::TEXT_DIM }
}

/// Rings the chosen tile and tones its caption, when a tile's choice changes.
pub(crate) fn paint_choice_tiles(
    tiles: Query<(Entity, &ChoiceTile, &Children), Changed<ChoiceTile>>,
    captions: Query<(), With<ChoiceCaption>>,
    mut commands: Commands,
) {
    for (tile, state, children) in &tiles {
        commands.entity(tile).insert(BorderTone(ring(state.chosen)));
        for &child in children {
            if captions.contains(child) {
                commands.entity(child).insert(TextTone(words(state.chosen)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chosen_tile_is_ringed_and_a_choice_made_elsewhere_moves_the_ring() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .add_systems(Update, paint_choice_tiles);
        let mut commands = app.world_mut().commands();
        let [a, b] = [true, false].map(|chosen| spawn_choice_tile(&mut commands, "Fade", chosen));
        app.world_mut().flush();
        app.update();
        let look = |app: &mut App, tile: ChoiceTileParts| {
            let world = app.world();
            let children = world.get::<Children>(tile.root).unwrap();
            let caption = children.iter().find(|c| world.get::<ChoiceCaption>(*c).is_some()).unwrap();
            (world.get::<BorderTone>(tile.root).unwrap().0.clone(), world.get::<TextTone>(caption).unwrap().0.clone())
        };
        assert_eq!(look(&mut app, a), (token::FILL_ACCENT, token::TEXT));
        assert_eq!(look(&mut app, b), (token::FILL_ITEM, token::TEXT_DIM));
        assert!(app.world().get::<ChoicePicture>(a.picture).is_some());
        app.world_mut().get_mut::<ChoiceTile>(a.root).unwrap().chosen = false;
        app.world_mut().get_mut::<ChoiceTile>(b.root).unwrap().chosen = true;
        app.update();
        assert_eq!(look(&mut app, a), (token::FILL_ITEM, token::TEXT_DIM));
        assert_eq!(look(&mut app, b), (token::FILL_ACCENT, token::TEXT));
    }
}
