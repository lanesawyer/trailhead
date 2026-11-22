//! A round check: a ring that is pressed to mark something done (a todo's "complete this"), over `bevy_ui_widgets`'
//! headless `Button`, so a press, Enter or Space sends `Activate` from it as from any button.
//!
//! It is round all through: an empty ring in a round target, with no square background under the pointer as a tool
//! button has. Under the pointer the ring draws the check it would get, in the accent, to say a press completes it;
//! checked (or held down) the ring is filled with the accent and the check sits in it. It does not check itself: the
//! caller answers `Activate` and sets [`CheckCircle::checked`] from what it keeps, so a row updated in place shows
//! the state its data has. Disabled with `InteractionDisabled`, it neither answers the pointer nor sends `Activate`.
//!
//! Not feathers' checkbox, which is square, carries its label and flips itself on a press.

use bevy::feathers::focus::FocusIndicator;
use bevy::input_focus::tab_navigation::TabIndex;
use bevy::picking::cursor::EntityCursor;
use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::text::FontSize;
use bevy::ui::{InteractionDisabled, Pressed};
use bevy::ui_widgets::Button;
use bevy::window::SystemCursorIcon;

use crate::Hint;
use crate::buttons::BUTTON_TEXT;
use crate::icons::{Icon, icon_text};
use crate::patch::{display, patch_node};
use crate::spacing::{extent, stroke};
use crate::text::size;
use crate::theme::{BackgroundTone, BorderTone, TextTone, Token, token};

/// A round check, `checked` or not, with `hint` shown while it is hovered (what a press does: "Complete").
pub fn check_circle(checked: bool, hint: impl Into<String>) -> impl Scene {
    let hint = hint.into();
    let rest = look(checked, false, false, false);
    let mark_tone = rest.mark.clone().unwrap_or(token::FILL_ACCENT);
    bsn! {
        Node {
            width: { px(extent::CONTROL_HEIGHT) },
            height: { px(extent::CONTROL_HEIGHT) },
            flex_shrink: 0.0,
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            border_radius: { BorderRadius::MAX },
        }
        Button
        CheckCircle { checked: { checked } }
        Hovered
        TabIndex(0)
        FocusIndicator
        EntityCursor::System(SystemCursorIcon::Pointer)
        Hint({ hint })
        Children [
            Node {
                width: { px(extent::CHECK_SIZE) },
                height: { px(extent::CHECK_SIZE) },
                border: { UiRect::all(px(stroke::RING)) },
                border_radius: { BorderRadius::MAX },
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
            }
            CheckRing
            BorderTone({ rest.ring })
            BackgroundTone({ rest.fill })
            Children [
                @icon_text(Icon::Check)
                CheckMark
                TextFont { font_size: { FontSize::from(size::SMALL) } }
                TextTone({ mark_tone })
                Node { display: { display(rest.mark.is_some()) } }
            ]
        ]
    }
}

/// Whether a [`check_circle`] is checked. Set it (with `set_if_neq`) from what the caller keeps; the kit repaints it.
#[derive(Component, Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct CheckCircle {
    pub checked: bool,
}

/// A check's ring, which the kit paints.
#[derive(Component, Clone, Copy, Default, Debug)]
pub struct CheckRing;

/// The check inside a ring, shown under the pointer and while checked.
#[derive(Component, Clone, Copy, Default, Debug)]
pub struct CheckMark;

/// How a check is drawn: its ring's border and fill, and its mark's tone when the mark shows.
#[derive(Debug, Clone, PartialEq)]
struct Look {
    ring: Token,
    fill: Token,
    mark: Option<Token>,
}

/// How a check that is `checked`, under the pointer (`hovered`), held down (`pressed`) or `disabled` is drawn.
#[expect(clippy::fn_params_excessive_bools, reason = "the four states a check is drawn from, each its own flag")]
fn look(checked: bool, hovered: bool, pressed: bool, disabled: bool) -> Look {
    if disabled {
        return Look {
            ring: token::TEXT_DISABLED,
            fill: token::FILL_ITEM,
            mark: checked.then_some(token::TEXT_DISABLED),
        };
    }
    if checked || pressed {
        let accent = if hovered { token::FILL_ACCENT_HOVER } else { token::FILL_ACCENT };
        return Look { ring: accent.clone(), fill: accent, mark: Some(BUTTON_TEXT) };
    }
    if hovered {
        // The check a press would give it, drawn in the ring.
        return Look { ring: token::FILL_ACCENT, fill: token::FILL_ITEM, mark: Some(token::FILL_ACCENT) };
    }
    Look { ring: token::TEXT_DIM, fill: token::FILL_ITEM, mark: None }
}

/// Paints each check as its state, the pointer and `InteractionDisabled` say, writing only what differs.
#[expect(clippy::type_complexity)]
pub(crate) fn paint_check_circles(
    checks: Query<(Entity, &CheckCircle, &Hovered, Has<Pressed>, Has<InteractionDisabled>)>,
    children: Query<&Children>,
    rings: Query<(&BorderTone, &BackgroundTone), With<CheckRing>>,
    marks: Query<&TextTone, With<CheckMark>>,
    mut nodes: Query<&mut Node>,
    mut commands: Commands,
) {
    for (check, state, hovered, pressed, disabled) in &checks {
        let wanted = look(state.checked, hovered.0, pressed, disabled);
        for part in children.iter_descendants(check) {
            if let Ok((BorderTone(ring), BackgroundTone(fill))) = rings.get(part) {
                if *ring != wanted.ring {
                    commands.entity(part).insert(BorderTone(wanted.ring.clone()));
                }
                if *fill != wanted.fill {
                    commands.entity(part).insert(BackgroundTone(wanted.fill.clone()));
                }
            } else if let Ok(TextTone(tone)) = marks.get(part) {
                if let Some(mark) = wanted.mark.as_ref().filter(|mark| *mark != tone) {
                    commands.entity(part).insert(TextTone(mark.clone()));
                }
                if let Ok(node) = nodes.get_mut(part) {
                    patch_node(node, |n| n.display = display(wanted.mark.is_some()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use bevy::input::keyboard::Key;
    use bevy::ui_widgets::{Activate, ButtonPlugin};

    use super::*;
    use crate::test_support::press_key;

    #[derive(Resource, Default)]
    struct Activated(Vec<Entity>);

    /// An app that paints checks and notes every `Activate`, with one check spawned, `checked` or not.
    fn app(checked: bool) -> (App, Entity) {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin, ButtonPlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .init_resource::<Activated>()
            .add_observer(|a: On<Activate>, mut seen: ResMut<Activated>| seen.0.push(a.entity))
            .add_systems(Update, paint_check_circles);
        let check = app.world_mut().commands().spawn_scene(check_circle(checked, "Complete")).id();
        app.world_mut().flush();
        app.update();
        app.update();
        (app, check)
    }

    /// The tone of the app's one mark when it shows, and its ring's fill.
    fn drawn(app: &mut App) -> (Option<Token>, Token) {
        let world = app.world_mut();
        let (tone, node) = world.query_filtered::<(&TextTone, &Node), With<CheckMark>>().single(world).unwrap();
        let mark = (node.display != Display::None).then(|| tone.0.clone());
        let fill = world.query_filtered::<&BackgroundTone, With<CheckRing>>().single(world).unwrap().0.clone();
        (mark, fill)
    }

    fn hover(app: &mut App, check: Entity, over: bool) {
        app.world_mut().entity_mut(check).insert(Hovered(over));
        app.update();
    }

    #[test]
    fn hovering_draws_the_check_and_leaving_takes_it_away() {
        let (mut app, check) = app(false);
        assert_eq!(drawn(&mut app), (None, token::FILL_ITEM), "an empty ring");
        hover(&mut app, check, true);
        assert_eq!(drawn(&mut app), (Some(token::FILL_ACCENT), token::FILL_ITEM), "the check it would get");
        hover(&mut app, check, false);
        assert_eq!(drawn(&mut app), (None, token::FILL_ITEM));
    }

    #[test]
    fn checked_it_keeps_its_check_filled_with_or_without_the_pointer() {
        let (mut app, check) = app(false);
        app.world_mut().get_mut::<CheckCircle>(check).unwrap().checked = true;
        app.update();
        assert_eq!(drawn(&mut app), (Some(BUTTON_TEXT), token::FILL_ACCENT));
        hover(&mut app, check, true);
        hover(&mut app, check, false);
        assert_eq!(drawn(&mut app), (Some(BUTTON_TEXT), token::FILL_ACCENT));
        app.world_mut().get_mut::<CheckCircle>(check).unwrap().checked = false;
        app.update();
        assert_eq!(drawn(&mut app), (None, token::FILL_ITEM), "unchecked in place");
    }

    #[test]
    fn spawned_checked_it_shows_its_check() {
        let (mut app, _) = app(true);
        assert_eq!(drawn(&mut app), (Some(BUTTON_TEXT), token::FILL_ACCENT));
    }

    #[test]
    fn a_press_sends_activate_and_a_disabled_one_does_not_and_shows_no_check() {
        let (mut app, check) = app(false);
        press_key(app.world_mut(), check, KeyCode::Enter, Key::Enter);
        app.world_mut().flush();
        assert_eq!(app.world().resource::<Activated>().0, [check]);
        app.world_mut().entity_mut(check).insert(InteractionDisabled);
        press_key(app.world_mut(), check, KeyCode::Space, Key::Space);
        app.world_mut().flush();
        assert_eq!(app.world().resource::<Activated>().0, [check], "disabled, nothing more");
        hover(&mut app, check, true);
        assert_eq!(drawn(&mut app), (None, token::FILL_ITEM), "no preview while disabled");
    }

    #[test]
    fn held_down_it_shows_checked_and_disabled_it_is_dim() {
        assert_eq!(look(false, true, true, false), look(true, true, false, false));
        let off = look(true, true, false, true);
        assert_eq!((off.ring, off.mark), (token::TEXT_DISABLED, Some(token::TEXT_DISABLED)));
    }

    #[test]
    fn it_is_a_round_focusable_button_with_its_hint() {
        let (app, check) = app(false);
        let world = app.world();
        assert!(world.get::<Button>(check).is_some());
        assert_eq!(world.get::<TabIndex>(check), Some(&TabIndex(0)));
        assert_eq!(world.get::<Hint>(check).map(|h| h.0.as_str()), Some("Complete"));
        let node = world.get::<Node>(check).unwrap();
        assert_eq!(
            (node.width, node.height, node.border_radius),
            (px(extent::CONTROL_HEIGHT), node.width, BorderRadius::MAX)
        );
        assert!(world.get::<BackgroundTone>(check).is_none(), "no square under the pointer");
    }
}
