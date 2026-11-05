//! A badge: a short label in a small rounded chip, for what a card or row says about a task (its key, priority,
//! grade, holder, open questions).

use bevy::feathers::theme::{ThemeBackgroundColor, ThemeToken};
use bevy::prelude::*;

use super::{radius, size, space, text_dim, text_toned, token};

/// A chip with `label` at the small size. `tone` colors the text (such as [`token::WARN`] for open questions);
/// without one the text is dim, so a row of badges stays quiet beside the title.
pub fn badge(label: impl Into<String>, tone: Option<ThemeToken>) -> impl Scene {
    let label = label.into();
    let inside: Box<dyn SceneList> = match tone {
        Some(tone) => Box::new(bsn_list! { @text_toned(label, size::SMALL, tone) }),
        None => Box::new(bsn_list! { @text_dim(label, size::SMALL) }),
    };
    bsn! {
        Node {
            padding: { UiRect::axes(px(space::ICON_LABEL), px(space::BADGE_INSET)) },
            border_radius: { BorderRadius::all(px(radius::SMALL)) },
        }
        ThemeBackgroundColor({ token::BADGE_BG })
        Children [ { inside } ]
    }
}
