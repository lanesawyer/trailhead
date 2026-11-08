//! The colors Lookout adds on top of the kit's theme.
//!
//! The kit's ([`trailhead_ui::Theme::new`]) is the base; Lookout paints its own tokens onto it with
//! [`trailhead_ui::Theme::paint`]: one background per status column, the board and card surfaces, and a tone per priority.
//! A view names a token, never a color.

use bevy::prelude::*;
use trailhead_model::{CommentKind, Grade, Priority, Status};
use trailhead_ui::{Icon, Theme, Token, palette};

/// The kit's tokens and Lookout's own.
pub mod token {
    pub use trailhead_ui::token::*;

    use trailhead_ui::Token;

    /// What the board's columns and the panels sit on.
    pub const BOARD_BG: Token = Token::new("lookout.board.bg");
    /// A task card.
    pub const CARD_BG: Token = Token::new("lookout.card.bg");

    /// One tone per priority, the same wherever a priority shows: P1 red, P2 orange, P3 blue, P4 gray.
    pub const PRIORITY_1: Token = Token::new("lookout.priority.1");
    pub const PRIORITY_2: Token = Token::new("lookout.priority.2");
    pub const PRIORITY_3: Token = Token::new("lookout.priority.3");
    pub const PRIORITY_4: Token = Token::new("lookout.priority.4");

    /// One tone per grade, apart from the priorities' warm-to-cool run: G1 green, G2 teal, G3 purple, G4 magenta.
    pub const GRADE_1: Token = Token::new("lookout.grade.1");
    pub const GRADE_2: Token = Token::new("lookout.grade.2");
    pub const GRADE_3: Token = Token::new("lookout.grade.3");
    pub const GRADE_4: Token = Token::new("lookout.grade.4");

    /// One background per status column, in lifecycle order.
    pub const STATUS_OPEN: Token = Token::new("lookout.status.open");
    pub const STATUS_IN_PROGRESS: Token = Token::new("lookout.status.in_progress");
    pub const STATUS_NEEDS_INPUT: Token = Token::new("lookout.status.needs_input");
    pub const STATUS_IN_REVIEW: Token = Token::new("lookout.status.in_review");
    pub const STATUS_APPROVED: Token = Token::new("lookout.status.approved");
    pub const STATUS_DONE: Token = Token::new("lookout.status.done");
    pub const STATUS_CANCELLED: Token = Token::new("lookout.status.cancelled");
}

/// The background token of a status column.
pub fn status_token(status: Status) -> Token {
    match status {
        Status::Open => token::STATUS_OPEN,
        Status::InProgress => token::STATUS_IN_PROGRESS,
        Status::NeedsInput => token::STATUS_NEEDS_INPUT,
        Status::InReview => token::STATUS_IN_REVIEW,
        Status::Approved => token::STATUS_APPROVED,
        Status::Done => token::STATUS_DONE,
        Status::Cancelled => token::STATUS_CANCELLED,
    }
}

/// A status as words, the one way every view shows it: "In Progress" for `in_progress`. Only for display: requests,
/// filters and the mirror keep the API value.
pub fn status_label(status: Status) -> &'static str {
    match status {
        Status::Open => "Open",
        Status::InProgress => "In Progress",
        Status::NeedsInput => "Needs Input",
        Status::InReview => "In Review",
        Status::Approved => "Approved",
        Status::Done => "Done",
        Status::Cancelled => "Cancelled",
    }
}

/// The tone of a priority, the same wherever a priority is shown.
pub fn priority_tone(p: Priority) -> Token {
    match p.get() {
        1 => token::PRIORITY_1,
        2 => token::PRIORITY_2,
        3 => token::PRIORITY_3,
        _ => token::PRIORITY_4,
    }
}

/// The tone of a grade, the same wherever a grade is shown.
pub fn grade_tone(g: Grade) -> Token {
    match g.get() {
        1 => token::GRADE_1,
        2 => token::GRADE_2,
        3 => token::GRADE_3,
        _ => token::GRADE_4,
    }
}

/// The icon of a kind of comment, the same wherever a comment shows, so its kind reads before its words.
pub fn comment_icon(kind: CommentKind) -> Icon {
    match kind {
        CommentKind::Note => Icon::Comment,
        CommentKind::Handoff => Icon::Handshake,
        CommentKind::Decision => Icon::Signpost,
        CommentKind::Question => Icon::Help,
        CommentKind::Progress => Icon::Footprints,
        CommentKind::Review => Icon::Eye,
    }
}

/// The kit's theme with Lookout's tokens added, which `main.rs` hands to the kit's `ThemePlugin`.
pub fn theme() -> Theme {
    let mut theme = Theme::new();
    theme.paint(&token::BOARD_BG, palette::GRAY_0);
    theme.paint(&token::CARD_BG, palette::GRAY_2);

    // The most urgent priority is the kit's danger red, read from the theme so the two can't drift apart.
    let danger = theme.color(&token::DANGER);
    theme.paint(&token::PRIORITY_1, danger);
    theme.paint(&token::PRIORITY_2, Color::oklch(0.76, 0.15, 60.0));
    theme.paint(&token::PRIORITY_3, Color::oklch(0.70, 0.13, 250.0));
    theme.paint(&token::PRIORITY_4, Color::oklch(0.65, 0.0, 0.0));

    theme.paint(&token::GRADE_1, Color::oklch(0.74, 0.15, 145.0));
    theme.paint(&token::GRADE_2, Color::oklch(0.74, 0.11, 190.0));
    theme.paint(&token::GRADE_3, Color::oklch(0.68, 0.15, 295.0));
    theme.paint(&token::GRADE_4, Color::oklch(0.68, 0.19, 340.0));

    // Columns tinted just enough to tell apart, each toward the hue of what the status means.
    let column = |hue: f32| Color::oklch(0.24, 0.025, hue);
    theme.paint(&token::STATUS_OPEN, column(250.0));
    theme.paint(&token::STATUS_IN_PROGRESS, column(210.0));
    theme.paint(&token::STATUS_NEEDS_INPUT, column(75.0));
    theme.paint(&token::STATUS_IN_REVIEW, column(300.0));
    theme.paint(&token::STATUS_APPROVED, column(150.0));
    theme.paint(&token::STATUS_DONE, Color::oklch(0.22, 0.0, 0.0));
    theme.paint(&token::STATUS_CANCELLED, Color::oklch(0.20, 0.0, 0.0));
    theme
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_of_comment_has_its_own_icon() {
        let icons: std::collections::HashSet<Icon> = CommentKind::ALL.into_iter().map(comment_icon).collect();
        assert_eq!(icons.len(), CommentKind::ALL.len());
    }

    #[test]
    fn every_lookout_token_resolves_rather_than_falling_back_to_fuchsia() {
        let theme = theme();
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
            token::GRADE_1,
            token::GRADE_2,
            token::GRADE_3,
            token::GRADE_4,
            token::STATUS_OPEN,
            token::STATUS_IN_PROGRESS,
            token::STATUS_NEEDS_INPUT,
            token::STATUS_IN_REVIEW,
            token::STATUS_APPROVED,
            token::STATUS_DONE,
            token::STATUS_CANCELLED,
        ];
        for t in all {
            assert!(theme.has(&t), "{t} isn't in the theme");
        }
        // Feathers' own controls still resolve.
        assert!(theme.has(&token::BUTTON_BG));
    }

    #[test]
    fn each_priority_has_its_own_token_and_color() {
        let theme = theme();
        let colors: Vec<Color> = Priority::ALL.map(|p| theme.color(&priority_tone(p))).to_vec();
        for (i, a) in colors.iter().enumerate() {
            for b in &colors[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn the_top_priority_is_the_danger_color() {
        let theme = theme();
        assert_eq!(theme.color(&token::PRIORITY_1), theme.color(&token::DANGER));
    }

    #[test]
    fn each_grade_has_its_own_color_apart_from_the_priorities() {
        let theme = theme();
        let color = |t: Token| format!("{:?}", theme.color(&t));
        let grades: Vec<String> = Grade::ALL.map(|g| color(grade_tone(g))).to_vec();
        let priorities: Vec<String> = Priority::ALL.map(|p| color(priority_tone(p))).to_vec();
        let all: std::collections::HashSet<_> = grades.iter().chain(&priorities).collect();
        assert_eq!(all.len(), 8, "{grades:?} {priorities:?}");
    }

    #[test]
    fn each_status_has_its_own_column_color() {
        let theme = theme();
        let colors: Vec<_> = Status::ALL.iter().map(|s| format!("{:?}", theme.color(&status_token(*s)))).collect();
        let distinct: std::collections::HashSet<_> = colors.iter().collect();
        assert_eq!(distinct.len(), Status::ALL.len(), "{colors:?}");
    }

    #[test]
    fn in_the_light_theme_priorities_and_grades_still_read_on_a_card() {
        let light = theme().light();
        let lightness = |t: &Token| Oklcha::from(light.color(t)).lightness;
        let card = lightness(&token::CARD_BG);
        assert!(card > 0.5, "the card turns light");
        let tones = Priority::ALL.map(priority_tone).into_iter().chain(Grade::ALL.map(grade_tone));
        for tone in tones {
            assert!(card - lightness(&tone) > 0.1, "{tone} is too faint on a light card");
        }
    }

    /// The least lightness between a hover and the surface under it that reads as a response.
    const HOVER_CONTRAST: f32 = 0.05;

    #[test]
    fn a_plain_buttons_hover_shows_on_the_card_and_the_panel_in_either_theme() {
        // The card's Edit, Comment and Answer buttons are plain, as are the inspector's and settings' (TRAIL-1091).
        // Feathers' own plain hover is a shade of the surface, a gap too small to see.
        for theme in [theme(), theme().light()] {
            let lightness = |t: &Token| Oklcha::from(theme.color(t)).lightness;
            let hover = lightness(&token::BUTTON_PLAIN_BG_HOVER);
            for surface in [token::CARD_BG, token::PANEL_BG] {
                let gap = (hover - lightness(&surface)).abs();
                assert!(
                    gap >= HOVER_CONTRAST,
                    "a plain button's hover on {surface} is {gap:.3} away, too close to see"
                );
            }
        }
    }

    #[test]
    fn statuses_read_as_words() {
        let words = Status::ALL.map(status_label);
        assert_eq!(words, ["Open", "In Progress", "Needs Input", "In Review", "Approved", "Done", "Cancelled"]);
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
            "Status::ALL.iter().map(|s| s.to_string())",
        ];
        let mut bad = Vec::new();
        // Every view file, found by walking `src/`, so a new one is covered without being listed. This file names
        // the shapes it forbids, so it is the one left out.
        let mut files = Vec::new();
        let mut folders = vec![root.clone()];
        while let Some(folder) = folders.pop() {
            for entry in std::fs::read_dir(&folder).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    folders.push(path);
                } else if path.extension().is_some_and(|ext| ext == "rs") && !path.ends_with("widgets/theme.rs") {
                    files.push(path);
                }
            }
        }
        files.sort();
        assert!(files.len() >= 6, "found only {files:?}");
        for path in files {
            let file = path.strip_prefix(&root).unwrap().display().to_string();
            let source = std::fs::read_to_string(&path).unwrap();
            let source = source.split("#[cfg(test)]").next().unwrap();
            for (n, line) in source.lines().enumerate() {
                let line = line.trim();
                if line.starts_with("//") || line.contains("Name::new") {
                    continue;
                }
                let own_words = Status::ALL.iter().map(|s| s.as_str()).flat_map(|s| {
                    let spaced = s.replace('_', " ");
                    // A single word is the API value too (`column_shell("open")`), so only a (text, color) pair is a label.
                    let spelled = if spaced == *s { format!("(\"{s}\".into(),") } else { format!("\"{spaced}\"") };
                    [spelled, format!("\"{}\"", spaced.to_uppercase())]
                });
                if skipping.iter().any(|p| line.contains(p)) || own_words.into_iter().any(|p| line.contains(&p)) {
                    bad.push(format!("{file}:{}: {line}", n + 1));
                }
            }
        }
        assert!(bad.is_empty(), "show a status with widgets::status_label:\n{}", bad.join("\n"));
    }

    #[test]
    fn lookout_never_reaches_past_the_kit_to_feathers() {
        // Feathers stays inside trailhead-ui: what Lookout needs from it, the kit wraps and exports (TRAIL-1102). The
        // needles are split so this file doesn't find itself.
        let needles = [concat!("feathers", "::"), concat!("bevy_", "feathers")];
        let crate_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut bad = Vec::new();
        let mut folders = vec![crate_root.join("src"), crate_root.join("examples"), crate_root.join("tests")];
        let mut files = vec![crate_root.join("Cargo.toml")];
        while let Some(folder) = folders.pop() {
            let Ok(entries) = std::fs::read_dir(&folder) else { continue };
            for entry in entries {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    folders.push(path);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    files.push(path);
                }
            }
        }
        assert!(files.len() >= 10, "found only {files:?}");
        for path in files {
            let source = std::fs::read_to_string(&path).unwrap();
            for (n, line) in source.lines().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//") || (code.starts_with('#') && path.ends_with("Cargo.toml")) {
                    continue;
                }
                if needles.iter().any(|needle| line.contains(needle)) {
                    bad.push(format!("{}:{}: {}", path.strip_prefix(crate_root).unwrap().display(), n + 1, code));
                }
            }
        }
        assert!(bad.is_empty(), "use (or grow) the trailhead-ui kit instead of feathers:\n{}", bad.join("\n"));
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
