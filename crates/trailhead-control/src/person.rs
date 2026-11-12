//! A person's PR goes next (TRAIL-804): GitHub's merge queue needs an organization's repo, so a person signals
//! "mine goes next" by turning on the host's auto-merge for their PR. While one is open the merger holds its own merges
//! and keeps that PR up to date, so the host can merge it when its CI is green. It never merges one itself, and never
//! turns auto-merge on or off.

use std::collections::HashSet;

use tokio::time::Instant;
use trailhead_factory::{BranchUpdate, CiState, MergeMode, PrState, Repo, Result};

use crate::Factory;

/// The person's PR the merger is holding for.
pub(crate) struct Held {
    pub number: u64,
    /// The head the merger asked the host to update the branch on, and when (as `Factory::updating`).
    updating: Option<(String, Instant)>,
    /// The head the host said conflicts when asked to update the branch, so the warning is logged once per head.
    conflicted: Option<String>,
}

impl Factory {
    /// Whether the merger is holding its merges for a person's PR.
    pub(crate) fn merges_held(&self) -> bool {
        self.held.lock().unwrap().is_some()
    }

    /// One merger pass over the open PRs that aren't the factory's own (`factory_prs`, the numbers tasks name): finds
    /// the oldest with auto-merge on, holds merges for it, and brings its branch up to date. A failure is logged and
    /// leaves the hold as it was, so merges don't slip through on a host that failed to answer.
    pub(crate) async fn person_pass(&self, factory_prs: &HashSet<u64>) {
        if let Err(e) = self.person_hold(factory_prs).await {
            tracing::warn!(error = %e, "couldn't look for a person's PR with auto-merge on; keeping the hold as it was");
        }
    }

    async fn person_hold(&self, factory_prs: &HashSet<u64>) -> Result<()> {
        let Some(codehost) = &self.codehost else { return Ok(()) };
        if self.merge_mode() != MergeMode::On {
            self.clear_hold();
            return Ok(());
        }
        if self.code_host_pause().is_some() {
            return Ok(());
        }
        let repo = Repo { owner: self.config.repo.owner.clone(), name: self.config.repo.name.clone() };
        let open = codehost.open_prs(&repo, &self.config.repo.base_branch).await?;
        let Some(first) = open.into_iter().find(|p| p.auto_merge && !factory_prs.contains(&p.pr.number)) else {
            self.clear_hold();
            return Ok(());
        };
        let pr = &first.pr;
        let number = pr.number;
        let mut blocked = None;
        if let PrState::Open { mergeable: Some(false), .. } = codehost.pr_state(pr).await? {
            blocked = Some("it conflicts with its base".to_string());
        } else if let CiState::Failed { checks } = codehost.ci(pr).await?.state {
            blocked = Some(format!("CI failed: {}", checks.join(", ")));
        }
        let said = match &blocked {
            Some(why) => format!("#{number} has auto-merge on but {why}; turn auto-merge off or fix it"),
            None => format!("#{number} has auto-merge on"),
        };
        if self.status.holding(Some(said.clone())) {
            tracing::info!("holding merges: {said}");
        }
        // The hold stays as it is until every fallible call has answered: an error returns with it untouched.
        let (mut updating, mut conflicted) = self
            .held
            .lock()
            .unwrap()
            .as_ref()
            .filter(|h| h.number == number)
            .map_or((None, None), |h| (h.updating.clone(), h.conflicted.clone()));
        if blocked.is_none() && codehost.behind(pr).await? {
            let head = codehost.head(pr).await?;
            let waiting =
                updating.as_ref().is_some_and(|(at, since)| *at == head && since.elapsed() < crate::UPDATE_PATIENCE);
            if !waiting {
                match codehost.update_branch(pr).await {
                    Ok(BranchUpdate::Updated) => {
                        tracing::info!(pr = %pr.url, "updated the branch of the PR merges are held for");
                        updating = Some((head, Instant::now()));
                        conflicted = None;
                    }
                    Ok(BranchUpdate::UpToDate) => conflicted = None,
                    Ok(BranchUpdate::Conflict) => {
                        if conflicted.as_ref() != Some(&head) {
                            tracing::warn!(pr = %pr.url, "the PR merges are held for conflicts with its base; auto-merge stays on, so the hold stays");
                            conflicted = Some(head);
                        }
                    }
                    Err(e) => {
                        self.set_hold(number, updating, conflicted);
                        return Err(e);
                    }
                }
            }
        }
        self.set_hold(number, updating, conflicted);
        Ok(())
    }

    fn set_hold(&self, number: u64, updating: Option<(String, Instant)>, conflicted: Option<String>) {
        *self.held.lock().unwrap() = Some(Held { number, updating, conflicted });
    }

    fn clear_hold(&self) {
        *self.held.lock().unwrap() = None;
        if self.status.holding(None) {
            tracing::info!("no longer holding merges");
        }
    }
}
