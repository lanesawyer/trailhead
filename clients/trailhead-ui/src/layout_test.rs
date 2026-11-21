//! A headless app that lays UI nodes out as Bevy's own layout tests do, for the kit's tests of what measures layout.

use bevy::app::{HierarchyPropagatePlugin, PropagateSet};
use bevy::camera::{ComputedCameraValues, RenderTargetInfo, Viewport};
use bevy::prelude::*;
use bevy::ui::update::propagate_ui_target_cameras;
use bevy::ui::{ComputedUiRenderTargetInfo, ComputedUiTargetCamera, ui_layout_system};

/// The screen the app lays out on, in physical pixels.
pub(crate) const SCREEN: UVec2 = UVec2::new(800, 600);

/// An app that lays out UI nodes headless on a [`SCREEN`] of `scale` physical pixels to a logical one, with no
/// window, the UI drawn at `ui_scale` (Bevy's `UiScale`, which [`TextSize`](crate::TextSize) sets).
pub(crate) fn layout_app(scale: f32, ui_scale: f32) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .add_plugins(HierarchyPropagatePlugin::<ComputedUiTargetCamera>::new(PostUpdate))
        .add_plugins(HierarchyPropagatePlugin::<ComputedUiRenderTargetInfo>::new(PostUpdate))
        .insert_resource(UiScale(ui_scale))
        .init_resource::<bevy::ui::ui_surface::UiSurface>()
        .init_resource::<bevy::text::FontCx>()
        .init_resource::<bevy::text::RemSize>()
        .add_systems(PostUpdate, (ApplyDeferred, propagate_ui_target_cameras, ui_layout_system).chain())
        .configure_sets(
            PostUpdate,
            PropagateSet::<ComputedUiTargetCamera>::default()
                .after(propagate_ui_target_cameras)
                .before(ui_layout_system),
        )
        .configure_sets(
            PostUpdate,
            PropagateSet::<ComputedUiRenderTargetInfo>::default()
                .after(propagate_ui_target_cameras)
                .before(ui_layout_system),
        );
    app.world_mut().spawn((
        Camera2d,
        Camera {
            computed: ComputedCameraValues {
                target_info: Some(RenderTargetInfo { physical_size: SCREEN, scale_factor: scale }),
                ..default()
            },
            viewport: Some(Viewport { physical_size: SCREEN, ..default() }),
            ..default()
        },
    ));
    app
}
