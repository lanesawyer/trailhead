//! The colors Lookout adds on top of the kit's theme.
//!
//! The kit's ([`trailhead_ui::Theme::new`]) is the base, and its semantic tokens (surfaces, text, fills, statuses) are
//! what Lookout's views name. Lookout's own colors are categorical, encoding data rather than meaning something to the
//! kit: one background and one text tone per status, a tone per priority and per grade, each painted with
//! [`trailhead_ui::Theme::paint`]. The top priority is the kit's danger color, an alias. A view names a token, never a
//! color.

use bevy::prelude::*;
use trailhead_factory::SandboxState;
use trailhead_model::{CommentKind, Grade, Priority, Status};
use trailhead_ui::{Icon, Theme, Token};

/// The kit's tokens and Lookout's own.
pub mod token {
    pub use trailhead_ui::token::*;

    use trailhead_ui::Token;

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

    /// The factory, wherever it shows (the board's Factory badge): lime, a hue no priority, grade or status uses.
    pub const FACTORY: Token = Token::new("lookout.factory");

    /// One background per status column, in lifecycle order.
    pub const STATUS_OPEN: Token = Token::new("lookout.status.open");
    pub const STATUS_IN_PROGRESS: Token = Token::new("lookout.status.in_progress");
    pub const STATUS_NEEDS_INPUT: Token = Token::new("lookout.status.needs_input");
    pub const STATUS_IN_REVIEW: Token = Token::new("lookout.status.in_review");
    pub const STATUS_APPROVED: Token = Token::new("lookout.status.approved");
    pub const STATUS_DONE: Token = Token::new("lookout.status.done");
    pub const STATUS_CANCELLED: Token = Token::new("lookout.status.cancelled");

    /// One text tone per status, in its column's hue, for a status written on a panel or card rather than filled in.
    pub const STATUS_OPEN_TEXT: Token = Token::new("lookout.status.open.text");
    pub const STATUS_IN_PROGRESS_TEXT: Token = Token::new("lookout.status.in_progress.text");
    pub const STATUS_NEEDS_INPUT_TEXT: Token = Token::new("lookout.status.needs_input.text");
    pub const STATUS_IN_REVIEW_TEXT: Token = Token::new("lookout.status.in_review.text");
    pub const STATUS_APPROVED_TEXT: Token = Token::new("lookout.status.approved.text");
    pub const STATUS_DONE_TEXT: Token = Token::new("lookout.status.done.text");
    pub const STATUS_CANCELLED_TEXT: Token = Token::new("lookout.status.cancelled.text");

    /// One background per sandbox state, for a campsite on the trail map.
    pub const SANDBOX_IDLE: Token = Token::new("lookout.sandbox.idle");
    pub const SANDBOX_BUSY: Token = Token::new("lookout.sandbox.busy");
    pub const SANDBOX_PAUSED: Token = Token::new("lookout.sandbox.paused");
    pub const SANDBOX_BROKEN: Token = Token::new("lookout.sandbox.broken");

    /// An agent walking the trail map, and the initial written on it.
    pub const AGENT: Token = Token::new("lookout.agent");
    pub const AGENT_TEXT: Token = Token::new("lookout.agent.text");
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

/// The background of a campsite whose sandbox is in `state`.
pub fn sandbox_token(state: SandboxState) -> Token {
    match state {
        SandboxState::Idle => token::SANDBOX_IDLE,
        SandboxState::Busy => token::SANDBOX_BUSY,
        SandboxState::Paused => token::SANDBOX_PAUSED,
        SandboxState::Broken => token::SANDBOX_BROKEN,
    }
}

/// The text tone of a status, the same wherever a status is written. [`status_token`] is its column's background, too
/// dark to read as text.
pub fn status_tone(status: Status) -> Token {
    match status {
        Status::Open => token::STATUS_OPEN_TEXT,
        Status::InProgress => token::STATUS_IN_PROGRESS_TEXT,
        Status::NeedsInput => token::STATUS_NEEDS_INPUT_TEXT,
        Status::InReview => token::STATUS_IN_REVIEW_TEXT,
        Status::Approved => token::STATUS_APPROVED_TEXT,
        Status::Done => token::STATUS_DONE_TEXT,
        Status::Cancelled => token::STATUS_CANCELLED_TEXT,
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
    // The most urgent priority is the kit's danger red.
    theme.alias(&token::PRIORITY_1, &token::STATUS_DANGER);
    theme.paint(&token::PRIORITY_2, Color::oklch(0.76, 0.15, 60.0));
    theme.paint(&token::PRIORITY_3, Color::oklch(0.70, 0.13, 250.0));
    theme.paint(&token::PRIORITY_4, Color::oklch(0.65, 0.0, 0.0));

    theme.paint(&token::GRADE_1, Color::oklch(0.74, 0.15, 145.0));
    theme.paint(&token::GRADE_2, Color::oklch(0.74, 0.11, 190.0));
    theme.paint(&token::GRADE_3, Color::oklch(0.68, 0.15, 295.0));
    theme.paint(&token::GRADE_4, Color::oklch(0.68, 0.19, 340.0));

    theme.paint(&token::FACTORY, Color::oklch(0.76, 0.15, 115.0));

    // Columns tinted just enough to tell apart, each toward the hue of what the status means.
    let column = |hue: f32| Color::oklch(0.24, 0.025, hue);
    theme.paint(&token::STATUS_OPEN, column(250.0));
    theme.paint(&token::STATUS_IN_PROGRESS, column(210.0));
    theme.paint(&token::STATUS_NEEDS_INPUT, column(75.0));
    theme.paint(&token::STATUS_IN_REVIEW, column(300.0));
    theme.paint(&token::STATUS_APPROVED, column(150.0));
    theme.paint(&token::STATUS_DONE, Color::oklch(0.22, 0.0, 0.0));
    theme.paint(&token::STATUS_CANCELLED, Color::oklch(0.20, 0.0, 0.0));

    // Written in the same hues, as light as the priorities, so they read on a panel or card.
    let words = |hue: f32| Color::oklch(0.74, 0.12, hue);
    theme.paint(&token::STATUS_OPEN_TEXT, words(250.0));
    theme.paint(&token::STATUS_IN_PROGRESS_TEXT, words(210.0));
    theme.paint(&token::STATUS_NEEDS_INPUT_TEXT, words(75.0));
    theme.paint(&token::STATUS_IN_REVIEW_TEXT, words(300.0));
    theme.paint(&token::STATUS_APPROVED_TEXT, words(150.0));
    theme.paint(&token::STATUS_DONE_TEXT, Color::oklch(0.76, 0.0, 0.0));
    theme.paint(&token::STATUS_CANCELLED_TEXT, Color::oklch(0.66, 0.0, 0.0));

    // Campsites, each toward the hue of what its state means (green resting, blue working, amber paused, red broken),
    // all as dark, so a camp's text reads on any of them.
    let camp = |hue: f32| Color::oklch(0.38, 0.07, hue);
    theme.paint(&token::SANDBOX_IDLE, camp(150.0));
    theme.paint(&token::SANDBOX_BUSY, camp(245.0));
    theme.paint(&token::SANDBOX_PAUSED, camp(80.0));
    theme.paint(&token::SANDBOX_BROKEN, camp(25.0));

    theme.paint(&token::AGENT, Color::srgb(0.95, 0.75, 0.3));
    theme.paint(&token::AGENT_TEXT, Color::BLACK);
    theme
}

#[cfg(test)]
mod tests {
    use trailhead_ui::Surface;

    use super::*;

    #[test]
    fn each_kind_of_comment_has_its_own_icon() {
        let icons: std::collections::HashSet<Icon> = CommentKind::ALL.into_iter().map(comment_icon).collect();
        assert_eq!(icons.len(), CommentKind::ALL.len());
    }

    #[test]
    fn every_lookout_token_resolves_rather_than_falling_back_to_fuchsia() {
        let theme = theme();
        let lookouts =
            Priority::ALL.map(priority_tone).into_iter().chain(Grade::ALL.map(grade_tone)).chain([token::FACTORY]);
        let statuses = Status::ALL.into_iter().flat_map(|s| [status_token(s), status_tone(s)]);
        for t in lookouts.chain(statuses).chain(token::ALL) {
            assert!(theme.has(&t), "{t} isn't in the theme");
        }
    }

    #[test]
    fn no_color_is_painted_that_a_meaning_already_has() {
        let repeated = theme().repeated_colors();
        assert!(repeated.is_empty(), "alias these to the meaning instead:\n{}", repeated.join("\n"));
    }

    #[test]
    fn each_sandbox_state_has_its_own_color_and_a_camps_text_reads_on_it() {
        let theme = theme();
        let lightness = |t: &Token| Oklcha::from(theme.color(t)).lightness;
        let states = [SandboxState::Idle, SandboxState::Busy, SandboxState::Paused, SandboxState::Broken];
        let colors: std::collections::HashSet<_> =
            states.iter().map(|s| format!("{:?}", theme.color(&sandbox_token(*s)))).collect();
        assert_eq!(colors.len(), states.len());
        for state in states {
            let gap = lightness(&token::TEXT) - lightness(&sandbox_token(state));
            assert!(gap >= TEXT_CONTRAST, "a camp's text on {state:?} is {gap:.2} lighter, too dark to read");
        }
        assert!(lightness(&token::AGENT) - lightness(&token::AGENT_TEXT) >= TEXT_CONTRAST);
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
        assert_eq!(theme.color(&token::PRIORITY_1), theme.color(&token::STATUS_DANGER));
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
        let card = lightness(&token::SURFACE_RAISED);
        assert!(card > 0.5, "the card turns light");
        for tone in written_tones() {
            assert!(card - lightness(&tone) > 0.1, "{tone} is too faint on a light card");
        }
    }

    /// Every tone written as text in one of Lookout's own colors: priorities, grades, statuses.
    fn written_tones() -> Vec<Token> {
        let priorities = Priority::ALL.map(priority_tone).into_iter();
        priorities
            .chain(Grade::ALL.map(grade_tone))
            .chain(Status::ALL.map(status_tone))
            .chain([token::FACTORY])
            .collect()
    }

    /// The least lightness between text and the surface under it that reads easily.
    const TEXT_CONTRAST: f32 = 0.3;

    #[test]
    fn in_the_dark_theme_every_written_tone_reads_on_the_panel_and_a_card() {
        // The status select once wrote each status in its column's background, near-black on a dark panel.
        let dark = theme();
        let lightness = |t: &Token| Oklcha::from(dark.color(t)).lightness;
        for surface in [token::SURFACE_PANEL, token::SURFACE_RAISED] {
            for tone in written_tones() {
                let gap = lightness(&tone) - lightness(&surface);
                assert!(gap >= TEXT_CONTRAST, "{tone} on {surface} is {gap:.2} lighter, too dark to read");
            }
        }
    }

    /// The least lightness between a hover and the surface under it that reads as a response.
    const HOVER_CONTRAST: f32 = 0.05;

    #[test]
    fn a_plain_buttons_hover_shows_on_a_card_resting_or_hovered_and_on_the_panel_in_either_theme() {
        // The card's Edit, Comment and Answer buttons are plain, as are the inspector's and settings' (TRAIL-1091).
        // Feathers' own plain hover is a shade of the surface, a gap too small to see.
        for theme in [theme(), theme().light()] {
            let on = |t: &Token, surface| Oklcha::from(theme.color_on(t, surface)).lightness;
            for (surface, under) in [
                (Surface::Raised, token::SURFACE_RAISED),
                (Surface::Raised, CARD_HOVER),
                (Surface::Panel, token::SURFACE_PANEL),
            ] {
                let gap = (on(&token::FILL_ITEM_HOVER, surface) - on(&under, surface)).abs();
                assert!(gap >= HOVER_CONTRAST, "a plain button's hover on {under} is {gap:.3} away, too close to see");
            }
        }
    }

    /// What a card wears under the pointer (`widgets/card.rs`).
    const CARD_HOVER: Token = token::FILL_SOLID;

    #[test]
    fn a_cards_hover_shows_on_the_card_in_either_theme() {
        for theme in [theme(), theme().light()] {
            let on = |t: &Token| Oklcha::from(theme.color_on(t, Surface::Raised)).lightness;
            let gap = (on(&CARD_HOVER) - on(&token::SURFACE_RAISED)).abs();
            assert!(gap >= HOVER_CONTRAST, "a card's hover is {gap:.3} away from the card, too close to see");
        }
    }

    #[test]
    fn statuses_read_as_words() {
        let words = Status::ALL.map(status_label);
        assert_eq!(words, ["Open", "In Progress", "Needs Input", "In Review", "Approved", "Done", "Cancelled"]);
    }
}
