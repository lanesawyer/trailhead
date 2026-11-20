//! The example app keeps to the design system and reaches nothing outside itself: the shell's app lint over its sources.

use trailhead_shell::lint::{self, APP, Allowed};

/// Where the app steps off, and why. Add to this only with a reason; an entry that finds nothing fails.
const ALLOWED: &[Allowed] = &[];

#[test]
fn the_example_app_keeps_to_the_design_system() {
    let found = lint::check(env!("CARGO_MANIFEST_DIR"), &["src", "tests", "Cargo.toml"], APP, ALLOWED);
    assert!(found.is_empty(), "\n{}", found.join("\n"));
}
