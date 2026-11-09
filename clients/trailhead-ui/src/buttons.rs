//! Buttons, built on feathers' `FeathersButton` and `FeathersToolButton`.
//!
//! - [`button`]: a caption, optionally after an icon, in a [`ButtonKind`]. One `Primary` per surface (New task,
//!   Save); `Danger` for something destructive (cancel, revert); the rest `Normal`.
//! - [`tool_button`]: an icon alone, with a [`Hint`] saying what it does. `Plain` on cards and lists, where a row
//!   of backgrounds would be noise; `Normal` in a toolbar.
//!
//! A button that opens a menu is [`super::spawn_icon_menu`], with its chevron. One that acts never has a chevron.
//!
//! What a press does is the caller's: put a component on the button and an observer of `Activate` for it, or `on(..)`
//! after the scene.

use bevy::feathers::controls::{ButtonVariant, FeathersButton, FeathersToolButton};
use bevy::feathers::display::label;
use bevy::feathers::tokens;
use bevy::prelude::*;
use bevy::ui::InteractionDisabled;

use super::{Hint, Icon, TextTone, Token, button_icon, icon_text, space, token};

/// A button's caption: feathers' own, pale over a button's gray and on the accent alike.
pub(crate) const BUTTON_TEXT: Token = Token(tokens::BUTTON_TEXT);
/// A disabled button's caption, dimmed.
const BUTTON_TEXT_DISABLED: Token = Token(tokens::BUTTON_TEXT_DISABLED);
/// A disabled primary button's caption, dimmed on the dimmed accent.
const BUTTON_PRIMARY_TEXT_DISABLED: Token = Token(tokens::BUTTON_PRIMARY_TEXT_DISABLED);

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
    /// A normal button with its caption in the danger color: something destructive (cancel, revert).
    Danger,
}

impl ButtonKind {
    /// The caption's color.
    pub(crate) fn caption(self) -> Token {
        match self {
            ButtonKind::Danger => token::STATUS_DANGER,
            _ => BUTTON_TEXT,
        }
    }
}

impl From<ButtonKind> for ButtonVariant {
    fn from(kind: ButtonKind) -> Self {
        match kind {
            ButtonKind::Normal | ButtonKind::Danger => ButtonVariant::Normal,
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
        TextTone({ tone.clone() })
        CaptionTone({ tone })
    }
}

/// An icon in a caption, in `tone` like [`toned_text`].
pub(crate) fn toned_icon(icon: Icon, tone: Token) -> impl Scene {
    bsn! {
        @icon_text(icon)
        TextTone({ tone.clone() })
        CaptionTone({ tone })
    }
}

/// A caption's tone while its button is enabled. A caption carries its own [`TextTone`], which beats the dimmed color
/// feathers gives a disabled button's children, so [`dim_disabled_captions`] swaps it for the disabled one.
#[derive(Component, Clone, Default, PartialEq, Debug)]
pub(crate) struct CaptionTone(pub(crate) Token);

/// Dims the captions of disabled buttons and gives them back their tone once enabled.
pub(crate) fn dim_disabled_captions(
    captions: Query<(Entity, &CaptionTone, &TextTone)>,
    buttons: Query<(&ButtonVariant, Has<InteractionDisabled>)>,
    parents: Query<&ChildOf>,
    mut commands: Commands,
) {
    for (caption, CaptionTone(tone), TextTone(shown)) in &captions {
        let Some((variant, disabled)) = parents.iter_ancestors(caption).find_map(|e| buttons.get(e).ok()) else {
            continue;
        };
        let wanted = match (disabled, variant) {
            (false, _) => tone,
            (true, ButtonVariant::Primary) => &BUTTON_PRIMARY_TEXT_DISABLED,
            (true, _) => &BUTTON_TEXT_DISABLED,
        };
        if shown != wanted {
            commands.entity(caption).insert(TextTone(wanted.clone()));
        }
    }
}

/// A button with `caption`, after `icon` when there is one.
pub fn button(caption: impl Into<String>, icon: Option<Icon>, kind: ButtonKind) -> impl Scene {
    let caption = caption.into();
    let tone = kind.caption();
    let inside: Box<dyn SceneList> = match icon {
        Some(icon) => Box::new(bsn_list! {
            @toned_icon(icon, tone.clone()) -- @toned_text(caption, tone)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_disabled_buttons_caption_dims_and_takes_its_tone_back_once_enabled() {
        let mut app = App::new();
        app.add_systems(Update, dim_disabled_captions);
        let tone = |app: &App, e| app.world().get::<TextTone>(e).unwrap().0.clone();
        let primary = app.world_mut().spawn((ButtonVariant::Primary, InteractionDisabled)).id();
        let caption = app.world_mut().spawn((TextTone(BUTTON_TEXT), CaptionTone(BUTTON_TEXT), ChildOf(primary))).id();
        let danger = app.world_mut().spawn((ButtonVariant::Normal, InteractionDisabled)).id();
        let red = token::STATUS_DANGER;
        let warning = app.world_mut().spawn((TextTone(red.clone()), CaptionTone(red.clone()), ChildOf(danger))).id();
        app.update();
        assert_eq!(tone(&app, caption), BUTTON_PRIMARY_TEXT_DISABLED);
        assert_eq!(tone(&app, warning), BUTTON_TEXT_DISABLED, "a danger caption dims too");
        app.world_mut().entity_mut(primary).remove::<InteractionDisabled>();
        app.world_mut().entity_mut(danger).remove::<InteractionDisabled>();
        app.update();
        assert_eq!(tone(&app, caption), BUTTON_TEXT);
        assert_eq!(tone(&app, warning), red);
    }
}
