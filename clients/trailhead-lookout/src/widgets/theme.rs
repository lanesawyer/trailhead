//! The colors Lookout adds on top of the kit's theme.
//!
//! The kit's ([`trailhead_ui::theme_props`]) is the base; Lookout paints its own tokens onto it with
//! [`trailhead_ui::paint`]: one background per status column, the board and card surfaces, and a tone per priority.
//! A view names a token, never a color.

use bevy::feathers::palette;
use bevy::feathers::theme::{ThemeProps, ThemeToken, UiTheme};
use bevy::prelude::*;
use trailhead_ui::paint;

/// The kit's tokens and Lookout's own. Feathers' (`bevy::feathers::tokens`) cover the controls.
pub mod token {
    pub use trailhead_ui::token::*;

    use bevy::feathers::theme::ThemeToken;

    /// What the board's columns and the panels sit on.
    pub const BOARD_BG: ThemeToken = ThemeToken::new_static("lookout.board.bg");
    /// A task card.
    pub const CARD_BG: ThemeToken = ThemeToken::new_static("lookout.card.bg");

    /// One tone per priority, the same wherever a priority shows: P1 red, P2 orange, P3 blue, P4 gray.
    pub const PRIORITY_1: ThemeToken = ThemeToken::new_static("lookout.priority.1");
    pub const PRIORITY_2: ThemeToken = ThemeToken::new_static("lookout.priority.2");
    pub const PRIORITY_3: ThemeToken = ThemeToken::new_static("lookout.priority.3");
    pub const PRIORITY_4: ThemeToken = ThemeToken::new_static("lookout.priority.4");

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

/// A status as words, the one way every view shows it: "In Progress" for `in_progress`. A status the API adds later is
/// title-cased from its snake_case ("on_hold" is "On Hold"), so it still reads well. Only for display: requests,
/// filters and the mirror keep the API value.
pub fn status_label(status: &str) -> String {
    status
        .split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            chars.next().map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The tone of priority `p` (1 to 4), the same wherever a priority is shown. Any other number has none.
pub fn priority_tone(p: i64) -> Option<ThemeToken> {
    match p {
        1 => Some(token::PRIORITY_1),
        2 => Some(token::PRIORITY_2),
        3 => Some(token::PRIORITY_3),
        4 => Some(token::PRIORITY_4),
        _ => None,
    }
}

/// The theme `main.rs` inserts.
pub fn theme() -> UiTheme {
    UiTheme(props())
}

/// The kit's theme with Lookout's tokens added. Public so a test can check every token resolves.
pub fn props() -> ThemeProps {
    let mut theme = trailhead_ui::theme_props();
    paint(&mut theme, &token::BOARD_BG, palette::GRAY_0);
    paint(&mut theme, &token::CARD_BG, palette::GRAY_2);

    paint(&mut theme, &token::PRIORITY_1, Color::oklch(0.68, 0.17, 25.0));
    paint(&mut theme, &token::PRIORITY_2, Color::oklch(0.76, 0.15, 60.0));
    paint(&mut theme, &token::PRIORITY_3, Color::oklch(0.70, 0.13, 250.0));
    paint(&mut theme, &token::PRIORITY_4, Color::oklch(0.65, 0.0, 0.0));

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

#[cfg(test)]
mod tests {
    use bevy::feathers::tokens;

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
            token::PRIORITY_1,
            token::PRIORITY_2,
            token::PRIORITY_3,
            token::PRIORITY_4,
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
    fn each_priority_has_its_own_token_and_color() {
        let theme = props();
        let colors: Vec<Color> =
            (1..=4).map(|p| theme.semantic_base[&theme.token_assignments[&priority_tone(p).unwrap()]]).collect();
        for (i, a) in colors.iter().enumerate() {
            for b in &colors[i + 1..] {
                assert_ne!(a, b);
            }
        }
        assert!(priority_tone(0).is_none() && priority_tone(5).is_none());
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

    #[test]
    fn statuses_read_as_words() {
        let words: Vec<_> = crate::board::STATUSES.iter().map(|s| status_label(s)).collect();
        assert_eq!(words, ["Open", "In Progress", "Needs Input", "In Review", "Approved", "Done", "Cancelled"]);
        assert_eq!(status_label("on_hold"), "On Hold");
    }

    #[test]
    fn no_view_formats_a_status_itself() {
        // A status shown to a person goes through `status_label`; these are the shapes that skip it.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let skipping = [
            "{status}",
            "{st}",
            "text(status,",
            "text(status.",
            "select(statuses",
            "select(STATUSES",
            "STATUSES.iter().map(|s| s.to_string())",
        ];
        let mut bad = Vec::new();
        for file in ["board.rs", "inspector.rs", "author.rs", "map.rs", "widgets/column.rs", "widgets/card.rs"] {
            let source = std::fs::read_to_string(root.join(file)).unwrap();
            let source = source.split("#[cfg(test)]").next().unwrap();
            for (n, line) in source.lines().enumerate() {
                let line = line.trim();
                if !line.starts_with("//") && !line.contains("Name::new") && skipping.iter().any(|p| line.contains(p)) {
                    bad.push(format!("{file}:{}: {line}", n + 1));
                }
            }
        }
        assert!(bad.is_empty(), "show a status with widgets::status_label:\n{}", bad.join("\n"));
    }

    #[test]
    fn a_spacing_is_named_rather_than_typed() {
        // `board.rs` predates the kit; each view moves onto it in its own task (TRAIL-798 to TRAIL-801). Take it off
        // this list when it's moved; nothing new goes on it.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let typed = trailhead_ui::spacing_check::typed_spacings(&root, &["board.rs"]);
        assert!(typed.is_empty(), "name these spacings with widgets::space instead:\n{}", typed.join("\n"));
    }
}
