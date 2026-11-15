//! The guard for "a surface is spawned once and updated in place" (TRAIL-880).
//!
//! Despawning all of a parent's children and spawning them again whenever something changes flashes the surface,
//! closes a dropdown under the pointer and drops scroll positions (TRAIL-853 for the columns, this task for the
//! header). This scans the view sources for the shape of that redraw and fails on any that is not in [`ALLOWED`].
//!
//! Flagged: a `despawn_children` / `despawn_related` call, a `despawn()` of a loop variable named `child`, and a
//! `despawn` inside a function named `redraw*`, and any function that despawns inside a loop (`for`, `while`, `loop`,
//! nested or not, or a `for_each` closure; say over a
//! marker query) and then spawns, whatever it is named. Opening and closing a surface (a panel or modal on a state change)
//! is not a redraw and is allowed.

use std::fs;
use std::path::Path;

/// (file, function, why). Add to this only for opening and closing a surface, never for a change in what it shows.
const ALLOWED: &[(&str, &str, &str)] = &[
    ("author.rs", "redraw_form", "opens and closes the form's modal"),
    (
        "board/panel.rs",
        "dress_cards",
        "a card is a leaf redrawn only when its own view model changed (TRAIL-853); in place is a follow-up",
    ),
    (
        "board/panel.rs",
        "dress_slots",
        "a column's \"Show more\" button, relabelled when its count changes; in place is a follow-up",
    ),
    ("hint.rs", "hide_hint", "closes a hint, which lives as a child of what it describes"),
    ("hint.rs", "show_hint", "opens a hint, first closing any other still shown"),
    ("select.rs", "set_toned", "a select's rows, respawned only when its options changed; the select and popup stay"),
    ("project_form.rs", "redraw_form", "opens and closes the new project form's modal"),
    ("map.rs", "redraw", "the trail map's terrain is respawned on every model change; in place is a follow-up"),
];

/// The text after `fn ` when the line starts a function, past any visibility
/// (`pub`, `pub(crate)`, `pub(in path)`) and qualifiers (`const`, `async`, `unsafe`, `extern "C"`).
fn fn_name_start(line: &str) -> Option<&str> {
    let mut rest = line;
    if let Some(after) = rest.strip_prefix("pub") {
        rest = after;
        if let Some(inner) = rest.strip_prefix('(') {
            rest = &inner[inner.find(')')? + 1..];
        }
        rest = rest.strip_prefix(' ')?;
    }
    loop {
        if let Some(after) = rest.strip_prefix("fn ") {
            return Some(after);
        }
        if let Some(after) = rest.strip_prefix("extern \"") {
            rest = after.split_once("\" ")?.1;
            continue;
        }
        rest = ["const ", "async ", "unsafe "].iter().find_map(|q| rest.strip_prefix(q))?;
    }
}

/// Whether the line opens a loop body: `for`, `while` (and `while let`) or `loop`, past any label.
fn starts_loop(trimmed: &str) -> bool {
    let rest = match trimmed.strip_prefix('\'') {
        Some(labelled) => labelled.split_once(": ").map_or(trimmed, |(_, after)| after),
        None => trimmed,
    };
    ["for ", "while ", "loop "].iter().any(|k| rest.starts_with(k))
}

fn flagged(source: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut current = String::new();
    let mut loops: Vec<usize> = Vec::new();
    let mut loop_despawn = false;
    for line in source.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        if let Some(rest) = fn_name_start(trimmed) {
            current = rest.split(['(', '<']).next().unwrap_or_default().to_string();
            loops.clear();
            loop_despawn = false;
        }
        let indent = line.len() - trimmed.len();
        while loops.last().is_some_and(|&at| indent <= at && !trimmed.starts_with(['}', '{', ')'])) {
            loops.pop();
        }
        let for_each = trimmed.contains(".for_each(");
        if starts_loop(trimmed) || for_each {
            loops.push(indent);
        }
        let is_despawn = line.contains("despawn");
        let respawn = loop_despawn && !is_despawn && (line.contains("spawn(") || line.contains("spawn_"));
        if is_despawn && (for_each || loops.iter().any(|&at| indent > at)) {
            loop_despawn = true;
        }
        let wholesale = line.contains("despawn_children")
            || line.contains("despawn_related")
            || (line.contains("despawn()") && (line.contains("(*child)") || line.contains("(child)")))
            || (line.contains("despawn") && current.starts_with("redraw"))
            || respawn;
        if wholesale && !found.contains(&current) {
            found.push(current.clone());
        }
    }
    found
}

#[test]
fn no_view_redraws_a_surface_wholesale() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut bad = Vec::new();
    let mut allowed_seen = Vec::new();
    for dir in [root.join("src"), root.join("../trailhead-ui/src")] {
        for path in trailhead_ui::lint::sources(&dir, &["."]) {
            let file = path.strip_prefix(&dir).unwrap().to_str().unwrap().to_string();
            // Not the test modules: a test may despawn what it likes.
            let source = fs::read_to_string(&path).unwrap();
            let tests = trailhead_ui::lint::test_lines(&source);
            let code: Vec<&str> = source.lines().zip(tests).filter(|(_, test)| !test).map(|(line, _)| line).collect();
            for function in flagged(&code.join("\n")) {
                if ALLOWED.iter().any(|(f, name, _)| *f == file && *name == function) {
                    allowed_seen.push((file.clone(), function));
                } else {
                    bad.push(format!("{file}: {function}"));
                }
            }
        }
    }
    assert!(
        bad.is_empty(),
        "these despawn a parent's children to redraw it; spawn once and update in place, or use widgets::keyed \
         (see the trailhead-ui skill, section 4): {bad:?}"
    );
    for (file, name, _) in ALLOWED {
        assert!(
            allowed_seen.iter().any(|(f, n)| f == file && n == name),
            "{file}: {name} is allowed but no longer redraws wholesale; remove it from ALLOWED"
        );
    }
}

#[test]
fn the_scan_sees_a_wholesale_redraw() {
    let bad = "fn dress(children: &Children) {\n    for child in children {\n        commands.entity(*child).despawn();\n    }\n}\n";
    assert_eq!(flagged(bad), ["dress"]);
    let fine = "fn dress() {\n    commands.entity(panel).despawn();\n}\n";
    assert!(flagged(fine).is_empty());
    let marker = "fn refresh(terrain: Query<Entity>) {\n    for e in &terrain {\n        commands.entity(e).despawn();\n    }\n    commands.spawn(Node::default());\n}\n";
    assert_eq!(flagged(marker), ["refresh"]);
    let close_only = "fn close(terrain: Query<Entity>) {\n    for e in &terrain {\n        commands.entity(e).despawn();\n    }\n}\nfn open() {\n    commands.spawn(Node::default());\n}\n";
    assert!(flagged(close_only).is_empty());
    for qualifier in ["pub(crate) fn", "async fn", "pub const fn", "pub(crate) async fn", "pub(in crate::a) unsafe fn"]
    {
        let next = format!(
            "fn close(terrain: Query<Entity>) {{\n    for e in &terrain {{\n        commands.entity(e).despawn();\n    }}\n}}\n{qualifier} open() {{\n    commands.spawn(Node::default());\n}}\n"
        );
        assert!(flagged(&next).is_empty(), "{qualifier}");
    }
    let nested = "fn r(q: Query<Entity>) {\n    for a in &q {\n        for b in &a {\n            touch(b);\n        }\n        commands.entity(a).despawn();\n    }\n    commands.spawn(Node::default());\n}\n";
    assert_eq!(flagged(nested), ["r"]);
    let each = "fn r(q: Query<Entity>) {\n    q.iter().for_each(|e| commands.entity(e).despawn());\n    commands.spawn(Node::default());\n}\n";
    assert_eq!(flagged(each), ["r"]);
    let each_multi = "fn r(q: Query<Entity>) {\n    q.iter()\n        .for_each(|e| {\n            commands.entity(e).despawn();\n        });\n    commands.spawn(Node::default());\n}\n";
    assert_eq!(flagged(each_multi), ["r"]);
    let looped = "fn r(q: Query<Entity>) {\n    while let Some(e) = it.next() {\n        commands.entity(e).despawn();\n    }\n    commands.spawn(Node::default());\n}\n";
    assert_eq!(flagged(looped), ["r"]);
    let header = "fn r(q: Query<Entity>) {\n    for (a, b) in q\n        .iter()\n        .zip(w)\n    {\n        commands.entity(a).despawn();\n    }\n    commands.spawn(Node::default());\n}\n";
    assert_eq!(flagged(header), ["r"]);
}
