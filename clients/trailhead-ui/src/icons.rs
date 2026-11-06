//! Icons, drawn as glyphs from the Lucide icon font (ported from bevy-data-explorer).
//!
//! A font rather than images: glyphs are rasterised at the physical size, so they stay sharp at any scale, and they
//! take the text color a button gives its caption, so hover and disabled repaint them for free. Feathers' own icons
//! are Lucide drawings, so the two sets match.
//!
//! The whole font is embedded (`assets/lucide.ttf`, licensed in `assets/LUCIDE-LICENSE`), so adding an icon is one
//! variant here. The codepoints come from the font's own glyph names (the `cmap` and `post` tables); they aren't
//! stable across Lucide releases, so take a new one from this same font file.

use bevy::app::PropagateOver;
use bevy::feathers::theme::ThemeTextColor;
use bevy::feathers::tokens;
use bevy::prelude::*;
use bevy::text::{FontSourceTemplate, FontWeight};

const FONT: &str = "embedded://trailhead_ui/assets/lucide.ttf";
const SIZE: f32 = 15.0;

/// Embeds the icon font, at the path [`icon_text`] loads it from.
pub struct IconsPlugin;

impl Plugin for IconsPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "assets/lucide.ttf");
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Icon {
    /// A row of rows: the board.
    Board,
    Bot,
    Check,
    ChevronDown,
    ChevronRight,
    ChevronUp,
    Circle,
    CircleAlert,
    CircleCheck,
    CircleX,
    Comment,
    Copy,
    Ellipsis,
    ExternalLink,
    Filter,
    Flag,
    Help,
    History,
    Info,
    /// Open the inspector on something.
    Inspect,
    LogIn,
    Map,
    Pencil,
    Plus,
    PullRequest,
    Refresh,
    Release,
    Search,
    Send,
    Settings,
    Shield,
    Stop,
    TriangleAlert,
    Undo,
    User,
    Wifi,
    WifiOff,
    X,
}

impl Icon {
    #[cfg(test)]
    const ALL: [Icon; 38] = [
        Icon::Board,
        Icon::Bot,
        Icon::Check,
        Icon::ChevronDown,
        Icon::ChevronRight,
        Icon::ChevronUp,
        Icon::Circle,
        Icon::CircleAlert,
        Icon::CircleCheck,
        Icon::CircleX,
        Icon::Comment,
        Icon::Copy,
        Icon::Ellipsis,
        Icon::ExternalLink,
        Icon::Filter,
        Icon::Flag,
        Icon::Help,
        Icon::History,
        Icon::Info,
        Icon::Inspect,
        Icon::LogIn,
        Icon::Map,
        Icon::Pencil,
        Icon::Plus,
        Icon::PullRequest,
        Icon::Refresh,
        Icon::Release,
        Icon::Search,
        Icon::Send,
        Icon::Settings,
        Icon::Shield,
        Icon::Stop,
        Icon::TriangleAlert,
        Icon::Undo,
        Icon::User,
        Icon::Wifi,
        Icon::WifiOff,
        Icon::X,
    ];

    /// The icon's character in the Lucide font.
    pub fn glyph(self) -> &'static str {
        match self {
            Icon::Board => "\u{e4dc}",         // kanban
            Icon::Bot => "\u{e1bb}",           // bot
            Icon::Check => "\u{e06c}",         // check
            Icon::ChevronDown => "\u{e06d}",   // chevron-down
            Icon::ChevronRight => "\u{e06f}",  // chevron-right
            Icon::ChevronUp => "\u{e070}",     // chevron-up
            Icon::Circle => "\u{e076}",        // circle
            Icon::CircleAlert => "\u{e077}",   // circle-alert
            Icon::CircleCheck => "\u{e226}",   // circle-check
            Icon::CircleX => "\u{e084}",       // circle-x
            Icon::Comment => "\u{e117}",       // message-square
            Icon::Copy => "\u{e09e}",          // copy
            Icon::Ellipsis => "\u{e0b6}",      // ellipsis
            Icon::ExternalLink => "\u{e0b9}",  // external-link
            Icon::Filter => "\u{e0dc}",        // filter
            Icon::Flag => "\u{e0d1}",          // flag
            Icon::Help => "\u{e082}",          // circle-help
            Icon::History => "\u{e1f5}",       // history
            Icon::Info => "\u{e0f9}",          // info
            Icon::Inspect => "\u{e434}",       // panel-right-open
            Icon::LogIn => "\u{e10d}",         // log-in
            Icon::Map => "\u{e110}",           // map
            Icon::Pencil => "\u{e1f9}",        // pencil
            Icon::Plus => "\u{e13d}",          // plus
            Icon::PullRequest => "\u{e0e5}",   // git-pull-request
            Icon::Refresh => "\u{e145}",       // refresh-cw
            Icon::Release => "\u{e10c}",       // lock-open
            Icon::Search => "\u{e151}",        // search
            Icon::Send => "\u{e152}",          // send
            Icon::Settings => "\u{e154}",      // settings
            Icon::Shield => "\u{e158}",        // shield
            Icon::Stop => "\u{e051}",          // ban
            Icon::TriangleAlert => "\u{e193}", // triangle-alert
            Icon::Undo => "\u{e2a1}",          // undo-2
            Icon::User => "\u{e19f}",          // user
            Icon::Wifi => "\u{e1ae}",          // wifi
            Icon::WifiOff => "\u{e1af}",       // wifi-off
            Icon::X => "\u{e1b2}",             // x
        }
    }
}

/// An icon with no color of its own, for a caller to give one.
///
/// `PropagateOver` for the same reason feathers' `label` carries it: a button propagates its own font to the text
/// under it, which has no glyph at these codepoints.
pub fn icon_text(icon: Icon) -> impl Scene {
    bsn! {
        Text({ icon.glyph().to_string() })
        TextFont {
            font: FontSourceTemplate::Handle(FONT),
            font_size: FontSize::Px(SIZE),
            weight: FontWeight::NORMAL,
        }
        PropagateOver<TextFont>
    }
}

/// An icon as a button's caption, in the button's text color.
pub fn button_icon(icon: Icon) -> impl Scene {
    bsn! {
        @icon_text(icon)
        ThemeTextColor({ tokens::BUTTON_TEXT })
    }
}

#[cfg(test)]
mod tests {
    use bevy::asset::LoadState;

    use super::*;

    #[test]
    fn the_embedded_font_loads_from_the_path_icons_name() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), bevy::text::TextPlugin, IconsPlugin));
        let font: Handle<Font> = app.world().resource::<AssetServer>().load(FONT);
        for _ in 0..200 {
            app.update();
            match app.world().resource::<AssetServer>().load_state(&font) {
                LoadState::Loaded => return,
                LoadState::Failed(e) => panic!("{FONT} didn't load: {e}"),
                _ => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
        panic!("{FONT} still loading after 2 s");
    }

    #[test]
    fn every_icon_is_its_own_glyph_in_the_private_use_area() {
        let glyphs: std::collections::HashSet<&str> = Icon::ALL.iter().map(|icon| icon.glyph()).collect();
        assert_eq!(glyphs.len(), Icon::ALL.len());
        for glyph in glyphs {
            let c = glyph.chars().next().unwrap();
            assert!(('\u{e000}'..='\u{f8ff}').contains(&c));
        }
    }
}
