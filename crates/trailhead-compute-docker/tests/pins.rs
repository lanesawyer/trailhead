//! The Rust version is written in several files that can't read each other: `mise.toml` for a person's machine, the
//! sandbox image, and the workflows. They have to name the same exact version, so one commit moves them all
//! (`docs/factory/operating.md`, "Tool versions").

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &str) -> String {
    std::fs::read_to_string(root().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// The text between `after` and the next `until` on the first line that has `after`.
fn value<'a>(text: &'a str, after: &str, until: char) -> Option<&'a str> {
    let rest = &text[text.find(after)? + after.len()..];
    Some(rest[..rest.find(until)?].trim())
}

fn exact(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

fn mise_rust() -> String {
    value(&read("mise.toml"), "\nrust = { version = \"", '"').expect("mise.toml pins rust").to_string()
}

#[test]
fn mise_toml_names_no_moving_version() {
    let mise = read("mise.toml");
    let tools = &mise[mise.find("[tools]").unwrap()..mise.find("[tasks.").unwrap()];
    for word in ["latest", "stable", "lts"] {
        assert!(!tools.contains(&format!("\"{word}\"")), "mise.toml has a tool at \"{word}\"; pin it to a number");
    }
    assert!(exact(&mise_rust()), "mise.toml's rust is {:?}, not an exact version", mise_rust());
}

#[test]
fn the_sandbox_image_installs_the_rust_mise_toml_pins() {
    let dockerfile = read("crates/trailhead-compute-docker/image/Dockerfile");
    assert_eq!(value(&dockerfile, "ARG RUST_VERSION=", '\n'), Some(mise_rust().as_str()));
}

#[test]
fn the_workflows_build_with_the_rust_mise_toml_pins() {
    let rust = mise_rust();
    let mut steps = 0;
    for entry in std::fs::read_dir(root().join(".github/workflows")).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        for line in text.lines().filter(|l| l.contains("dtolnay/rust-toolchain@")) {
            steps += 1;
            assert!(
                line.trim_end().ends_with(&format!("@{rust}")),
                "{}: `{}` is not Rust {rust}",
                path.display(),
                line.trim()
            );
        }
    }
    assert!(steps > 0, "no workflow installs a toolchain: has the action changed?");
}
