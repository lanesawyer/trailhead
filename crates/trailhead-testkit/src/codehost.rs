use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use trailhead_factory::{
    BranchUpdate, ChangedFile, Ci, CiState, CodeHost, Commit, Error, FileStatus, MergeOutcome, NewPr, OpenPr, PrRef,
    PrState, RateLimit, Repo, Result,
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
    /// The head `pr_state` reports instead of the real one, as a slow host does right after a push.
    pub lagging_head: Option<String>,
    /// The base moves on while a run works, so a conflict outlives the push that was meant to fix it.
    pub conflict_survives_push: bool,
    /// The base branch has commits the PR's branch lacks.
    pub behind: bool,
    /// What the host lists as changed, when a test sets it; otherwise it follows the published commits.
    pub files: Option<Vec<ChangedFile>>,
    /// CI on the PR's head: pending until a test says otherwise.
    pub ci: CiState,
    /// CI on earlier heads, by sha, as a test sets it.
    pub ci_at: BTreeMap<String, CiState>,
    /// The log tails of failed checks, by check name.
    pub ci_logs: Vec<(String, String)>,
    /// Whether the host's auto-merge is on, as a person turns it on.
    pub auto_merge: bool,
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
    /// How many times `head` was asked for a PR's head.
    head_lookups: usize,
    /// Whether `behind` fails, like a host that is down.
    fail_behind: bool,
    /// How many times each `CodeHost` method was called, by name.
    calls: BTreeMap<&'static str, usize>,
    refuse_publish: Option<String>,
    /// Publishes still to fail with a transient error, and how many were attempted.
    flaky_publishes: usize,
    /// Publishes still to apply and then fail with a transient error, like a ref write that landed behind a 5xx.
    flaky_after_publishes: usize,
    publish_attempts: usize,
    /// `open_pr` and `comment` calls still to fail with a transient error.
    flaky_open_prs: usize,
    flaky_comments: usize,
    refuse_merge: Option<String>,
    rate_limit: Option<RateLimit>,
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

    /// Makes the next `times` publishes fail with a transient error (`Error::Other`), like a GitHub 500.
    pub fn flaky_publish(&self, times: usize) {
        self.state.lock().unwrap().flaky_publishes = times;
    }

    /// Makes the next `times` publishes land and then fail with a transient error, like a ref write that succeeded
    /// behind a GitHub 502.
    pub fn flaky_publish_after_applying(&self, times: usize) {
        self.state.lock().unwrap().flaky_after_publishes = times;
    }

    /// Makes the next `times` `open_pr` calls fail with a transient error, like a GitHub 502.
    pub fn flaky_open_pr(&self, times: usize) {
        self.state.lock().unwrap().flaky_open_prs = times;
    }

    /// Makes the next `times` `comment` calls fail with a transient error.
    pub fn flaky_comment(&self, times: usize) {
        self.state.lock().unwrap().flaky_comments = times;
    }

    /// How many times `publish` was called.
    pub fn publish_attempts(&self) -> usize {
        self.state.lock().unwrap().publish_attempts
    }

    /// Makes every later merge refused with this message, like a required check GitHub is still waiting on, until
    /// called with `None`.
    pub fn refuse_merge(&self, why: Option<&str>) {
        self.state.lock().unwrap().refuse_merge = why.map(str::to_string);
    }

    /// Makes the host report this rate limit from now on.
    pub fn set_rate_limit(&self, limit: Option<RateLimit>) {
        self.state.lock().unwrap().rate_limit = limit;
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

    /// How many times the host was asked for a PR's head.
    pub fn head_lookups(&self) -> usize {
        self.state.lock().unwrap().head_lookups
    }

    /// How many times the `CodeHost` method of this name (`"pr_state"`, `"merge"`, ...) was called.
    pub fn calls(&self, method: &str) -> usize {
        self.state.lock().unwrap().calls.get(method).copied().unwrap_or(0)
    }

    /// How many calls the host got in all, whatever the method.
    pub fn total_calls(&self) -> usize {
        self.state.lock().unwrap().calls.values().sum()
    }

    fn count(&self, method: &'static str) {
        *self.state.lock().unwrap().calls.entry(method).or_default() += 1;
    }

    /// Closes a PR without merging it, as a person would on GitHub.
    pub fn close(&self, pr: &PrRef) {
        self.find(pr, |p, _| p.closed = true).unwrap();
    }

    /// Sets whether the host says the PR merges cleanly (`None`: not worked out yet).
    pub fn set_mergeable(&self, pr: &PrRef, mergeable: Option<bool>) {
        self.find(pr, |p, _| p.mergeable = mergeable).unwrap();
    }

    /// Makes a conflict outlast the next push, as when the base moves again during a rework.
    pub fn keep_conflict_on_push(&self, pr: &PrRef) {
        self.find(pr, |p, _| p.conflict_survives_push = true).unwrap();
    }

    /// Makes `pr_state` report `head` as the PR's head (`None`: the real one), as a host that hasn't caught up with a
    /// push yet does.
    pub fn set_lagging_head(&self, pr: &PrRef, head: Option<&str>) {
        self.find(pr, |p, _| p.lagging_head = head.map(str::to_string)).unwrap();
    }

    /// A person deletes the PR's comment with this id.
    pub fn delete_comment(&self, pr: &PrRef, id: u64) {
        self.find(pr, |p, _| p.deleted.push(id)).unwrap();
    }

    /// Sets whether the PR's branch is behind its base.
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

    /// Sets how CI stood on `sha`, a head the PR had earlier (the current head's is `set_ci`'s).
    pub fn set_ci_at(&self, pr: &PrRef, sha: &str, ci: CiState) {
        self.find(pr, |p, _| p.ci_at.insert(sha.to_string(), ci)).unwrap();
    }

    /// Makes `behind` fail with a transient error until called with `false`.
    pub fn fail_behind(&self, on: bool) {
        self.state.lock().unwrap().fail_behind = on;
    }

    /// Turns the host's auto-merge on or off for the PR, as a person does.
    pub fn set_auto_merge(&self, pr: &PrRef, on: bool) {
        self.find(pr, |p, _| p.auto_merge = on).unwrap();
    }

    /// The host merges the PR by itself, as auto-merge does once the checks pass.
    pub fn merge_by_host(&self, pr: &PrRef) {
        self.find(pr, |p, _| p.merged = true).unwrap();
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
    fn rate_limit(&self) -> Option<RateLimit> {
        self.state.lock().unwrap().rate_limit
    }

    async fn publish(&self, repo: &Repo, start: &str, branch: &str, commits: &[Commit]) -> Result<()> {
        self.count("publish");
        let mut s = self.state.lock().unwrap();
        s.publish_attempts += 1;
        if s.flaky_publishes > 0 {
            s.flaky_publishes -= 1;
            return Err(Error::Other("github 500 Internal Server Error on POST /repos/o/r/git/blobs: \"\"".into()));
        }
        if let Some(why) = &s.refuse_publish {
            return Err(Error::NotFound(why.clone()));
        }
        let key = (repo.slug(), branch.to_string());
        // The work is already on the branch, from an attempt whose answer was lost: nothing left to do.
        let landed = !commits.is_empty()
            && s.branches.get(&key).is_some_and(|b| {
                let matches = b.len() >= commits.len()
                    && b[b.len() - commits.len()..].iter().map(|c| &c.message).eq(commits.iter().map(|c| &c.message));
                // The head that publish was made on top of: the branch's first start, or the head before it.
                let before = if b.len() == commits.len() {
                    s.starts.get(&key).cloned()
                } else {
                    Some(format!("{branch}@{}", b.len() - commits.len()))
                };
                matches && before.is_some_and(|before| before == start)
            });
        if landed {
            return Ok(());
        }
        if let Some(head) = s.heads.get(&key).filter(|head| *head != start) {
            return Err(Error::Conflict(format!("branch {branch} is at {head}, not at {start}")));
        }
        s.starts.entry(key.clone()).or_insert_with(|| start.to_string());
        let n = s.branches.get(&key).map_or(0, Vec::len) + commits.len();
        s.heads.insert(key.clone(), format!("{branch}@{n}"));
        // A push is a new head: the host works out afresh whether it merges, and the conflict a rework fixed is gone.
        for p in s.prs.iter_mut().filter(|p| {
            p.pr.repo == *repo && p.spec.branch == branch && p.mergeable == Some(false) && !p.conflict_survives_push
        }) {
            p.mergeable = Some(true);
        }
        s.branches.entry(key).or_default().extend_from_slice(commits);
        if s.flaky_after_publishes > 0 {
            s.flaky_after_publishes -= 1;
            return Err(Error::Other("github 502 Bad Gateway on PATCH /repos/o/r/git/refs/heads/b: \"\"".into()));
        }
        Ok(())
    }

    async fn open_pr(&self, repo: &Repo, pr: NewPr) -> Result<PrRef> {
        self.count("open_pr");
        let mut s = self.state.lock().unwrap();
        if s.flaky_open_prs > 0 {
            s.flaky_open_prs -= 1;
            return Err(Error::Other("github 502 Bad Gateway on POST /repos/o/r/pulls: \"\"".into()));
        }
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
            lagging_head: None,
            conflict_survives_push: false,
            behind: false,
            files: None,
            ci: CiState::Pending,
            ci_at: BTreeMap::new(),
            ci_logs: Vec::new(),
            auto_merge: false,
        });
        Ok(r)
    }

    async fn diff(&self, pr: &PrRef) -> Result<String> {
        self.count("diff");
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
        self.count("comment");
        {
            let mut s = self.state.lock().unwrap();
            if s.flaky_comments > 0 {
                s.flaky_comments -= 1;
                return Err(Error::Other("github 502 Bad Gateway on POST /repos/o/r/issues/1/comments: \"\"".into()));
            }
        }
        self.find(pr, |p, _| {
            p.comments.push(body.to_string());
            p.comments.len() as u64
        })
    }

    async fn edit_comment(&self, pr: &PrRef, id: u64, body: &str) -> Result<()> {
        self.count("edit_comment");
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
        self.count("merge");
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

    async fn open_prs(&self, repo: &Repo, base: &str) -> Result<Vec<OpenPr>> {
        self.count("open_prs");
        let s = self.state.lock().unwrap();
        Ok(s.prs
            .iter()
            .filter(|p| p.pr.repo == *repo && p.spec.base == base && !p.merged && !p.closed)
            .map(|p| OpenPr { pr: p.pr.clone(), auto_merge: p.auto_merge })
            .collect())
    }

    async fn behind(&self, pr: &PrRef) -> Result<bool> {
        if self.state.lock().unwrap().fail_behind {
            return Err(trailhead_factory::Error::Other("the host is down".into()));
        }
        self.count("behind");
        self.find(pr, |p, _| p.behind)
    }

    async fn update_branch(&self, pr: &PrRef) -> Result<BranchUpdate> {
        self.count("update_branch");
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
        self.count("carries_verdict");
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
        self.count("pr_state");
        self.find(pr, |p, s| match (p.merged, p.closed) {
            (true, _) => PrState::Merged { commit: Some(FakeCodeHost::merge_commit(&p.pr)) },
            (false, true) => PrState::Closed,
            _ => PrState::Open {
                mergeable: p.mergeable,
                head: p.lagging_head.clone().or_else(|| s.heads.get(&(pr.repo.slug(), p.spec.branch.clone())).cloned()),
            },
        })
    }

    async fn title(&self, pr: &PrRef) -> Result<String> {
        self.count("title");
        self.find(pr, |p, _| p.spec.title.clone())
    }

    async fn head(&self, pr: &PrRef) -> Result<String> {
        self.count("head");
        self.state.lock().unwrap().head_lookups += 1;
        self.find(pr, |p, s| s.heads.get(&(pr.repo.slug(), p.spec.branch.clone())).cloned().unwrap_or_default())
    }

    async fn ci(&self, pr: &PrRef) -> Result<Ci> {
        self.count("ci");
        let head = CodeHost::head(self, pr).await?;
        self.find(pr, |p, _| Ci { head, state: p.ci.clone() })
    }

    async fn ci_at(&self, pr: &PrRef, sha: &str) -> Result<CiState> {
        self.count("ci_at");
        self.find(pr, |p, s| {
            let current = s.heads.get(&(pr.repo.slug(), p.spec.branch.clone())).map(String::as_str);
            match p.ci_at.get(sha) {
                Some(state) => state.clone(),
                None if current == Some(sha) => p.ci.clone(),
                None => CiState::Pending,
            }
        })
    }

    async fn ci_logs(&self, pr: &PrRef, checks: &[String]) -> Result<Vec<(String, String)>> {
        self.count("ci_logs");
        self.find(pr, |p, _| p.ci_logs.iter().filter(|(n, _)| checks.contains(n)).cloned().collect())
    }

    async fn files(&self, pr: &PrRef) -> Result<Vec<ChangedFile>> {
        self.count("files");
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
                    let file = ChangedFile {
                        path: change.path.clone(),
                        status,
                        additions,
                        deletions: 0,
                        previous_path: None,
                        patch: None,
                    };
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
