//! A toggle switch with its label after it, over feathers' `FeathersToggleSwitch`: for a setting that is on or off
//! (show archived projects).
//!
//! The switch keeps its own state: a press flips it and sends `ValueChange<bool>` from the switch (from
//! `bevy::ui_widgets`) with the new value, which the caller observes for what the setting does.

use bevy::feathers::controls::FeathersToggleSwitch;
use bevy::prelude::*;
use bevy::ui::Checked;
use bevy::ui_widgets::checkbox_self_update;

use super::{size, space, text};

/// A switch, on when `checked`, with `label` after it. `own` goes on the switch, where the caller puts what finds it.
pub fn toggle_switch(label: impl Into<String>, checked: bool, own: impl Scene) -> impl Scene {
    let label = label.into();
    let switch: Box<dyn Scene> = if checked {
        Box::new(bsn! { @FeathersToggleSwitch Checked on(checkbox_self_update) @{own} })
    } else {
        Box::new(bsn! { @FeathersToggleSwitch on(checkbox_self_update) @{own} })
    };
    bsn! {
        Node { align_items: AlignItems::Center, column_gap: { px(space::ICON_LABEL) }, flex_shrink: 0.0 }
        Children [
            @{switch}
            --
            @text(label, size::BODY)
        ]
    }
}
