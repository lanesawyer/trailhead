use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use trailhead_factory::{
    BranchUpdate, ChangedFile, Ci, CiState, CodeHost, Commit, Error, FileStatus, MergeOutcome, NewPr, PrRef, PrState,
    Repo, Result,
};

#[derive(Debug, Clone)]
pub struct FakePr {
    pub pr: PrRef,
    pub spec: NewPr,
    /// Comments in the order made; a comment's id is its position plus one.
    pub comments: Vec<String>,
    /// Ids of comments a person deleted (they stay listed, so the other ids hold).
    pub deleted: Vec<u64>,
    pub merged: bool,
    /// Closed without merging.
    pub closed: bool,
    /// What the host says of whether it merges cleanly: `None` while it is still working it out.
    pub mergeable: Option<bool>,
    /// The base branch has commits the PR's branch lacks.
    pub behind: bool,
    /// What the host lists as changed, when a test sets it; otherwise it follows the published commits.
    pub files: Option<Vec<ChangedFile>>,
    /// CI on the PR's head: pending until a test says otherwise.
    pub ci: CiState,
    /// The log tails of failed checks, by check name.
    pub ci_logs: Vec<(String, String)>,
}

#[derive(Default)]
struct State {
    branches: BTreeMap<(String, String), Vec<Commit>>,
    /// Each branch's head: the commit it started from, then one made-up sha per publish.
    heads: BTreeMap<(String, String), String>,
    /// The commit each branch was first published on top of.
    starts: BTreeMap<(String, String), String>,
    prs: Vec<FakePr>,
    conflicts: bool,
    /// Updates the host accepted but hasn't made yet, like GitHub's 202.
    queue_updates: bool,
    /// How many times `update_branch` was asked to do something.
    update_requests: usize,
    /// How many times `files` was asked for a PR's file list.
    file_listings: usize,
    refuse_publish: Option<String>,
    refuse_merge: Option<String>,
}

#[derive(Clone, Default)]
pub struct FakeCodeHost {
    state: Arc<Mutex<State>>,
}

impl FakeCodeHost {
    pub fn new() -> FakeCodeHost {
        FakeCodeHost::default()
    }

    pub fn commits(&self, repo: &Repo, branch: &str) -> Vec<Commit> {
        let s = self.state.lock().unwrap();
        s.branches.get(&(repo.slug(), branch.to_string())).cloned().unwrap_or_default()
    }

    /// The commit a branch was first published on top of.
    pub fn start(&self, repo: &Repo, branch: &str) -> Option<String> {
        self.state.lock().unwrap().starts.get(&(repo.slug(), branch.to_string())).cloned()
    }

    /// A branch's head: a made-up sha per publish, none before the first.
    pub fn head(&self, repo: &Repo, branch: &str) -> Option<String> {
        self.state.lock().unwrap().heads.get(&(repo.slug(), branch.to_string())).cloned()
    }

    pub fn prs(&self) -> Vec<FakePr> {
        self.state.lock().unwrap().prs.clone()
    }

    /// Makes every later publish fail with this message, like GitHub refusing a push.
    pub fn refuse_publish(&self, why: &str) {
        self.state.lock().unwrap().refuse_publish = Some(why.to_string());
    }

    /// Makes every later merge refused with this message, like a required check GitHub is still waiting on, until
    /// called with `None`.
    pub fn refuse_merge(&self, why: Option<&str>) {
        self.state.lock().unwrap().refuse_merge = why.map(str::to_string);
    }

    /// Makes every later merge report a conflict.
    pub fn set_conflicts(&self, on: bool) {
        self.state.lock().unwrap().conflicts = on;
    }

    /// Makes `update_branch` queue the merge like GitHub's 202: it answers `Updated` and the head stays put, the PR
    /// still behind, until `land_updates` runs.
    pub fn queue_updates(&self, on: bool) {
        self.state.lock().unwrap().queue_updates = on;
    }

    /// Makes the host do the updates it queued: later `update_branch` calls act at once again.
    pub async fn land_updates(&self, pr: &PrRef) {
        let queued = std::mem::replace(&mut self.state.lock().unwrap().queue_updates, false);
        if queued {
            self.update_branch(pr).await.unwrap();
        }
    }

    /// How many times the host was asked to list a PR's files.
    pub fn file_listings(&self) -> usize {
        self.state.lock().unwrap().file_listings
    }

    /// How many times `update_branch` was called on a PR that was behind.
    pub fn update_requests(&self) -> usize {
        self.state.lock().unwrap().update_requests
    }

    /// Closes a PR without merging it, as a person would on GitHub.
    pub fn close(&self, pr: &PrRef) {
        self.find(pr, |p, _| p.closed = true).unwrap();
    }

    /// Sets whether the host says the PR merges cleanly (`None`: not worked out yet).
    pub fn set_mergeable(&self, pr: &PrRef, mergeable: Option<bool>) {
        self.find(pr, |p, _| p.mergeable = mergeable).unwrap();
    }

    /// Sets whether the PR's branch is behind its base.
    /// A person deletes the PR's comment with this id.
    pub fn delete_comment(&self, pr: &PrRef, id: u64) {
        self.find(pr, |p, _| p.deleted.push(id)).unwrap();
    }

    pub fn set_behind(&self, pr: &PrRef, behind: bool) {
        self.find(pr, |p, _| p.behind = behind).unwrap();
    }

    /// Sets the files the host lists for a PR, such as a lockfile the fake agent never wrote.
    pub fn set_files(&self, pr: &PrRef, files: Vec<ChangedFile>) {
        self.find(pr, |p, _| p.files = Some(files)).unwrap();
    }

    /// Sets how CI stands on the PR's head. A new head starts over at pending.
    pub fn set_ci(&self, pr: &PrRef, ci: CiState) {
        self.find(pr, |p, _| p.ci = ci).unwrap();
    }

    /// Sets the log tails the host keeps of failed checks.
    pub fn set_ci_logs(&self, pr: &PrRef, logs: Vec<(String, String)>) {
        self.find(pr, |p, _| p.ci_logs = logs).unwrap();
    }

    /// Someone pushes to the PR's branch, moving its head and restarting CI.
    pub fn push_to(&self, pr: &PrRef) {
        let (key, head) = self
            .find(pr, |p, s| {
                let key = (pr.repo.slug(), p.spec.branch.clone());
                let head = format!("{}+", s.heads[&key]);
                p.ci = CiState::Pending;
                (key, head)
            })
            .unwrap();
        self.state.lock().unwrap().heads.insert(key, head);
    }

    /// The commit a merged PR became: made up from its number.
    pub fn merge_commit(pr: &PrRef) -> String {
        format!("merge-{}", pr.number)
    }

    fn find<T>(&self, pr: &PrRef, f: impl FnOnce(&mut FakePr, &State) -> T) -> Result<T> {
        let mut s = self.state.lock().unwrap();
        let i = s.prs.iter().position(|p| p.pr == *pr).ok_or_else(|| Error::NotFound(format!("PR {}", pr.url)))?;
        let mut p = s.prs[i].clone();
        let out = f(&mut p, &s);
        s.prs[i] = p;
        Ok(out)
    }
}

#[async_trait]
impl CodeHost for FakeCodeHost {
    async fn publish(&self, repo: &Repo, start: &str, branch: &str, commits: &[Commit]) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        if let Some(why) = &s.refuse_publish {
            return Err(Error::NotFound(why.clone()));
        }
        let key = (repo.slug(), branch.to_string());
        if let Some(head) = s.heads.get(&key).filter(|head| *head != start) {
            return Err(Error::Conflict(format!("branch {branch} is at {head}, not at {start}")));
        }
        s.starts.entry(key.clone()).or_insert_with(|| start.to_string());
        let n = s.branches.get(&key).map_or(0, Vec::len) + commits.len();
        s.heads.insert(key.clone(), format!("{branch}@{n}"));
        s.branches.entry(key).or_default().extend_from_slice(commits);
        Ok(())
    }

    async fn open_pr(&self, repo: &Repo, pr: NewPr) -> Result<PrRef> {
        let mut s = self.state.lock().unwrap();
        if !s.branches.contains_key(&(repo.slug(), pr.branch.clone())) {
            return Err(Error::NotFound(format!("branch {}", pr.branch)));
        }
        let number = s.prs.len() as u64 + 1;
        let r =
            PrRef { repo: repo.clone(), number, url: format!("https://example.invalid/{}/pull/{number}", repo.slug()) };
        s.prs.push(FakePr {
            pr: r.clone(),
            spec: pr,
            comments: Vec::new(),
            deleted: Vec::new(),
            merged: false,
            closed: false,
            mergeable: Some(true),
            behind: false,
            files: None,
            ci: CiState::Pending,
            ci_logs: Vec::new(),
        });
        Ok(r)
    }

    async fn diff(&self, pr: &PrRef) -> Result<String> {
        self.find(pr, |p, s| {
            let commits = s.branches.get(&(pr.repo.slug(), p.spec.branch.clone())).cloned().unwrap_or_default();
            let mut out = String::new();
            for c in commits {
                for change in c.changes {
                    match change.contents {
                        Some(body) => {
                            out.push_str(&format!("+++ {}\n{}\n", change.path, String::from_utf8_lossy(&body)))
                        }
                        None => out.push_str(&format!("--- {} (deleted)\n", change.path)),
                    }
                }
            }
            out
        })
    }

    async fn comment(&self, pr: &PrRef, body: &str) -> Result<u64> {
        self.find(pr, |p, _| {
            p.comments.push(body.to_string());
            p.comments.len() as u64
        })
    }

    async fn edit_comment(&self, pr: &PrRef, id: u64, body: &str) -> Result<()> {
        let edited = self.find(pr, |p, _| {
            let slot = (id as usize).checked_sub(1).and_then(|i| p.comments.get_mut(i));
            match slot {
                Some(c) if !p.deleted.contains(&id) => {
                    *c = body.to_string();
                    true
                }
                _ => false,
            }
        })?;
        if edited { Ok(()) } else { Err(Error::NotFound(format!("comment {id}"))) }
    }

    async fn merge(&self, pr: &PrRef, head: &str) -> Result<MergeOutcome> {
        let (conflicts, refused) = {
            let s = self.state.lock().unwrap();
            (s.conflicts, s.refuse_merge.clone())
        };
        self.find(pr, |p, s| {
            if s.heads.get(&(pr.repo.slug(), p.spec.branch.clone())).map(String::as_str) != Some(head) {
                return MergeOutcome::HeadMoved;
            }
            if conflicts {
                return MergeOutcome::Conflict;
            }
            if let Some(why) = refused {
                return MergeOutcome::Refused(why);
            }
            p.merged = true;
            MergeOutcome::Merged
        })
    }

    async fn behind(&self, pr: &PrRef) -> Result<bool> {
        self.find(pr, |p, _| p.behind)
    }

    async fn update_branch(&self, pr: &PrRef) -> Result<BranchUpdate> {
        // Like the host: nothing to merge means up to date, even if conflicts are on.
        if !self.find(pr, |p, _| p.behind)? {
            return Ok(BranchUpdate::UpToDate);
        }
        if self.state.lock().unwrap().conflicts {
            return Ok(BranchUpdate::Conflict);
        }
        {
            let mut s = self.state.lock().unwrap();
            s.update_requests += 1;
            if s.queue_updates {
                return Ok(BranchUpdate::Updated);
            }
        }
        let branch = self.find(pr, |p, _| {
            p.behind = false;
            p.spec.branch.clone()
        })?;
        // A merge commit on top of the branch, like the host makes.
        let mut s = self.state.lock().unwrap();
        let key = (pr.repo.slug(), branch.clone());
        let commits = s.branches.entry(key.clone()).or_default();
        commits.push(Commit {
            message: format!("Merge {} into {branch}", pr.repo.slug()),
            changes: vec![],
            merges: vec!["base".into()],
        });
        let n = commits.len();
        s.heads.insert(key, format!("{branch}@{n}"));
        Ok(BranchUpdate::Updated)
    }

    async fn carries_verdict(&self, pr: &PrRef, reviewed: &str) -> Result<bool> {
        self.find(pr, |p, s| {
            let key = (pr.repo.slug(), p.spec.branch.clone());
            let commits = s.branches.get(&key).map_or(&[][..], Vec::as_slice);
            if s.heads.get(&key).map(String::as_str) == Some(reviewed) {
                return true;
            }
            // Heads are `branch@n`, n commits in; a head a push moved past that isn't one of them.
            let Some(n) = reviewed.strip_prefix(&format!("{}@", p.spec.branch)).and_then(|n| n.parse::<usize>().ok())
            else {
                return false;
            };
            s.heads.get(&key).is_some_and(|h| *h == format!("{}@{}", p.spec.branch, commits.len()))
                && commits.get(n..).is_some_and(|since| since.iter().all(|c| !c.merges.is_empty()))
        })
    }

    async fn pr_state(&self, pr: &PrRef) -> Result<PrState> {
        self.find(pr, |p, _| match (p.merged, p.closed) {
            (true, _) => PrState::Merged { commit: Some(FakeCodeHost::merge_commit(&p.pr)) },
            (false, true) => PrState::Closed,
            _ => PrState::Open { mergeable: p.mergeable },
        })
    }

    async fn title(&self, pr: &PrRef) -> Result<String> {
        self.find(pr, |p, _| p.spec.title.clone())
    }

    async fn head(&self, pr: &PrRef) -> Result<String> {
        self.find(pr, |p, s| s.heads.get(&(pr.repo.slug(), p.spec.branch.clone())).cloned().unwrap_or_default())
    }

    async fn ci(&self, pr: &PrRef) -> Result<Ci> {
        let head = CodeHost::head(self, pr).await?;
        self.find(pr, |p, _| Ci { head, state: p.ci.clone() })
    }

    async fn ci_logs(&self, pr: &PrRef, checks: &[String]) -> Result<Vec<(String, String)>> {
        self.find(pr, |p, _| p.ci_logs.iter().filter(|(n, _)| checks.contains(n)).cloned().collect())
    }

    async fn files(&self, pr: &PrRef) -> Result<Vec<ChangedFile>> {
        self.state.lock().unwrap().file_listings += 1;
        self.find(pr, |p, s| {
            if let Some(files) = &p.files {
                return files.clone();
            }
            // The last commit to touch a path decides how it ends up.
            let mut by_path: BTreeMap<String, ChangedFile> = BTreeMap::new();
            for c in s.branches.get(&(pr.repo.slug(), p.spec.branch.clone())).into_iter().flatten() {
                for change in &c.changes {
                    let (status, additions) = match &change.contents {
                        Some(body) => (FileStatus::Added, body.iter().filter(|b| **b == b'\n').count().max(1) as u32),
                        None => (FileStatus::Removed, 0),
                    };
                    let file =
                        ChangedFile { path: change.path.clone(), status, additions, deletions: 0, previous_path: None };
                    by_path.insert(change.path.clone(), file);
                }
            }
            by_path.into_values().collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn open(host: &FakeCodeHost) -> PrRef {
        let repo = Repo { owner: "o".into(), name: "r".into() };
        let spec = NewPr { branch: "b".into(), base: "main".into(), title: "t".into(), body: String::new() };
        host.publish(&repo, "base", "b", &[]).await.unwrap();
        host.open_pr(&repo, spec).await.unwrap()
    }

    #[tokio::test]
    async fn update_branch_merges_only_when_behind() {
        let host = FakeCodeHost::new();
        let pr = open(&host).await;
        assert!(!host.behind(&pr).await.unwrap());
        assert_eq!(host.update_branch(&pr).await.unwrap(), BranchUpdate::UpToDate);

        host.set_behind(&pr, true);
        assert!(host.behind(&pr).await.unwrap());
        assert_eq!(host.update_branch(&pr).await.unwrap(), BranchUpdate::Updated);
        assert!(!host.behind(&pr).await.unwrap());
        assert_eq!(host.update_branch(&pr).await.unwrap(), BranchUpdate::UpToDate);
    }

    #[tokio::test]
    async fn update_branch_conflicts_only_when_behind() {
        let host = FakeCodeHost::new();
        let pr = open(&host).await;
        host.set_conflicts(true);
        assert_eq!(host.update_branch(&pr).await.unwrap(), BranchUpdate::UpToDate);

        host.set_behind(&pr, true);
        assert_eq!(host.update_branch(&pr).await.unwrap(), BranchUpdate::Conflict);
        assert!(host.behind(&pr).await.unwrap());
    }

    #[tokio::test]
    async fn carries_a_verdict_over_merges_of_the_base_only() {
        let host = FakeCodeHost::new();
        let pr = open(&host).await;
        let reviewed = host.head(&pr.repo, "b").unwrap();
        assert!(host.carries_verdict(&pr, &reviewed).await.unwrap());

        host.set_behind(&pr, true);
        host.update_branch(&pr).await.unwrap();
        assert!(host.carries_verdict(&pr, &reviewed).await.unwrap());

        let work = Commit { message: "rework".into(), changes: vec![], merges: vec![] };
        host.publish(&pr.repo, &host.head(&pr.repo, "b").unwrap(), "b", &[work]).await.unwrap();
        assert!(!host.carries_verdict(&pr, &reviewed).await.unwrap());

        host.push_to(&pr);
        assert!(!host.carries_verdict(&pr, &reviewed).await.unwrap());
    }
}
