//! Buttons, built on feathers' `FeathersButton` and `FeathersToolButton`.
//!
//! - [`danger_button`]: a normal button in the danger color, for cancel and revert.
//! - [`button`]: a caption, optionally after an icon, in a [`ButtonKind`]. One `Primary` per surface (New task,
//!   Save); the rest `Normal`.
//! - [`tool_button`]: an icon alone, with a [`Hint`] saying what it does. `Plain` on cards and lists, where a row
//!   of backgrounds would be noise; `Normal` in a toolbar.
//!
//! A button that opens a menu is [`super::icon_menu`], with its chevron. One that acts never has a chevron.
//!
//! What a press does is the caller's: put a component on the button and an observer of `Activate` for it, or `on(..)`
//! after the scene.

use bevy::feathers::controls::{ButtonVariant, FeathersButton, FeathersToolButton};
use bevy::feathers::display::label;
use bevy::feathers::tokens;
use bevy::prelude::*;

use super::{Hint, Icon, TextTone, Token, button_icon, icon_text, space, token};

/// A button's caption: feathers' own, pale over a button's gray and on the accent alike.
pub(crate) const BUTTON_TEXT: Token = Token(tokens::BUTTON_TEXT);

/// How much a button stands out.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum ButtonKind {
    /// A button's own background.
    #[default]
    Normal,
    /// The accent: the one call to action on a surface.
    Primary,
    /// No background until hovered or pressed: on cards, lists and links.
    Plain,
}

impl From<ButtonKind> for ButtonVariant {
    fn from(kind: ButtonKind) -> Self {
        match kind {
            ButtonKind::Normal => ButtonVariant::Normal,
            ButtonKind::Primary => ButtonVariant::Primary,
            ButtonKind::Plain => ButtonVariant::Plain,
        }
    }
}

/// A button's caption, in the button's own text color.
///
/// Feathers' `label` names `TEXT_MAIN` on the text itself, which beats the color a button gives its children, so a
/// caption made with `label` stays the window's text color on a primary or disabled button.
pub fn button_text(text: impl Into<String>) -> impl Scene {
    toned_text(text, BUTTON_TEXT)
}

/// A caption in `tone`, which a button's own color would otherwise replace.
pub(crate) fn toned_text(text: impl Into<String>, tone: Token) -> impl Scene {
    let text = text.into();
    bsn! {
        @label(text)
        TextTone({ tone })
    }
}

/// A button with `caption`, after `icon` when there is one.
pub fn button(caption: impl Into<String>, icon: Option<Icon>, kind: ButtonKind) -> impl Scene {
    toned_button(caption, icon, kind, BUTTON_TEXT)
}

/// A normal button for something destructive (cancel, revert): its caption and icon in the danger color.
pub fn danger_button(caption: impl Into<String>, icon: Option<Icon>) -> impl Scene {
    toned_button(caption, icon, ButtonKind::Normal, token::STATUS_DANGER)
}

fn toned_button(caption: impl Into<String>, icon: Option<Icon>, kind: ButtonKind, tone: Token) -> impl Scene {
    let caption = caption.into();
    let inside: Box<dyn SceneList> = match icon {
        Some(icon) => Box::new(bsn_list! {
            @icon_text(icon) TextTone({ tone.clone() }) -- @toned_text(caption, tone)
        }),
        None => Box::new(bsn_list! { @toned_text(caption, tone) }),
    };
    bsn! {
        @FeathersButton {
            @caption: { inside },
            @variant: { ButtonVariant::from(kind) },
        }
        Node { column_gap: { px(space::ICON_LABEL) } }
    }
}

/// An icon-only button, with `hint` shown while it's hovered.
pub fn tool_button(icon: Icon, hint: impl Into<String>, kind: ButtonKind) -> impl Scene {
    let hint = hint.into();
    bsn! {
        @FeathersToolButton {
            @caption: { bsn_list! { @button_icon(icon) } },
            @variant: { ButtonVariant::from(kind) },
        }
        Hint({ hint })
    }
}
