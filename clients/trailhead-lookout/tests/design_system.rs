//! Lookout keeps to the kit's design system: the `trailhead-ui` lint's rules over its sources, plus Lookout's own.

use trailhead_model::Status;
use trailhead_ui::lint::{self, Allowed, Line, Rule};

/// A status shown to a person goes through `widgets::status_label`, the one way every view writes one.
const STATUS_LABEL: Rule = Rule {
    name: "status-label",
    fix: "show a status with widgets::status_label",
    in_tests: false,
    finds: formats_a_status,
};

/// The shapes that skip `status_label`: a status formatted or listed as it is, or its words spelled out by hand.
fn formats_a_status(line: &Line) -> bool {
    let code = line.code.trim();
    if code.contains("Name::new") {
        return false;
    }
    let skipping = [
        "{status}",
        "{st}",
        "text(status,",
        "text(status.",
        "select(statuses",
        "Status::ALL.iter().map(|s| s.to_string())",
    ];
    let own_words = Status::ALL.iter().map(|s| s.as_str()).flat_map(|s| {
        let spaced = s.replace('_', " ");
        // A single word is the API value too (`column_shell("open")`), so only a (text, color) pair is a label.
        let spelled = if spaced == *s { format!("(\"{s}\".into(),") } else { format!("\"{spaced}\"") };
        [spelled, format!("\"{}\"", spaced.to_uppercase())]
    });
    skipping.iter().any(|p| code.contains(p)) || own_words.into_iter().any(|p| code.contains(&p))
}

/// Where Lookout steps off, and why. Add to this only with a reason; an entry that finds nothing fails.
const ALLOWED: &[Allowed] = &[
    Allowed { path: "src/widgets/theme.rs", rule: "color-literal", why: "builds Lookout's theme" },
    Allowed { path: "src/icon.rs", rule: "palette", why: "draws the window icon's pixels, which no theme repaints" },
    Allowed { path: "src/map.rs", rule: "color-literal", why: "the map isn't on tokens yet (TRAIL-1153)" },
    Allowed { path: "src/map.rs", rule: "raw-color", why: "the map isn't on tokens yet (TRAIL-1153)" },
    Allowed { path: "src/map.rs", rule: "font-size", why: "the map isn't on the kit's text yet (TRAIL-1153)" },
    Allowed { path: "src/map.rs", rule: "position", why: "the map isn't on the kit's spacing yet (TRAIL-1153)" },
    Allowed { path: "tests/design_system.rs", rule: "*", why: "names what the rules find" },
];

#[test]
fn lookout_keeps_to_the_design_system() {
    let rules: Vec<Rule> = lint::APP.iter().copied().chain([STATUS_LABEL]).collect();
    let found = lint::check(env!("CARGO_MANIFEST_DIR"), &["src", "tests", "examples", "Cargo.toml"], &rules, ALLOWED);
    assert!(found.is_empty(), "\n{}", found.join("\n"));
}
