//! Node: npm and its hosts, `package-lock.json`.

use super::{Language, LockLine};

pub struct Node;

impl Language for Node {
    fn name(&self) -> &'static str {
        "node"
    }

    fn tool_hosts(&self, backend: &str, name: &str) -> Option<&'static [&'static [&'static str]]> {
        match (backend, name) {
            ("npm", _) => Some(&[&["registry.npmjs.org"]]),
            ("", "node") => Some(&[&["nodejs.org"]]),
            // aqua's registry fetches pnpm from its GitHub releases.
            ("", "pnpm") => Some(&[crate::daemon::GITHUB]),
            _ => None,
        }
    }

    /// `pnpm-lock.yaml` and `yarn.lock` put a dependency's version where a patch can't tell it from a new one, so
    /// they stay raised.
    fn lockfile(&self, file_name: &str) -> Option<LockLine> {
        (file_name == "package-lock.json").then_some(npm_line_ok as LockLine)
    }
}

/// `package-lock.json` lists a dependency as `"name": "range"` inside a `dependencies` object. A `version` or
/// `resolved` has the same shape, and a range that is a URL, path, git spec or alias fetches something new.
fn npm_line_ok(line: &str) -> bool {
    const OBJECTS: [&str; 4] =
        ["\"dependencies\": {", "\"devDependencies\": {", "\"peerDependencies\": {", "\"optionalDependencies\": {"];
    if OBJECTS.contains(&line) || line == "}" || line == "}," {
        return true;
    }
    let Some((name, range)) = line.trim_end_matches(',').split_once("\": \"") else { return false };
    let Some(range) = range.strip_suffix('"') else { return false };
    let name = name.strip_prefix('"').unwrap_or_default();
    let not_a_dependency = matches!(name, "version" | "resolved" | "integrity" | "name" | "license");
    !not_a_dependency && !name.contains('"') && !range.contains(['"', ':', '/', '\\'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_dependency_ranges_are_ok() {
        for ok in ["\"dependencies\": {", "\"a\": \"^1.0.0\",", "\"b\": \"~2.1.0\"", "},"] {
            assert!(npm_line_ok(ok), "{ok}");
        }
        for bad in [
            "\"version\": \"1.0.0\"",
            "\"resolved\": \"https://r/x.tgz\"",
            "\"b\": \"github:evil/b\"",
            "\"b\": \"file:../b\"",
            "\"node_modules/b\": {",
            "\"integrity\": \"sha512-aa\"",
        ] {
            assert!(!npm_line_ok(bad), "{bad}");
        }
    }
}
