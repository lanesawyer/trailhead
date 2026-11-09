//! How far apart things are, in one place.
//!
//! [`step`] is the scale: few values, far enough apart to tell neighbors apart. [`space`] names what a space is for,
//! and each name points at a step. UI code uses [`space`], never a step and never a literal, so changing how roomy
//! an app is is an edit here. A space with a new meaning gets a new name here. [`crate::lint`]'s `spacing` rule finds
//! a gap, padding or margin written as a number.
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

    /// The join between controls drawn as one, such as a button group's options. A seam rather than a space, so it
    /// sits off the scale.
    pub const SEAM: f32 = 1.0;
    /// Lines within one item: a card's key over its title, a field's label over its input.
    pub const STACKED: f32 = step::XXS;
    /// An icon and the text beside it, inside one control.
    pub const ICON_LABEL: f32 = step::XS;
    /// Controls side by side in a row: a card's icon buttons, the badges on a card.
    pub const CONTROLS: f32 = step::XS;
    /// Rows of a list that read as one block: placeholder bars.
    pub const LIST_ITEMS: f32 = step::XS;
    /// Cards stacked in one column.
    pub const CARDS: f32 = step::S;
    /// Rows of controls in a section or panel.
    pub const ROWS: f32 = step::S;
    /// Groups of rows within one panel, and columns side by side.
    pub const GROUPS: f32 = step::M;
    /// How far a nested item sits in from its parent: a child's card under its parent's.
    pub const INDENT: f32 = step::M;
    /// The least room a hint keeps from the edge of the window, or of the surface it stays inside.
    pub const HINT_MARGIN: f32 = step::XS;

    /// The colored dot of a status chip, across.
    pub const DOT: f32 = step::S;

    /// Inside a badge.
    pub const BADGE_INSET: f32 = step::XXS;
    /// Inside a small labelled box, tight against its edge.
    pub const LABEL_INSET: f32 = step::XS;
    /// Inside an inline text's input, before and after the text: the input reaches out by as much, so its text lines
    /// up with the text it stands in for.
    pub const INPUT_INSET: f32 = step::XS;
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

/// How big a panel is and where it sits.
pub mod extent {
    /// An app's sidebar, down the left of the window, until its edge is dragged.
    pub const SIDEBAR_WIDTH: f32 = 220.0;
    /// The narrowest a sidebar is dragged to: its places' names and its footer still read.
    pub const SIDEBAR_MIN_WIDTH: f32 = 180.0;
    /// The widest a sidebar is dragged to, so it never crowds out the page beside it.
    pub const SIDEBAR_MAX_WIDTH: f32 = 420.0;
    /// How wide the strip along a sidebar's edge that drags it is, half over the sidebar and half over the page.
    pub const DRAG_EDGE: f32 = 6.0;
    /// A panel down the right of the window, beside the page: an inspector.
    pub const SIDE_PANEL_WIDTH: f32 = 360.0;
    /// A modal's width, unless its kind says otherwise.
    pub const MODAL_WIDTH: f32 = 640.0;
    /// A search box's width. Its hint is laid over the field rather than in it, so without one the box shrinks to
    /// nearly nothing in a row of controls.
    pub const SEARCH_WIDTH: f32 = 220.0;
    /// The label's column in a sidebar field, so the values line up down the sidebar.
    pub const FIELD_LABEL_WIDTH: f32 = 90.0;
    /// A detail modal's width: room for a main column and its sidebar (`spawn_detail_panes`).
    pub const DETAIL_WIDTH: f32 = 960.0;
    /// The sidebar of facts in a detail modal.
    pub const DETAIL_SIDEBAR_WIDTH: f32 = 280.0;
}

/// How thick a line is: a border, a rule between panes.
pub mod stroke {
    /// The thinnest line: a border, a divider.
    pub const HAIRLINE: f32 = 1.0;
    /// The line a drag edge shows under the pointer and while dragged.
    pub const GRIP: f32 = 2.0;
}

#[cfg(test)]
mod tests {
    use super::step;

    const STEPS: [f32; 4] = [step::XXS, step::XS, step::S, step::M];

    #[test]
    fn every_space_is_a_step() {
        // Read from this file, so a new space is checked without being listed. SEAM is a join, off the scale.
        let source = include_str!("spacing.rs");
        let body = source.split("pub mod space {").nth(1).unwrap().split("\n}").next().unwrap();
        let spaces: Vec<&str> = body.lines().filter_map(|l| l.trim().strip_prefix("pub const ")).collect();
        assert!(spaces.len() >= 10, "found only {spaces:?}");
        for space in spaces {
            let (name, value) = space.split_once(": f32 = ").unwrap();
            assert!(name == "SEAM" || value.starts_with("step::"), "{name} is off the scale: {value}");
        }
    }

    #[test]
    fn neighboring_steps_are_far_enough_apart_to_tell_apart() {
        for pair in STEPS.windows(2) {
            assert!(pair[1] / pair[0] >= 4.0 / 3.0, "{pair:?} are too close");
        }
    }
}
