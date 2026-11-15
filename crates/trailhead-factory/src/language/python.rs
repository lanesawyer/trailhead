//! Python: PyPI and the interpreter's hosts, `uv.lock`.

use super::{Language, LockLine};

pub struct Python;

const PYPI: &[&str] = &["pypi.org", "files.pythonhosted.org"];

impl Language for Python {
    fn name(&self) -> &'static str {
        "python"
    }

    fn tool_hosts(&self, backend: &str, name: &str) -> Option<&'static [&'static str]> {
        match (backend, name) {
            ("pipx" | "pypi", _) => Some(PYPI),
            // Prebuilt interpreters are GitHub release assets; pip comes from PyPI.
            ("", "python") => Some(&[
                "github.com",
                "api.github.com",
                "objects.githubusercontent.com",
                "release-assets.githubusercontent.com",
                "pypi.org",
                "files.pythonhosted.org",
            ]),
            _ => None,
        }
    }

    /// `poetry.lock` and the rest stay raised: they put a dependency's version where a patch can't tell it from a new one.
    fn lockfile(&self, file_name: &str) -> Option<LockLine> {
        (file_name == "uv.lock").then_some(uv_line_ok as LockLine)
    }
}

/// `uv.lock` lists a dependency as an inline table, `{ name = "x" },`, in `dependencies` and in the member's
/// `requires-dist`, and in the dev groups and extras (`dev = [`, `[package.metadata.requires-dev]`). Only the keys that
/// narrow an already-locked dependency are allowed (`name`, `specifier`, `marker`, `extra`, `extras`); a table that says
/// where to fetch from, or carries a `source` or `version`, is a new source.
fn uv_line_ok(line: &str) -> bool {
    const HEADERS: [&str; 4] = [
        "[package.metadata]",
        "[package.dev-dependencies]",
        "[package.optional-dependencies]",
        "[package.metadata.requires-dev]",
    ];
    if line == "]" || HEADERS.contains(&line) || uv_entry_ok(line) {
        return true;
    }
    // `dependencies = [`, `requires-dist = [`, and a dev group or extra's own name (`dev = [`, `"my.extra" = [`),
    // alone on its line or, as `requires-dev` writes it, with its entry inline. Only entries are accepted, so an
    // opener for another list (`wheels = [`) still can't smuggle a source in.
    let Some((name, rest)) = line.split_once(" = [") else { return false };
    let name = name.trim_matches('"');
    let name_ok = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    name_ok && (rest.is_empty() || rest.strip_suffix(']').is_some_and(|inner| uv_entry_ok(inner.trim())))
}

/// Whether `line` is one inline table whose keys are all known and whose first is `name`, however it is spaced.
fn uv_entry_ok(line: &str) -> bool {
    const KEYS: [&str; 5] = ["name", "specifier", "marker", "extra", "extras"];
    let line = line.strip_suffix(',').unwrap_or(line);
    let Some(inner) = line.strip_prefix('{').and_then(|l| l.strip_suffix('}')) else { return false };
    // Split on the commas outside strings and brackets. A `{` outside a string is a nested table: refused.
    let mut fields: Vec<&str> = Vec::new();
    let (mut start, mut depth) = (0, 0usize);
    let mut quote: Option<char> = None;
    let mut chars = inner.char_indices();
    while let Some((i, c)) = chars.next() {
        match (quote, c) {
            (Some('"'), '\\') => {
                chars.next();
            }
            (Some(q), _) => {
                if c == q {
                    quote = None;
                }
            }
            (None, '"' | '\'') => quote = Some(c),
            (None, '[') => depth += 1,
            (None, ']') => match depth.checked_sub(1) {
                Some(d) => depth = d,
                None => return false,
            },
            (None, '{' | '}') => return false,
            (None, ',') if depth == 0 => {
                fields.push(&inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if quote.is_some() || depth != 0 {
        return false;
    }
    fields.push(&inner[start..]);
    let mut keys = fields.iter().map(|f| f.split_once('=').map(|(k, _)| k.trim()));
    keys.next() == Some(Some("name")) && keys.all(|k| k.is_some_and(|k| KEYS.contains(&k)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_narrowing_keys_of_a_known_dependency_are_ok() {
        for ok in [
            "{ name = \"x\",specifier=\">=1,<2\" },",
            "{ name = \"x\", marker = \"python_full_version >= '3.9' and sys_platform == 'linux'\" },",
            "{ name = \"x\", extra = [\"a\", \"b\"], marker = \"url = 1\" },",
            "requires-dist = [",
        ] {
            assert!(uv_line_ok(ok), "{ok}");
        }
        for bad in [
            "{ name = \"x\", git = \"https://x\" },",
            "{ name = \"x\", url = \"https://x\" },",
            "{ name = \"x\", index = \"https://x/simple\" },",
            "{ name = \"x\",url=\"https://x\" },",
            "{ name = \"x\", source = { url = \"https://x\" } },",
            "{ name = \"x\", version = \"9\" },",
            "{ name = \"x\", specifier = \"a, url = b\" , path = \"u\" },",
            "{ specifier = \">=1\", name = \"x\" },",
            "{ name = \"x\" } junk",
            "version = \"1.0.0\"",
            "[[package]]",
        ] {
            assert!(!uv_line_ok(bad), "{bad}");
        }
    }
}
