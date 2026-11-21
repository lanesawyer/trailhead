//! The projects a repo's factory works: the repo file's own project, under its queue, and every project whose metadata
//! points at the repo (`docs/factory/operating.md`). Pointed projects are looked up on each pass, so one pointed at the
//! repo while the factory runs is worked from its next poll, with no restart.

use std::collections::HashSet;

use trailhead_factory::{Result, Scope, TaskKey};

use crate::Factory;

impl Factory {
    /// The scopes this pass works: the file's own project first, then each project pointed at this repo. A listing that
    /// fails fails the pass's read, as any tracker read does.
    pub(crate) async fn scopes(&self) -> Result<Vec<Scope>> {
        let listed = self.tracker.project_repos().await?;
        let slug = self.config.repo.repo().slug();
        let mut scopes = Scope::for_repo(self.project(), &self.config.repo.queue, &slug, listed);
        self.skip_misdirected(&mut scopes, &slug);
        *self.pointed.lock().unwrap() = scopes.iter().filter(|s| s.pointed).cloned().collect();
        Ok(scopes)
    }

    /// Drops the pointed projects that are another repo's file project on this host, a mistake (both repos would work
    /// them), with a warning once for as long as each stays that way.
    fn skip_misdirected(&self, scopes: &mut Vec<Scope>, slug: &str) {
        let mut misdirected = HashSet::new();
        scopes.retain(|s| {
            let skip = s.pointed && self.host.is_file_project(&s.project);
            if skip {
                misdirected.insert(s.project.clone());
            }
            !skip
        });
        let mut warned = self.misdirected.lock().unwrap();
        for project in misdirected.difference(&warned) {
            tracing::warn!(
                project,
                repo = slug,
                "a project points at this repo but is another repo's own project in its .trailhead.toml; skipping it until one of them changes"
            );
        }
        *warned = misdirected;
    }

    /// The path the task's project builds in, if the project points at this repo with one, as last listed.
    pub(crate) fn path_of(&self, task: &TaskKey) -> Option<String> {
        self.pointed.lock().unwrap().iter().find(|s| s.project == task.project()).and_then(|s| s.path.clone())
    }
}
