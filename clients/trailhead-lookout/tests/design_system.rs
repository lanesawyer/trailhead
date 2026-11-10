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
    if code.contains("Name::new") || is_http_status(code) {
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

/// A line about an HTTP response's status, a number and not a task's `Status`: a `{status}` there is fine.
/// Lookout words every response as "the server answered {status}".
fn is_http_status(code: &str) -> bool {
    ["StatusCode", ".as_u16()", "answered {"].iter().any(|p| code.contains(p))
}

/// Where Lookout steps off, and why. Add to this only with a reason; an entry that finds nothing fails.
const ALLOWED: &[Allowed] = &[
    Allowed { path: "src/widgets/theme.rs", rule: "color-literal", why: "builds Lookout's theme" },
    Allowed { path: "src/icon.rs", rule: "palette", why: "draws the window icon's pixels, which no theme repaints" },
    Allowed { path: "tests/design_system.rs", rule: "*", why: "names what the rules find" },
];

#[test]
fn lookout_keeps_to_the_design_system() {
    let rules: Vec<Rule> = lint::APP.iter().copied().chain([STATUS_LABEL]).collect();
    let found = lint::check(env!("CARGO_MANIFEST_DIR"), &["src", "tests", "examples", "Cargo.toml"], &rules, ALLOWED);
    assert!(found.is_empty(), "\n{}", found.join("\n"));
}

fn finds(code: &str) -> bool {
    formats_a_status(&Line { path: "src/x.rs", code, in_tests: false })
}

#[test]
fn a_task_status_formatted_as_it_is_is_found() {
    assert!(finds(r#"format!("now {status}")"#));
    assert!(finds("text(status, size::BODY)"));
    assert!(finds("select(statuses, None, 4)"));
}

#[test]
fn an_http_status_is_not_a_task_status() {
    assert!(!finds(r#"format!("the server answered {status}")"#));
    assert!(!finds(r#"if status == reqwest::StatusCode::NOT_FOUND { log("{status}") }"#));
}

#[test]
fn a_task_status_beside_a_closed_check_or_a_url_is_still_found() {
    assert!(finds(r#"if !task.status.is_closed() { format!("{status}") }"#));
    assert!(finds(r#"let url = self.http.get(u); format!("{status}")"#));
}
