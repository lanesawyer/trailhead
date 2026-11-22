//! A badge: a short label, after an icon when there is one, in a small rounded chip, for what a card or row says
//! about a task (its key, priority, grade, holder, open questions) or a gate's state.
//!
//! A badge whose words, icon or tone change, or that comes and goes, is changed in place with [`BadgeEdit`].

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use super::{BackgroundTone, Icon, TextTone, Token, icon_text, radius, size, space, text_toned, token};
use crate::patch::{display, patch_node, set_text};

/// A chip with `label` at the small size, with `icon` before it when given. `tone` colors the text and icon (such as
/// [`token::STATUS_WARN`] for open questions); without one they are dim, so a row of badges stays quiet beside the
/// title.
pub fn badge(label: impl Into<String>, icon: Option<Icon>, tone: Option<Token>) -> impl Scene {
    let label = label.into();
    let tone = tone.unwrap_or(token::TEXT_DIM);
    let glyph = icon.unwrap_or(Icon::Circle);
    bsn! {
        Node {
            align_items: AlignItems::Center,
            column_gap: { px(space::ICON_LABEL) },
            padding: { UiRect::axes(px(space::ICON_LABEL), px(space::BADGE_INSET)) },
            border_radius: { BorderRadius::all(px(radius::SMALL)) },
        }
        BackgroundTone({ token::FILL_SOLID })
        Children [
            @icon_text(glyph)
            BadgeIcon
            TextTone({ tone.clone() })
            Node { display: { display(icon.is_some()) } }
            --
            @text_toned(label, size::SMALL, tone)
            BadgeLabel
        ]
    }
}

/// A badge's icon, which [`BadgeEdit`] redraws and hides when the badge has none.
#[derive(Component, Default, Clone)]
pub struct BadgeIcon;

/// A badge's words, which [`BadgeEdit`] rewrites.
#[derive(Component, Default, Clone)]
pub struct BadgeLabel;

/// Changes a spawned [`badge`] in place: its words, icon and tone, and whether it is shown. Writes only what differs.
#[derive(SystemParam)]
pub struct BadgeEdit<'w, 's> {
    children: Query<'w, 's, &'static Children>,
    icons: Query<'w, 's, (), With<BadgeIcon>>,
    labels: Query<'w, 's, (), With<BadgeLabel>>,
    texts: Query<'w, 's, &'static mut Text>,
    tones: Query<'w, 's, &'static TextTone>,
    nodes: Query<'w, 's, &'static mut Node>,
    commands: Commands<'w, 's>,
}

impl BadgeEdit<'_, '_> {
    /// Gives the badge these words, this icon (or none) and this tone (or dim), as [`badge`] would have drawn them.
    pub fn set(&mut self, badge: Entity, label: &str, icon: Option<Icon>, tone: Option<Token>) {
        let tone = tone.unwrap_or(token::TEXT_DIM);
        let parts: Vec<Entity> = self.children.iter_descendants(badge).collect();
        for part in parts {
            let words = if self.labels.contains(part) {
                label
            } else if self.icons.contains(part) {
                if let Ok(node) = self.nodes.get_mut(part) {
                    patch_node(node, |n| n.display = display(icon.is_some()));
                }
                icon.map_or("", Icon::glyph)
            } else {
                continue;
            };
            if let Ok(text) = self.texts.get_mut(part)
                && !words.is_empty()
            {
                set_text(text, words);
            }
            if self.tones.get(part).is_ok_and(|t| t.0 != tone) {
                self.commands.entity(part).insert(TextTone(tone.clone()));
            }
        }
    }

    /// Shows or hides the badge, keeping its place among its siblings for when it comes back.
    pub fn set_shown(&mut self, badge: Entity, shown: bool) {
        if let Ok(node) = self.nodes.get_mut(badge) {
            patch_node(node, |n| n.display = display(shown));
        }
    }
}

#[cfg(test)]
mod tests {
    use bevy::ecs::system::RunSystemOnce;

    use super::*;

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>();
        app
    }

    fn spawn(app: &mut App, icon: Option<Icon>, tone: Option<Token>) -> Entity {
        let badge = app.world_mut().commands().spawn_scene(badge("label", icon, tone)).id();
        app.world_mut().flush();
        app.update();
        badge
    }

    /// The badge's label and icon: their text, tone and whether each shows.
    fn parts(app: &mut App, badge: Entity) -> [(String, Token, bool); 2] {
        let world = app.world_mut();
        let part = |marker: fn(&World, Entity) -> bool| {
            let mut found = vec![badge];
            let mut at = 0;
            while at < found.len() {
                if let Some(children) = world.get::<Children>(found[at]) {
                    found.extend(children.iter());
                }
                at += 1;
            }
            let e = found.into_iter().find(|e| marker(world, *e)).unwrap();
            let shown = world.get::<Node>(e).is_none_or(|n| n.display != Display::None);
            (world.get::<Text>(e).unwrap().0.clone(), world.get::<TextTone>(e).unwrap().0.clone(), shown)
        };
        [part(|w, e| w.get::<BadgeLabel>(e).is_some()), part(|w, e| w.get::<BadgeIcon>(e).is_some())]
    }

    #[test]
    fn a_badges_icon_and_label_take_its_tone_or_stay_dim() {
        let mut app = app();
        for icon in [None, Some(Icon::Info)] {
            for tone in [None, Some(token::STATUS_WARN)] {
                let badge = spawn(&mut app, icon, tone.clone());
                let [label, glyph] = parts(&mut app, badge);
                let case = format!("icon {icon:?}, tone {tone:?}");
                let wanted = tone.unwrap_or(token::TEXT_DIM);
                assert_eq!(label, ("label".to_string(), wanted.clone(), true), "{case}");
                assert_eq!((glyph.1, glyph.2), (wanted, icon.is_some()), "{case}");
            }
        }
    }

    #[test]
    fn a_badge_is_changed_in_place_and_hidden_and_shown_again() {
        let mut app = app();
        let badge = spawn(&mut app, None, None);
        app.world_mut()
            .run_system_once(move |mut edit: BadgeEdit| {
                edit.set(badge, "P1", Some(Icon::Flag), Some(token::STATUS_DANGER));
            })
            .unwrap();
        let [label, glyph] = parts(&mut app, badge);
        assert_eq!(label, ("P1".to_string(), token::STATUS_DANGER, true));
        assert_eq!(glyph, (Icon::Flag.glyph().to_string(), token::STATUS_DANGER, true));

        app.world_mut().run_system_once(move |mut edit: BadgeEdit| edit.set(badge, "P2", None, None)).unwrap();
        let [label, glyph] = parts(&mut app, badge);
        assert_eq!(label, ("P2".to_string(), token::TEXT_DIM, true));
        assert!(!glyph.2, "no icon, so it is hidden");

        app.world_mut().run_system_once(move |mut edit: BadgeEdit| edit.set_shown(badge, false)).unwrap();
        assert_eq!(app.world().get::<Node>(badge).unwrap().display, Display::None);
        app.world_mut().run_system_once(move |mut edit: BadgeEdit| edit.set_shown(badge, true)).unwrap();
        assert_eq!(app.world().get::<Node>(badge).unwrap().display, Display::Flex);
    }
}
