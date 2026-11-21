//! The shell's builder (`docs/app-platform.md`, "The builder"): run by the server when the user config has
//! `[shell] repo`, it watches the apps repo's base branch, builds the shell from each new commit in a clone of its
//! own, and keeps the newest build and the one before it under `<data dir>/shell` ([`Layout`]) for the launcher,
//! `trailhead app`, to run.
//!
//! It never stops the server: a commit that doesn't build is recorded and skipped, and a newer one is tried.

mod config;
mod layout;
mod tools;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use tokio::sync::{RwLock, watch};
use tokio::task::JoinHandle;
use trailhead_model::shell::{BuildError, MAX_CHANGES, ShellBuild};

pub use config::ShellConfig;
pub use layout::{BINARY, Built, Layout, State};
pub use tools::{Boxed, Job, Origin, System, Tools};

/// How many of cargo's last lines a failure keeps in the state.
const TAIL_LINES: usize = 40;

/// How long a read of the changes since a build waits, for a checkout to finish and for git, before it answers
/// without them.
const CHANGES_WAIT: Duration = Duration::from_secs(5);

/// What one look at the apps repo did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// The head is built already, failed already or is bad.
    Idle,
    Built(String),
    Failed(String),
}

/// The builder for one apps repo.
pub struct Builder<T> {
    tools: Arc<T>,
    config: ShellConfig,
    repo: PathBuf,
    layout: Layout,
    /// Held to write while the clone is checked out (fetched, checked out, cleaned), and to read while [`Builds`]
    /// reads its history, so the two never meet.
    checkout: Arc<RwLock<()>>,
    /// The checkout's `origin` URL as last read, which [`Builds::repo`] answers from without asking git.
    origin: Arc<std::sync::RwLock<Option<String>>>,
}

impl<T: Tools> Builder<T> {
    pub fn new(tools: T, config: ShellConfig, repo: PathBuf, layout: Layout) -> Self {
        Builder { tools: Arc::new(tools), config, repo, layout, checkout: Arc::default(), origin: Arc::default() }
    }

    /// What this builder has built, for the server to answer from.
    pub fn builds(&self) -> Builds {
        Builds {
            layout: self.layout.clone(),
            tools: self.tools.clone(),
            checkout: self.checkout.clone(),
            origin: self.origin.clone(),
        }
    }

    /// Looks at the base branch once, and builds its head when it is new: not built, not failed and not bad.
    pub async fn step(&self) -> Result<Step, String> {
        let origin = self.tools.origin(&self.repo).await?;
        let known = self.origin.read().unwrap_or_else(|e| e.into_inner()).as_deref() == Some(origin.url.as_str());
        if !known {
            *self.origin.write().unwrap_or_else(|e| e.into_inner()) = Some(origin.url.clone());
        }
        let head = self.tools.head(&origin).await?;
        let mut state = self.layout.load();
        let bad = self.layout.bad();
        let seen = state.current.as_ref().map(|b| &b.rev) == Some(&head)
            || state.error.as_ref().map(|e| &e.rev) == Some(&head)
            || bad.contains(&head);
        if seen {
            return Ok(Step::Idle);
        }
        state.building = Some(head.clone());
        self.save(&state);
        tracing::info!(target: "shell", rev = short(&head), "building the shell");
        let built = self.build(&origin, &head).await;
        state.building = None;
        let step = match built {
            Ok(()) => {
                let old = state.current.take();
                if let Some(old) = old.filter(|o| !bad.contains(&o.rev)) {
                    state.previous = Some(old);
                }
                state.current = Some(Built { rev: head.clone(), at: Timestamp::now() });
                state.error = None;
                tracing::info!(target: "shell", rev = short(&head), "the shell is built");
                Step::Built(head)
            }
            Err(message) => {
                tracing::warn!(target: "shell", rev = short(&head), error = %message, "the shell didn't build");
                let output = tail(&self.layout.log(), TAIL_LINES);
                state.error = Some(BuildError { rev: head.clone(), message, output, at: Timestamp::now() });
                Step::Failed(head)
            }
        };
        self.save(&state);
        self.prune(&state);
        Ok(step)
    }

    async fn build(&self, origin: &Origin, rev: &str) -> Result<(), String> {
        // Emptied first, so a checkout that fails doesn't report the last build's output as its own.
        std::fs::create_dir_all(&self.layout.dir)
            .and_then(|()| std::fs::write(self.layout.log(), ""))
            .map_err(|e| e.to_string())?;
        let src = self.layout.src();
        {
            let _checking_out = self.checkout.write().await;
            self.tools.checkout(&src, origin, rev).await?;
        }
        let job = Job {
            src,
            target: self.layout.target(),
            log: self.layout.log(),
            package: self.config.package.clone(),
            profile: self.config.profile.clone(),
            jobs: self.config.jobs,
        };
        let binary = self.tools.build(&job).await?;
        self.install(&binary, rev).map_err(|e| format!("keeping the build: {e}"))
    }

    /// Copies the binary into `builds/<rev>`, beside it first, so the launcher never finds half of one.
    fn install(&self, binary: &Path, rev: &str) -> std::io::Result<()> {
        let builds = self.layout.builds();
        let beside = builds.join(format!("{rev}.new"));
        let _ = std::fs::remove_dir_all(&beside);
        std::fs::create_dir_all(&beside)?;
        std::fs::copy(binary, beside.join(BINARY))?;
        let dir = builds.join(rev);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::rename(&beside, &dir)
    }

    /// Removes every build but the current one and the one before it.
    fn prune(&self, state: &State) {
        let keep: Vec<&str> = [&state.current, &state.previous].into_iter().flatten().map(|b| b.rev.as_str()).collect();
        let Ok(entries) = std::fs::read_dir(self.layout.builds()) else { return };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !keep.contains(&name.as_str())
                && let Err(e) = std::fs::remove_dir_all(entry.path())
            {
                tracing::warn!(target: "shell", build = name, error = %e, "couldn't remove an old build");
            }
        }
    }

    fn save(&self, state: &State) {
        if let Err(e) = self.layout.save(state) {
            tracing::warn!(target: "shell", error = %e, "couldn't write the builder's state");
        }
    }

    /// Looks every `poll_secs` until stopped. A build in progress when it stops is killed, and `building` cleared.
    async fn run(self, mut stop: watch::Receiver<bool>) {
        self.clear_building();
        let mut tick = tokio::time::interval(Duration::from_secs(self.config.poll_secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_error: Option<String> = None;
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = stop.wait_for(|s| *s) => break,
            }
            let step = tokio::select! {
                step = self.step() => step,
                _ = stop.wait_for(|s| *s) => break,
            };
            match step {
                Err(e) if last_error.as_ref() != Some(&e) => {
                    tracing::warn!(target: "shell", error = %e, "couldn't look at the apps repo");
                    last_error = Some(e);
                }
                Err(e) => tracing::debug!(target: "shell", error = %e, "couldn't look at the apps repo"),
                Ok(_) => last_error = None,
            }
        }
        self.clear_building();
    }

    fn clear_building(&self) {
        let mut state = self.layout.load();
        if state.building.take().is_some() {
            self.save(&state);
        }
    }

    /// Runs the builder on its own task until [`Running::stop`].
    pub fn spawn(self) -> Running {
        let (stop, stopped) = watch::channel(false);
        let builds = self.builds();
        Running { stop, builds, task: tokio::spawn(self.run(stopped)) }
    }
}

/// What a builder has built, read for `GET /v1/shell/build`: its state, and the commits since a build in its clone.
#[derive(Clone)]
pub struct Builds {
    layout: Layout,
    tools: Arc<dyn Tools>,
    checkout: Arc<RwLock<()>>,
    origin: Arc<std::sync::RwLock<Option<String>>>,
}

impl Builds {
    /// Builds kept in `layout`, their history read with `tools`. A builder's own is [`Builder::builds`], which
    /// never reads the history while it checks the clone out; this one has no builder to wait for.
    pub fn new(layout: Layout, tools: impl Tools) -> Builds {
        Builds { layout, tools: Arc::new(tools), checkout: Arc::default(), origin: Arc::default() }
    }

    /// These builds, from an apps repo whose `origin` is `url`, which a builder otherwise learns on its first look.
    #[must_use]
    pub fn from_origin(self, url: impl Into<String>) -> Builds {
        *self.origin.write().unwrap_or_else(|e| e.into_inner()) = Some(url.into());
        self
    }

    /// The apps repo as `owner/name`, from its checkout's `origin` as the builder last read it: `None` before its
    /// first look, and for an `origin` that isn't on GitHub.
    pub fn repo(&self) -> Option<String> {
        self.origin.read().unwrap_or_else(|e| e.into_inner()).as_deref().and_then(github_slug)
    }

    /// The state and the bad builds, and with `since` the commits after it up to the newest build
    /// ([`ShellBuild::changes`]). The changes are left empty, never an error, when `since` isn't a commit hash, is
    /// the newest build or isn't in the builder's clone, and when git doesn't answer within [`CHANGES_WAIT`].
    pub async fn read(&self, since: Option<&str>) -> ShellBuild {
        let mut build = self.layout.read();
        let Some(since) = since.filter(|s| is_rev(s)) else { return build };
        let Some(newest) = build.newest().filter(|n| is_rev(n) && *n != since).map(String::from) else { return build };
        let src = self.layout.src();
        let read = async {
            let _reading = self.checkout.read().await;
            self.tools.log(&src, since, &newest).await
        };
        match tokio::time::timeout(CHANGES_WAIT, read).await {
            Ok(Ok(mut subjects)) => {
                build.changes_total = subjects.len();
                subjects.truncate(MAX_CHANGES);
                build.changes = subjects;
            }
            Ok(Err(e)) => tracing::debug!(target: "shell", since, error = %e, "no changes since that build"),
            Err(_) => tracing::debug!(target: "shell", since, "reading the changes since a build took too long"),
        }
        build
    }
}

impl Builds {
    /// The source of the build `rev`, as a gzipped tar, for a launcher on another machine to build itself
    /// (`docs/app-platform.md`, "The window on another machine"). Only the builds kept: the newest and the one
    /// before it. `None` for anything else.
    pub async fn source(&self, rev: &str) -> Option<Result<Vec<u8>, String>> {
        let state = self.layout.load();
        let kept = [&state.current, &state.previous].into_iter().flatten().any(|b| b.rev == rev);
        if !kept {
            return None;
        }
        let src = self.layout.src();
        let _reading = self.checkout.read().await;
        Some(self.tools.archive(&src, rev).await)
    }
}

impl std::fmt::Debug for Builds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Builds").field("layout", &self.layout).finish_non_exhaustive()
    }
}

/// `owner/name` of a GitHub remote URL, in any of its usual forms (`https://github.com/o/n.git`, `git@github.com:o/n`).
pub fn github_slug(url: &str) -> Option<String> {
    let path = url.trim().split_once("github.com").map(|(_, rest)| rest)?;
    let path = path.trim_start_matches([':', '/']).trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    (!owner.is_empty() && !name.is_empty() && !name.contains('/')).then(|| format!("{owner}/{name}"))
}

/// Whether `rev` is a commit hash, whole or abbreviated: 7 to 40 hex digits. Anything else never reaches git.
fn is_rev(rev: &str) -> bool {
    (7..=40).contains(&rev.len()) && rev.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A builder running on its own task.
pub struct Running {
    stop: watch::Sender<bool>,
    builds: Builds,
    task: JoinHandle<()>,
}

impl Running {
    /// What it has built, for the server to answer from.
    pub fn builds(&self) -> Builds {
        self.builds.clone()
    }

    /// Stops it, killing a build in progress, and waits for it to finish.
    pub async fn stop(self) {
        self.stop.send_replace(true);
        if let Err(e) = self.task.await {
            tracing::warn!(target: "shell", error = %e, "the builder's task ended badly");
        }
    }
}

/// Starts the builder the user config in `config_dir` asks for, keeping its builds under `data_dir`. `Ok(None)`
/// when `[shell]` names no repo. The error is a configuration error, for the server to log; it doesn't stop the
/// server.
pub fn start(config_dir: &Path, data_dir: &Path) -> Result<Option<Running>, String> {
    let Some(config) = ShellConfig::read(config_dir)? else { return Ok(None) };
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let repo = config.checked_repo_dir(home.as_deref())?.expect("a config that was read names a repo");
    tracing::info!(target: "shell", repo = %repo.display(), "the shell's builder is on");
    Ok(Some(Builder::new(System::new(), config, repo, Layout::new(data_dir)).spawn()))
}

/// A commit's first twelve characters, for the log.
fn short(rev: &str) -> &str {
    rev.get(..12).unwrap_or(rev)
}

/// The last `lines` lines of a file, or nothing when it can't be read.
fn tail(path: &Path, lines: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

#[cfg(test)]
mod tests;
