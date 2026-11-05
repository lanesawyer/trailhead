//! How far apart things are, in one place.
//!
//! [`step`] is the scale: few values, far enough apart to tell neighbors apart. [`space`] names what a space is for,
//! and each name points at a step. UI code uses [`space`], never a step and never a literal, so changing how roomy
//! Lookout is is an edit here. A space with a new meaning gets a new name here. `a_spacing_is_named_rather_than_typed`
//! fails on any gap, padding or margin written as a number.
//!
//! Ported from bevy-data-explorer's `widgets/spacing.rs`.

/// The scale every space is drawn from: multiples of four, but the smallest, which lines up with feathers. Larger
/// steps join when a space needs one.
pub mod step {
    pub const XXS: f32 = 2.0;
    pub const XS: f32 = 4.0;
    pub const S: f32 = 8.0;
    pub const M: f32 = 12.0;
}

/// What a space is for.
pub mod space {
    use super::step;

    /// Lines within one item: a card's key over its title, a field's label over its input.
    pub const STACKED: f32 = step::XXS;
    /// An icon and the text beside it, inside one control.
    pub const ICON_LABEL: f32 = step::XS;
    /// Controls side by side in a row: a card's icon buttons, the badges on a card.
    pub const CONTROLS: f32 = step::XS;
    /// Cards stacked in one column.
    pub const CARDS: f32 = step::S;
    /// Rows of controls in a section or panel.
    pub const ROWS: f32 = step::S;
    /// Groups of rows within one panel, and the board's columns side by side.
    pub const GROUPS: f32 = step::M;
    /// How far a subtask's card sits in from its parent's.
    pub const INDENT: f32 = step::M;

    /// Inside a badge.
    pub const BADGE_INSET: f32 = step::XXS;
    /// Inside a card, a field, the hint over a button.
    pub const CONTROL_INSET: f32 = step::S;
    /// Inside a panel, a column, the header bar.
    pub const PANEL_INSET: f32 = step::M;
}

/// Corner radii, in one place for the same reason.
pub mod radius {
    /// A badge or a hint.
    pub const SMALL: f32 = 3.0;
    /// A card, a field, a button (feathers rounds its own at 4).
    pub const CONTROL: f32 = 4.0;
    /// A column or a panel.
    pub const PANEL: f32 = 6.0;
}

#[cfg(test)]
mod tests {
    use super::{space, step};

    const STEPS: [f32; 4] = [step::XXS, step::XS, step::S, step::M];

    /// Views written before the kit, each moved onto it by its own task (TRAIL-798 to TRAIL-801). Take a file off
    /// this list when it's moved; nothing new goes on it.
    const NOT_YET_ON_THE_KIT: [&str; 4] = ["board.rs", "author.rs", "inspector.rs", "connect.rs"];

    #[test]
    fn every_space_is_a_step() {
        for named in [
            space::STACKED,
            space::ICON_LABEL,
            space::CONTROLS,
            space::CARDS,
            space::ROWS,
            space::GROUPS,
            space::INDENT,
            space::BADGE_INSET,
            space::CONTROL_INSET,
            space::PANEL_INSET,
        ] {
            assert!(STEPS.contains(&named), "{named} is off the scale");
        }
    }

    #[test]
    fn neighboring_steps_are_far_enough_apart_to_tell_apart() {
        for pair in STEPS.windows(2) {
            assert!(pair[1] / pair[0] >= 4.0 / 3.0, "{pair:?} are too close");
        }
    }

    /// Whether `line` writes a gap, padding or margin as a number: `px(8)`, `Val::Px(8.0)`, or a constant named for
    /// one.
    fn types_a_spacing(line: &str) -> bool {
        let code = line.split("//").next().unwrap_or("");
        if let Some((name, value)) = code.split_once("const ").and_then(|(_, rest)| rest.split_once(": f32 = ")) {
            let spacing = ["GAP", "INSET", "INDENT", "MARGIN", "PADDING"].iter().any(|word| name.contains(word));
            return spacing && value.trim_start().starts_with(|c: char| c.is_ascii_digit());
        }
        if !["gap", "padding", "margin"].iter().any(|word| code.contains(word)) {
            return false;
        }
        ["Val::Px(", "px("].iter().any(|call| {
            code.match_indices(call).any(|(at, found)| {
                // `px(` also ends `Val::Px(`'s lowercase cousins; only a call that starts a word counts.
                let before = code[..at].chars().last();
                if *call == "px(" && before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == ':') {
                    return false;
                }
                let rest = code[at + found.len()..].trim_start_matches('-');
                rest.starts_with(|c: char| c.is_ascii_digit()) && !rest.starts_with("0)") && !rest.starts_with("0.0)")
            })
        })
    }

    #[test]
    fn the_check_finds_what_it_is_meant_to() {
        assert!(types_a_spacing("row_gap: px(6),"));
        assert!(types_a_spacing("padding: UiRect::all(px(8)),"));
        assert!(types_a_spacing("row_gap: { Val::Px(6.0) },"));
        assert!(types_a_spacing("margin: UiRect::left(px(16 * indent)),"));
        assert!(types_a_spacing("const CHROME_GAP: f32 = 4.0;"));
        assert!(!types_a_spacing("row_gap: px(space::ROWS),"));
        assert!(!types_a_spacing("padding: UiRect::all(px(0)),"));
        assert!(!types_a_spacing("width: px(260),"));
        assert!(!types_a_spacing("// row_gap: px(6) was too loose"));
        assert!(!types_a_spacing("const CHROME_GAP: f32 = space::CONTROLS;"));
    }

    #[test]
    fn a_spacing_is_named_rather_than_typed() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut typed = Vec::new();
        let mut folders = vec![root];
        while let Some(folder) = folders.pop() {
            for entry in std::fs::read_dir(&folder).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    folders.push(path);
                    continue;
                }
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
                // This file writes numbers for the scale itself, and in the examples the check is tested against.
                if path.extension().is_none_or(|ext| ext != "rs")
                    || path.ends_with("widgets/spacing.rs")
                    || NOT_YET_ON_THE_KIT.contains(&name)
                {
                    continue;
                }
                let source = std::fs::read_to_string(&path).unwrap();
                for (number, line) in source.lines().enumerate() {
                    if types_a_spacing(line) {
                        typed.push(format!("{}:{}: {}", path.display(), number + 1, line.trim()));
                    }
                }
            }
        }
        assert!(typed.is_empty(), "name these spacings with widgets::space instead:\n{}", typed.join("\n"));
    }
}
