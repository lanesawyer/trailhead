//! A message in a conversation, as a messaging app shows one: a rounded box against the end edge for what the reader
//! wrote and the start edge for everyone else's, each side in its own color, so whose is whose reads at a glance.
//!
//! Put it in a column (its parent lays its children out top to bottom) and spawn the message's lines as its children,
//! a [`bubble_head`] first. A message that is only a head and words, kept in step with what it shows, is a
//! [`message`], changed in place with [`MessageEdit`].

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use super::{BackgroundTone, Icon, TextTone, Token, icon_text, radius, size, space, text_dim, text_toned, token};

/// Whose a [`bubble`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Someone else's: against the start edge.
    Theirs,
    /// The reader's own: against the end edge.
    Mine,
}

/// The widest a bubble grows, as a share of its column, so the two sides stay apart.
const MAX_WIDTH_PERCENT: f32 = 80.0;

/// A message box on `side`.
pub fn bubble(side: Side) -> impl Scene {
    let (align, tone) = match side {
        Side::Theirs => (AlignSelf::FlexStart, token::SURFACE_RAISED),
        Side::Mine => (AlignSelf::FlexEnd, token::FILL_ACCENT),
    };
    bsn! {
        Node {
            flex_direction: FlexDirection::Column,
            row_gap: { px(space::STACKED) },
            padding: { UiRect::all(px(space::CONTROL_INSET)) },
            border_radius: { BorderRadius::all(px(radius::PANEL)) },
            max_width: { percent(MAX_WIDTH_PERCENT) },
            align_self: { align },
        }
        BackgroundTone({ tone })
    }
}

/// A bubble's first line, dim and small: `icon` for what kind of message it is, then `words` about it (who wrote it,
/// when).
pub fn bubble_head(icon: Icon, words: impl Into<String>) -> impl Scene {
    let words = words.into();
    bsn! {
        Node { align_items: AlignItems::Center, column_gap: { px(space::ICON_LABEL) } }
        Children [
                @icon_text(icon) TextTone({ token::TEXT_DIM }) MessagePart::Icon
            -- @text_dim(words, size::SECONDARY) MessagePart::Head
        ]
    }
}

/// A part of a [`message`] (and of any [`bubble_head`]) that [`MessageEdit`] changes.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MessagePart {
    /// The head's icon.
    #[default]
    Icon,
    /// The head's words.
    Head,
    /// What the message says.
    Body,
}

/// A whole message in a conversation: a [`bubble`] on `side`, headed by `icon` and `head` (who wrote it, when), saying
/// `body` in `tone` ([`token::TEXT`], or a status's for one that waits on the reader). Change it in place with
/// [`MessageEdit`]; spawn more lines (a button row) as its children after it has spawned.
pub fn message(side: Side, icon: Icon, head: impl Into<String>, body: impl Into<String>, tone: Token) -> impl Scene {
    let (head, body) = (head.into(), body.into());
    bsn! {
        @bubble(side)
        Children [
                @bubble_head(icon, head)
            -- @text_toned(body, size::BODY, tone) MessagePart::Body
        ]
    }
}

/// Changes a spawned [`message`] in place, writing only what differs.
#[derive(SystemParam)]
pub struct MessageEdit<'w, 's> {
    parts: Query<'w, 's, &'static MessagePart>,
    children: Query<'w, 's, &'static Children>,
    texts: Query<'w, 's, &'static mut Text>,
    tones: Query<'w, 's, &'static TextTone>,
    commands: Commands<'w, 's>,
}

impl MessageEdit<'_, '_> {
    /// Shows `icon`, `head` and `body` in `tone` on `message`.
    pub fn set(&mut self, message: Entity, icon: Icon, head: &str, body: &str, tone: &Token) {
        let found: Vec<(Entity, MessagePart)> =
            self.children.iter_descendants(message).filter_map(|e| self.parts.get(e).ok().map(|p| (e, *p))).collect();
        for (entity, part) in found {
            let words = match part {
                MessagePart::Icon => icon.glyph(),
                MessagePart::Head => head,
                MessagePart::Body => body,
            };
            if let Ok(text) = self.texts.get_mut(entity) {
                super::set_text(text, words);
            }
            if part == MessagePart::Body && self.tones.get(entity).is_ok_and(|t| t.0 != *tone) {
                self.commands.entity(entity).insert(TextTone(tone.clone()));
            }
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

    /// A part of a message, its words and its tone.
    type Part = (MessagePart, String, Option<Token>);

    /// Every entity under `root`, and the text of each part of the message with it.
    fn parts(app: &mut App, root: Entity) -> (Vec<Entity>, Vec<Part>) {
        let world = app.world_mut();
        let mut found = vec![root];
        let mut at = 0;
        while at < found.len() {
            if let Some(children) = world.get::<Children>(found[at]) {
                found.extend(children.iter());
            }
            at += 1;
        }
        let said = found
            .iter()
            .filter_map(|e| {
                let part = *world.get::<MessagePart>(*e)?;
                Some((part, world.get::<Text>(*e)?.0.clone(), world.get::<TextTone>(*e).map(|t| t.0.clone())))
            })
            .collect();
        (found, said)
    }

    #[test]
    fn a_message_is_changed_in_place() {
        let mut app = app();
        let scene = message(Side::Theirs, Icon::Help, "planner · now", "which db?", token::STATUS_WARN);
        let said = app.world_mut().commands().spawn_scene(scene).id();
        app.world_mut().flush();
        app.update();
        let (before, parts_before) = parts(&mut app, said);
        assert_eq!(parts_before.len(), 3);
        assert!(parts_before.contains(&(MessagePart::Body, "which db?".into(), Some(token::STATUS_WARN))));
        app.world_mut()
            .run_system_once(move |mut edit: MessageEdit| {
                edit.set(said, Icon::Comment, "planner · later", "sqlite, then", &token::TEXT);
            })
            .unwrap();
        app.update();
        let (after, parts_after) = parts(&mut app, said);
        assert_eq!(after, before, "the same entities");
        assert!(parts_after.iter().any(|(p, words, _)| *p == MessagePart::Head && words == "planner · later"));
        assert!(parts_after.contains(&(MessagePart::Body, "sqlite, then".into(), Some(token::TEXT))));
        assert!(parts_after.contains(&(MessagePart::Icon, Icon::Comment.glyph().into(), Some(token::TEXT_DIM))));
    }
}
