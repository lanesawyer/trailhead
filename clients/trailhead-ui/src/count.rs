//! A count on a button: how many of something wait behind it (the unread notifications on a bell, an app's own on its
//! button on the rail). A small accent pill over the button's top right corner, hidden at nothing.

use bevy::prelude::*;

use crate::buttons::BUTTON_TEXT;
use crate::patch::{display, patch_node, set_text};
use crate::spacing::{extent, space};
use crate::text::{size, text_toned};
use crate::theme::{BackgroundTone, token};

/// The most a count shows in figures; past it, it says `99+`.
pub const MOST_COUNTED: usize = 99;

/// How many a [`count_badge`] says, on its root. Set it with `set_if_neq`; the kit rewrites the figure and hides the
/// badge at 0.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CountBadge {
    pub count: usize,
}

/// The figure inside a [`count_badge`], which the kit rewrites.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct CountWords;

/// A count of `count` over the top right corner of what holds it: spawn it as the last child of a button (a
/// `tool_button`, a `rail_item`). It takes no presses, so the button under it still gets them.
pub fn count_badge(count: usize) -> impl Scene {
    let words = count_words(count);
    bsn! {
        Node {
            position_type: PositionType::Absolute,
            top: { px(-space::COUNT_OFFSET) },
            right: { px(-space::COUNT_OFFSET) },
            min_width: { px(extent::COUNT_SIZE) },
            height: { px(extent::COUNT_SIZE) },
            padding: { UiRect::horizontal(px(space::BADGE_INSET)) },
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            border_radius: { BorderRadius::MAX },
            display: { display(count > 0) },
        }
        BackgroundTone({ token::FILL_ACCENT })
        CountBadge { count: { count } }
        Pickable::IGNORE
        Children [
            @text_toned(words, size::SMALL, BUTTON_TEXT)
            CountWords
            Pickable::IGNORE
        ]
    }
}

/// What a count says: the figure, or `99+` past [`MOST_COUNTED`].
pub fn count_words(count: usize) -> String {
    if count > MOST_COUNTED { format!("{MOST_COUNTED}+") } else { count.to_string() }
}

/// Rewrites each changed count's figure and shows or hides it, in place.
pub(crate) fn show_counts(
    counts: Query<(Entity, &CountBadge, &Children), Changed<CountBadge>>,
    words: Query<(), With<CountWords>>,
    mut texts: Query<&mut Text>,
    mut nodes: Query<&mut Node>,
) {
    for (badge, count, children) in &counts {
        if let Ok(node) = nodes.get_mut(badge) {
            patch_node(node, |n| n.display = display(count.count > 0));
        }
        for child in children.iter().filter(|c| words.contains(*c)) {
            if let Ok(text) = texts.get_mut(child) {
                set_text(text, &count_words(count.count));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_count_says_its_figure_up_to_ninety_nine() {
        assert_eq!([0, 1, 99, 100, 4000].map(count_words), ["0", "1", "99", "99+", "99+"]);
    }

    #[test]
    fn a_count_is_rewritten_and_hidden_in_place() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .add_systems(Update, show_counts);
        let badge = app.world_mut().commands().spawn_scene(count_badge(3)).id();
        app.world_mut().flush();
        app.update();
        let figure = app.world().get::<Children>(badge).unwrap()[0];
        let said = |app: &App| {
            let shown = app.world().get::<Node>(badge).unwrap().display != Display::None;
            (app.world().get::<Text>(figure).unwrap().0.clone(), shown)
        };
        assert_eq!(said(&app), ("3".into(), true));
        app.world_mut().get_mut::<CountBadge>(badge).unwrap().count = 120;
        app.update();
        assert_eq!(said(&app), ("99+".into(), true));
        app.world_mut().get_mut::<CountBadge>(badge).unwrap().count = 0;
        app.update();
        assert!(!said(&app).1, "nothing to count, so it is hidden");
        assert_eq!(app.world().get::<Children>(badge).unwrap()[0], figure, "the same figure throughout");
    }
}
