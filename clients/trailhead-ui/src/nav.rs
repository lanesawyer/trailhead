//! An app's sidebar: a column down the left of the window with the app's name, its places to go, and a footer for
//! state that is always there (a connection chip, settings).
//!
//! [`sidebar`] is the column, a root of its own; spawn its contents as its children, and a [`sidebar_footer`] last to
//! hold the bottom. A place to go is a [`nav_item`], a plain button the width of the sidebar that shows a background
//! while it is the page shown: set that with [`set_nav_current`]; [`nav_item_noted`] adds a dim note after its caption. A group of them goes under a [`nav_heading`].
//!
//! The pages an app shows beside the sidebar fill [`beside_sidebar`], so the two never overlap.

use bevy::feathers::controls::{ButtonVariant, FeathersButton};
use bevy::feathers::theme::{ThemeBackgroundColor, ThemeBorderColor};
use bevy::prelude::*;

use super::{Icon, button, button_text, extent, size, space, text_dim, token};

/// The sidebar down the left of the window, [`extent::SIDEBAR_WIDTH`] wide.
pub fn sidebar() -> impl Scene {
    bsn! {
        Node {
            position_type: PositionType::Absolute,
            left: px(0),
            top: px(0),
            bottom: px(0),
            width: { px(extent::SIDEBAR_WIDTH) },
            flex_direction: FlexDirection::Column,
            row_gap: { px(space::ROWS) },
            padding: { UiRect::all(px(space::PANEL_INSET)) },
            border: { UiRect::right(px(1)) },
            min_height: px(0),
        }
        ThemeBackgroundColor({ token::PANEL_BG })
        ThemeBorderColor({ token::DIVIDER })
    }
}

/// The bottom of a [`sidebar`]: pushed down to the sidebar's foot, its children in a row.
pub fn sidebar_footer() -> impl Scene {
    bsn! {
        Node {
            margin: { UiRect::top(Val::Auto) },
            align_items: AlignItems::Center,
            justify_content: JustifyContent::SpaceBetween,
            column_gap: { px(space::CONTROLS) },
        }
    }
}

/// The dim words over a group of [`nav_item`]s.
pub fn nav_heading(words: impl Into<String>) -> impl Scene {
    let words = words.into();
    bsn! {
        @text_dim(words, size::SECONDARY)
        Node { margin: { UiRect::top(px(space::GROUPS)) } }
    }
}

/// A place to go in a [`sidebar`]: `caption` after `icon`, the width of the sidebar. Plain until it is the page shown
/// ([`set_nav_current`]), and like every button it shows a background under the pointer.
pub fn nav_item(caption: impl Into<String>, icon: Option<Icon>) -> impl Scene {
    let caption = caption.into();
    bsn! {
        @button(caption, icon, ButtonVariant::Plain)
        Node { justify_content: JustifyContent::Start }
        NavItem
    }
}

/// A [`nav_item`] with `note` after `caption`, smaller and dimmer: a project's name with its key.
pub fn nav_item_noted(caption: impl Into<String>, note: impl Into<String>) -> impl Scene {
    let (caption, note) = (caption.into(), note.into());
    bsn! {
        @FeathersButton {
            @caption: { bsn_list! {
                @button_text(caption)
                --
                @text_dim(note, size::SMALL)
                Node { margin: { UiRect::left(Val::Auto) } }
            } },
            @variant: { ButtonVariant::Plain },
        }
        Node { justify_content: JustifyContent::Start, column_gap: { px(space::ICON_LABEL) } }
        NavItem
    }
}

/// Marks a [`nav_item`].
#[derive(Component, Clone, Default)]
pub struct NavItem;

/// Shows a [`nav_item`] as the page shown, or not.
pub fn set_nav_current(mut variant: Mut<ButtonVariant>, current: bool) {
    variant.set_if_neq(if current { ButtonVariant::Normal } else { ButtonVariant::Plain });
}

/// The area right of a [`sidebar`], for a page to fill.
pub fn beside_sidebar() -> Node {
    Node {
        position_type: PositionType::Absolute,
        left: px(extent::SIDEBAR_WIDTH),
        right: px(0),
        top: px(0),
        bottom: px(0),
        ..default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nav_item_is_plain_until_it_is_the_page_shown() {
        let mut world = World::new();
        let item = world.spawn(ButtonVariant::Plain).id();
        let set = |world: &mut World, current| {
            let variant = world.get_mut::<ButtonVariant>(item).unwrap();
            set_nav_current(variant, current);
        };
        set(&mut world, true);
        assert_eq!(world.get::<ButtonVariant>(item), Some(&ButtonVariant::Normal));
        set(&mut world, false);
        assert_eq!(world.get::<ButtonVariant>(item), Some(&ButtonVariant::Plain));
    }

    #[test]
    fn a_page_beside_the_sidebar_starts_where_it_ends() {
        let page = beside_sidebar();
        assert_eq!(page.left, px(extent::SIDEBAR_WIDTH));
        assert_eq!((page.right, page.top, page.bottom), (px(0), px(0), px(0)));
    }
}
