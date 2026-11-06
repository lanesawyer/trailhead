//! The one modal every form and dialog uses (ported from bevy-data-explorer's `widgets/modal.rs`): feathers'
//! `FeathersDialog` on a backdrop covering the window, centered, with a title and a close X.
//!
//! - **Closing.** The X, Escape and a click outside all end as feathers' `RequestClose`, and [`ModalPlugin`] answers it
//!   by despawning the modal. That is the default for every modal, so no form can opt out by accident; one that can't
//!   close right now (a save in flight) says so with [`ModalLocked`] and stays. [`ModalClosed`] is sent first, for the
//!   owner to drop what it holds.
//! - **Focus.** Feathers' dialog takes focus when it opens and keeps Tab inside it.
//! - **Never taller than the window.** The frame is capped to [`MAX_HEIGHT_VH`] of the window and only the body
//!   scrolls, with the kit's scrollbar, so the footer (Save, Cancel) stays in view.
//! - **Use.** A marker type implements [`Modal`] (title, width); [`open_modal`] spawns it. Once it has spawned,
//!   [`ModalSlots`] on the root names the [`ModalBody`] to fill and the [`ModalFooter`] for its buttons. Close it by
//!   despawning the root. Layering is [`super::layer::MODAL`].

use bevy::feathers::controls::{FeathersDialog, FeathersDialogClose};
use bevy::prelude::*;
use bevy::ui_widgets::{ControlOrientation, RequestClose};

use super::{SCROLLBAR_WIDTH, ScrollDown, extent, layer, scrollbar, size, space, text};

/// How much of the window's height the modal may take, with the barrier's margin around it.
pub const MAX_HEIGHT_VH: f32 = 80.0;
/// How much of the window's width, for a window narrower than the modal.
pub const MAX_WIDTH_VW: f32 = 85.0;

/// A kind of modal: a marker component that says what its title and width are.
pub trait Modal: Component {
    fn title(&self) -> String;
    fn width(&self) -> f32 {
        extent::MODAL_WIDTH
    }
}

/// The backdrop that covers the window, which every modal's entity is.
#[derive(Component, Clone, Default)]
pub struct ModalRoot;

/// The box inside the dialog that is capped to the window's height.
#[derive(Component, Clone, Default)]
pub struct ModalFrame;

/// Where the modal's content goes; it scrolls.
#[derive(Component, Clone, Default)]
pub struct ModalBody;

/// Where the modal's buttons go; it never scrolls away.
#[derive(Component, Clone, Default)]
pub struct ModalFooter;

/// Put on a modal's root while it can't close: a request to close it is ignored.
#[derive(Component, Clone, Default)]
pub struct ModalLocked;

/// On a modal's root once it has spawned: the nodes to fill.
#[derive(Component, Clone, Copy, Debug)]
pub struct ModalSlots {
    pub body: Entity,
    pub footer: Entity,
}

/// A modal is about to be despawned because it was asked to close.
#[derive(Event, Clone, Copy, Debug)]
pub struct ModalClosed {
    pub modal: Entity,
}

/// Opens `modal` over everything on the board, and returns its root.
pub fn open_modal<M: Modal>(commands: &mut Commands, modal: M) -> Entity {
    let (title, width) = (modal.title(), modal.width());
    commands
        .spawn_scene(bsn! {
            @FeathersDialog {
                @contents: { frame(title, width) },
            }
            GlobalZIndex({ layer::MODAL })
        })
        .insert((Name::new("modal"), ModalRoot, modal))
        .id()
}

fn frame(title: String, width: f32) -> Box<dyn SceneList> {
    Box::new(bsn_list! {
        Node {
            flex_direction: FlexDirection::Column,
            row_gap: { px(space::ROWS) },
            width: { px(width) },
            max_width: { vw(MAX_WIDTH_VW) },
            max_height: { vh(MAX_HEIGHT_VH) },
        }
        ModalFrame
        Children [
            Node { justify_content: JustifyContent::SpaceBetween, align_items: AlignItems::Center }
            Children [ @text(title, size::HEADING) -- @FeathersDialogClose ]
            --
            // The body's box may shrink below its content, so the body scrolls and the footer keeps its place.
            Node { flex_direction: FlexDirection::Column, min_height: px(0), flex_shrink: 1.0 }
            Children [
                Node {
                    flex_direction: FlexDirection::Column,
                    min_height: px(0),
                    row_gap: { px(space::ROWS) },
                    padding: { UiRect::right(px(SCROLLBAR_WIDTH + space::SCROLLBAR_GAP)) },
                    overflow: { Overflow::scroll_y() },
                }
                ModalBody
                ScrollDown
            ]
            --
            Node { column_gap: { px(space::CONTROLS) }, align_items: AlignItems::Center, flex_shrink: 0.0 }
            ModalFooter
        ]
    })
}

/// Despawns a modal asked to close, unless it is locked.
fn on_request_close(
    mut close: On<RequestClose>,
    roots: Query<Has<ModalLocked>, With<ModalRoot>>,
    mut commands: Commands,
) {
    let Ok(locked) = roots.get(close.event_target()) else { return };
    close.propagate(false);
    if !locked {
        let modal = close.event_target();
        commands.trigger(ModalClosed { modal });
        commands.entity(modal).despawn();
    }
}

/// Finds a new modal's body and footer, and gives the body its scrollbar.
fn link_slots(
    bodies: Query<Entity, Added<ModalBody>>,
    parents: Query<&ChildOf>,
    roots: Query<(), With<ModalRoot>>,
    footers: Query<Entity, With<ModalFooter>>,
    mut commands: Commands,
) {
    for body in &bodies {
        let Some(root) = parents.iter_ancestors(body).find(|e| roots.contains(*e)) else { continue };
        let Some(footer) = footers.iter().find(|f| parents.iter_ancestors(*f).any(|e| e == root)) else { continue };
        if let Ok(holder) = parents.get(body) {
            commands.queue_spawn_scene(scrollbar(body, ControlOrientation::Vertical)).insert(ChildOf(holder.parent()));
        }
        commands.entity(root).insert(ModalSlots { body, footer });
    }
}

/// The modal's behavior: closing and finding its slots.
pub struct ModalPlugin;

impl Plugin for ModalPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(on_request_close).add_systems(Update, link_slots);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[derive(Component)]
    struct Test;

    impl Modal for Test {
        fn title(&self) -> String {
            "Test".into()
        }
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .add_plugins(ModalPlugin);
        app
    }

    fn open(app: &mut App) -> Entity {
        let modal = open_modal(&mut app.world_mut().commands(), Test);
        app.world_mut().flush();
        for _ in 0..3 {
            app.update();
        }
        modal
    }

    fn count(app: &mut App) -> usize {
        app.world_mut().query_filtered::<(), With<ModalRoot>>().iter(app.world()).count()
    }

    #[test]
    fn opening_spawns_one_modal_with_its_slots_above_the_board() {
        let mut app = app();
        let modal = open(&mut app);
        assert_eq!(count(&mut app), 1);
        assert!(app.world().get::<Test>(modal).is_some());
        assert!(app.world().get::<ModalSlots>(modal).is_some());
        assert_eq!(app.world().get::<GlobalZIndex>(modal).map(|z| z.0), Some(layer::MODAL));
    }

    #[test]
    fn a_request_to_close_from_inside_despawns_it_and_says_so() {
        let mut app = app();
        let closed = Arc::new(AtomicUsize::new(0));
        let seen = closed.clone();
        app.add_observer(move |_: On<ModalClosed>| {
            seen.fetch_add(1, Ordering::SeqCst);
        });
        let modal = open(&mut app);
        // The close X is a button inside the dialog; its request climbs to the root.
        let slots = *app.world().get::<ModalSlots>(modal).unwrap();
        app.world_mut().trigger(RequestClose { source: slots.footer });
        app.update();
        assert_eq!(count(&mut app), 0);
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn escape_and_a_click_outside_ask_the_root_to_close_it() {
        let mut app = app();
        let modal = open(&mut app);
        // Feathers sends both from the backdrop, which is the root itself.
        app.world_mut().trigger(RequestClose { source: modal });
        app.update();
        assert_eq!(count(&mut app), 0);
    }

    #[test]
    fn a_locked_modal_stays() {
        let mut app = app();
        let modal = open(&mut app);
        app.world_mut().entity_mut(modal).insert(ModalLocked);
        app.world_mut().trigger(RequestClose { source: modal });
        app.update();
        assert_eq!(count(&mut app), 1);
    }

    #[test]
    fn the_body_scrolls_inside_a_frame_capped_to_the_window() {
        let mut app = app();
        let modal = open(&mut app);
        let slots = *app.world().get::<ModalSlots>(modal).unwrap();
        let body = app.world().get::<Node>(slots.body).unwrap();
        assert_eq!(body.overflow, Overflow::scroll_y());
        assert_eq!(body.min_height, px(0));
        assert!(app.world().get::<ScrollDown>(slots.body).is_some());
        let frame = app.world_mut().query_filtered::<&Node, With<ModalFrame>>().single(app.world()).unwrap();
        assert_eq!(frame.max_height, vh(MAX_HEIGHT_VH));
        // The footer is not inside the body, so it stays in view.
        assert!(!app.world().get::<ChildOf>(slots.footer).is_some_and(|c| c.parent() == slots.body));
        // And the body got its scrollbar.
        let bars = app.world_mut().query::<&bevy::ui_widgets::Scrollbar>().iter(app.world()).count();
        assert_eq!(bars, 1);
    }
}
