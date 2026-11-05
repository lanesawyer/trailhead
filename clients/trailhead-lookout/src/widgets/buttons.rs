//! Buttons, built on feathers' `FeathersButton` and `FeathersToolButton`.
//!
//! - [`danger_button`]: a normal button in the danger color, for cancel and revert.
//! - [`button`]: a caption, optionally after an icon, in a [`ButtonVariant`]. One `Primary` per surface (New task,
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
use bevy::feathers::theme::{ThemeTextColor, ThemeToken};
use bevy::feathers::tokens;
use bevy::prelude::*;

use super::{Hint, Icon, button_icon, icon_text, space, token};

/// A button's caption, in the button's own text color.
///
/// Feathers' `label` names `TEXT_MAIN` on the text itself, which beats the color a button gives its children, so a
/// caption made with `label` stays the window's text color on a primary or disabled button.
pub fn button_text(text: impl Into<String>) -> impl Scene {
    toned_text(text, tokens::BUTTON_TEXT)
}

/// A caption in `tone`, which a button's own color would otherwise replace.
fn toned_text(text: impl Into<String>, tone: ThemeToken) -> impl Scene {
    let text = text.into();
    bsn! {
        @label(text)
        ThemeTextColor({ tone })
    }
}

/// A button with `caption`, after `icon` when there is one.
pub fn button(caption: impl Into<String>, icon: Option<Icon>, variant: ButtonVariant) -> impl Scene {
    toned_button(caption, icon, variant, tokens::BUTTON_TEXT)
}

/// A normal button for something destructive (cancel, revert): its caption and icon in the danger color.
pub fn danger_button(caption: impl Into<String>, icon: Option<Icon>) -> impl Scene {
    toned_button(caption, icon, ButtonVariant::Normal, token::DANGER)
}

fn toned_button(
    caption: impl Into<String>,
    icon: Option<Icon>,
    variant: ButtonVariant,
    tone: ThemeToken,
) -> impl Scene {
    let caption = caption.into();
    let inside: Box<dyn SceneList> = match icon {
        Some(icon) => Box::new(bsn_list! {
            @icon_text(icon) ThemeTextColor({ tone.clone() }) -- @toned_text(caption, tone)
        }),
        None => Box::new(bsn_list! { @toned_text(caption, tone) }),
    };
    bsn! {
        @FeathersButton {
            @caption: { inside },
            @variant: { variant },
        }
        Node { column_gap: { px(space::ICON_LABEL) } }
    }
}

/// An icon-only button, with `hint` shown while it's hovered.
pub fn tool_button(icon: Icon, hint: impl Into<String>, variant: ButtonVariant) -> impl Scene {
    let hint = hint.into();
    bsn! {
        @FeathersToolButton {
            @caption: { bsn_list! { @button_icon(icon) } },
            @variant: { variant },
        }
        Hint({ hint })
    }
}
