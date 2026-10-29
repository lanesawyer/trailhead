use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::{CheckResult, CheckpointId, Commit, Endpoint, HumanContext, ModelId, Result, Role, TaskKey, Usage};

/// What the agent is asked to do: the task's spec as Trailhead holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSpec {
    pub key: TaskKey,
    pub title: String,
    pub description: String,
    /// Why the previous attempt at the task failed, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_failure: Option<String>,
    /// Lane's answers to the agent's earlier questions, and the task's decisions and notes.
    #[serde(default, skip_serializing_if = "HumanContext::is_empty")]
    pub context: HumanContext,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunContext {
    pub role: Role,
    pub task: TaskSpec,
    pub workdir: PathBuf,
    pub model: ModelId,
    pub endpoint: Endpoint,
    pub max_steps: u32,
    /// For reviewers: the change under review. Never the author's transcript.
    pub diff: Option<String>,
    /// For reviewers: the checks the worker already ran in its own sandbox, which the reviewer shouldn't rerun.
    #[serde(default)]
    pub checks: Vec<CheckResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feedback {
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerdictKind {
    Approve,
    RequestChanges,
}

/// Something a reviewer noticed that shouldn't hold up the change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowUp {
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// Trailhead's scale, 1 (most urgent) to 4; the project's default when absent.
    #[serde(default)]
    pub priority: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub kind: VerdictKind,
    pub body: String,
    pub reviewer: String,
    pub model: Option<ModelId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunOutcome {
    /// A worker finished: its local commits, to be published by the daemon.
    Changes {
        commits: Vec<Commit>,
        summary: String,
    },
    /// A reviewer finished. The daemon fills in `reviewer` and `model`.
    Verdict {
        kind: VerdictKind,
        body: String,
        /// Non-blocking suggestions, to be filed as tasks when the review approves.
        #[serde(default)]
        follow_ups: Vec<FollowUp>,
    },
    NeedsInput {
        question: String,
    },
    Failed {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunEvent {
    StepStarted {
        step: u32,
    },
    ToolCall {
        name: String,
        /// A short, one-line summary of the call (the command, file or pattern), for progress logs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    CheckpointTaken {
        checkpoint: CheckpointId,
    },
    /// Usage of one model call, as reported in its response.
    Usage {
        model: ModelId,
        usage: Usage,
    },
    NeedsInput {
        question: String,
    },
    /// How full one of the subscription's usage windows is (Claude's `five_hour` or `seven_day`), as the harness
    /// last heard. The control plane stops starting runs past a threshold until the window resets.
    Quota {
        window: String,
        used_percent: u32,
        /// Unix seconds.
        resets_at: i64,
    },
}

pub type EventSink = mpsc::UnboundedSender<RunEvent>;

/// An agent running inside the sandbox: Claude Code, Codex, an own loop.
#[async_trait]
pub trait Harness: Send + Sync {
    async fn run(&self, ctx: RunContext, events: EventSink) -> Result<RunOutcome>;
    async fn resume(&self, ctx: RunContext, feedback: Feedback, events: EventSink) -> Result<RunOutcome>;
}

/// Whether Claude Code's error text is trouble with the API call (a dropped connection, a timeout or no response,
/// overload, a server error) and not with the task: a resume can get past it, and it doesn't count as a failed
/// attempt. Not auth, billing or invalid-request errors. One matcher, so resuming and attempt counting agree.
pub fn is_transient_api_error(text: &str) -> bool {
    // The label leads the text, as Claude Code writes it (maybe after `claude: `); a free-text reason that only
    // mentions an API error somewhere isn't API trouble.
    let Some(at) = text.find("API Error").filter(|at| matches!(text[..*at].trim(), "" | "claude:")) else {
        return false;
    };
    const PHRASES: &[&str] = &[
        "No response from API",
        "ECONNRESET",
        "ETIMEDOUT",
        "EPIPE",
        "Connection error",
        "Connection reset",
        "connection reset",
        "Connection closed",
        "connection closed",
        "socket hang up",
        "socket closed",
        "timed out",
        "Request timeout",
        "Overloaded",
        "overloaded",
    ];
    if PHRASES.iter().any(|p| text[at..].contains(p)) {
        return true;
    }
    // The status right after the label, as in `API Error: 503 {...}` or `API Error (529 ...)`; as a whole number, so
    // that `max_tokens: 5000` further along isn't taken for a 500.
    let status = text[at + "API Error".len()..].trim_start_matches([':', '(', ' ']);
    let digits = status.bytes().take_while(u8::is_ascii_digit).count();
    digits == 3 && status.starts_with('5')
}

#[cfg(test)]
mod api_error_tests {
    use super::is_transient_api_error;

    #[test]
    fn matches_api_trouble_not_the_tasks() {
        for yes in [
            "claude: API Error: No response from API (waited 3m)",
            "API Error: 503 {\"type\":\"error\"}",
            "API Error (529 overloaded_error)",
            "API Error: Request timed out",
            "API Error: ECONNRESET",
        ] {
            assert!(is_transient_api_error(yes), "{yes}");
        }
        for no in [
            "API Error: 400 invalid request, max_tokens: 5000",
            "API Error: 401 authentication failed",
            "I got a 503 from the test server",
            "the build timed out",
            "Couldn't fix the API Error handling; the test run timed out",
        ] {
            assert!(!is_transient_api_error(no), "{no}");
        }
    }
}

/// The types `cliff.toml` groups, so a title with one reaches the changelog.
const COMMIT_TYPES: [&str; 9] = ["feat", "fix", "perf", "refactor", "docs", "test", "chore", "ci", "build"];

/// Whether `title` is `type(scope)!: subject` with a type from `cliff.toml`, a lowercase scope that is optional, and a
/// subject, matching `^(feat|fix|...)(\([a-z0-9-]+\))?!?: .+`.
pub fn is_conventional(title: &str) -> bool {
    let Some((head, subject)) = title.split_once(": ") else { return false };
    if subject.trim().is_empty() || title.contains('\n') {
        return false;
    }
    let head = head.strip_suffix('!').unwrap_or(head);
    let (kind, scope) = match head.split_once('(') {
        Some((kind, rest)) => match rest.strip_suffix(')') {
            Some(scope) => (kind, Some(scope)),
            None => return false,
        },
        None => (head, None),
    };
    COMMIT_TYPES.contains(&kind)
        && scope.is_none_or(|s| {
            !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
}

/// The commit subject and PR title for a task: the agent's conventional `title` with the task key appended, or, when
/// it's missing or not conventional, `chore: {task title} ({key})`. The flag is false for the fallback.
pub fn conventional_title(agent_title: Option<&str>, key: &TaskKey, task_title: &str) -> (String, bool) {
    match agent_title.map(str::trim).filter(|t| is_conventional(t)) {
        Some(t) => (format!("{t} ({})", key.0), true),
        None => (format!("chore: {} ({})", task_title.trim(), key.0), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conventional_titles() {
        for ok in ["fix(factory): send it", "feat: x", "refactor(core)!: y", "ci(a-b1): z"] {
            assert!(is_conventional(ok), "{ok}");
        }
        for bad in [
            "TRAIL-1: x",
            "fix:x",
            "fix(): x",
            "Fix(core): x",
            "fix(Core): x",
            "wip: x",
            "fix(a: x",
            "feat: ",
            "fix: a\nb",
        ] {
            assert!(!is_conventional(bad), "{bad}");
        }
        let key = TaskKey("TRAIL-9".into());
        assert_eq!(conventional_title(Some(" fix(core): a "), &key, "T"), ("fix(core): a (TRAIL-9)".into(), true));
        assert_eq!(conventional_title(Some("nope"), &key, "T"), ("chore: T (TRAIL-9)".into(), false));
        assert_eq!(conventional_title(None, &key, "T"), ("chore: T (TRAIL-9)".into(), false));
    }
}
