use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::Result;

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
}
