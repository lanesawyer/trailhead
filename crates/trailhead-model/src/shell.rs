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
    /// The apps repo as `owner/name` on GitHub, from its checkout's `origin`, so a shell can link a build to its
    /// commit. `None` before the builder's first look, for an `origin` that isn't on GitHub, and when there is no
    /// builder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
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

/// The models a person can give an ask's planner, as the factory's config names them, newest first.
pub const PLANNER_MODELS: [&str; 4] = ["fable", "opus", "sonnet", "haiku"];

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
    /// Plan it first: the ask becomes a spec task a planner works with the person, not a task for a worker
    /// (`docs/app-platform.md`, "Planning an ask"). A new app is always planned, whatever this says.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub plan: bool,
    /// The planner's model for this ask, one of [`PLANNER_MODELS`]; the repo's planner profile decides when absent.
    /// Only for an ask that is planned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl Ask {
    /// A change to the app in `project`.
    pub fn change(text: impl Into<String>, project: impl Into<String>) -> Ask {
        Ask { text: text.into(), project: Some(project.into()), ..Ask::default() }
    }

    /// A new app called `name`.
    pub fn new_app(text: impl Into<String>, name: impl Into<String>) -> Ask {
        Ask { text: text.into(), new_app: Some(NewApp { name: name.into(), key: None }), ..Ask::default() }
    }

    /// This new app, its project's key chosen by the person.
    pub fn keyed(mut self, key: impl Into<String>) -> Ask {
        if let Some(app) = &mut self.new_app {
            app.key = Some(key.into());
        }
        self
    }

    /// This ask, planned first, by `model` when one is named.
    pub fn planned(self, model: Option<&str>) -> Ask {
        Ask { plan: true, model: model.map(str::to_string), ..self }
    }
}

/// The app a person asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NewApp {
    /// What it is called: its id and the project's name come from it, and its project's key when `key` is absent.
    pub name: String,
    /// The project's key, chosen by the person ([`chosen_key`]); [`key_from`] the name when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// A name's words: its letters and digits, accented Latin letters taken as their plain ones, everything else a break.
pub fn words(name: &str) -> Vec<String> {
    let mut words = vec![String::new()];
    for c in name.chars() {
        let plain = fold(c);
        match plain {
            Some(p) => words.last_mut().expect("never empty").push_str(p),
            None if c.is_ascii_alphanumeric() => words.last_mut().expect("never empty").push(c),
            None if c == '\'' || c == '\u{2019}' => {}
            None => words.push(String::new()),
        }
    }
    words.retain(|w| !w.is_empty());
    words
}

/// The plain letters an accented Latin one is written with.
fn fold(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' => "a",
        'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' | 'Ā' => "A",
        'æ' => "ae",
        'Æ' => "AE",
        'ç' | 'č' | 'ć' => "c",
        'Ç' | 'Č' | 'Ć' => "C",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ę' => "e",
        'È' | 'É' | 'Ê' | 'Ë' | 'Ē' | 'Ę' => "E",
        'ì' | 'í' | 'î' | 'ï' | 'ī' => "i",
        'Ì' | 'Í' | 'Î' | 'Ï' | 'Ī' => "I",
        'ł' => "l",
        'Ł' => "L",
        'ñ' | 'ń' => "n",
        'Ñ' | 'Ń' => "N",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' => "o",
        'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ø' | 'Ō' => "O",
        'œ' => "oe",
        'Œ' => "OE",
        'š' | 'ś' => "s",
        'Š' | 'Ś' => "S",
        'ß' => "ss",
        'ù' | 'ú' | 'û' | 'ü' | 'ū' => "u",
        'Ù' | 'Ú' | 'Û' | 'Ü' | 'Ū' => "U",
        'ý' | 'ÿ' => "y",
        'Ý' | 'Ÿ' => "Y",
        'ž' | 'ź' | 'ż' => "z",
        'Ž' | 'Ź' | 'Ż' => "Z",
        _ => return None,
    })
}

/// How long a project key made from a name may be.
const KEY_FROM_NAME: std::ops::RangeInclusive<usize> = 2..=6;

/// The project key a new app's name makes when the person chooses none, before the server adds a digit to one that
/// is taken: a name of one word is that word when it fits (Cairn makes CAIRN), else its first four letters; a name of
/// several is their initials, padded from the first word to at least two, at most six. `Err` says why a name makes
/// no key.
pub fn key_from(name: &str) -> Result<String, String> {
    let (least, most) = (*KEY_FROM_NAME.start(), *KEY_FROM_NAME.end());
    let lettered: Vec<String> = words(name)
        .iter()
        .map(|w| w.chars().filter(char::is_ascii_alphabetic).collect::<String>().to_ascii_uppercase())
        .filter(|w| !w.is_empty())
        .collect();
    let Some(first) = lettered.first() else {
        return Err(format!("{name:?} has no letters to make a project key from"));
    };
    let mut base: String = match lettered.len() {
        1 if first.len() <= most => first.clone(),
        1 => first.chars().take(4).collect(),
        _ => lettered.iter().take(most).filter_map(|w| w.chars().next()).collect(),
    };
    for c in first.chars().skip(1) {
        if base.len() >= least {
            break;
        }
        base.push(c);
    }
    if base.len() < least {
        return Err(format!("{name:?} is too short to make a project key from: give it at least two letters"));
    }
    Ok(base)
}

/// The project key a person typed, as the engine keeps one: trimmed and in capitals, 2 to 10 letters or digits,
/// starting with a letter. `Err` says what is wrong with it.
pub fn chosen_key(typed: &str) -> Result<String, String> {
    let key = typed.trim().to_ascii_uppercase();
    let ok = (2..=10).contains(&key.len())
        && key.starts_with(|c: char| c.is_ascii_uppercase())
        && key.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    if ok {
        Ok(key)
    } else {
        Err(format!("A project's key is 2 to 10 letters or digits, starting with a letter: {key:?} isn't."))
    }
}

/// What an ask made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Asked {
    pub project: AskedProject,
    /// The task made. `None` only when a new app's project was made but its first task wasn't; `notes` says why.
    pub task: Option<AskedTask>,
    /// Whether the task is a spec task for a planner, which talks the ask over before any work starts.
    #[serde(default)]
    pub planned: bool,
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
        let keyed = serde_json::to_value(Ask::new_app("notes", "Trail Notes").keyed("TRAILN")).unwrap();
        assert_eq!(keyed, serde_json::json!({"text": "notes", "new_app": {"name": "Trail Notes", "key": "TRAILN"}}));
        let planned = serde_json::to_value(Ask::change("more", "TICK").planned(Some("opus"))).unwrap();
        assert_eq!(planned, serde_json::json!({"text": "more", "project": "TICK", "plan": true, "model": "opus"}));
    }

    #[test]
    fn a_chosen_key_is_put_in_capitals_and_checked() {
        assert_eq!(chosen_key(" cairn ").as_deref(), Ok("CAIRN"));
        assert_eq!(chosen_key("tn2").as_deref(), Ok("TN2"));
        for bad in ["", "c", "2cairn", "cai-rn", "elevenchars", "café"] {
            assert!(chosen_key(bad).is_err(), "{bad:?}");
        }
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
