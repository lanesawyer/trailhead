//! Go: its toolchain's download hosts. `go.sum` isn't read: a checksum line can't be told from a new module.

use super::Language;

pub struct Go;

impl Language for Go {
    fn name(&self) -> &'static str {
        "go"
    }

    fn tool_hosts(&self, backend: &str, name: &str) -> Option<&'static [&'static str]> {
        match (backend, name) {
            ("", "go") => Some(&["go.dev", "dl.google.com"]),
            _ => None,
        }
    }
}
