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

/// The environment variable a launcher that builds the shell on its own machine (`[shell] from`) sets to its
/// builder's state file. A shell started with it offers an update when that file names a newer build than its own,
/// not when the server does: the server's newest build isn't one this machine can start until it has built it too.
pub const LOCAL_STATE_ENV: &str = "TRAILHEAD_SHELL_LOCAL_STATE";

/// The most commit subjects [`ShellBuild::changes`] lists.
pub const MAX_CHANGES: usize = 30;

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
    /// Asked with `?since=<rev>` (the build a shell runs): the subject lines of the commits after it up to
    /// [`ShellBuild::newest`], newest first, at most [`MAX_CHANGES`] of them. Empty without `since`, when `since` is
    /// the newest build or unknown to the builder, and when there is no builder.
    #[serde(default)]
    pub changes: Vec<String>,
    /// How many commits there are after `since` in all, of which `changes` lists the newest.
    #[serde(default)]
    pub changes_total: usize,
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

/// The most characters an ask's text may have.
pub const MAX_ASK_CHARS: usize = 8000;

/// `GET /v1/shell/ask`: what a person can ask for work on from the window.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AskTargets {
    /// Whether `[shell] repo` is set in the server's user config. When it isn't, nothing can be asked for and the
    /// rest is empty.
    pub configured: bool,
    /// The apps repo, `owner/name`, from its checkout's `origin`: `None` until the builder has read it, or when it
    /// isn't on GitHub.
    pub repo: Option<String>,
    /// The active projects pointed at the apps repo (`metadata.factory.repo`), by name.
    #[serde(default)]
    pub targets: Vec<AskTarget>,
}

/// A project that can be asked for a change: one app in the apps repo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AskTarget {
    /// The project's key.
    pub project: String,
    pub name: String,
    /// Where in the apps repo it builds (`apps/ticklist`); `None` for the whole repo.
    pub path: Option<String>,
}

/// `POST /v1/shell/ask`: a change to an app (`project`) or a new app (`new_app`), exactly one of the two.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Ask {
    /// What the person wants, in their words: it becomes the task's description. At most [`MAX_ASK_CHARS`].
    pub text: String,
    /// The key of one of [`AskTargets::targets`], for a change to that app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// A new app, with a project of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_app: Option<NewApp>,
}

impl Ask {
    /// A change to the app in `project`.
    pub fn change(text: impl Into<String>, project: impl Into<String>) -> Ask {
        Ask { text: text.into(), project: Some(project.into()), new_app: None }
    }

    /// A new app called `name`.
    pub fn new_app(text: impl Into<String>, name: impl Into<String>) -> Ask {
        Ask { text: text.into(), project: None, new_app: Some(NewApp { name: name.into() }) }
    }
}

/// The app a person asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NewApp {
    /// What it is called: its id, its project's key and the project's name come from it.
    pub name: String,
}

/// What an ask made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Asked {
    pub project: AskedProject,
    /// The task made. `None` only when a new app's project was made but its first task wasn't; `notes` says why.
    pub task: Option<AskedTask>,
    /// What a person should know: a part that wasn't done, and how to do it by hand.
    #[serde(default)]
    pub notes: Vec<String>,
}

/// The project an ask went to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AskedProject {
    pub key: String,
    pub name: String,
    /// Whether the ask made it (a new app).
    pub created: bool,
}

/// The task an ask made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AskedTask {
    pub key: String,
    pub title: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ask_says_only_what_it_is_about() {
        let change = serde_json::to_value(Ask::change("more", "TICK")).unwrap();
        assert_eq!(change, serde_json::json!({"text": "more", "project": "TICK"}));
        let new = serde_json::to_value(Ask::new_app("notes", "Trail Notes")).unwrap();
        assert_eq!(new, serde_json::json!({"text": "notes", "new_app": {"name": "Trail Notes"}}));
    }

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
