use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{ChangedFile, Result};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Repo {
    pub owner: String,
    pub name: String,
}

impl Repo {
    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// A file's new contents, or `None` to delete it. Commits are published through
/// a REST API (GitHub's Git Data API), not `git push`. A symlink's contents are its target path, never the bytes of
/// what it points to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub contents: Option<Vec<u8>>,
    /// Absent from a change sent by a build older than modes (TRAIL-1432), which published everything as regular.
    #[serde(default)]
    pub mode: FileMode,
}

impl FileChange {
    /// A regular file's new contents, or its deletion.
    pub fn new(path: impl Into<String>, contents: Option<Vec<u8>>) -> FileChange {
        FileChange { path: path.into(), contents, mode: FileMode::Regular }
    }
}

/// The git mode a changed file is published with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileMode {
    #[default]
    Regular,
    Executable,
    Symlink,
}

impl FileMode {
    /// The mode as a git tree entry spells it.
    pub fn git(self) -> &'static str {
        match self {
            FileMode::Regular => "100644",
            FileMode::Executable => "100755",
            FileMode::Symlink => "120000",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    pub message: String,
    pub changes: Vec<FileChange>,
    /// Further parents besides the one it's laid on, making it a merge commit: a rework that merged the base branch
    /// into the PR branch (TRAIL-199).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merges: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewPr {
    pub branch: String,
    pub base: String,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PrRef {
    pub repo: Repo,
    pub number: u64,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MergeOutcome {
    Merged,
    /// The PR conflicts with its base branch: sends the task back to running.
    Conflict,
    /// The PR's head is no longer the commit the merge was asked for; nothing was merged.
    HeadMoved,
    /// The host won't merge it for another reason, such as a required check or the branch rules; nothing was merged.
    Refused(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BranchUpdate {
    /// The base branch is merged into the PR branch; its head is now a new commit, and CI starts over on it.
    Updated,
    /// The PR branch already has everything on the base branch, say because an update queued earlier has landed.
    /// Nothing changed; look at the head again.
    UpToDate,
    /// The base branch doesn't merge into the PR branch cleanly. Sends the task back to running.
    Conflict,
}

/// Where a PR stands on the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrState {
    /// Still open. `mergeable` is whether it merges cleanly into its base: `Some(false)` when it conflicts, `None`
    /// while the host is still working it out. `head` is the head commit the PR has on the host, which a reading of
    /// `mergeable` is only known to belong to once it matches the head that was pushed (TRAIL-446); `None` when the
    /// host didn't say.
    Open { mergeable: Option<bool>, head: Option<String> },
    /// Merged, into the base branch as `commit` when the host says which.
    Merged { commit: Option<String> },
    /// Closed without merging.
    Closed,
}

/// How the repo's CI stands on one commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CiState {
    /// Some check hasn't finished, or none has started yet.
    Pending,
    Passed,
    /// The names of the checks that failed.
    Failed {
        checks: Vec<String>,
    },
}

/// CI on a PR's current head commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ci {
    pub head: String,
    pub state: CiState,
}

/// An open PR on the base branch, as listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenPr {
    pub pr: PrRef,
    /// Whether the host's auto-merge is on for it, which merges it once its checks pass (TRAIL-804).
    pub auto_merge: bool,
}

/// The code host's rate limit as of its last answer. The limit is shared with whoever else uses the credential
/// (Lane's `gh`, say), so the control plane backs off before it's spent (TRAIL-535).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimit {
    /// Requests allowed per window.
    pub limit: u64,
    /// Requests left in this window.
    pub remaining: u64,
    /// When the window resets, in unix seconds.
    pub resets_at: i64,
}

#[async_trait]
pub trait CodeHost: Send + Sync {
    /// Publishes commits made on top of `start`, the commit sha the checkout was at when the work began. A new
    /// `branch` is created there, never on the base branch's current head: commits carry whole files, so laying them
    /// on a newer head would silently undo whatever landed since (TRAIL-161). An existing branch must still be at
    /// `start`, otherwise this fails with `Conflict` rather than overwrite what's on it.
    async fn publish(&self, repo: &Repo, start: &str, branch: &str, commits: &[Commit]) -> Result<()>;
    async fn open_pr(&self, repo: &Repo, pr: NewPr) -> Result<PrRef>;
    async fn diff(&self, pr: &PrRef) -> Result<String>;
    /// Comments on the PR and returns the new comment's id.
    async fn comment(&self, pr: &PrRef, body: &str) -> Result<u64>;
    /// Replaces the body of a comment made with `comment`. `NotFound` if it has since been deleted.
    async fn edit_comment(&self, pr: &PrRef, id: u64, body: &str) -> Result<()>;
    /// Squash-merges the PR, but only while its head is still `head`, the commit the gates passed on.
    async fn merge(&self, pr: &PrRef, head: &str) -> Result<MergeOutcome>;
    /// Whether the PR is still open (and if it merges cleanly), was merged (by anyone), or was closed without
    /// merging. One call, so reconcile costs the host one request per PR.
    async fn pr_state(&self, pr: &PrRef) -> Result<PrState>;
    /// Whether the PR's branch lacks commits that are on its base branch, which a repo requiring branches to be up to
    /// date refuses to merge (TRAIL-212).
    async fn behind(&self, pr: &PrRef) -> Result<bool>;
    /// Merges the base branch into the PR's branch as a merge commit, never a rebase and force-push, so a reviewer's
    /// verdict on the old head still describes the work. A failure that isn't a merge conflict (the head moved under
    /// the update, say) is an error, not `Conflict`, which costs a rework.
    async fn update_branch(&self, pr: &PrRef) -> Result<BranchUpdate>;
    /// Whether a reviewer's verdict on `reviewed`, a head the PR had earlier, still counts on its current head: the
    /// only commits since are merges of the base branch, each a merge commit whose second parent is on the base
    /// branch, which is what `update_branch` makes. Also true when `reviewed` is the head. False, not an error, when
    /// the host can't tell.
    ///
    /// This is a check of the commits' shape, not of their content: a merge whose second parent is on the base passes
    /// even if it changes the tree beyond a clean merge, so a conflict-resolving rework (which merges the base too)
    /// or a person's hand-edited merge of the base looks like an update. The caller rules out reworks by only
    /// carrying a verdict given on the current attempt; hand-made merges stay a known gap.
    async fn carries_verdict(&self, pr: &PrRef, reviewed: &str) -> Result<bool>;
    /// The open PRs whose base is `base`, oldest first, each with whether the host's auto-merge is on for it.
    async fn open_prs(&self, _repo: &Repo, _base: &str) -> Result<Vec<OpenPr>> {
        Ok(Vec::new())
    }
    /// The PR's current title, which a squash merge takes as its commit subject.
    async fn title(&self, pr: &PrRef) -> Result<String>;
    /// The files the PR changes, with how and by how many lines, for grading it.
    async fn files(&self, pr: &PrRef) -> Result<Vec<ChangedFile>>;
    /// The commit the PR's branch is at now.
    async fn head(&self, pr: &PrRef) -> Result<String>;
    /// The repo's CI on the PR's current head: its check runs and commit statuses, which are pending until every one
    /// has finished and at least one exists, and failed with the names of those that failed as soon as any has.
    async fn ci(&self, pr: &PrRef) -> Result<Ci>;
    /// The repo's CI on `sha`, a head the PR has now or had earlier, read as `ci` reads the current one. Pending when
    /// the host can't say, which is the default for a host that only reports the current head.
    async fn ci_at(&self, _pr: &PrRef, _sha: &str) -> Result<CiState> {
        Ok(CiState::Pending)
    }
    /// The end of the log of each of the named failed checks on the PR's current head, as (check, log), for the
    /// checks the host keeps a log of; a check without one is left out. Best effort: a host with no logs says none.
    async fn ci_logs(&self, _pr: &PrRef, _checks: &[String]) -> Result<Vec<(String, String)>> {
        Ok(Vec::new())
    }
    /// The rate limit as the host last reported it, or `None` when it doesn't say or hasn't been asked yet.
    fn rate_limit(&self) -> Option<RateLimit> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_from_a_build_without_modes_is_a_regular_file() {
        let change: FileChange = serde_json::from_str(r#"{"path":"run.sh","contents":[104,105]}"#).unwrap();
        assert_eq!(change, FileChange::new("run.sh", Some(b"hi".to_vec())));
        assert_eq!(change.mode, FileMode::Regular);
    }

    #[test]
    fn a_mode_survives_the_round_trip() {
        let change = FileChange { path: "link".into(), contents: Some(b"target".to_vec()), mode: FileMode::Symlink };
        let back: FileChange = serde_json::from_str(&serde_json::to_string(&change).unwrap()).unwrap();
        assert_eq!(back, change);
    }
}
