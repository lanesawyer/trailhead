//! An app's sidebar: a column down the left of the window with the app's name, its places to go, and a footer for
//! state that is always there (a connection chip, settings).
//!
//! [`sidebar`] is the column, a root of its own; spawn its contents as its children, and a [`sidebar_footer`] last to
//! hold the bottom. A place to go is a [`nav_item`], a plain button the width of the sidebar that shows a background
//! while it is the page shown: set that on its [`NavItem`]; [`nav_item_noted`] adds a dim note just after its caption and a dim value at its end. A group of them goes under a [`nav_heading`].
//!
//! The pages an app shows beside the sidebar fill [`beside_sidebar`], so the two never overlap.

use bevy::feathers::controls::{ButtonVariant, FeathersButton};
use bevy::prelude::*;

use super::{BorderTone, ButtonKind, Icon, Surface, button, button_text, extent, size, space, stroke, text_dim, token};

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
            border: { UiRect::right(px(stroke::HAIRLINE)) },
            min_height: px(0),
        }
        Surface::Panel
        BorderTone({ token::BORDER })
    }
}

/// The bottom of a [`sidebar`]: pushed down to the sidebar's foot, its children in a row. Like the sidebar's places, it
/// keeps its height in a short window, so the part that scrolls gives way instead.
pub fn sidebar_footer() -> impl Scene {
    bsn! {
        Node {
            margin: { UiRect::top(Val::Auto) },
            flex_shrink: { 0.0_f32 },
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
        Node { margin: { UiRect::top(px(space::GROUPS)) }, flex_shrink: { 0.0_f32 } }
    }
}

/// A place to go in a [`sidebar`]: `caption` after `icon`, the width of the sidebar. Plain until it is the page shown
/// ([`NavItem::current`]), and like every button it shows a background under the pointer. It keeps its height in a
/// sidebar too short for its places, which scroll instead.
pub fn nav_item(caption: impl Into<String>, icon: Option<Icon>) -> impl Scene {
    let caption = caption.into();
    bsn! {
        @button(caption, icon, ButtonKind::Plain)
        Node { justify_content: JustifyContent::Start, flex_shrink: { 0.0_f32 } }
        NavItem
    }
}

/// A [`nav_item`] with `note` just after `caption` and `end` at the far right, both smaller and dimmer: a project's
/// name, its key, and how many of its tasks are open. Their texts come in that order, for updating in place.
pub fn nav_item_noted(caption: impl Into<String>, note: impl Into<String>, end: impl Into<String>) -> impl Scene {
    let (caption, note, end) = (caption.into(), note.into(), end.into());
    bsn! {
        @FeathersButton {
            @caption: { bsn_list! {
                @button_text(caption)
                --
                @text_dim(note, size::SMALL)
                --
                @text_dim(end, size::SMALL)
                Node { margin: { UiRect::left(Val::Auto) } }
            } },
            @variant: { ButtonVariant::Plain },
        }
        Node { justify_content: JustifyContent::Start, column_gap: { px(space::ICON_LABEL) }, flex_shrink: { 0.0_f32 } }
        NavItem
    }
}

/// A [`nav_item`], and whether it is the page shown, which gives it a background. Set `current` with `set_if_neq`.
#[derive(Component, Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct NavItem {
    pub current: bool,
}

/// Give each nav item whose `current` changed the background that says so.
pub(crate) fn paint_nav_items(mut items: Query<(&NavItem, &mut ButtonVariant), Changed<NavItem>>) {
    for (item, mut variant) in &mut items {
        variant.set_if_neq(if item.current { ButtonVariant::Normal } else { ButtonVariant::Plain });
    }
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
        let mut app = App::new();
        app.add_systems(Update, paint_nav_items);
        let item = app.world_mut().spawn((NavItem::default(), ButtonVariant::Plain)).id();
        let set = |app: &mut App, current| {
            app.world_mut().get_mut::<NavItem>(item).unwrap().set_if_neq(NavItem { current });
            app.update();
            app.world().get::<ButtonVariant>(item).unwrap().clone()
        };
        assert_eq!(set(&mut app, true), ButtonVariant::Normal);
        assert_eq!(set(&mut app, false), ButtonVariant::Plain);
    }

    #[test]
    fn a_page_beside_the_sidebar_starts_where_it_ends() {
        let page = beside_sidebar();
        assert_eq!(page.left, px(extent::SIDEBAR_WIDTH));
        assert_eq!((page.right, page.top, page.bottom), (px(0), px(0), px(0)));
    }
}
