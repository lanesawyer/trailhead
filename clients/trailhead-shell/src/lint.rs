//! What a generated app is held to beyond the design system: it reaches nothing outside itself.
//!
//! An app is written by an agent and merged with little review (`docs/app-platform.md`), so what it may reach is
//! enforced here, not trusted: no files, no network, no processes, no `unsafe`. What an app needs from outside it
//! comes through the shell: the [`LocalStore`](crate::LocalStore), a [`Fetch`](crate::net::Fetch) and the
//! [`commands`](crate::commands). The rules are the kit's line checks (`trailhead_ui::lint`) with these added, and an
//! app runs [`APP`], this module's set, from one test that imports only this module:
//!
//! ```ignore
//! let found = trailhead_shell::lint::check(env!("CARGO_MANIFEST_DIR"), &["src", "tests", "Cargo.toml"], APP, ALLOWED);
//! assert!(found.is_empty(), "{}", found.join("\n"));
//! ```
//!
//! The kit's scan for a surface redrawn wholesale, [`redraws`], is here too, so that test imports nothing else.
//!
//! Apps are held to [`APP`]. The shell and Lookout own the window, the files and the network, so they run the kit's
//! rules only. Like the kit's rules these look at what a line says, not what it means: they stop a slip and a lazy
//! shortcut, and a dependency review is what stops a determined one.

pub use trailhead_ui::lint::{Allowed, AllowedRedraw, Line, Rule, check, redraws};

/// The kit's rules plus the four here: the one set an app's lint test passes to [`check`].
pub const APP: &[Rule] = &APP_RULES;

const OWN: &[Rule] = &[NO_FS, NO_NET, NO_PROCESS, NO_UNSAFE];

const APP_RULES: [Rule; trailhead_ui::lint::APP.len() + OWN.len()] = join(trailhead_ui::lint::APP, OWN);

/// `first` then `second` as one array: a `const` slice can't be concatenated any other way on stable.
const fn join<const N: usize>(first: &[Rule], second: &[Rule]) -> [Rule; N] {
    assert!(first.len() + second.len() == N);
    let mut all = [NO_FS; N];
    let mut at = 0;
    while at < N {
        all[at] = if at < first.len() { first[at] } else { second[at - first.len()] };
        at += 1;
    }
    all
}

/// An app touches no file of its own: what must survive a restart goes in the shell's `LocalStore`, and its data lives
/// on the server. Looks in tests too: a test has no need of `std::fs` to make a directory for a `LocalStore`, since
/// `tempfile` (which no needle names) makes one, and the shell's `test-support` helpers set up the rest. Looks in
/// `Cargo.toml` for `tokio`'s `fs` feature.
pub const NO_FS: Rule = Rule {
    name: "no-fs",
    fix: "keep what must survive in the shell's LocalStore; the app's data belongs on the server",
    in_tests: true,
    manifest: true,
    finds: |line| {
        if is_manifest(line) {
            return tokio_feature(line.code, &["fs"]);
        }
        let code = outside_strings(line.code);
        ["std::fs", "tokio::fs", "OpenOptions"].iter().any(|needle| has_path(&code, needle))
            || ["fs::", "File::"].iter().any(|needle| has_word(&code, needle))
            || braced_import(&code, "std", "fs")
    },
};

/// An app reaches the network only through the shell: reads through its `Fetch`, writes through its commands, so there
/// is one connection to audit. Looks in tests too: a test that opens a socket is as suspicious as code that does (the
/// shell's `test-support` server is what a test goes live against). Looks in `Cargo.toml` for the crates that speak
/// the network and for `tokio`'s `net` feature.
pub const NO_NET: Rule = Rule {
    name: "no-net",
    fix: "read through the shell's Fetch and write through its commands",
    in_tests: true,
    manifest: true,
    finds: |line| {
        const CRATES: [&str; 3] = ["reqwest", "hyper", "iroh"];
        if is_manifest(line) {
            return manifest_dep(line.code, &CRATES) || tokio_feature(line.code, &["net", "full"]);
        }
        let code = outside_strings(line.code);
        CRATES.iter().any(|name| has_path(&code, name))
            || ["std::net", "tokio::net"].iter().any(|needle| has_path(&code, needle))
            || ["TcpStream", "TcpListener", "UdpSocket"].iter().any(|name| has_path(&code, name))
            || braced_import(&code, "std", "net")
    },
};

/// An app runs nothing outside itself, and does not change the environment the shell and the other apps share. Looks
/// in tests too: a test that starts a program is as suspicious as code that does. Looks in `Cargo.toml` for `tokio`'s
/// `process` feature.
pub const NO_PROCESS: Rule = Rule {
    name: "no-process",
    fix: "an app runs nothing outside itself",
    in_tests: true,
    manifest: true,
    finds: |line| {
        if is_manifest(line) {
            return tokio_feature(line.code, &["process"]);
        }
        let code = outside_strings(line.code);
        ["std::process", "tokio::process", "env::set_var", "env::remove_var"].iter().any(|n| has_path(&code, n))
            || ["Command::new", "set_var(", "remove_var("].iter().any(|needle| has_word(&code, needle))
            || braced_import(&code, "std", "process")
    },
};

/// An app has no `unsafe`: not a block, fn, impl or trait, and not an `#[unsafe(...)]` attribute, so its memory safety
/// is the compiler's. `#![forbid(unsafe_code)]` passes (the word is part of a lint name there) but lifting the lint
/// with `allow` or `expect` does not. Looks in tests too: `unsafe` is no safer there. Looks in `Cargo.toml` for `libc`
/// and `nix`, whose use is `unsafe` by another name.
pub const NO_UNSAFE: Rule = Rule {
    name: "no-unsafe",
    fix: "write it in safe Rust; if the app needs something safe Rust can't do, the kit or the shell grows it",
    in_tests: true,
    manifest: true,
    finds: |line| {
        if is_manifest(line) {
            return manifest_dep(line.code, &["libc", "nix"]);
        }
        let code = outside_strings(line.code);
        let lifts = ["allow(", "expect(", "warn(", "cfg_attr("].iter().any(|attr| code.contains(attr))
            && has_path(&code, "unsafe_code");
        lifts
            || ["libc::", "nix::"].iter().any(|name| has_word(&code, name))
            || code.match_indices("unsafe").any(|(at, found)| {
                let after = code[at + found.len()..].chars().next();
                !before_is_ident(&code, at) && after.is_none_or(|c| !is_ident(c))
            })
    },
};

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn before_is_ident(code: &str, at: usize) -> bool {
    code[..at].chars().last().is_some_and(is_ident)
}

fn is_manifest(line: &Line) -> bool {
    line.path.ends_with(".toml")
}

/// `code` with what is inside string literals removed, so a message that says "std::fs" is not a use of it.
fn outside_strings(code: &str) -> String {
    code.split('"').step_by(2).collect()
}

/// Whether `word` is in `code` as a whole path segment start: not straight after an identifier character (a `:` is fine,
/// so `trailhead_wire::iroh` is found), and not followed by a letter or digit (a `_` is fine, so `hyper_util` is found
/// and `hyperlink` is not).
fn has_path(code: &str, word: &str) -> bool {
    code.match_indices(word).any(|(at, found)| {
        let after = code[at + found.len()..].chars().next();
        !before_is_ident(code, at) && after.is_none_or(|c| !c.is_alphanumeric())
    })
}

/// Whether `word` (which may end in `::` or `(`) starts a word in `code`: not after an identifier character or a `:`.
fn has_word(code: &str, word: &str) -> bool {
    code.match_indices(word).any(|(at, _)| code[..at].chars().last().is_none_or(|c| !(is_ident(c) || c == ':')))
}

/// Whether `code` imports `name` out of a brace of `root`: `use std::{fs, io};`.
fn braced_import(code: &str, root: &str, name: &str) -> bool {
    let prefix = format!("{root}::{{");
    code.match_indices(&prefix).any(|(at, found)| {
        let inside = &code[at + found.len()..];
        let inside = inside.split('}').next().unwrap_or(inside);
        inside.split(|c: char| !is_ident(c)).any(|part| part == name)
    })
}

/// Whether a `Cargo.toml` line declares a dependency on one of `names` (or a crate named with one as its prefix,
/// `hyper-util`), as `name = ..`, `name.workspace = true`, `[dependencies.name]` or `package = "name"`.
fn manifest_dep(code: &str, names: &[&str]) -> bool {
    let key = manifest_key(code);
    let package = code
        .split_once("package")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(_, rest)| rest.split('"').next().unwrap_or(rest));
    names.iter().any(|name| {
        let named =
            |key: &str| key.strip_prefix(name).is_some_and(|rest| rest.is_empty() || rest.starts_with(['-', '_']));
        named(key) || package.is_some_and(named)
    })
}

/// The crate a `Cargo.toml` line is about: the key at its start, without a `[dependencies.` table header around it.
fn manifest_key(code: &str) -> &str {
    let key = code.trim().trim_start_matches('[');
    let key = key.rsplit_once("dependencies.").map_or(key, |(_, name)| name);
    let end = key.find(|c: char| !(is_ident(c) || c == '-')).unwrap_or(key.len());
    &key[..end]
}

/// Whether a `Cargo.toml` line turns on one of `tokio`'s `features`, on `tokio`'s own line or as `tokio/feature` in a
/// feature list.
fn tokio_feature(code: &str, features: &[&str]) -> bool {
    features.iter().any(|feature| {
        code.contains(&format!("\"tokio/{feature}\""))
            || (manifest_key(code) == "tokio" && code.contains(&format!("\"{feature}\"")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(rule: Rule, path: &str, code: &str) -> bool {
        (rule.finds)(&Line { path, code, in_tests: false })
    }

    fn rust(rule: Rule, code: &str) -> bool {
        found(rule, "src/view.rs", code)
    }

    fn toml(rule: Rule, code: &str) -> bool {
        found(rule, "Cargo.toml", code)
    }

    #[test]
    fn no_fs_finds_files_and_lets_the_word_file_through() {
        for code in [
            "let text = std::fs::read_to_string(path)?;",
            "use std::fs;",
            "use std::{fs, io};",
            "let f = File::create(path);",
            "let f = std::fs::File::open(path);",
            "tokio::fs::write(p, b).await",
            "OpenOptions::new().append(true)",
            "fs::remove_file(path)",
        ] {
            assert!(rust(NO_FS, code), "missed {code}");
        }
        for code in [
            "let label = \"Open file: std::fs::read\";",
            "let dir = tempfile::tempdir().unwrap();",
            "let store = LocalStore::open(Some(dir.path().into()), &[ID]);",
            "let profile = ProfileFile::new();",
            "use std::{collections::HashMap, io};",
            "let file_name = \"x\";",
        ] {
            assert!(!rust(NO_FS, code), "found {code}");
        }
        assert!(toml(NO_FS, "tokio = { workspace = true, features = [\"fs\"] }"));
        assert!(!toml(NO_FS, "tokio = { workspace = true, features = [\"time\"] }"));
        assert!(!toml(NO_FS, "tempfile.workspace = true"));
    }

    #[test]
    fn no_net_finds_sockets_and_http_and_the_crates_for_them() {
        for code in [
            "use std::net::TcpStream;",
            "use std::{io, net};",
            "let s = tokio::net::TcpListener::bind(a).await;",
            "let c = reqwest::Client::new();",
            "use hyper_util::rt::TokioExecutor;",
            "let e = trailhead_wire::iroh::Endpoint::builder();",
            "let socket = UdpSocket::bind(a);",
        ] {
            assert!(rust(NO_NET, code), "missed {code}");
        }
        for code in [
            "let link = hyperlink(label);",
            "let text = \"retry the reqwest later\";",
            "let fetch = Fetch::get(\"/v1/tasks\");",
            "let network = Network::default();",
        ] {
            assert!(!rust(NO_NET, code), "found {code}");
        }
        for code in [
            "reqwest.workspace = true",
            "hyper = \"1\"",
            "iroh = { workspace = true }",
            "[dependencies.iroh-relay]",
            "tokio = { workspace = true, features = [\"net\", \"time\"] }",
            "tokio = { workspace = true, features = [\"full\"] }",
            "test-support = [\"tokio/net\"]",
            "http = { package = \"hyper\", version = \"1\" }",
        ] {
            assert!(toml(NO_NET, code), "missed {code}");
        }
        for code in [
            "tokio = { workspace = true, features = [\"time\"] }",
            "description = \"reads through hyper\"",
            "tempfile.workspace = true",
            "hyperlink = \"1\"",
        ] {
            assert!(!toml(NO_NET, code), "found {code}");
        }
    }

    #[test]
    fn no_process_finds_programs_and_the_environment() {
        for code in [
            "let out = std::process::Command::new(\"ls\").output();",
            "use std::process;",
            "use std::{io, process};",
            "Command::new(\"git\")",
            "tokio::process::Command::new(\"x\")",
            "unsafe_free(); std::env::set_var(\"A\", \"b\");",
            "env::remove_var(\"A\");",
            "set_var(\"A\", \"b\")",
        ] {
            assert!(rust(NO_PROCESS, code), "missed {code}");
        }
        for code in [
            "commands.spawn(Node::default());",
            "EntityCommand::new(x)",
            "let key = std::env::var(\"HOME\");",
            "let hint = \"run Command::new elsewhere\";",
            "let processed = process_rows(rows);",
        ] {
            assert!(!rust(NO_PROCESS, code), "found {code}");
        }
        assert!(toml(NO_PROCESS, "tokio = { workspace = true, features = [\"process\"] }"));
        assert!(!toml(NO_PROCESS, "tokio = { workspace = true, features = [\"time\"] }"));
    }

    #[test]
    fn no_unsafe_finds_the_keyword_and_not_the_lint_that_forbids_it() {
        for code in [
            "unsafe { *ptr }",
            "pub unsafe fn read(p: *const u8) {}",
            "unsafe impl Send for X {}",
            "unsafe trait T {}",
            "#[unsafe(no_mangle)]",
            "let v = unsafe{ x };",
            "#![allow(unsafe_code)]",
            "#[expect(unsafe_code)]",
            "let n = libc::getpid();",
        ] {
            assert!(rust(NO_UNSAFE, code), "missed {code}");
        }
        for code in [
            "#![forbid(unsafe_code)]",
            "let unsafe_count = 3;",
            "let label = \"this is unsafe\";",
            "fn not_unsafe_at_all() {}",
            "let nixie = 1;",
        ] {
            assert!(!rust(NO_UNSAFE, code), "found {code}");
        }
        assert!(toml(NO_UNSAFE, "libc = \"0.2\""));
        assert!(toml(NO_UNSAFE, "nix = { version = \"0.29\" }"));
        assert!(toml(NO_UNSAFE, "[dev-dependencies.nix]"));
        assert!(!toml(NO_UNSAFE, "bevy = { version = \"0.20\", features = [\"std\"] }"));
        assert!(!toml(NO_UNSAFE, "tempfile.workspace = true"));
    }

    #[test]
    fn a_test_is_held_to_the_same_rules_but_tempfile_is_not_one_of_their_needles() {
        for rule in [NO_FS, NO_NET, NO_PROCESS, NO_UNSAFE] {
            assert!(rule.in_tests && rule.manifest, "{}", rule.name);
        }
        assert!(!rust(NO_FS, "let dir = tempfile::tempdir().unwrap();"));
        assert!(!rust(NO_FS, "let dir = tempfile::Builder::new().tempdir().unwrap();"));
    }

    #[test]
    fn app_is_the_kits_rules_then_these() {
        let names: Vec<&str> = APP.iter().map(|rule| rule.name).collect();
        let kit: Vec<&str> = trailhead_ui::lint::APP.iter().map(|rule| rule.name).collect();
        assert_eq!(names[..kit.len()], kit);
        assert_eq!(names[kit.len()..], ["no-fs", "no-net", "no-process", "no-unsafe"]);
    }

    #[test]
    fn a_manifest_and_a_test_are_checked_as_the_rules_say() {
        let root = std::env::temp_dir().join(format!("trailhead-shell-lint-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[dependencies]\nreqwest.workspace = true # http\n").unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "// std::fs is not allowed\nfn a() {}\n#[cfg(test)]\nmod tests {\n    fn t() { std::process::exit(1); }\n}\n",
        )
        .unwrap();
        let found = check(&root, &["src", "Cargo.toml"], APP, &[]);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].starts_with("Cargo.toml:2: [no-net]"), "{found:?}");
        assert!(found[1].starts_with("src/lib.rs:5: [no-process]"), "{found:?}");
        std::fs::remove_dir_all(root).unwrap();
    }
}
