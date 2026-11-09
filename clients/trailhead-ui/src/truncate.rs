//! Cutting text to fit a width.
//!
//! Ported from bevy-data-explorer's `widgets/truncate.rs`.

use crate::text::Size;

/// Rough width of a glyph as a fraction of the font size.
///
/// Bevy has no text truncation, and measuring would mean laying the string out
/// and reacting to the result a frame later, which oscillates as the measured
/// average shifts with the characters left. A fixed estimate is stable, and
/// erring narrow truncates a little early rather than overflowing.
const GLYPH_WIDTH: f32 = 0.58;

/// How wide `chars` characters come out at `font_size`, by the same estimate
/// [`truncate_to_width`] cuts by.
///
/// The two share the estimate so that a box sized by one and filled by the
/// other agree: a column as wide as its widest value never truncates it.
pub fn width_of(chars: usize, font_size: Size) -> f32 {
    chars as f32 * (font_size.px() * GLYPH_WIDTH).max(1.0)
}

/// Fit `text` into `width`, ending with an ellipsis if it has to be cut.
pub fn truncate_to_width(text: &str, width: f32, font_size: Size) -> String {
    let glyph = (font_size.px() * GLYPH_WIDTH).max(1.0);
    let fits = (width / glyph).floor().max(0.0) as usize;
    if text.chars().count() <= fits {
        return text.to_string();
    }
    // One character is given back to the ellipsis itself.
    let keep = fits.saturating_sub(1);
    if keep == 0 {
        return String::new();
    }
    text.chars().take(keep).collect::<String>() + "\u{2026}"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::size;

    #[test]
    fn a_box_sized_for_a_string_holds_the_whole_of_it() {
        for text in ["Class", "gene_symbol", "protein kinase C, theta"] {
            let width = width_of(text.chars().count(), size::SECONDARY);
            assert_eq!(truncate_to_width(text, width, size::SECONDARY), text);
        }
    }

    #[test]
    fn a_title_that_fits_is_left_alone() {
        assert_eq!(truncate_to_width("Class", 200.0, size::BODY), "Class");
    }

    #[test]
    fn a_long_title_is_cut_and_marked() {
        let cut = truncate_to_width("Subclass Bootstrapping Probability", 80.0, size::BODY);
        assert!(cut.ends_with('\u{2026}'));
        assert!(cut.chars().count() < "Subclass Bootstrapping Probability".chars().count());
        assert!(cut.starts_with("Subcl"));
    }

    #[test]
    fn a_narrower_header_cuts_more() {
        let wide = truncate_to_width("Neurotransmitter Type", 120.0, size::BODY);
        let narrow = truncate_to_width("Neurotransmitter Type", 60.0, size::BODY);
        assert!(narrow.chars().count() < wide.chars().count());
    }

    #[test]
    fn no_room_at_all_yields_nothing_rather_than_a_bare_ellipsis() {
        assert_eq!(truncate_to_width("Class", 0.0, size::BODY), "");
        assert_eq!(truncate_to_width("Class", 4.0, size::BODY), "");
    }

    #[test]
    fn truncation_never_splits_a_character() {
        // Cutting by bytes would panic on a multi-byte name.
        let cut = truncate_to_width("Größe über alles", 40.0, size::BODY);
        assert!(cut.is_char_boundary(cut.len()));
    }
}
