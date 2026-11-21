//! The built shell (`docs/app-platform.md`, "The builder"): what the server's builder says about the shells it built,
//! and what the launcher (`trailhead app`) and a running shell agree on.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// The exit code a shell ends with to be started again on the newest build. The launcher runs the newest build when
/// a shell exits with it; any other code ends the launcher too.
pub const RESTART_CODE: u8 = 75;

/// The environment variable the launcher sets to the build a shell was started from, so a shell knows which build it
/// is without being compiled to know it. A shell started without it never offers an update.
pub const BUILD_ENV: &str = "TRAILHEAD_SHELL_BUILD";

/// `GET /v1/shell/build`: the builds the server's builder keeps, by the apps repo's commit each was built from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ShellBuild {
    /// Whether `[shell] repo` is set in the server's user config. When it isn't, there is no builder and the rest is
    /// empty.
    pub configured: bool,
    /// The newest build that succeeded.
    pub current: Option<String>,
    /// The build before it, kept to fall back to.
    pub previous: Option<String>,
    /// The commit being built now.
    pub building: Option<String>,
    /// The last build that failed. It is not tried again; a newer commit is.
    pub error: Option<BuildError>,
    /// Builds the launcher found failing at start and fell back from. Never run again, and never built again.
    #[serde(default)]
    pub bad: Vec<String>,
}

impl ShellBuild {
    /// The build to run: the current one, or the previous one when the current one is bad.
    pub fn newest(&self) -> Option<&str> {
        [&self.current, &self.previous].into_iter().flatten().map(String::as_str).find(|r| !self.is_bad(r))
    }

    pub fn is_bad(&self, rev: &str) -> bool {
        self.bad.iter().any(|b| b == rev)
    }

    /// The build to fall back to from `rev`: the previous one, when it is another build and not bad.
    pub fn fallback_from(&self, rev: &str) -> Option<&str> {
        self.previous.as_deref().filter(|p| *p != rev && !self.is_bad(p))
    }
}

/// A build that failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BuildError {
    pub rev: String,
    pub message: String,
    /// The last lines of cargo's output; the whole of it is in `build.log` beside the builds.
    pub output: String,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(current: Option<&str>, previous: Option<&str>, bad: &[&str]) -> ShellBuild {
        ShellBuild {
            configured: true,
            current: current.map(Into::into),
            previous: previous.map(Into::into),
            bad: bad.iter().map(|b| b.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn the_newest_build_is_the_current_one_unless_it_is_bad() {
        assert_eq!(build(Some("b"), Some("a"), &[]).newest(), Some("b"));
        assert_eq!(build(Some("b"), Some("a"), &["b"]).newest(), Some("a"));
        assert_eq!(build(Some("b"), Some("a"), &["b", "a"]).newest(), None);
        assert_eq!(build(None, None, &[]).newest(), None);
    }

    #[test]
    fn the_fallback_is_the_previous_build_when_it_is_another_and_not_bad() {
        assert_eq!(build(Some("b"), Some("a"), &[]).fallback_from("b"), Some("a"));
        assert_eq!(build(Some("b"), Some("a"), &["a"]).fallback_from("b"), None);
        assert_eq!(build(Some("b"), Some("a"), &["b"]).fallback_from("a"), None);
        assert_eq!(build(Some("b"), None, &[]).fallback_from("b"), None);
    }
}
