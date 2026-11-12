//! The heading of a panel or modal: the title on the left, wrapping when long, and a close X pinned top-right.
//!
//! The X's top lines up with the title's top, so a title that wraps to a second line leaves the X in the corner.
//! The header says nothing about what closing does: the X sends feathers' `RequestClose` from itself, which climbs
//! to whatever owns the panel (the modal's root, the inspector) and is answered there.
//!
//! [`PanelTools`] holds the X; a panel puts its own header controls (a modal's arrows and menu) in it before the X.

use bevy::prelude::*;
use bevy::ui_widgets::{Activate, RequestClose};

use super::{ButtonKind, Icon, size, space, text, tool_button};

/// Marks the title's text, so a title that changes is set in place.
#[derive(Component, Default, Clone)]
pub struct PanelTitle;

/// The row at the header's right, ending with the X: insert a panel's own controls at its start.
#[derive(Component, Default, Clone)]
pub struct PanelTools;

/// A panel's title row, with the close X at its right.
pub fn panel_header(title: impl Into<String>) -> impl Scene {
    let title = title.into();
    let close = bsn_list! {
        @tool_button(Icon::X, "Close", ButtonKind::Plain)
        Node { flex_shrink: 0.0 }
        on(|activate: On<Activate>, mut commands: Commands| {
            commands.trigger(RequestClose { source: activate.event_target() });
        })
    };
    bsn! {
        Node {
            align_items: AlignItems::FlexStart,
            justify_content: JustifyContent::SpaceBetween,
            column_gap: { px(space::CONTROLS) },
            flex_shrink: 0.0,
        }
        Children [
            @text(title, size::HEADING) PanelTitle Node { flex_grow: 1.0, flex_shrink: 1.0, min_width: px(0) }
            --
            Node { align_items: AlignItems::Center, column_gap: { px(space::CONTROLS) }, flex_shrink: 0.0 }
            PanelTools
            Children [ {close} ]
        ]
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>();
        app
    }

    fn settle(app: &mut App) {
        app.world_mut().flush();
        for _ in 0..3 {
            app.update();
        }
    }

    #[test]
    fn the_header_has_a_title_and_a_tools_row_holding_the_x() {
        let mut app = app();
        let header = app.world_mut().commands().spawn_scene(panel_header("Inspector")).id();
        settle(&mut app);
        let world = app.world_mut();
        let titles: Vec<String> =
            world.query_filtered::<&Text, With<PanelTitle>>().iter(world).map(|t| t.0.clone()).collect();
        assert_eq!(titles, ["Inspector"]);
        let tools = world.query_filtered::<Entity, With<PanelTools>>().single(world).unwrap();
        assert_eq!(world.get::<ChildOf>(tools).map(|c| c.parent()), Some(header));
        assert_eq!(world.get::<Children>(tools).map_or(0, |c| c.len()), 1, "the X is the only tool");
    }

    #[test]
    fn activating_the_x_sends_a_request_to_close_from_the_x_up_to_the_owner() {
        let mut app = app();
        let owner = app.world_mut().spawn_empty().id();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        app.world_mut().entity_mut(owner).observe(move |close: On<RequestClose>| {
            sink.lock().unwrap().push((close.source, close.original_event_target()));
        });
        let header = app.world_mut().commands().spawn_scene(panel_header("Inspector")).insert(ChildOf(owner)).id();
        settle(&mut app);
        let world = app.world_mut();
        let tools = world.query_filtered::<Entity, With<PanelTools>>().single(world).unwrap();
        let x = world.get::<Children>(tools).unwrap()[0];
        assert_ne!(x, header);
        world.trigger(Activate { entity: x });
        settle(&mut app);
        assert_eq!(*seen.lock().unwrap(), [(owner, x)], "the owner hears a request that started at the X");
    }
}
