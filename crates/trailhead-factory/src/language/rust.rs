//! Rust: the toolchain's host, `Cargo.lock`, and `.rs` sources.

use super::{Language, LockLine};

pub struct Rust;

impl Language for Rust {
    fn name(&self) -> &'static str {
        "rust"
    }

    fn tool_hosts(&self, backend: &str, name: &str) -> Option<&'static [&'static [&'static str]]> {
        match (backend, name) {
            ("", "rust") => Some(&[&["static.rust-lang.org"]]),
            _ => None,
        }
    }

    fn lockfile(&self, file_name: &str) -> Option<LockLine> {
        (file_name == "Cargo.lock").then_some(cargo_line_ok as LockLine)
    }

    fn lock_patch_safe(&self, file_name: &str, patch: &str) -> Option<bool> {
        (file_name == "Cargo.lock").then(|| cargo_patch_safe(patch))
    }

    fn is_source(&self, path: &str) -> bool {
        path.ends_with(".rs")
    }

    /// Every added or removed line is blank or starts with `//` (so `///` and `//!` docs too). A line of code with a
    /// comment after it counts as code, and so does a `/* */` comment. A line inside a multi-line string literal that
    /// happens to start with `//` reads as a comment; that is the one way this can pass a change in behaviour, and it
    /// is rare enough to accept. An empty patch is not comment-only.
    fn is_comment_only(&self, patch: &str) -> bool {
        let mut changed = 0;
        for line in patch.lines() {
            let Some(rest) = line.strip_prefix('+').or_else(|| line.strip_prefix('-')) else { continue };
            changed += 1;
            let rest = rest.trim();
            if !(rest.is_empty() || rest.starts_with("//")) {
                return false;
            }
        }
        changed > 0
    }
}

fn cargo_line_ok(line: &str) -> bool {
    (line.starts_with('"') && line.ends_with("\",")) || line == "dependencies = [" || line == "]"
}

/// Whether a `Cargo.lock` patch brings in nothing new to fetch. Each changed line is blank, edits a dependency list
/// ([`cargo_line_ok`]), or is the `[[package]]`, `name` or `version` line of a workspace package's entry: one with no
/// `source`, which cargo writes for every package it fetches from a registry or git. A new workspace member (an app
/// added to the repo) is such an entry. An entry is read from the hunk's own lines, context included. `source` comes
/// straight after `version`, so a hunk that changes an entry shows the line after its `version` in its context; one
/// that stops at an entry's `version`, where a `source` may follow unseen, is not safe.
fn cargo_patch_safe(patch: &str) -> bool {
    let mut changed = 0;
    let mut hunk: Vec<(bool, &str)> = Vec::new();
    for line in patch.lines().chain(std::iter::once("@@")) {
        if line.starts_with("@@") {
            match hunk_safe(&hunk) {
                Some(n) => changed += n,
                None => return false,
            }
            hunk.clear();
            continue;
        }
        match line.strip_prefix(['+', '-']) {
            Some(text) => hunk.push((true, text.trim())),
            // `\ No newline at end of file` says nothing about an entry.
            None if line.starts_with('\\') => {}
            None => hunk.push((false, line.trim())),
        }
    }
    changed > 0
}

/// How many lines one hunk of a `Cargo.lock` patch changes, or `None` when one of them may bring in something new
/// ([`cargo_patch_safe`]).
fn hunk_safe(hunk: &[(bool, &str)]) -> Option<usize> {
    let mut changed = 0;
    // Entries start at a table header; lines before the first belong to one whose start the hunk doesn't show.
    let starts = hunk.iter().enumerate().filter(|(_, (_, t))| t.starts_with('[')).map(|(i, _)| i);
    let bounds: Vec<usize> = std::iter::once(0).chain(starts).chain(std::iter::once(hunk.len())).collect();
    for entry in bounds.windows(2).map(|w| &hunk[w[0]..w[1]]) {
        let own_entry = entry.first().is_some_and(|(_, t)| *t == "[[package]]")
            && !entry.iter().any(|(_, t)| t.starts_with("source = "))
            && entry.iter().rposition(|(_, t)| quoted(t, "version")).is_some_and(|i| i + 1 < entry.len());
        for (_, text) in entry.iter().filter(|(changed, _)| *changed) {
            changed += 1;
            let own_line = *text == "[[package]]" || quoted(text, "name") || quoted(text, "version");
            if !(text.is_empty() || cargo_line_ok(text) || (own_line && own_entry)) {
                return None;
            }
        }
    }
    Some(changed)
}

/// Whether `line` is `key = "<value>"`.
fn quoted(line: &str, key: &str) -> bool {
    line.strip_prefix(key).and_then(|r| r.strip_prefix(" = \"")).is_some_and(|r| r.len() > 1 && r.ends_with('"'))
}
