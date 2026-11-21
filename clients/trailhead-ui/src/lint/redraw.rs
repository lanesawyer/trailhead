//! The guard for "a surface is spawned once and updated in place" (TRAIL-880): a scan for the shape of a wholesale
//! redraw.
//!
//! Despawning all of a parent's children and spawning them again whenever something changes flashes the surface,
//! closes a dropdown under the pointer and drops scroll positions. Flagged: a `despawn_children` / `despawn_related`
//! call, a `despawn()` of a loop variable named `child`, a `despawn` inside a function named `redraw*`, and any
//! function that despawns inside a loop (`for`, `while`, `loop`, nested or not, or a `for_each` closure) and then
//! spawns, whatever it is named. Opening and closing a surface (a panel or modal on a state change) is not a redraw,
//! and goes on the caller's list as an [`AllowedRedraw`]. Test modules are not scanned: a test may despawn what it
//! likes.

use std::path::Path;

use super::{sources, test_lines};

/// A function that may despawn and spawn again, and why. Only for opening and closing a surface, never for a change in
/// what it shows.
pub struct AllowedRedraw {
    /// The file, relative to the crate's root: `src/author.rs`.
    pub path: &'static str,
    /// The function's name.
    pub function: &'static str,
    /// What surface it opens or closes.
    pub why: &'static str,
}

/// Every function in the `.rs` files under `dirs` (folders or single files, relative to `crate_root`) that redraws a
/// surface wholesale, as `path: function`, less what `allowed` lets through, plus each `allowed` entry that let
/// nothing through.
pub fn redraws(crate_root: impl AsRef<Path>, dirs: &[&str], allowed: &[AllowedRedraw]) -> Vec<String> {
    let crate_root = crate_root.as_ref();
    let mut found = Vec::new();
    let mut used = vec![false; allowed.len()];
    for path in sources(crate_root, dirs) {
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let relative = path.strip_prefix(crate_root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        let source = std::fs::read_to_string(&path).unwrap_or_default();
        let tests = test_lines(&source);
        let code: Vec<&str> = source.lines().zip(tests).filter(|(_, test)| !test).map(|(line, _)| line).collect();
        for function in flagged(&code.join("\n")) {
            match allowed.iter().position(|a| a.path == relative && a.function == function) {
                Some(at) => used[at] = true,
                None => found.push(format!(
                    "{relative}: {function} despawns a parent's children to redraw it; spawn once and update in place, \
                     or use the kit's Keyed"
                )),
            }
        }
    }
    for (entry, used) in allowed.iter().zip(used) {
        if !used {
            found.push(format!(
                "{}: {} is allowed but no longer redraws wholesale: take it off the list",
                entry.path, entry.function
            ));
        }
    }
    found
}

/// The text after `fn ` when the line starts a function, past any visibility (`pub`, `pub(crate)`, `pub(in path)`)
/// and qualifiers (`const`, `async`, `unsafe`, `extern "C"`).
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

/// The functions in `source` that redraw wholesale, in the order they come.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kit_redraws_nothing_wholesale() {
        let allowed = &[
            AllowedRedraw {
                path: "src/hint.rs",
                function: "hide_hint",
                why: "closes a hint, which lives as a child of what it describes",
            },
            AllowedRedraw {
                path: "src/hint.rs",
                function: "show_hint",
                why: "opens a hint, first closing any other still shown",
            },
            AllowedRedraw { path: "src/lint/redraw.rs", function: "redraws", why: "names what the scan finds" },
            AllowedRedraw { path: "src/lint/redraw.rs", function: "flagged", why: "names what the scan finds" },
        ];
        let found = redraws(env!("CARGO_MANIFEST_DIR"), &["src"], allowed);
        assert!(found.is_empty(), "\n{}", found.join("\n"));
    }

    #[test]
    fn an_allowed_function_that_no_longer_redraws_is_reported() {
        let stale = &[AllowedRedraw { path: "src/badge.rs", function: "badge", why: "never did" }];
        let found = redraws(env!("CARGO_MANIFEST_DIR"), &["src/badge.rs"], stale);
        assert_eq!(found, ["src/badge.rs: badge is allowed but no longer redraws wholesale: take it off the list"]);
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
        for qualifier in
            ["pub(crate) fn", "async fn", "pub const fn", "pub(crate) async fn", "pub(in crate::a) unsafe fn"]
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
}
