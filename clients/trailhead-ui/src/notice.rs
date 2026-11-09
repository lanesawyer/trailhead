//! A message about how something went, boxed and marked by what kind of news it is.
//!
//! Every status line is one of these (a thing that wouldn't open, a save that landed, a sign-in under way), so they
//! look alike wherever they sit. Whoever owns one writes a [`Notice`] onto it and the kit draws it; an empty message
//! hides the box rather than leaving an empty one.
//!
//! Ported from bevy-data-explorer's `widgets/notice.rs`, with its palette swapped for the kit's tokens.

use bevy::prelude::*;
use bevy::text::LineHeight;

use crate::icons::{Icon, icon_text};
use crate::patch::{display, patch_node, set_text};
use crate::spacing::{radius, space, stroke};
use crate::text::{size, text};
use crate::theme::{BackgroundTone, BorderTone, TextTone, Token, token};

/// What kind of news a message is, which picks its icon and color.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum Tone {
    /// Something under way, or worth knowing.
    #[default]
    Info,
    Success,
    /// Nothing failed, or not outright, but it's worth reading before going on.
    Warning,
    Error,
}

impl Tone {
    fn icon(self) -> Icon {
        match self {
            Tone::Info => Icon::Info,
            Tone::Success => Icon::CircleCheck,
            Tone::Warning => Icon::TriangleAlert,
            Tone::Error => Icon::CircleAlert,
        }
    }

    /// The token a notice of this tone is drawn in.
    pub fn token(self) -> Token {
        match self {
            Tone::Info => token::STATUS_INFO,
            Tone::Success => token::STATUS_OK,
            Tone::Warning => token::STATUS_WARN,
            Tone::Error => token::STATUS_DANGER,
        }
    }

    /// The faint token a notice of this tone is filled with.
    pub fn subtle(self) -> Token {
        match self {
            Tone::Info => token::STATUS_INFO_SUBTLE,
            Tone::Success => token::STATUS_OK_SUBTLE,
            Tone::Warning => token::STATUS_WARN_SUBTLE,
            Tone::Error => token::STATUS_DANGER_SUBTLE,
        }
    }
}

/// What a notice says. Empty hides it.
#[derive(Component, Clone, Default, PartialEq, Debug)]
pub struct Notice {
    pub tone: Tone,
    pub message: String,
}

impl Notice {
    pub fn new(tone: Tone, message: impl Into<String>) -> Self {
        Notice { tone, message: message.into() }
    }
}

#[derive(Component, Clone, Default)]
pub(crate) struct NoticeIcon;

#[derive(Component, Clone, Default)]
pub(crate) struct NoticeText;

/// The height of every line in a notice, icon and text alike. The icon is larger than the text, and on their own line
/// boxes the text sat high beside it; given the same one, each is centered in it, and a message that wraps keeps the
/// icon level with its first line.
const LINE_HEIGHT: f32 = 18.0;

/// How far the text is lowered to look centered. Fira Sans draws its capitals and ascenders high in any line box:
/// measured on screen, their middle sat 1.5px above the icon's. An offset rather than padding, so the box keeps its
/// height and the icon its place.
const TEXT_DROP: f32 = 1.5;
/// A notice, hidden until something is written to it.
pub fn notice() -> impl Scene {
    bsn! {
        Notice
        Node {
            display: { Display::None },
            width: { percent(100) },
            align_items: { AlignItems::FlexStart },
            column_gap: { px(space::ROWS) },
            padding: { UiRect::all(px(space::CONTROL_INSET)) },
            border: { UiRect::all(px(stroke::HAIRLINE)) },
            border_radius: { BorderRadius::all(px(radius::CONTROL)) },
        }
        BackgroundTone({ Tone::Info.subtle() })
        BorderTone({ Tone::Info.token() })
        Children [
            @icon_text(Icon::Info) NoticeIcon TextTone({ Tone::Info.token() }) LineHeight::Px(LINE_HEIGHT)
            --
            @text(String::new(), size::SECONDARY)
            NoticeText
            LineHeight::Px(LINE_HEIGHT)
            Node {
                flex_grow: { 1.0_f32 },
                min_width: { px(0) },
                top: { px(TEXT_DROP) },
            }
        ]
    }
}

/// Draw each notice from what it says: shown or hidden, its words, and its tone's icon and tokens.
#[allow(clippy::type_complexity)]
pub(crate) fn sync_notices(
    notices: Query<(Entity, Ref<Notice>, &Children, &mut Node, &BorderTone)>,
    mut icons: Query<(Entity, &mut Text), (With<NoticeIcon>, Without<NoticeText>)>,
    mut texts: Query<&mut Text, (With<NoticeText>, Without<NoticeIcon>)>,
    mut commands: Commands,
) {
    for (entity, notice, children, node, border) in notices {
        if !notice.is_changed() {
            continue;
        }
        patch_node(node, |node| node.display = display(!notice.message.is_empty()));
        let tone = notice.tone.token();
        if border.0 != tone {
            commands.entity(entity).insert((BackgroundTone(notice.tone.subtle()), BorderTone(tone.clone())));
        }
        for &child in children {
            if let Ok((icon, glyph)) = icons.get_mut(child) {
                set_text(glyph, notice.tone.icon().glyph());
                commands.entity(icon).insert(TextTone(tone.clone()));
            }
            if let Ok(text) = texts.get_mut(child) {
                set_text(text, &notice.message);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tone_has_its_own_icon_and_color() {
        let theme = crate::theme::Theme::new();
        let tones = [Tone::Info, Tone::Success, Tone::Warning, Tone::Error];
        for (i, a) in tones.iter().enumerate() {
            for b in &tones[i + 1..] {
                assert_ne!(a.icon(), b.icon());
                assert_ne!(theme.color(&a.token()), theme.color(&b.token()));
                assert_ne!(theme.color(&a.subtle()), theme.color(&b.subtle()));
            }
        }
    }

    #[test]
    fn a_notice_shows_only_while_it_has_something_to_say() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .add_systems(Update, sync_notices);
        let notice = app.world_mut().commands().spawn_scene(notice()).id();
        app.world_mut().flush();
        app.update();
        assert_eq!(app.world().get::<Node>(notice).unwrap().display, Display::None);

        app.world_mut().entity_mut(notice).insert(Notice::new(Tone::Error, "couldn't open it"));
        app.update();
        assert_eq!(app.world().get::<Node>(notice).unwrap().display, Display::Flex);
        let mut texts = app.world_mut().query_filtered::<&Text, With<NoticeText>>();
        assert_eq!(texts.single(app.world()).unwrap().0, "couldn't open it");
        assert_eq!(app.world().get::<BackgroundTone>(notice).unwrap().0, token::STATUS_DANGER_SUBTLE);
    }
}
