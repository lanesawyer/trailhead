//! A small status chip: a colored dot and a few words, with the full explanation in a [`Hint`].
//!
//! For state that is always there and rarely needs reading, such as the connection in the header bar.

use bevy::ecs::system::SystemParam;
use bevy::feathers::theme::{ThemeBackgroundColor, ThemeToken};
use bevy::prelude::*;

use super::{Hint, radius, size, space, text};

/// A small round dot in `tone`.
pub fn dot(tone: ThemeToken) -> impl Scene {
    bsn! {
        Node {
            width: { px(space::DOT) },
            height: { px(space::DOT) },
            border_radius: { BorderRadius::all(px(radius::PANEL)) },
        }
        ThemeBackgroundColor({ tone })
        ChipDot
    }
}

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
                @dot(tone)
            -- @text(words, size::SECONDARY) ChipWords
        ]
    }
}

/// A chip's dot, which [`ChipEdit`] recolors.
#[derive(Component, Default, Clone)]
pub struct ChipDot;

/// A chip's words, which [`ChipEdit`] rewrites.
#[derive(Component, Default, Clone)]
pub struct ChipWords;

/// Changes a spawned [`chip`] in place: the dot's tone, the words and the hint. Writes only what differs.
#[derive(SystemParam)]
pub struct ChipEdit<'w, 's> {
    children: Query<'w, 's, &'static Children>,
    dots: Query<'w, 's, &'static ThemeBackgroundColor, With<ChipDot>>,
    words: Query<'w, 's, &'static mut Text, With<ChipWords>>,
    hints: Query<'w, 's, &'static mut Hint>,
    commands: Commands<'w, 's>,
}

impl ChipEdit<'_, '_> {
    pub fn set(&mut self, chip: Entity, tone: ThemeToken, words: &str, hint: &str) {
        if let Ok(mut h) = self.hints.get_mut(chip)
            && h.0 != hint
        {
            h.0 = hint.to_string();
        }
        for e in self.children.iter_descendants(chip) {
            if let Ok(dot) = self.dots.get(e)
                && dot.0 != tone
            {
                self.commands.entity(e).insert(ThemeBackgroundColor(tone.clone()));
            }
            if let Ok(mut text) = self.words.get_mut(e)
                && text.0 != words
            {
                text.0 = words.to_string();
            }
        }
    }
}
