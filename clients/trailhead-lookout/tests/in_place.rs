//! The guard for "a surface is spawned once and updated in place" (TRAIL-880): the kit's scan for a wholesale redraw
//! over Lookout's sources. The kit scans its own.

use trailhead_ui::lint::{AllowedRedraw, redraws};

/// Add to this only for opening and closing a surface, never for a change in what it shows.
const ALLOWED: &[AllowedRedraw] = &[
    AllowedRedraw { path: "src/author.rs", function: "redraw_form", why: "opens and closes the form's modal" },
    AllowedRedraw {
        path: "src/project_form.rs",
        function: "redraw_form",
        why: "opens and closes the new project form's modal",
    },
];

#[test]
fn no_view_redraws_a_surface_wholesale() {
    let found = redraws(env!("CARGO_MANIFEST_DIR"), &["src"], ALLOWED);
    assert!(found.is_empty(), "\n{}", found.join("\n"));
}
