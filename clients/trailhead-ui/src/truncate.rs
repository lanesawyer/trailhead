//! Cutting text to fit a width.
//!
//! Ported from bevy-data-explorer's `widgets/truncate.rs`.

use bevy::prelude::*;

use crate::patch::set_text;
use crate::text::{Size, size};

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

/// A text cut with an ellipsis to the room its parent leaves it beside its siblings. It holds the whole text: to
/// change what it says, set `full`, not the `Text`, which is rewritten to fit.
///
/// The parent's width has to come from outside it (growing into its row, `min_width: 0`), not from hugging what it
/// holds, or each cut would narrow the parent and the next frame would cut again.
#[derive(Component, Clone, PartialEq, Debug)]
pub struct Truncated {
    pub full: String,
    /// The size the text is set at, which the cut is measured by.
    pub size: Size,
}

impl Default for Truncated {
    fn default() -> Self {
        Truncated { full: String::new(), size: size::BODY }
    }
}

/// Cuts each [`Truncated`] text to the room its parent has left once its siblings and the gaps between them are
/// laid out.
pub(crate) fn fit_truncated(
    truncated: Query<(Entity, &Truncated, &ChildOf)>,
    parents: Query<(&ComputedNode, &Node, &Children)>,
    nodes: Query<&ComputedNode>,
    mut texts: Query<&mut Text>,
) {
    for (entity, text, parent) in &truncated {
        let Ok((room, layout, children)) = parents.get(parent.parent()) else { continue };
        let inset = room.content_inset();
        let siblings: f32 =
            children.iter().filter(|c| *c != entity).filter_map(|c| nodes.get(c).ok()).map(|n| n.size.x).sum();
        let physical = room.size.x - inset.min_inset.x - inset.max_inset.x - siblings;
        let gap = match layout.column_gap {
            Val::Px(gap) => gap * children.len().saturating_sub(1) as f32,
            _ => 0.0,
        };
        let width = physical * room.inverse_scale_factor - gap;
        if room.size.x <= 0.0 {
            continue;
        }
        if let Ok(shown) = texts.get_mut(entity) {
            set_text(shown, &truncate_to_width(&text.full, width, text.size));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn computed(width: f32) -> ComputedNode {
        ComputedNode { size: Vec2::new(width, 20.0), inverse_scale_factor: 1.0, ..default() }
    }

    #[test]
    fn a_truncated_text_takes_the_room_its_siblings_and_gaps_leave() {
        let mut app = App::new();
        app.add_systems(Update, fit_truncated);
        let row = app.world_mut().spawn((Node { column_gap: px(4), ..default() }, computed(100.0))).id();
        let full = "Sawyer Suite Tasks and more";
        let caption = app
            .world_mut()
            .spawn((Text::new(full), Truncated { full: full.into(), size: size::BODY }, computed(0.0), ChildOf(row)))
            .id();
        app.world_mut().spawn((computed(30.0), ChildOf(row)));
        app.update();
        let shown = app.world().get::<Text>(caption).unwrap().0.clone();
        assert_eq!(shown, truncate_to_width(full, 100.0 - 30.0 - 4.0, size::BODY));
        assert!(shown.ends_with('\u{2026}'), "{shown}");
        // Wider, it fits whole again.
        app.world_mut().entity_mut(row).insert(computed(400.0));
        app.update();
        assert_eq!(app.world().get::<Text>(caption).unwrap().0, full);
    }
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
