//! Risk grades for a PR (`docs/factory/review.md#grades`): the task's grade, raised by what the diff touches.

use serde::{Deserialize, Serialize};

/// How a PR changed one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    Added,
    Modified,
    Removed,
    Renamed,
    /// Copied, or a mode change: anything else the host reports.
    Other,
}

/// One file of a PR, as the code host lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    pub status: FileStatus,
    pub additions: u32,
    pub deletions: u32,
    /// Where a renamed file was before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_path: Option<String>,
}

impl ChangedFile {
    pub fn lines(&self) -> u64 {
        u64::from(self.additions) + u64::from(self.deletions)
    }
}

/// A PR's grade, and what raised it above the task's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrGrade {
    pub grade: u8,
    /// The task's own grade; none for a task that was never graded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<u8>,
    /// Why the grade is above the task's: the files or rules that did it, and the reviewer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

impl PrGrade {
    /// Raises the grade to `grade` on the reviewer's say-so. Returns whether it was a raise; a reviewer can't lower.
    pub fn raise(&mut self, grade: u8, reason: &str) -> bool {
        if !(1..=4).contains(&grade) || grade <= self.grade {
            return false;
        }
        self.grade = grade;
        self.reasons.push(format!("the reviewer raised it: {}", reason.trim()));
        true
    }

    /// Whether the grade is above the task's, so the task should be raised to match.
    pub fn raised(&self) -> bool {
        self.grade > self.task.unwrap_or(0)
    }

    /// `Grade 3 (task was 1; Cargo.lock changed)`.
    pub fn describe(&self) -> String {
        let mut notes = Vec::new();
        if self.raised() {
            match self.task {
                Some(t) => notes.push(format!("task was {t}")),
                None => notes.push("task had no grade".to_string()),
            }
        }
        notes.extend(self.reasons.iter().cloned());
        match notes.is_empty() {
            true => format!("Grade {}", self.grade),
            false => format!("Grade {} ({})", self.grade, notes.join("; ")),
        }
    }
}

/// Matches a `/`-separated path against a glob: `*` and `?` within a segment, and a `**` segment for any number of
/// segments, none included (so `**/Cargo.toml` matches the root's too).
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pattern: Vec<&str> = pattern.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    segments_match(&pattern, &path)
}

fn segments_match(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| segments_match(rest, &path[skip..])),
        Some((p, rest)) => match path.split_first() {
            Some((s, tail)) => segment_match(p.as_bytes(), s.as_bytes()) && segments_match(rest, tail),
            None => false,
        },
    }
}

fn segment_match(pattern: &[u8], text: &[u8]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some((b'*', rest)) => (0..=text.len()).any(|skip| segment_match(rest, &text[skip..])),
        Some((b'?', rest)) => !text.is_empty() && segment_match(rest, &text[1..]),
        Some((c, rest)) => text.first() == Some(c) && segment_match(rest, &text[1..]),
    }
}

/// Whether the path looks like a test: in a `tests`-like directory, or named like one.
pub fn is_test_path(path: &str) -> bool {
    let mut segments: Vec<&str> = path.split('/').collect();
    let file = segments.pop().unwrap_or_default();
    if segments.iter().any(|s| matches!(*s, "test" | "tests" | "__tests__" | "spec" | "specs")) {
        return true;
    }
    let stem = file.split('.').next().unwrap_or(file);
    stem.starts_with("test_")
        || stem.ends_with("_test")
        || stem.ends_with("_spec")
        || file.contains(".test.")
        || file.contains(".spec.")
}

/// Files that define what the repo's checks run: editing one can weaken the gate it feeds.
const CHECK_DEFINITIONS: &[&str] = &[
    "factory.toml",
    "mise.toml",
    ".mise.toml",
    "Makefile",
    "justfile",
    "tox.ini",
    ".pre-commit-config.yaml",
    ".github/workflows/**",
];

pub fn is_check_definition(path: &str) -> bool {
    CHECK_DEFINITIONS.iter().any(|p| glob_match(p, path))
}

/// Floor of a deleted or renamed test, or an edited check's definition: never grade 1.
const TEST_OR_CHECK_GRADE: u8 = 2;

/// Reasons shown in full before the rest are counted.
const SHOWN_REASONS: usize = 3;

/// Path globs per grade and the line limit, from `[grades]` in `factory.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grader<'a> {
    /// (grade, globs), any order.
    pub rules: Vec<(u8, &'a [String])>,
    pub max_lines: u32,
}

impl Grader<'_> {
    /// The PR's grade: the highest of the task's, any rule a changed file matches, the line limit and the floor for
    /// touching tests and checks. `reasons` name what put it above the task's, if anything did.
    pub fn grade(&self, task: Option<u8>, files: &[ChangedFile]) -> PrGrade {
        // (grade, reason) per finding.
        let mut found: Vec<(u8, String)> = Vec::new();
        for f in files {
            let rule = self
                .rules
                .iter()
                .filter(|(_, globs)| globs.iter().any(|g| glob_match(g, &f.path)))
                .map(|(grade, _)| *grade)
                .max();
            if let Some(grade) = rule {
                found.push((grade, format!("{} changed", f.path)));
            }
            let lost_test = |p: &str| is_test_path(p);
            match (f.status, &f.previous_path) {
                (FileStatus::Removed, _) if lost_test(&f.path) => {
                    found.push((TEST_OR_CHECK_GRADE, format!("test {} deleted", f.path)))
                }
                (FileStatus::Renamed, Some(old)) if lost_test(old) => {
                    found.push((TEST_OR_CHECK_GRADE, format!("test {old} renamed")))
                }
                _ => {}
            }
            if is_check_definition(&f.path) || f.previous_path.as_deref().is_some_and(is_check_definition) {
                found.push((TEST_OR_CHECK_GRADE, format!("{} (a check's definition) changed", f.path)));
            }
        }
        let lines: u64 = files.iter().map(ChangedFile::lines).sum();
        if lines > u64::from(self.max_lines) {
            found.push((3, format!("{lines} lines changed, over {}", self.max_lines)));
        }

        let base = task.unwrap_or(1);
        let top = found.iter().map(|(g, _)| *g).max().unwrap_or(0).max(base);
        let mut reasons: Vec<String> = Vec::new();
        if top > base {
            for (_, why) in found.iter().filter(|(g, _)| *g == top) {
                if !reasons.contains(why) {
                    reasons.push(why.clone());
                }
            }
            if reasons.len() > SHOWN_REASONS {
                let more = reasons.len() - SHOWN_REASONS;
                reasons.truncate(SHOWN_REASONS);
                reasons.push(format!("and {more} more"));
            }
        }
        PrGrade { grade: top, task, reasons }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, status: FileStatus, lines: u32) -> ChangedFile {
        ChangedFile { path: path.into(), status, additions: lines, deletions: 0, previous_path: None }
    }

    fn modified(path: &str) -> ChangedFile {
        file(path, FileStatus::Modified, 1)
    }

    struct Rules {
        two: Vec<String>,
        three: Vec<String>,
        four: Vec<String>,
    }

    fn rules() -> Rules {
        let v = |g: &[&str]| g.iter().map(|s| s.to_string()).collect();
        Rules {
            two: v(&["crates/**"]),
            three: v(&["**/Cargo.toml", "Cargo.lock", "**/migrations/**"]),
            four: v(&[".github/**", "factory.toml"]),
        }
    }

    fn grader(r: &Rules) -> Grader<'_> {
        Grader { rules: vec![(2, &r.two), (3, &r.three), (4, &r.four)], max_lines: 600 }
    }

    #[test]
    fn globs() {
        assert!(glob_match("crates/**", "crates/a/b.rs"));
        assert!(glob_match("**/Cargo.toml", "Cargo.toml"));
        assert!(glob_match("**/Cargo.toml", "crates/a/Cargo.toml"));
        assert!(glob_match("**/migrations/**", "crates/core/migrations/0001.sql"));
        assert!(glob_match("docs/*.md", "docs/a.md"));
        assert!(!glob_match("docs/*.md", "docs/a/b.md"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("Cargo.lock", "crates/Cargo.lock"));
        assert!(!glob_match("crates/**", "docs/crates/a.rs"));
    }

    #[test]
    fn docs_only_stays_at_the_tasks_grade() {
        let r = rules();
        let g = grader(&r).grade(Some(1), &[modified("docs/a.md")]);
        assert_eq!(g, PrGrade { grade: 1, task: Some(1), reasons: vec![] });
        assert_eq!(g.describe(), "Grade 1");
        assert!(!g.raised());
    }

    #[test]
    fn the_highest_matching_rule_raises() {
        let r = rules();
        let g = grader(&r).grade(Some(1), &[modified("crates/a/src/lib.rs"), modified("Cargo.lock")]);
        assert_eq!(g.grade, 3);
        assert_eq!(g.describe(), "Grade 3 (task was 1; Cargo.lock changed)");
        let g = grader(&r).grade(Some(1), &[modified("crates/a/src/lib.rs")]);
        assert_eq!(g.describe(), "Grade 2 (task was 1; crates/a/src/lib.rs changed)");
        let g = grader(&r).grade(Some(3), &[modified("crates/a/src/lib.rs")]);
        assert_eq!((g.grade, g.reasons.is_empty()), (3, true), "a higher task grade stands");
        let g = grader(&r).grade(Some(2), &[modified("factory.toml")]);
        assert_eq!(g.grade, 4);
    }

    #[test]
    fn a_big_diff_is_at_least_grade_three() {
        let r = rules();
        let big = [file("docs/a.md", FileStatus::Modified, 400), file("docs/b.md", FileStatus::Added, 201)];
        let g = grader(&r).grade(Some(1), &big);
        assert_eq!(g.grade, 3);
        assert_eq!(g.reasons, ["601 lines changed, over 600"]);
        let ok = [file("docs/a.md", FileStatus::Modified, 600)];
        assert_eq!(grader(&r).grade(Some(1), &ok).grade, 1, "the limit itself is fine");
    }

    #[test]
    fn deleting_a_test_or_editing_a_check_is_never_grade_one() {
        let r = Rules { two: vec![], three: vec![], four: vec![] };
        let g = grader(&r).grade(Some(1), &[file("crates/a/tests/it.rs", FileStatus::Removed, 0)]);
        assert_eq!((g.grade, g.reasons), (2, vec!["test crates/a/tests/it.rs deleted".to_string()]));
        let renamed =
            ChangedFile { previous_path: Some("src/test_a.py".into()), ..file("src/a.py", FileStatus::Renamed, 0) };
        assert_eq!(grader(&r).grade(Some(1), &[renamed]).grade, 2);
        let g = grader(&r).grade(Some(1), &[modified("mise.toml")]);
        assert_eq!(g.grade, 2);
        assert_eq!(grader(&r).grade(Some(1), &[modified(".github/workflows/ci.yml")]).grade, 2);
        // Adding or editing a test is grade 1.
        let edit = [modified("crates/a/tests/it.rs"), file("tests/new.rs", FileStatus::Added, 5)];
        assert_eq!(grader(&r).grade(Some(1), &edit).grade, 1);
    }

    #[test]
    fn many_reasons_are_counted() {
        let r = rules();
        let files: Vec<_> = (0..6).map(|i| modified(&format!("crates/f{i}.rs"))).collect();
        let g = grader(&r).grade(Some(1), &files);
        assert_eq!(g.reasons.len(), 4);
        assert_eq!(g.reasons[3], "and 3 more");
    }

    #[test]
    fn an_ungraded_task_counts_as_grade_one() {
        let r = rules();
        let g = grader(&r).grade(None, &[modified("Cargo.lock")]);
        assert_eq!(g.describe(), "Grade 3 (task had no grade; Cargo.lock changed)");
    }

    #[test]
    fn a_reviewer_can_raise_but_not_lower() {
        let r = rules();
        let mut g = grader(&r).grade(Some(1), &[modified("Cargo.lock")]);
        assert!(!g.raise(2, "meh"));
        assert!(!g.raise(9, "nonsense"));
        assert!(g.raise(4, "touches auth"));
        assert_eq!(g.describe(), "Grade 4 (task was 1; Cargo.lock changed; the reviewer raised it: touches auth)");
    }
}
