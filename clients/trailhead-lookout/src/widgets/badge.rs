//! A badge: a short label, after an icon when there is one, in a small rounded chip, for what a card or row says
//! about a task (its key, priority, grade, holder, open questions) or a gate's state.

use bevy::feathers::theme::{ThemeBackgroundColor, ThemeTextColor, ThemeToken};
use bevy::prelude::*;

use super::{Icon, icon_text, radius, size, space, text_dim, text_toned, token};

/// A chip with `label` at the small size, with `icon` before it when given. `tone` colors the text and icon (such as
/// [`token::WARN`] for open questions); without one the text is dim, so a row of badges stays quiet beside the title.
pub fn badge(label: impl Into<String>, icon: Option<Icon>, tone: Option<ThemeToken>) -> impl Scene {
    let label = label.into();
    let inside: Box<dyn SceneList> = match (icon, tone) {
        (Some(icon), Some(tone)) => Box::new(bsn_list! {
            @icon_text(icon) ThemeTextColor({ tone.clone() })
            -- @text_toned(label, size::SMALL, tone)
        }),
        (Some(icon), None) => Box::new(bsn_list! {
            @icon_text(icon)
            -- @text_dim(label, size::SMALL)
        }),
        (None, Some(tone)) => Box::new(bsn_list! { @text_toned(label, size::SMALL, tone) }),
        (None, None) => Box::new(bsn_list! { @text_dim(label, size::SMALL) }),
    };
    bsn! {
        Node {
            align_items: AlignItems::Center,
            column_gap: { px(space::ICON_LABEL) },
            padding: { UiRect::axes(px(space::ICON_LABEL), px(space::BADGE_INSET)) },
            border_radius: { BorderRadius::all(px(radius::SMALL)) },
        }
        ThemeBackgroundColor({ token::BADGE_BG })
        Children [ { inside } ]
    }
}
