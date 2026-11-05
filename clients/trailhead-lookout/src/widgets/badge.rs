//! A small badge: a short word, after an icon when there is one, on a rounded chip. For a gate's state, a card's
//! priority or grade.

use bevy::feathers::theme::{ThemeBackgroundColor, ThemeTextColor, ThemeToken};
use bevy::prelude::*;

use super::{Icon, icon_text, radius, size, space, text, token};

/// A badge saying `label`, with `icon` before it, both in `tone` (a theme text color, such as [`token::OK`]).
pub fn badge(label: impl Into<String>, icon: Option<Icon>, tone: ThemeToken) -> impl Scene {
    let label = label.into();
    let inside: Box<dyn SceneList> = match icon {
        Some(icon) => Box::new(bsn_list! {
            @icon_text(icon) ThemeTextColor({ tone.clone() })
            -- @text(label, size::SMALL) ThemeTextColor({ tone })
        }),
        None => Box::new(bsn_list! { @text(label, size::SMALL) ThemeTextColor({ tone }) }),
    };
    bsn! {
        Node {
            align_items: AlignItems::Center,
            column_gap: { px(space::ICON_LABEL) },
            padding: { UiRect::axes(px(space::CONTROL_INSET), px(space::BADGE_INSET)) },
            border_radius: { BorderRadius::all(px(radius::SMALL)) },
        }
        ThemeBackgroundColor({ token::BADGE_BG })
        Children [ {inside} ]
    }
}
