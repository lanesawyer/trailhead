//! A button that opens a URL in the system's browser.
//!
//! Ported from bevy-data-explorer's `widgets/link.rs`.

use bevy::feathers::controls::{ButtonVariant, FeathersButton};

use crate::buttons::ButtonKind;
use bevy::prelude::*;
use bevy::ui_widgets::Activate;

use crate::buttons::{button_text, tool_button};
use crate::icons::{Icon, button_icon};
use crate::spacing::space;

/// The URL a [`link_button`] opens.
#[derive(Component, Clone, Default)]
pub struct ExternalLink {
    pub url: String,
}

/// A link, drawn as a button with `icon` beside `text`. `Plain` shows no background until hovered, which is what reads
/// as a link in running text.
pub fn link_button(icon: Icon, text: impl Into<String>, url: impl Into<String>, kind: ButtonKind) -> impl Scene {
    let (text, url) = (text.into(), url.into());
    bsn! {
        @FeathersButton {
            @variant: { ButtonVariant::from(kind) },
            @caption: { bsn_list! {@button_icon(icon) -- @button_text(text)} }
        }
        Node { column_gap: { px(space::ICON_LABEL) } }
        ExternalLink { url: { url } }
        on(on_link_pressed)
    }
}

/// A link drawn as an icon-only [`tool_button`], with `hint` under the pointer: for a row of tools, a card's say.
pub fn link_tool_button(icon: Icon, hint: impl Into<String>, url: impl Into<String>, kind: ButtonKind) -> impl Scene {
    let url = url.into();
    bsn! {
        @tool_button(icon, hint, kind)
        ExternalLink { url: { url } }
        on(on_link_pressed)
    }
}

fn on_link_pressed(activate: On<Activate>, links: Query<&ExternalLink>) {
    let Ok(link) = links.get(activate.entity) else {
        return;
    };
    // Detached: a browser that isn't already running would otherwise hold the frame until it finished starting.
    match open::that_detached(&link.url) {
        Ok(()) => info!("opened {}", link.url),
        Err(error) => warn!("could not open {}: {error}", link.url),
    }
}
