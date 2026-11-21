//! The builder's state machine, on a fake git and cargo.

use std::collections::HashSet;
use std::sync::Mutex;

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
}

impl Fake {
    fn set_head(&self, rev: &str) {
        *self.head.lock().unwrap() = rev.into();
    }
}

impl Tools for Fake {
    fn origin<'a>(&'a self, _repo: &'a Path) -> Boxed<'a, Result<Origin, String>> {
        Box::pin(async { Ok(Origin { url: "git@example:apps".into(), branch: "main".into() }) })
    }

    fn head<'a>(&'a self, _origin: &'a Origin) -> Boxed<'a, Result<String, String>> {
        Box::pin(async { Ok(self.head.lock().unwrap().clone()) })
    }

    fn checkout<'a>(&'a self, _src: &'a Path, _origin: &'a Origin, rev: &'a str) -> Boxed<'a, Result<(), String>> {
        Box::pin(async move {
            *self.checked_out.lock().unwrap() = rev.into();
            Ok(())
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

#[test]
fn a_tail_is_the_last_lines() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log");
    std::fs::write(&log, "1\n2\n3\n").unwrap();
    assert_eq!(tail(&log, 2), "2\n3");
    assert_eq!(tail(&log, 9), "1\n2\n3");
    assert_eq!(tail(&dir.path().join("none"), 2), "");
}
