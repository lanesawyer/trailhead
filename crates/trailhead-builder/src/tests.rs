//! The builder's state machine, on a fake git and cargo.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use super::*;

/// What a test runs while a build is going.
type During = Box<dyn FnMut(&Layout, &Fake) + Send>;

/// A repo whose head a test moves, and a cargo that builds a file holding the commit it was given.
#[derive(Default)]
struct Fake {
    head: Mutex<String>,
    checked_out: Mutex<String>,
    failing: Mutex<HashSet<String>>,
    built: Mutex<Vec<String>>,
    /// Run while a build is going, with the layout: a test moving the head, or looking at the state.
    during: Mutex<Option<During>>,
    layout: Mutex<Option<Layout>>,
    /// What `git log` answers, and what it was asked.
    log: Mutex<Option<Result<Vec<String>, String>>>,
    logged: Mutex<Vec<(PathBuf, String, String)>>,
    /// The builder's checkout lock, and whether it was held while checking out.
    lock: Mutex<Option<Arc<RwLock<()>>>>,
    locked_in_checkout: Mutex<Option<bool>>,
    /// The checkout's `origin` URL, when a test sets one.
    url: Mutex<Option<String>>,
}

impl Fake {
    fn set_head(&self, rev: &str) {
        *self.head.lock().unwrap() = rev.into();
    }
}

impl Tools for Fake {
    fn origin<'a>(&'a self, _repo: &'a Path) -> Boxed<'a, Result<Origin, String>> {
        let url = self.url.lock().unwrap().clone().unwrap_or_else(|| "git@example:apps".into());
        Box::pin(async { Ok(Origin { url, branch: "main".into() }) })
    }

    fn head<'a>(&'a self, _origin: &'a Origin) -> Boxed<'a, Result<String, String>> {
        Box::pin(async { Ok(self.head.lock().unwrap().clone()) })
    }

    fn checkout<'a>(&'a self, _src: &'a Path, _origin: &'a Origin, rev: &'a str) -> Boxed<'a, Result<(), String>> {
        Box::pin(async move {
            *self.checked_out.lock().unwrap() = rev.into();
            if let Some(lock) = self.lock.lock().unwrap().as_ref() {
                *self.locked_in_checkout.lock().unwrap() = Some(lock.try_read().is_err());
            }
            Ok(())
        })
    }

    fn archive<'a>(&'a self, _src: &'a Path, rev: &'a str) -> Boxed<'a, Result<Vec<u8>, String>> {
        Box::pin(async move { Ok(format!("source of {rev}").into_bytes()) })
    }

    fn log<'a>(&'a self, src: &'a Path, since: &'a str, until: &'a str) -> Boxed<'a, Result<Vec<String>, String>> {
        Box::pin(async move {
            self.logged.lock().unwrap().push((src.to_path_buf(), since.into(), until.into()));
            self.log.lock().unwrap().clone().unwrap_or(Ok(Vec::new()))
        })
    }

    fn build<'a>(&'a self, job: &'a Job) -> Boxed<'a, Result<PathBuf, String>> {
        Box::pin(async move {
            let rev = self.checked_out.lock().unwrap().clone();
            self.built.lock().unwrap().push(rev.clone());
            let layout = self.layout.lock().unwrap().clone().unwrap();
            if let Some(during) = self.during.lock().unwrap().as_mut() {
                during(&layout, self);
            }
            std::fs::write(&job.log, format!("Compiling shell\nerror: in {rev}\n")).unwrap();
            if self.failing.lock().unwrap().contains(&rev) {
                return Err("cargo build failed (exit status: 101)".into());
            }
            let binary = job.target.join("release").join(BINARY);
            std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
            std::fs::write(&binary, &rev).unwrap();
            Ok(binary)
        })
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    layout: Layout,
    builder: Builder<std::sync::Arc<Fake>>,
    fake: std::sync::Arc<Fake>,
}

impl Tools for std::sync::Arc<Fake> {
    fn origin<'a>(&'a self, repo: &'a Path) -> Boxed<'a, Result<Origin, String>> {
        (**self).origin(repo)
    }

    fn head<'a>(&'a self, origin: &'a Origin) -> Boxed<'a, Result<String, String>> {
        (**self).head(origin)
    }

    fn checkout<'a>(&'a self, src: &'a Path, origin: &'a Origin, rev: &'a str) -> Boxed<'a, Result<(), String>> {
        (**self).checkout(src, origin, rev)
    }

    fn build<'a>(&'a self, job: &'a Job) -> Boxed<'a, Result<PathBuf, String>> {
        (**self).build(job)
    }

    fn archive<'a>(&'a self, src: &'a Path, rev: &'a str) -> Boxed<'a, Result<Vec<u8>, String>> {
        (**self).archive(src, rev)
    }

    fn log<'a>(&'a self, src: &'a Path, since: &'a str, until: &'a str) -> Boxed<'a, Result<Vec<String>, String>> {
        (**self).log(src, since, until)
    }
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let layout = Layout::new(dir.path());
    let fake = std::sync::Arc::new(Fake::default());
    *fake.layout.lock().unwrap() = Some(layout.clone());
    let config = ShellConfig::from_user_config("[shell]\nrepo = \"/apps\"\n").unwrap().unwrap();
    let builder = Builder::new(fake.clone(), config, "/apps".into(), layout.clone());
    Rig { _dir: dir, layout, builder, fake }
}

impl Rig {
    async fn step_at(&self, head: &str) -> Step {
        self.fake.set_head(head);
        self.builder.step().await.unwrap()
    }

    fn slots(&self) -> (Option<String>, Option<String>) {
        let read = self.layout.read();
        (read.current, read.previous)
    }

    fn builds(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.layout.builds())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn built(&self) -> Vec<String> {
        self.fake.built.lock().unwrap().clone()
    }
}

fn some(rev: &str) -> Option<String> {
    Some(rev.into())
}

#[tokio::test]
async fn a_new_head_is_built_the_slots_rotate_and_old_builds_are_pruned() {
    let rig = rig();
    assert_eq!(rig.step_at("a").await, Step::Built("a".into()));
    assert_eq!(rig.slots(), (some("a"), None));
    assert_eq!(std::fs::read_to_string(rig.layout.binary("a")).unwrap(), "a");
    assert_eq!(rig.step_at("a").await, Step::Idle, "a head already built is left alone");

    assert_eq!(rig.step_at("b").await, Step::Built("b".into()));
    assert_eq!(rig.slots(), (some("b"), some("a")));
    assert_eq!(rig.step_at("c").await, Step::Built("c".into()));
    assert_eq!(rig.slots(), (some("c"), some("b")));
    assert_eq!(rig.builds(), ["b", "c"]);
    assert_eq!(rig.built(), ["a", "b", "c"]);
    assert_eq!(rig.layout.read().building, None);
}

#[tokio::test]
async fn the_source_of_a_kept_build_is_handed_out_and_of_nothing_else() {
    let rig = rig();
    rig.step_at("a").await;
    rig.step_at("b").await;
    rig.step_at("c").await;
    let builds = rig.builder.builds();
    assert_eq!(builds.source("c").await, Some(Ok(b"source of c".to_vec())), "the newest");
    assert_eq!(builds.source("b").await, Some(Ok(b"source of b".to_vec())), "and the one before it");
    assert_eq!(builds.source("a").await, None, "a build no longer kept");
    assert_eq!(builds.source("HEAD").await, None, "nor whatever else git would take for a commit");
}

#[tokio::test]
async fn the_repo_is_the_origin_the_builder_last_read_as_owner_and_name() {
    let rig = rig();
    let builds = rig.builder.builds();
    assert_eq!(builds.repo(), None, "not before the builder's first look");
    rig.step_at("a").await;
    assert_eq!(builds.repo(), None, "an origin that isn't on GitHub");
    *rig.fake.url.lock().unwrap() = Some("git@github.com:lane/trailhead-apps.git".into());
    rig.step_at("a").await;
    assert_eq!(builds.repo().as_deref(), Some("lane/trailhead-apps"));
}

#[test]
fn a_github_remote_is_owner_and_name_in_its_usual_forms() {
    for url in [
        "https://github.com/lanesawyer/trailhead.git",
        "https://github.com/lanesawyer/trailhead",
        "git@github.com:lanesawyer/trailhead.git",
        "ssh://git@github.com/lanesawyer/trailhead.git",
    ] {
        assert_eq!(github_slug(url).as_deref(), Some("lanesawyer/trailhead"), "{url}");
    }
    assert_eq!(github_slug("https://example.com/a/b.git"), None);
    assert_eq!(github_slug("https://github.com/a"), None);
}

#[tokio::test]
async fn a_failed_build_keeps_the_current_one_and_is_not_tried_again() {
    let rig = rig();
    rig.step_at("a").await;
    rig.fake.failing.lock().unwrap().insert("b".into());
    assert_eq!(rig.step_at("b").await, Step::Failed("b".into()));
    assert_eq!(rig.slots(), (some("a"), None));
    let error = rig.layout.read().error.unwrap();
    assert_eq!((error.rev.as_str(), error.message.as_str()), ("b", "cargo build failed (exit status: 101)"));
    assert!(error.output.contains("error: in b"), "{}", error.output);
    assert_eq!(rig.builds(), ["a"]);

    assert_eq!(rig.step_at("b").await, Step::Idle);
    assert_eq!(rig.built(), ["a", "b"]);
    assert_eq!(rig.step_at("c").await, Step::Built("c".into()), "a newer head is tried");
    assert_eq!(rig.slots(), (some("c"), some("a")));
    assert_eq!(rig.layout.read().error, None);
}

#[tokio::test]
async fn a_head_that_moves_during_a_build_is_built_next() {
    let rig = rig();
    let seen = std::sync::Arc::new(Mutex::new(None));
    let saw = seen.clone();
    *rig.fake.during.lock().unwrap() = Some(Box::new(move |layout, fake| {
        if fake.head.lock().unwrap().as_str() == "a" {
            *saw.lock().unwrap() = layout.read().building;
            fake.set_head("b");
        }
    }));
    rig.fake.set_head("a");
    assert_eq!(rig.builder.step().await.unwrap(), Step::Built("a".into()));
    assert_eq!(*seen.lock().unwrap(), some("a"), "the state says what is building while it builds");
    assert_eq!(rig.slots(), (some("a"), None), "the build in progress finished");
    assert_eq!(rig.builder.step().await.unwrap(), Step::Built("b".into()));
    assert_eq!(rig.slots(), (some("b"), some("a")));
}

#[tokio::test]
async fn a_bad_build_is_not_built_again_or_kept_to_fall_back_to() {
    let rig = rig();
    rig.step_at("a").await;
    rig.step_at("b").await;
    rig.layout.mark_bad("b").unwrap();
    rig.layout.mark_bad("c").unwrap();
    assert_eq!(rig.step_at("c").await, Step::Idle);
    assert_eq!(rig.step_at("d").await, Step::Built("d".into()));
    assert_eq!(rig.slots(), (some("d"), some("a")), "the bad build is not the one to fall back to");
    assert_eq!(rig.builds(), ["a", "d"]);
}

#[tokio::test]
async fn stopping_the_builder_clears_a_build_left_in_progress() {
    let rig = rig();
    rig.layout.save(&State { building: some("z"), ..Default::default() }).unwrap();
    rig.fake.set_head("a");
    let Rig { builder, layout, _dir, .. } = rig;
    let running = builder.spawn();
    for _ in 0..100 {
        if layout.read().current.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    running.stop().await;
    let read = layout.read();
    assert_eq!((read.current, read.building), (some("a"), None));
}

/// Commit hashes for the tests that read the changes since a build.
fn hash(c: char) -> String {
    c.to_string().repeat(40)
}

#[tokio::test]
async fn the_changes_since_a_build_are_the_newest_first_capped_and_counted() {
    let rig = rig();
    let (a, b) = (hash('a'), hash('b'));
    rig.step_at(&a).await;
    rig.step_at(&b).await;
    let subjects: Vec<String> = (1..=41).rev().map(|n| format!("feat(x): change {n}")).collect();
    *rig.fake.log.lock().unwrap() = Some(Ok(subjects));
    let builds = rig.builder.builds();

    let read = builds.read(Some(&a)).await;
    assert_eq!((read.current.as_deref(), read.changes_total), (Some(b.as_str()), 41));
    assert_eq!(read.changes.len(), MAX_CHANGES);
    assert_eq!((read.changes[0].as_str(), read.changes[29].as_str()), ("feat(x): change 41", "feat(x): change 12"));
    assert_eq!(*rig.fake.logged.lock().unwrap(), [(rig.layout.src(), a.clone(), b.clone())]);

    rig.layout.mark_bad(&b).unwrap();
    builds.read(Some("aaaaaaa")).await;
    let asked = rig.fake.logged.lock().unwrap().last().cloned().unwrap();
    assert_eq!((asked.1.as_str(), asked.2), ("aaaaaaa", a), "up to the newest good build, from a short hash");
}

#[tokio::test]
async fn no_changes_are_read_without_a_commit_hash_before_the_newest_build() {
    let rig = rig();
    let a = hash('a');
    *rig.fake.log.lock().unwrap() = Some(Ok(vec!["fix: one".into()]));
    let builds = rig.builder.builds();
    assert_eq!(builds.read(Some(&hash('0'))).await.changes, Vec::<String>::new(), "nothing is built yet");
    rig.step_at(&a).await;
    for since in
        [None, Some(a.as_str()), Some("abc123"), Some("HEAD~3"), Some("--output=/tmp/x"), Some(&*"f".repeat(41))]
    {
        let read = builds.read(since).await;
        assert_eq!((read.changes.len(), read.changes_total), (0, 0), "{since:?}");
    }
    assert!(rig.fake.logged.lock().unwrap().is_empty(), "git was never asked");

    *rig.fake.log.lock().unwrap() = Some(Err("bad revision".into()));
    let read = builds.read(Some(&hash('9'))).await;
    assert_eq!((read.current, read.changes.len()), (Some(a), 0), "a commit the clone doesn't have is no error");
}

#[tokio::test]
async fn the_history_is_never_read_while_the_clone_is_checked_out() {
    let rig = rig();
    *rig.fake.lock.lock().unwrap() = Some(rig.builder.checkout.clone());
    rig.step_at("a").await;
    assert_eq!(*rig.fake.locked_in_checkout.lock().unwrap(), Some(true));
    assert!(rig.builder.checkout.try_write().is_ok(), "and let go after");
}

#[test]
fn a_tail_is_the_last_lines() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log");
    std::fs::write(&log, "1\n2\n3\n").unwrap();
    assert_eq!(tail(&log, 2), "2\n3");
    assert_eq!(tail(&log, 9), "1\n2\n3");
    assert_eq!(tail(&dir.path().join("none"), 2), "");
}
