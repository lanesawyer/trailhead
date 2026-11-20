//! The shell keeps to the kit's design system: the `trailhead-ui` lint's rules over its sources.

use trailhead_ui::lint::{self, Allowed};

/// Where the shell steps off, and why. Add to this only with a reason; an entry that finds nothing fails.
const ALLOWED: &[Allowed] = &[];

#[test]
fn the_shell_keeps_to_the_design_system() {
    let found = lint::check(env!("CARGO_MANIFEST_DIR"), &["src", "tests", "Cargo.toml"], lint::APP, ALLOWED);
    assert!(found.is_empty(), "\n{}", found.join("\n"));
}
