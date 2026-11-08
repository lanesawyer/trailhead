//! The colors the kit wears, in one place.
//!
//! Feathers' dark theme is the base. Its button hover is widened (as bevy-data-explorer found, feathers' own is too
//! faint to read as a response), and the kit's base palette is added as tokens: the surfaces panels sit on and the
//! tones for badges, hints and warnings. A view names a token, never a color, so the theme is one edit and a missing
//! token shows up as fuchsia rather than as a wrong color nobody notices.
//!
//! An app adds its own colors on top: declare its `ThemeToken`s, start from [`props`], [`paint`] each one, and insert
//! `UiTheme(props)` instead of [`theme`]. Lookout does this for its board, cards, priorities and status columns.

use bevy::feathers::dark_theme::create_dark_theme;
use bevy::feathers::palette;
use bevy::feathers::theme::{SemanticToken, ThemeProps, ThemeToken, UiTheme};
use bevy::feathers::tokens;
use bevy::prelude::*;

/// The kit's tokens. Feathers' (`bevy::feathers::tokens`) cover the controls.
pub mod token {
    use bevy::feathers::theme::ThemeToken;

    /// A panel over the app's content: an inspector, a connect panel, a form.
    pub const PANEL_BG: ThemeToken = ThemeToken::new_static("ui.panel.bg");
    /// A border, and the rule between sections of a panel.
    pub const DIVIDER: ThemeToken = ThemeToken::new_static("ui.divider");
    /// A small badge's background.
    pub const BADGE_BG: ThemeToken = ThemeToken::new_static("ui.badge.bg");
    /// The hint shown over a hovered tool button.
    pub const HINT_BG: ThemeToken = ThemeToken::new_static("ui.hint.bg");

    /// Something that wants a person: an open question, a refusal to look at.
    pub const WARN: ThemeToken = ThemeToken::new_static("ui.warn");
    /// Something destructive (cancel, revert) or failed (a failing gate).
    pub const DANGER: ThemeToken = ThemeToken::new_static("ui.danger");
    /// Something that went well: a passed gate, a live connection.
    pub const OK: ThemeToken = ThemeToken::new_static("ui.ok");
    /// Something under way, or worth knowing: a notice that isn't good or bad news.
    pub const INFO: ThemeToken = ThemeToken::new_static("ui.info");
    /// A placeholder bar where content is on its way, at its brightest.
    pub const SKELETON: ThemeToken = ThemeToken::new_static("ui.skeleton");
}

/// The kit's theme, for an app that adds no colors of its own.
pub fn theme() -> UiTheme {
    UiTheme(props())
}

/// Feathers' dark theme with the kit's tokens added: the start of an app's own theme. Public so a test can check
/// every token resolves.
pub fn props() -> ThemeProps {
    let mut theme = create_dark_theme();
    paint(&mut theme, &tokens::BUTTON_BG_HOVER, palette::GRAY_3.lighter(0.14));
    paint(&mut theme, &tokens::BUTTON_BG_PRESSED, palette::ACCENT.darker(0.12));
    paint(&mut theme, &tokens::BUTTON_PRIMARY_BG_HOVER, palette::ACCENT.lighter(0.08));
    // Plain buttons (icon buttons on cards and lists) hover like normal ones: feathers' plain hover is a shade of
    // the surface they sit on, so on a card it can't be seen.
    paint(&mut theme, &tokens::BUTTON_PLAIN_BG_HOVER, palette::GRAY_3.lighter(0.14));
    paint(&mut theme, &tokens::BUTTON_PLAIN_BG_PRESSED, palette::ACCENT.darker(0.12));

    paint(&mut theme, &token::PANEL_BG, palette::GRAY_1);
    paint(&mut theme, &token::DIVIDER, palette::GRAY_3);
    paint(&mut theme, &token::BADGE_BG, palette::GRAY_3);
    paint(&mut theme, &token::HINT_BG, palette::GRAY_3.darker(0.05));

    paint(&mut theme, &token::WARN, Color::oklch(0.80, 0.14, 75.0));
    paint(&mut theme, &token::DANGER, Color::oklch(0.68, 0.17, 25.0));
    paint(&mut theme, &token::OK, Color::oklch(0.75, 0.14, 150.0));
    paint(&mut theme, &token::INFO, Color::srgb(0.45, 0.68, 0.95));
    paint(&mut theme, &token::SKELETON, palette::GRAY_3);
    theme
}

/// Give `token` a color of its own, on a semantic slot named after it, so changing it repaints nothing else.
pub fn paint(theme: &mut ThemeProps, token: &ThemeToken, color: Color) -> SemanticToken {
    let own = SemanticToken::new(token.to_string().into());
    theme.token_assignments.insert(token.clone(), own.clone());
    theme.semantic_base.insert(own.clone(), color);
    own
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kit_token_resolves_rather_than_falling_back_to_fuchsia() {
        let theme = props();
        let all = [
            token::PANEL_BG,
            token::DIVIDER,
            token::BADGE_BG,
            token::HINT_BG,
            token::WARN,
            token::DANGER,
            token::OK,
            token::INFO,
            token::SKELETON,
        ];
        for t in all {
            assert!(theme.token_assignments.contains_key(&t), "{t} isn't in the theme");
        }
        // Feathers' own controls still resolve.
        assert!(theme.token_assignments.contains_key(&tokens::BUTTON_BG));
    }

    #[test]
    fn every_button_variant_hovers_to_a_color_other_than_its_rest() {
        let theme = UiTheme(props());
        assert_ne!(theme.color(&tokens::BUTTON_BG), theme.color(&tokens::BUTTON_BG_HOVER));
        assert_ne!(theme.color(&tokens::BUTTON_PRIMARY_BG), theme.color(&tokens::BUTTON_PRIMARY_BG_HOVER));
        assert_ne!(theme.color(&tokens::BUTTON_PLAIN_BG), theme.color(&tokens::BUTTON_PLAIN_BG_HOVER));
    }

    #[test]
    fn an_app_paints_its_own_token_on_top_of_the_kits() {
        const MINE: ThemeToken = ThemeToken::new_static("app.mine");
        let mut theme = props();
        paint(&mut theme, &MINE, Color::WHITE);
        assert_eq!(UiTheme(theme).color(&MINE), Color::WHITE);
    }
}
