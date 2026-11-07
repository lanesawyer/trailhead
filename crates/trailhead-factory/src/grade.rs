//! Risk grades for a PR (`docs/factory/review.md#grades`): the task's grade, raised by what the diff touches.

use serde::{Deserialize, Serialize};
use trailhead_model::Grade;

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
    /// The file's unified diff, when the host gave one and the grader reads it (a `Cargo.lock`); none otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
}

impl ChangedFile {
    pub fn lines(&self) -> u64 {
        u64::from(self.additions) + u64::from(self.deletions)
    }
}

/// A PR's grade, and what raised it above the task's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrGrade {
    pub grade: Grade,
    /// The task's own grade; none for a task that was never graded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<Grade>,
    /// Why the grade is above the task's: the files or rules that did it, and the reviewer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

impl PrGrade {
    /// Raises the grade to `grade` on the reviewer's say-so. Returns whether it was a raise; a reviewer can't lower.
    pub fn raise(&mut self, grade: Grade, reason: &str) -> bool {
        if grade <= self.grade {
            return false;
        }
        self.grade = grade;
        self.reasons.push(format!("the reviewer raised it: {}", reason.trim()));
        true
    }

    /// Whether the grade is above the task's, so the task should be raised to match.
    pub fn raised(&self) -> bool {
        self.task.is_none_or(|t| self.grade > t)
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

/// Compiles a path glob: `*` and `?` stay within a segment, `**` spans segments (none included, so `**/Cargo.toml`
/// matches the root's too), and `{a,b}` and `[ab]` work as in a shell.
pub fn compile_glob(pattern: &str) -> Result<globset::GlobMatcher, globset::Error> {
    Ok(globset::GlobBuilder::new(pattern).literal_separator(true).build()?.compile_matcher())
}

/// Matches a `/`-separated path against a glob (see [`compile_glob`]). A pattern that doesn't compile matches nothing;
/// config loading rejects those.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    compile_glob(pattern).is_ok_and(|m| m.is_match(path))
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
    ".trailhead.toml",
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

/// Whether a `Cargo.lock` patch only edits dependency lists: no package added or removed, no version, source or
/// checksum changed. That is a workspace member gaining or losing a dependency that is already locked, which brings in
/// nothing new to fetch. Anything else (including a line it can't read) is not safe.
pub fn is_dependency_list_only(patch: &str) -> bool {
    let mut changed = 0;
    for line in patch.lines() {
        let Some(rest) = line.strip_prefix('+').or_else(|| line.strip_prefix('-')) else { continue };
        changed += 1;
        let rest = rest.trim();
        let entry = rest.starts_with('"') && rest.ends_with("\",");
        if !(entry || rest == "dependencies = [" || rest == "]") {
            return false;
        }
    }
    changed > 0
}

/// Floor of a deleted or renamed test, or an edited check's definition: never grade 1.
const TEST_OR_CHECK_GRADE: Grade = Grade::new(2).unwrap();

/// Floor of a diff over the line limit.
const OVER_LINES_GRADE: Grade = Grade::new(3).unwrap();

/// Reasons shown in full before the rest are counted.
const SHOWN_REASONS: usize = 3;

/// Path globs per grade and the line limit, from `[factory.grades]` in `.trailhead.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grader<'a> {
    /// (grade, globs), any order.
    pub rules: Vec<(Grade, &'a [String])>,
    pub max_lines: u32,
}

impl Grader<'_> {
    /// The PR's grade: the highest of the task's, any rule a changed file matches, the line limit and the floor for
    /// touching tests and checks. `reasons` name what put it above the task's, if anything did.
    pub fn grade(&self, task: Option<Grade>, files: &[ChangedFile]) -> PrGrade {
        // (grade, reason) per finding.
        let mut found: Vec<(Grade, String)> = Vec::new();
        for f in files {
            let rule = self
                .rules
                .iter()
                .filter(|(_, globs)| globs.iter().any(|g| glob_match(g, &f.path)))
                .map(|(grade, _)| *grade)
                .max();
            let lock_edit = f.path.rsplit('/').next() == Some("Cargo.lock")
                && f.patch.as_deref().is_some_and(is_dependency_list_only);
            if let Some(grade) = rule.filter(|_| !lock_edit) {
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
            found.push((OVER_LINES_GRADE, format!("{lines} lines changed, over {}", self.max_lines)));
        }

        let base = task.unwrap_or(Grade::ALL[0]);
        let top = found.iter().map(|(g, _)| *g).max().unwrap_or(base).max(base);
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

    fn gr(n: u8) -> Grade {
        Grade::new(n).unwrap()
    }

    fn file(path: &str, status: FileStatus, lines: u32) -> ChangedFile {
        ChangedFile { path: path.into(), status, additions: lines, deletions: 0, previous_path: None, patch: None }
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
            four: v(&[".github/**", ".trailhead.toml"]),
        }
    }

    fn grader(r: &Rules) -> Grader<'_> {
        Grader { rules: vec![(gr(2), &r.two), (gr(3), &r.three), (gr(4), &r.four)], max_lines: 600 }
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
        assert!(glob_match("crates/{core,server}/**", "crates/server/src/a.rs"));
        assert!(!glob_match("crates/{core,server}/**", "crates/cli/src/a.rs"));
        assert!(glob_match("src/[ab].rs", "src/b.rs"));
        assert!(!glob_match("src/[ab].rs", "src/c.rs"));
        assert!(!glob_match("src/[ab", "src/[ab"));
    }

    #[test]
    fn docs_only_stays_at_the_tasks_grade() {
        let r = rules();
        let g = grader(&r).grade(Some(gr(1)), &[modified("docs/a.md")]);
        assert_eq!(g, PrGrade { grade: gr(1), task: Some(gr(1)), reasons: vec![] });
        assert_eq!(g.describe(), "Grade 1");
        assert!(!g.raised());
    }

    #[test]
    fn the_highest_matching_rule_raises() {
        let r = rules();
        let g = grader(&r).grade(Some(gr(1)), &[modified("crates/a/src/lib.rs"), modified("Cargo.lock")]);
        assert_eq!(g.grade, gr(3));
        assert_eq!(g.describe(), "Grade 3 (task was 1; Cargo.lock changed)");
        let g = grader(&r).grade(Some(gr(1)), &[modified("crates/a/src/lib.rs")]);
        assert_eq!(g.describe(), "Grade 2 (task was 1; crates/a/src/lib.rs changed)");
        let g = grader(&r).grade(Some(gr(3)), &[modified("crates/a/src/lib.rs")]);
        assert_eq!((g.grade, g.reasons.is_empty()), (gr(3), true), "a higher task grade stands");
        let g = grader(&r).grade(Some(gr(2)), &[modified(".trailhead.toml")]);
        assert_eq!(g.grade, gr(4));
    }

    #[test]
    fn a_big_diff_is_at_least_grade_three() {
        let r = rules();
        let big = [file("docs/a.md", FileStatus::Modified, 400), file("docs/b.md", FileStatus::Added, 201)];
        let g = grader(&r).grade(Some(gr(1)), &big);
        assert_eq!(g.grade, gr(3));
        assert_eq!(g.reasons, ["601 lines changed, over 600"]);
        let ok = [file("docs/a.md", FileStatus::Modified, 600)];
        assert_eq!(grader(&r).grade(Some(gr(1)), &ok).grade, gr(1), "the limit itself is fine");
    }

    #[test]
    fn deleting_a_test_or_editing_a_check_is_never_grade_one() {
        let r = Rules { two: vec![], three: vec![], four: vec![] };
        let g = grader(&r).grade(Some(gr(1)), &[file("crates/a/tests/it.rs", FileStatus::Removed, 0)]);
        assert_eq!((g.grade, g.reasons), (gr(2), vec!["test crates/a/tests/it.rs deleted".to_string()]));
        let renamed =
            ChangedFile { previous_path: Some("src/test_a.py".into()), ..file("src/a.py", FileStatus::Renamed, 0) };
        assert_eq!(grader(&r).grade(Some(gr(1)), &[renamed]).grade, gr(2));
        let g = grader(&r).grade(Some(gr(1)), &[modified("mise.toml")]);
        assert_eq!(g.grade, gr(2));
        assert_eq!(grader(&r).grade(Some(gr(1)), &[modified(".github/workflows/ci.yml")]).grade, gr(2));
        // Adding or editing a test is grade 1.
        let edit = [modified("crates/a/tests/it.rs"), file("tests/new.rs", FileStatus::Added, 5)];
        assert_eq!(grader(&r).grade(Some(gr(1)), &edit).grade, gr(1));
    }

    fn lock(patch: &str) -> ChangedFile {
        ChangedFile { patch: Some(patch.into()), ..modified("Cargo.lock") }
    }

    #[test]
    fn a_lock_that_only_edits_dependency_lists_is_not_raised() {
        let r = rules();
        let dev = "@@ -100,6 +100,7 @@ dependencies = [\n  \"serde\",\n+ \"tracing-subscriber\",\n  \"tokio\",\n ]";
        assert_eq!(grader(&r).grade(Some(gr(1)), &[lock(dev)]).grade, gr(1));
        let first = "@@ -1,3 +1,6 @@\n name = \"a\"\n+dependencies = [\n+ \"x 1.0.0\",\n+]";
        assert_eq!(grader(&r).grade(Some(gr(1)), &[lock(first)]).grade, gr(1));
        for bad in [
            "@@ -1,2 +1,8 @@\n+[[package]]\n+name = \"evil\"\n+version = \"1.0.0\"",
            "@@ -1,3 +1,3 @@\n name = \"x\"\n-version = \"1.0.0\"\n+version = \"1.0.1\"",
            "@@ -1,3 +1,3 @@\n-checksum = \"aa\"\n+checksum = \"bb\"",
            "@@ -1,3 +1,3 @@\n+source = \"git+https://x\"",
            "",
        ] {
            assert_eq!(grader(&r).grade(Some(gr(1)), &[lock(bad)]).grade, gr(3), "{bad}");
        }
        // No patch (too large for the host to show): treated as risky.
        assert_eq!(grader(&r).grade(Some(gr(1)), &[modified("Cargo.lock")]).grade, gr(3));
    }

    #[test]
    fn many_reasons_are_counted() {
        let r = rules();
        let files: Vec<_> = (0..6).map(|i| modified(&format!("crates/f{i}.rs"))).collect();
        let g = grader(&r).grade(Some(gr(1)), &files);
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
        let mut g = grader(&r).grade(Some(gr(1)), &[modified("Cargo.lock")]);
        assert!(!g.raise(gr(2), "meh"));
        assert!(g.raise(gr(4), "touches auth"));
        assert_eq!(g.describe(), "Grade 4 (task was 1; Cargo.lock changed; the reviewer raised it: touches auth)");
    }
}
