//! Programming-language specific knowledge, behind one interface (TRAIL-1566).
//!
//! What the factory knows about a language is the answer to four questions: which hosts installing its toolchain and
//! packages needs, which lockfiles it has and which edits to them bring in nothing new to fetch, which files are its
//! source, and which edits to those are comments only. Each language is one [`Language`] in its own module, compiled
//! in by a cargo feature of this crate (`rust`, `python`, `node`, `go`; all on by default), so a build for a stack
//! without Python carries no `uv` code. To support a new language, implement the trait in a new module here, add its
//! feature, and list it in [`languages`]; nothing else names a language.

#[cfg(feature = "go")]
mod go;
#[cfg(feature = "node")]
mod node;
#[cfg(feature = "python")]
mod python;
#[cfg(feature = "rust")]
mod rust;

/// Whether a lockfile line is one an edit that brings in nothing new may add or remove.
pub type LockLine = fn(&str) -> bool;

/// One programming language's part of the factory. Every method has a "knows nothing" default, so a language
/// implements only what it has.
pub trait Language: Send + Sync {
    /// The language's name, as its cargo feature is.
    fn name(&self) -> &'static str;

    /// The hosts installing a mise tool needs (`docs/factory/sandbox.md`), as the lists to concatenate: `backend` is the part before `:` in the
    /// tool key (`npm` in `npm:prettier`, empty for a core tool), `name` the core tool's name (empty with a backend).
    /// `None` for a tool this language doesn't own.
    fn tool_hosts(&self, _backend: &str, _name: &str) -> Option<&'static [&'static [&'static str]]> {
        None
    }

    /// The reader for the lockfile called `file_name` (a base name), if this language has it: whether a changed line
    /// only edits a dependency list. A lockfile no language reads is never taken to be safe.
    fn lockfile(&self, _file_name: &str) -> Option<LockLine> {
        None
    }

    /// Whether a patch to the lockfile `file_name` brings in nothing new to fetch, or `None` when this language
    /// doesn't have that lockfile. By default every changed line has to pass [`Language::lockfile`]'s reader; a
    /// language whose lockfile needs more than a line to judge reads the whole patch. An empty patch is not safe.
    fn lock_patch_safe(&self, file_name: &str, patch: &str) -> Option<bool> {
        let line_ok = self.lockfile(file_name)?;
        let mut changed = 0;
        for line in patch.lines() {
            let Some(rest) = line.strip_prefix('+').or_else(|| line.strip_prefix('-')) else { continue };
            changed += 1;
            if !line_ok(rest.trim()) {
                return Some(false);
            }
        }
        Some(changed > 0)
    }

    /// Whether `path` is a source file whose patch the grader reads for comment-only edits.
    fn is_source(&self, _path: &str) -> bool {
        false
    }

    /// Whether a source file's patch only changes comments. Only asked of a path `is_source` accepted.
    fn is_comment_only(&self, _patch: &str) -> bool {
        false
    }
}

/// The languages this build carries.
pub fn languages() -> &'static [&'static dyn Language] {
    &[
        #[cfg(feature = "rust")]
        &rust::Rust,
        #[cfg(feature = "python")]
        &python::Python,
        #[cfg(feature = "node")]
        &node::Node,
        #[cfg(feature = "go")]
        &go::Go,
    ]
}

/// The lockfile reader for `file_name` from whichever language has it.
pub(crate) fn lockfile(file_name: &str) -> Option<LockLine> {
    languages().iter().find_map(|l| l.lockfile(file_name))
}

/// Whether a patch to the lockfile `file_name` brings in nothing new, as the language that has it reads it; `false`
/// for a lockfile no language reads.
pub(crate) fn lock_patch_safe(file_name: &str, patch: &str) -> bool {
    languages().iter().find_map(|l| l.lock_patch_safe(file_name, patch)).unwrap_or(false)
}

/// Whether the grader reads `path`'s patch: a lockfile some language reads, or a language's source.
pub(crate) fn reads_patch(path: &str) -> bool {
    lockfile(path.rsplit('/').next().unwrap_or(path)).is_some() || languages().iter().any(|l| l.is_source(path))
}

/// Whether `path` is a source file whose patch only changes comments, in the language that owns it.
pub(crate) fn is_comment_only(path: &str, patch: &str) -> bool {
    languages().iter().any(|l| l.is_source(path) && l.is_comment_only(patch))
}

/// The hosts for a mise tool key from the languages built in.
pub(crate) fn tool_hosts(backend: &str, name: &str) -> Option<&'static [&'static [&'static str]]> {
    languages().iter().find_map(|l| l.tool_hosts(backend, name))
}

/// Whether `path` is a source file of a built-in language.
pub(crate) fn is_source(path: &str) -> bool {
    languages().iter().any(|l| l.is_source(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_enabled_feature_carries_its_language() {
        let names: Vec<_> = languages().iter().map(|l| l.name()).collect();
        for (enabled, name) in [
            (cfg!(feature = "rust"), "rust"),
            (cfg!(feature = "python"), "python"),
            (cfg!(feature = "node"), "node"),
            (cfg!(feature = "go"), "go"),
        ] {
            assert_eq!(names.contains(&name), enabled, "{name}");
        }
    }

    #[test]
    fn a_language_knows_only_its_own_tools_and_files() {
        assert_eq!(tool_hosts("", "zig"), None);
        assert!(!reads_patch("README.md"));
        assert!(lockfile("poetry.lock").is_none());
        #[cfg(feature = "python")]
        assert!(lockfile("uv.lock").is_some() && tool_hosts("pipx", "").is_some());
        #[cfg(not(feature = "python"))]
        assert!(lockfile("uv.lock").is_none() && tool_hosts("pipx", "").is_none());
        #[cfg(feature = "rust")]
        assert!(is_source("a/lib.rs") && lockfile("Cargo.lock").is_some());
        #[cfg(not(feature = "rust"))]
        assert!(!is_source("a/lib.rs") && lockfile("Cargo.lock").is_none());
    }
}
