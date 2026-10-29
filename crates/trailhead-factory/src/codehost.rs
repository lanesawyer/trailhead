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
/// a REST API (GitHub's Git Data API), not `git push`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub contents: Option<Vec<u8>>,
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
    /// Sends the task back to running.
    Conflict,
}

/// Where a PR stands on the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrState {
    Open,
    /// Merged, into the base branch as `commit` when the host says which.
    Merged {
        commit: Option<String>,
    },
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

#[async_trait]
pub trait CodeHost: Send + Sync {
    /// Publishes commits made on top of `start`, the commit sha the checkout was at when the work began. A new
    /// `branch` is created there, never on the base branch's current head: commits carry whole files, so laying them
    /// on a newer head would silently undo whatever landed since (TRAIL-161). An existing branch must still be at
    /// `start`, otherwise this fails with `Conflict` rather than overwrite what's on it.
    async fn publish(&self, repo: &Repo, start: &str, branch: &str, commits: &[Commit]) -> Result<()>;
    async fn open_pr(&self, repo: &Repo, pr: NewPr) -> Result<PrRef>;
    async fn diff(&self, pr: &PrRef) -> Result<String>;
    async fn comment(&self, pr: &PrRef, body: &str) -> Result<()>;
    async fn merge(&self, pr: &PrRef) -> Result<MergeOutcome>;
    /// Whether the PR is still open, was merged (by anyone), or was closed without merging.
    async fn pr_state(&self, pr: &PrRef) -> Result<PrState>;
    /// Whether the open PR merges cleanly into its base: `Some(false)` when it conflicts, `None` while the host is
    /// still working it out.
    async fn mergeable(&self, pr: &PrRef) -> Result<Option<bool>>;
    /// The PR's current title, which a squash merge takes as its commit subject.
    async fn title(&self, pr: &PrRef) -> Result<String>;
    /// The files the PR changes, with how and by how many lines, for grading it.
    async fn files(&self, pr: &PrRef) -> Result<Vec<ChangedFile>>;
    /// The commit the PR's branch is at now.
    async fn head(&self, pr: &PrRef) -> Result<String>;
    /// The repo's CI on the PR's current head: its check runs and commit statuses, which are pending until every one
    /// has finished and at least one exists, and failed with the names of those that failed as soon as any has.
    async fn ci(&self, pr: &PrRef) -> Result<Ci>;
}
