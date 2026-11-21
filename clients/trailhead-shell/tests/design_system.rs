//! The shell keeps to the kit's design system: the `trailhead-ui` lint's rules over its sources, and the kit's scan for
//! a surface redrawn wholesale.

use trailhead_ui::lint::{self, Allowed, AllowedRedraw};

/// Where the shell steps off, and why. Add to this only with a reason; an entry that finds nothing fails.
const ALLOWED: &[Allowed] =
    &[Allowed { path: "src/icon.rs", rule: "palette", why: "draws the window icon's pixels, which no theme repaints" }];

#[test]
fn the_shell_keeps_to_the_design_system() {
    let found =
        lint::check(env!("CARGO_MANIFEST_DIR"), &["src", "tests", "examples", "Cargo.toml"], lint::APP, ALLOWED);
    assert!(found.is_empty(), "\n{}", found.join("\n"));
}

/// Functions that despawn and spawn again to open and close a surface. Never for a change in what it shows.
const REDRAWS: &[AllowedRedraw] = &[];

#[test]
fn the_shell_updates_in_place() {
    let found = lint::redraws(env!("CARGO_MANIFEST_DIR"), &["src"], REDRAWS);
    assert!(found.is_empty(), "\n{}", found.join("\n"));
}
