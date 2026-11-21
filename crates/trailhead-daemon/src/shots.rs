//! Pictures of a change (`docs/factory/sandbox.md#pictures`): the repo's `[factory.shots]` command draws its views to
//! PNGs, once on the change and once on the commit it is against, and the views whose bytes differ are what the task
//! and the reviewer get. Drawing is the repo's business; this is what is kept of it.

use std::collections::BTreeMap;
use std::path::Path;

use trailhead_factory::messages::ShotChange;
use trailhead_model::{MAX_ATTACHMENT_BYTES, attachment_name};

/// The most views one run attaches, each with its before and its after. A task holds 64 pictures and 32 MiB, and a
/// change that redraws everything is told apart by the count, not by every picture.
pub const MAX_SHOTS: usize = 12;

/// One view the change alters: its picture before, after, or only one of them for a view the change adds or removes.
#[derive(Clone, PartialEq, Eq)]
pub struct Shot {
    pub name: String,
    pub before: Option<Vec<u8>>,
    pub after: Option<Vec<u8>>,
}

impl Shot {
    pub fn change(&self) -> ShotChange {
        match (&self.before, &self.after) {
            (None, _) => ShotChange::New,
            (_, None) => ShotChange::Removed,
            _ => ShotChange::Changed,
        }
    }
}

impl std::fmt::Debug for Shot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Shot({} {:?})", self.name, self.change())
    }
}

/// What taking a change's pictures came to. Never an error: a run goes on without its pictures.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Taken {
    /// The views that differ, by name, [`MAX_SHOTS`] at most.
    pub shots: Vec<Shot>,
    /// How many more differ than `shots` holds.
    pub more: usize,
    /// Why some or all of the pictures are missing, for the PR.
    pub failed: Option<String>,
}

impl Taken {
    pub fn failed(why: impl Into<String>) -> Taken {
        Taken { failed: Some(why.into()), ..Taken::default() }
    }

    /// The views of `after` that differ from `before`'s, and the ones only one side has. `before` is `None` when the
    /// base's pictures couldn't be taken: every view then counts as new.
    pub fn compare(
        before: Option<BTreeMap<String, Vec<u8>>>,
        after: BTreeMap<String, Vec<u8>>,
        failed: Option<String>,
    ) -> Taken {
        let mut before = before.unwrap_or_default();
        let mut shots = Vec::new();
        for (name, after) in after {
            match before.remove(&name) {
                Some(was) if was == after => {}
                was => shots.push(Shot { name, before: was, after: Some(after) }),
            }
        }
        shots.extend(before.into_iter().map(|(name, was)| Shot { name, before: Some(was), after: None }));
        shots.sort_by(|a, b| a.name.cmp(&b.name));
        let more = shots.len().saturating_sub(MAX_SHOTS);
        shots.truncate(MAX_SHOTS);
        Taken { shots, more, failed }
    }
}

/// What [`read`] finds: the pictures by name, and why any were left out.
pub type Read = (BTreeMap<String, Vec<u8>>, Vec<String>);

/// The PNGs in `dir`, by file name without the extension, and what was wrong with the ones left out: a name a task's
/// picture can't have, or more bytes than one may. A directory that isn't there holds none.
pub fn read(dir: &Path) -> std::io::Result<Read> {
    let mut pictures = BTreeMap::new();
    let mut left_out = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((pictures, left_out)),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("png") || !path.is_file() {
            continue;
        }
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let Ok(name) = attachment_name(&stem) else {
            left_out.push(format!("`{stem}.png` has a name a picture can't have"));
            continue;
        };
        let bytes = std::fs::read(&path)?;
        if bytes.len() > MAX_ATTACHMENT_BYTES {
            left_out.push(format!(
                "`{name}.png` is {} bytes, over the {MAX_ATTACHMENT_BYTES} a picture may be",
                bytes.len()
            ));
            continue;
        }
        pictures.insert(name, bytes);
    }
    left_out.sort();
    Ok((pictures, left_out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn views(views: &[(&str, &[u8])]) -> BTreeMap<String, Vec<u8>> {
        views.iter().map(|(n, b)| (n.to_string(), b.to_vec())).collect()
    }

    #[test]
    fn only_the_views_that_differ_are_kept() {
        let before = views(&[("board", b"a"), ("gone", b"g"), ("same", b"s")]);
        let after = views(&[("board", b"b"), ("fresh", b"f"), ("same", b"s")]);
        let taken = Taken::compare(Some(before), after, None);
        let got: Vec<_> = taken.shots.iter().map(|s| (s.name.as_str(), s.change())).collect();
        assert_eq!(
            got,
            [("board", ShotChange::Changed), ("fresh", ShotChange::New), ("gone", ShotChange::Removed)],
            "by name, without the view that didn't change"
        );
        assert_eq!((taken.more, taken.failed), (0, None));
    }

    #[test]
    fn without_the_base_every_view_is_new() {
        let taken = Taken::compare(None, views(&[("a", b"1"), ("b", b"2")]), Some("the base didn't build".into()));
        assert!(taken.shots.iter().all(|s| s.change() == ShotChange::New) && taken.shots.len() == 2);
        assert_eq!(taken.failed.as_deref(), Some("the base didn't build"));
    }

    #[test]
    fn a_change_that_redraws_everything_is_cut_to_the_first_views_and_counted() {
        let after: BTreeMap<String, Vec<u8>> = (0..MAX_SHOTS + 5).map(|i| (format!("v{i:02}"), vec![1])).collect();
        let taken = Taken::compare(Some(BTreeMap::new()), after, None);
        assert_eq!((taken.shots.len(), taken.more), (MAX_SHOTS, 5));
        assert_eq!(taken.shots[0].name, "v00");
    }

    #[test]
    fn reading_keeps_pngs_a_task_can_hold() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("board-dark.png"), b"png").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"x").unwrap();
        std::fs::write(dir.path().join("bad name.png"), b"x").unwrap();
        std::fs::write(dir.path().join("huge.png"), vec![0; MAX_ATTACHMENT_BYTES + 1]).unwrap();
        let (pictures, left_out) = read(dir.path()).unwrap();
        assert_eq!(pictures.keys().collect::<Vec<_>>(), ["board-dark"]);
        assert_eq!(left_out.len(), 2, "{left_out:?}");
        assert!(left_out[0].contains("bad name") && left_out[1].contains("huge"), "{left_out:?}");
        assert_eq!(read(&dir.path().join("missing")).unwrap().0.len(), 0);
    }
}
