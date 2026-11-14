//! A badge: a short label, after an icon when there is one, in a small rounded chip, for what a card or row says
//! about a task (its key, priority, grade, holder, open questions) or a gate's state.

use bevy::prelude::*;

use super::{BackgroundTone, Icon, TextTone, Token, icon_text, radius, size, space, text_dim, text_toned, token};

/// A chip with `label` at the small size, with `icon` before it when given. `tone` colors the text and icon (such as
/// [`token::STATUS_WARN`] for open questions); without one the text is dim, so a row of badges stays quiet beside the title.
pub fn badge(label: impl Into<String>, icon: Option<Icon>, tone: Option<Token>) -> impl Scene {
    let label = label.into();
    let inside: Box<dyn SceneList> = match (icon, tone) {
        (Some(icon), Some(tone)) => Box::new(bsn_list! {
            @icon_text(icon) TextTone({ tone.clone() })
            -- @text_toned(label, size::SMALL, tone)
        }),
        (Some(icon), None) => Box::new(bsn_list! {
            @icon_text(icon) TextTone({ token::TEXT_DIM })
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
        BackgroundTone({ token::FILL_SOLID })
        Children [ { inside } ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `TextTone` of each text a badge spawns (the icon glyph, the label) that has one, with its text.
    fn tones(icon: Option<Icon>, tone: Option<Token>) -> Vec<(String, Token)> {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>();
        app.world_mut().commands().spawn_scene(badge("label", icon, tone));
        app.world_mut().flush();
        app.update();
        let world = app.world_mut();
        world.query::<(&Text, &TextTone)>().iter(world).map(|(t, tone)| (t.0.clone(), tone.0.clone())).collect()
    }

    #[test]
    fn a_badges_icon_and_label_take_its_tone_or_stay_dim() {
        for icon in [None, Some(Icon::Info)] {
            for tone in [None, Some(token::STATUS_WARN)] {
                let found = tones(icon, tone.clone());
                let (label, glyph): (Vec<_>, Vec<_>) = found.into_iter().partition(|(text, _)| text == "label");
                let case = format!("icon {icon:?}, tone {tone:?}");
                // The icon glyph always has a tone: the badge's, or dim, never the white default.
                assert_eq!(glyph.len(), usize::from(icon.is_some()), "{case}");
                for (_, got) in glyph {
                    assert_eq!(got, tone.clone().unwrap_or(token::TEXT_DIM), "{case}");
                }
                // The label is toned when the badge is; otherwise `text_dim` dims it another way.
                assert_eq!(label.len(), usize::from(tone.is_some()), "{case}");
                for (_, got) in label {
                    assert_eq!(Some(got), tone, "{case}");
                }
            }
        }
    }
}
