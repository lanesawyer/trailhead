//! The colors Lookout wears, in one place.
//!
//! Feathers' dark theme is the base. Its button hover is widened (as bevy-data-explorer found, feathers' own is too
//! faint to read as a response), and Lookout's own colors are added as tokens: one background per status column,
//! the surfaces cards and panels sit on, and the tones for badges and warnings. A view names a token, never a color,
//! so the theme is one edit and a missing token shows up as fuchsia rather than as a wrong color nobody notices.

use bevy::feathers::dark_theme::create_dark_theme;
use bevy::feathers::palette;
use bevy::feathers::theme::{SemanticToken, ThemeProps, ThemeToken, UiTheme};
use bevy::feathers::tokens;
use bevy::prelude::*;

/// Lookout's own tokens. Feathers' (`bevy::feathers::tokens`) cover the controls.
pub mod token {
    use bevy::feathers::theme::ThemeToken;

    /// What the board's columns and the panels sit on.
    pub const BOARD_BG: ThemeToken = ThemeToken::new_static("lookout.board.bg");
    /// A panel over the board: the inspector, the connect panel, a form.
    pub const PANEL_BG: ThemeToken = ThemeToken::new_static("lookout.panel.bg");
    /// A task card.
    pub const CARD_BG: ThemeToken = ThemeToken::new_static("lookout.card.bg");
    /// A card's border, and the rule between sections of a panel.
    pub const DIVIDER: ThemeToken = ThemeToken::new_static("lookout.divider");
    /// A small badge's background (priority, grade, holder).
    pub const BADGE_BG: ThemeToken = ThemeToken::new_static("lookout.badge.bg");
    /// The hint shown over a hovered tool button.
    pub const HINT_BG: ThemeToken = ThemeToken::new_static("lookout.hint.bg");

    /// Something that wants a person: an open question, a refusal to look at.
    pub const WARN: ThemeToken = ThemeToken::new_static("lookout.warn");
    /// Something destructive (cancel, revert) or failed (a failing gate).
    pub const DANGER: ThemeToken = ThemeToken::new_static("lookout.danger");
    /// Something that went well: a passed gate, a live connection.
    pub const OK: ThemeToken = ThemeToken::new_static("lookout.ok");

    /// One background per status column, in lifecycle order.
    pub const STATUS_OPEN: ThemeToken = ThemeToken::new_static("lookout.status.open");
    pub const STATUS_IN_PROGRESS: ThemeToken = ThemeToken::new_static("lookout.status.in_progress");
    pub const STATUS_NEEDS_INPUT: ThemeToken = ThemeToken::new_static("lookout.status.needs_input");
    pub const STATUS_IN_REVIEW: ThemeToken = ThemeToken::new_static("lookout.status.in_review");
    pub const STATUS_APPROVED: ThemeToken = ThemeToken::new_static("lookout.status.approved");
    pub const STATUS_DONE: ThemeToken = ThemeToken::new_static("lookout.status.done");
    pub const STATUS_CANCELLED: ThemeToken = ThemeToken::new_static("lookout.status.cancelled");
}

/// The background token of a status column, by the status's API name. An unknown status gets the board's own.
pub fn status_token(status: &str) -> ThemeToken {
    match status {
        "open" => token::STATUS_OPEN,
        "in_progress" => token::STATUS_IN_PROGRESS,
        "needs_input" => token::STATUS_NEEDS_INPUT,
        "in_review" => token::STATUS_IN_REVIEW,
        "approved" => token::STATUS_APPROVED,
        "done" => token::STATUS_DONE,
        "cancelled" => token::STATUS_CANCELLED,
        _ => token::BOARD_BG,
    }
}

/// The theme `main.rs` inserts.
pub fn theme() -> UiTheme {
    UiTheme(props())
}

/// Feathers' dark theme with Lookout's tokens added. Public so a test can check every token resolves.
pub fn props() -> ThemeProps {
    let mut theme = create_dark_theme();
    paint(&mut theme, &tokens::BUTTON_BG_HOVER, palette::GRAY_3.lighter(0.14));
    paint(&mut theme, &tokens::BUTTON_BG_PRESSED, palette::ACCENT.darker(0.12));
    paint(&mut theme, &tokens::BUTTON_PRIMARY_BG_HOVER, palette::ACCENT.lighter(0.08));

    paint(&mut theme, &token::BOARD_BG, palette::GRAY_0);
    paint(&mut theme, &token::PANEL_BG, palette::GRAY_1);
    paint(&mut theme, &token::CARD_BG, palette::GRAY_2);
    paint(&mut theme, &token::DIVIDER, palette::GRAY_3);
    paint(&mut theme, &token::BADGE_BG, palette::GRAY_3);
    paint(&mut theme, &token::HINT_BG, palette::GRAY_3.darker(0.05));

    paint(&mut theme, &token::WARN, Color::oklch(0.80, 0.14, 75.0));
    paint(&mut theme, &token::DANGER, Color::oklch(0.68, 0.17, 25.0));
    paint(&mut theme, &token::OK, Color::oklch(0.75, 0.14, 150.0));

    // Columns tinted just enough to tell apart, each toward the hue of what the status means.
    let column = |hue: f32| Color::oklch(0.24, 0.025, hue);
    paint(&mut theme, &token::STATUS_OPEN, column(250.0));
    paint(&mut theme, &token::STATUS_IN_PROGRESS, column(210.0));
    paint(&mut theme, &token::STATUS_NEEDS_INPUT, column(75.0));
    paint(&mut theme, &token::STATUS_IN_REVIEW, column(300.0));
    paint(&mut theme, &token::STATUS_APPROVED, column(150.0));
    paint(&mut theme, &token::STATUS_DONE, Color::oklch(0.22, 0.0, 0.0));
    paint(&mut theme, &token::STATUS_CANCELLED, Color::oklch(0.20, 0.0, 0.0));
    theme
}

/// Give `token` a color of its own, on a semantic slot named after it, so changing it repaints nothing else.
fn paint(theme: &mut ThemeProps, token: &ThemeToken, color: Color) -> SemanticToken {
    let own = SemanticToken::new(token.to_string().into());
    theme.token_assignments.insert(token.clone(), own.clone());
    theme.semantic_base.insert(own.clone(), color);
    own
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_lookout_token_resolves_rather_than_falling_back_to_fuchsia() {
        let theme = props();
        let all = [
            token::BOARD_BG,
            token::PANEL_BG,
            token::CARD_BG,
            token::DIVIDER,
            token::BADGE_BG,
            token::HINT_BG,
            token::WARN,
            token::DANGER,
            token::OK,
            token::STATUS_OPEN,
            token::STATUS_IN_PROGRESS,
            token::STATUS_NEEDS_INPUT,
            token::STATUS_IN_REVIEW,
            token::STATUS_APPROVED,
            token::STATUS_DONE,
            token::STATUS_CANCELLED,
        ];
        for t in all {
            assert!(theme.token_assignments.contains_key(&t), "{t} isn't in the theme");
        }
        // Feathers' own controls still resolve.
        assert!(theme.token_assignments.contains_key(&tokens::BUTTON_BG));
    }

    #[test]
    fn each_status_has_its_own_column_color() {
        let theme = props();
        let statuses = ["open", "in_progress", "needs_input", "in_review", "approved", "done", "cancelled"];
        let colors: Vec<_> = statuses.iter().map(|s| format!("{:?}", theme.color(&status_token(s)))).collect();
        let distinct: std::collections::HashSet<_> = colors.iter().collect();
        assert_eq!(distinct.len(), statuses.len(), "{colors:?}");
        assert_eq!(status_token("nonsense"), token::BOARD_BG);
    }
}
