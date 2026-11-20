//! Rust: the toolchain's host, `Cargo.lock`, and `.rs` sources.

use super::{Language, LockLine};

pub struct Rust;

impl Language for Rust {
    fn name(&self) -> &'static str {
        "rust"
    }

    fn tool_hosts(&self, backend: &str, name: &str) -> Option<&'static [&'static str]> {
        match (backend, name) {
            ("", "rust") => Some(&["static.rust-lang.org"]),
            _ => None,
        }
    }

    fn lockfile(&self, file_name: &str) -> Option<LockLine> {
        (file_name == "Cargo.lock").then_some(cargo_line_ok as LockLine)
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
