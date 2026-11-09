//! The checks that keep an app on the design system, run over its sources from one test.
//!
//! Each [`Rule`] looks at one line of code at a time (comments stripped) and says whether it steps off the system: a
//! color written as a literal, a font size or spacing typed as a number, a feathers token where the kit has a meaning.
//! [`check`] walks a crate's sources once and runs every rule over them. A line a rule may find for a good reason goes
//! on the caller's allowlist as an [`Allowed`], which names the file, the rule and why; an entry that no longer finds
//! anything fails too, so the list can't go stale.
//!
//! An app runs [`APP`] (or its own list, adding rules about its domain) from one test:
//!
//! ```ignore
//! let found = trailhead_ui::lint::check(env!("CARGO_MANIFEST_DIR"), &["src", "tests"], APP, ALLOWED);
//! assert!(found.is_empty(), "{}", found.join("\n"));
//! ```
//!
//! Rules are about what a line looks like, not what it means, so they can be fooled; they catch the slips that build
//! cleanly and look fine in a dark theme on one screen.

use std::path::{Path, PathBuf};

/// One line of a source file, as a rule sees it.
pub struct Line<'a> {
    /// The file, relative to the crate's root, with `/` between folders: `src/map.rs`.
    pub path: &'a str,
    /// The line's code, without a trailing comment.
    pub code: &'a str,
    /// Whether the line is in the file's tests: at or after its `#[cfg(test)]`.
    pub in_tests: bool,
}

/// A check on single lines of code.
#[derive(Clone, Copy)]
pub struct Rule {
    /// What it's called on an [`Allowed`] and in a finding.
    pub name: &'static str,
    /// What to do instead, shown with each line it finds.
    pub fix: &'static str,
    /// Whether it looks inside tests, which build their own fixtures (a test may name a color to check a theme).
    pub in_tests: bool,
    /// Whether `line` steps off the system.
    pub finds: fn(&Line) -> bool,
}

/// A file a rule may find lines in, and why.
pub struct Allowed {
    /// The file, relative to the crate's root: `src/widgets/theme.rs`.
    pub path: &'static str,
    /// The [`Rule::name`], or `*` for every rule.
    pub rule: &'static str,
    /// Why this file is the exception. Name the task that will take it off the list, if one will.
    pub why: &'static str,
}

/// The rules every app built on the kit runs.
pub const APP: &[Rule] =
    &[NO_FEATHERS, COLOR_LITERAL, PALETTE, RAW_COLOR, FEATHERS_COLOR, FONT_SIZE, SPACING, RADIUS, STROKE, POSITION];

/// The rules the kit runs over itself: an app's, but the kit is where feathers lives.
pub const KIT: &[Rule] =
    &[COLOR_LITERAL, PALETTE, RAW_COLOR, FEATHERS_COLOR, FONT_SIZE, SPACING, RADIUS, STROKE, POSITION];

/// Feathers stays inside the kit: an app names only what the kit exports, so a feathers upgrade is one crate's change.
/// Looks in tests and `Cargo.toml` too.
pub const NO_FEATHERS: Rule = Rule {
    name: "no-feathers",
    fix: "use (or grow) the trailhead-ui kit instead of feathers",
    in_tests: true,
    finds: |line| ["feathers::", "bevy_feathers"].iter().any(|needle| line.code.contains(needle)),
};

/// A color is a theme token, so a theme switch repaints it.
pub const COLOR_LITERAL: Rule = Rule {
    name: "color-literal",
    fix: "name a token (the kit's `token::*`, or an app's own painted where its theme is built)",
    in_tests: false,
    finds: |line| {
        const LITERALS: [&str; 10] = [
            "Color::srgb",
            "Color::oklch",
            "Color::hsl",
            "Color::linear_rgb",
            "Color::WHITE",
            "Color::BLACK",
            "srgb_u8(",
            "Srgba::",
            "LinearRgba::",
            "Oklcha::new",
        ];
        LITERALS.iter().any(|literal| line.code.contains(literal))
    },
};

/// The palette is raw colors, for building a theme, not for a view.
pub const PALETTE: Rule = Rule {
    name: "palette",
    fix: "name a token; the palette is for where a theme is built",
    in_tests: false,
    finds: |line| has_word(line.code, "palette::"),
};

/// A node's color set by hand misses theme switches; a tone component follows them.
pub const RAW_COLOR: Rule = Rule {
    name: "raw-color",
    fix: "use BackgroundTone, BorderTone, TextTone or a Surface",
    in_tests: false,
    finds: |line| ["BackgroundColor", "BorderColor", "TextColor"].iter().any(|word| has_word(line.code, word)),
};

/// Feathers' own tokens and theme components bypass the kit's meanings; only the kit widget wrapping a control names
/// its tokens.
pub const FEATHERS_COLOR: Rule = Rule {
    name: "feathers-color",
    fix: "name a kit token (`token::*`) in a tone component",
    in_tests: false,
    finds: |line| {
        has_word(line.code, "tokens::")
            || ["ThemeBackgroundColor(", "ThemeBorderColor(", "ThemeTextColor("].iter().any(|c| has_word(line.code, c))
    },
};

/// Text sizes come from the kit's `size` scale.
pub const FONT_SIZE: Rule = Rule {
    name: "font-size",
    fix: "use a `size::*` (add one with a doc comment if none fits)",
    in_tests: false,
    finds: |line| {
        ["FontSize::Px(", "font_size(", "font_size: "].iter().any(|call| number_after(line.code, call))
            || typed_const(line.code, &["FONT", "TEXT_SIZE"])
    },
};

/// Gaps, padding and margins come from the kit's `space` scale.
pub const SPACING: Rule = Rule {
    name: "spacing",
    fix: "use a `space::*` (add one with a doc comment if none fits)",
    in_tests: false,
    finds: |line| {
        typed_const(line.code, &["GAP", "INSET", "INDENT", "MARGIN", "PADDING"])
            || (["gap", "padding", "margin"].iter().any(|word| line.code.contains(word)) && typed_px(line.code))
    },
};

/// Corners come from the kit's `radius` scale.
pub const RADIUS: Rule = Rule {
    name: "radius",
    fix: "use a `radius::*`",
    in_tests: false,
    finds: |line| line.code.contains("radius") && typed_px(line.code),
};

/// Border widths come from the kit's `stroke` scale.
pub const STROKE: Rule = Rule {
    name: "stroke",
    fix: "use a `stroke::*`",
    in_tests: false,
    finds: |line| {
        let code = line.code.replace("border_radius", "");
        has_word(&code, "border") && typed_px(&code)
    },
};

/// An offset typed as a number is a spacing in disguise.
pub const POSITION: Rule = Rule {
    name: "position",
    fix: "use a `space::*`, or a named constant saying what the offset is for",
    in_tests: false,
    finds: |line| {
        ["top:", "left:", "right:", "bottom:"].iter().any(|side| has_word(line.code, side)) && typed_px(line.code)
    },
};

/// Every rule's finding in the files under `dirs` (folders or single files, relative to `crate_root`), as
/// `path:line: [rule] code (fix)`, less what `allowed` lets through, plus each `allowed` entry that let nothing
/// through.
pub fn check(crate_root: impl AsRef<Path>, dirs: &[&str], rules: &[Rule], allowed: &[Allowed]) -> Vec<String> {
    let crate_root = crate_root.as_ref();
    let mut found = Vec::new();
    let mut used = vec![false; allowed.len()];
    for path in sources(crate_root, dirs) {
        let relative = path.strip_prefix(crate_root).unwrap_or(&path);
        let relative = relative.to_string_lossy().replace('\\', "/");
        let toml = relative.ends_with(".toml");
        let source = std::fs::read_to_string(&path).unwrap_or_default();
        let mut in_tests = false;
        for (number, text) in source.lines().enumerate() {
            in_tests |= text.trim_start().starts_with("#[cfg(test)]");
            let code = strip_comment(text, toml);
            if code.trim().is_empty() {
                continue;
            }
            let line = Line { path: &relative, code, in_tests };
            for rule in rules {
                if (in_tests && !rule.in_tests) || (toml && rule.name != NO_FEATHERS.name) || !(rule.finds)(&line) {
                    continue;
                }
                match allowed.iter().position(|a| a.path == relative && (a.rule == rule.name || a.rule == "*")) {
                    Some(at) => used[at] = true,
                    None => {
                        found.push(format!("{relative}:{}: [{}] {} ({})", number + 1, rule.name, text.trim(), rule.fix))
                    }
                }
            }
        }
    }
    for (entry, used) in allowed.iter().zip(used) {
        if !used {
            found.push(format!("{}: [{}] is allowed but finds nothing: take it off the list", entry.path, entry.rule));
        }
    }
    found
}

/// The `.rs` and `.toml` files under `dirs` (folders or single files, relative to `crate_root`), sorted: the one walk
/// every source check shares.
pub fn sources(crate_root: impl AsRef<Path>, dirs: &[&str]) -> Vec<PathBuf> {
    let crate_root = crate_root.as_ref();
    let mut files = Vec::new();
    let mut folders = Vec::new();
    for dir in dirs {
        let path = crate_root.join(dir);
        if path.is_dir() {
            folders.push(path);
        } else if path.is_file() {
            files.push(path);
        }
    }
    while let Some(folder) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                folders.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs" || ext == "toml") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// `text` without a trailing comment: `//` in Rust, `#` in TOML.
fn strip_comment(text: &str, toml: bool) -> &str {
    let marker = if toml { "#" } else { "//" };
    text.split(marker).next().unwrap_or("")
}

/// Whether `code` has a word starting at `at`: not straight after a letter, digit, `_` or `:`.
fn starts_word(code: &str, at: usize) -> bool {
    code[..at].chars().last().is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == ':'))
}

/// Whether `word` starts a word somewhere in `code`.
fn has_word(code: &str, word: &str) -> bool {
    code.match_indices(word).any(|(at, _)| starts_word(code, at))
}

/// Whether `rest` starts with a number other than zero.
fn starts_with_number(rest: &str) -> bool {
    let rest = rest.trim_start().trim_start_matches('-');
    rest.starts_with(|c: char| c.is_ascii_digit()) && !rest.starts_with("0)") && !rest.starts_with("0.0)")
}

/// Whether a number other than zero follows `call` somewhere in `code`.
fn number_after(code: &str, call: &str) -> bool {
    code.match_indices(call).any(|(at, found)| starts_with_number(&code[at + found.len()..]))
}

/// Whether `code` gives a length in pixels as a number other than zero: `px(8)`, `Val::Px(8.0)`.
fn typed_px(code: &str) -> bool {
    // A `px(` counts only where it starts a word, not as the tail of something longer.
    let px = code
        .match_indices("px(")
        .any(|(at, found)| starts_word(code, at) && starts_with_number(&code[at + found.len()..]));
    px || number_after(code, "Val::Px(")
}

/// Whether `code` declares a constant whose name holds one of `words`, as a number.
fn typed_const(code: &str, words: &[&str]) -> bool {
    let Some((name, value)) = code.split_once("const ").and_then(|(_, rest)| rest.split_once(": f32 = ")) else {
        return false;
    };
    words.iter().any(|word| name.contains(word)) && value.trim_start().starts_with(|c: char| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finds(rule: Rule, code: &str) -> bool {
        (rule.finds)(&Line { path: "src/view.rs", code, in_tests: false })
    }

    #[test]
    fn each_rule_finds_what_it_is_meant_to_and_lets_the_named_through() {
        let cases: [(Rule, &[&str], &[&str]); 10] = [
            (NO_FEATHERS, &["use bevy::feathers::tokens;", "bevy_feathers = \"0.20\""], &["use trailhead_ui::token;"]),
            (
                COLOR_LITERAL,
                &["Color::srgb(0.2, 0.2, 0.2)", "let ink = Color::WHITE;", "Color::oklch(0.7, 0.1, 30.0)"],
                &["theme.color(&token::TEXT)", "Color::NONE"],
            ),
            (PALETTE, &["theme.paint(&MINE, palette::GRAY_2);"], &["use trailhead_ui::palette_picker;"]),
            (
                RAW_COLOR,
                &["BackgroundColor(color),", "TextColor(Color::BLACK)", "mut q: Query<&mut BorderColor>"],
                &["ThemeBackgroundColor(t)", "ClearColor(c)", "BackgroundTone(token::SURFACE_PANEL)"],
            ),
            (
                FEATHERS_COLOR,
                &["ThemeBorderColor(tokens::GROUP_BORDER)", "Token(tokens::BUTTON_TEXT)"],
                &["BorderTone(token::BORDER)"],
            ),
            (
                FONT_SIZE,
                &["TextFont::from_font_size(12.0)", "font_size: FontSize::Px(11.0)", "const TITLE_FONT: f32 = 13.0;"],
                &["text(title, size::BODY)", "FontSize::Px(size)", "const TITLE_FONT: f32 = size::BODY;"],
            ),
            (
                SPACING,
                &[
                    "row_gap: px(6),",
                    "padding: UiRect::all(px(8)),",
                    "margin: UiRect::left(px(16 * indent)),",
                    "const CHROME_GAP: f32 = 4.0;",
                ],
                &["row_gap: px(space::ROWS),", "padding: UiRect::all(px(0)),", "width: px(260),"],
            ),
            (
                RADIUS,
                &["border_radius: BorderRadius::all(px(4)),"],
                &["border_radius: BorderRadius::all(px(radius::CONTROL)),"],
            ),
            (
                STROKE,
                &["border: { UiRect::all(px(1)) },", "border: UiRect::right(px(1)),"],
                &["border: UiRect::all(px(stroke::HAIRLINE)),", "border_radius: BorderRadius::all(px(4)),"],
            ),
            (
                POSITION,
                &["top: px(8),", "left: Val::Px(16.0),"],
                &["left: px(0),", "top: px(pos.y),", "left: px(ORIGIN.x),"],
            ),
        ];
        for (rule, found, passed) in cases {
            for code in found {
                assert!(finds(rule, code), "[{}] missed {code}", rule.name);
            }
            for code in passed {
                assert!(!finds(rule, code), "[{}] found {code}", rule.name);
            }
        }
    }

    #[test]
    fn a_comment_is_not_code_and_a_test_is_left_to_rules_that_look_in_tests() {
        let root = std::env::temp_dir().join(format!("trailhead-ui-lint-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/view.rs"),
            "// row_gap: px(6) was too loose\nlet a = 1;\n#[cfg(test)]\nlet ink = Color::WHITE;\nuse bevy::feathers::x;\n",
        )
        .unwrap();
        let found = check(&root, &["src"], &[SPACING, COLOR_LITERAL, NO_FEATHERS], &[]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].starts_with("src/view.rs:5: [no-feathers]"), "{found:?}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_allowed_file_passes_and_an_allowance_that_finds_nothing_fails() {
        let root = std::env::temp_dir().join(format!("trailhead-ui-lint-allowed-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/theme.rs"), "let ink = Color::WHITE;\n").unwrap();
        let allowed = [
            Allowed { path: "src/theme.rs", rule: "color-literal", why: "builds the theme" },
            Allowed { path: "src/board.rs", rule: "spacing", why: "gone" },
        ];
        let found = check(&root, &["src"], &[COLOR_LITERAL, SPACING], &allowed);
        assert_eq!(found, ["src/board.rs: [spacing] is allowed but finds nothing: take it off the list"]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_kit_keeps_to_its_own_system() {
        let found = check(env!("CARGO_MANIFEST_DIR"), &["src"], KIT, KIT_ALLOWED);
        assert!(found.is_empty(), "\n{}", found.join("\n"));
    }

    /// Where the kit itself steps off: the scales and the theme are defined somewhere, and a few widgets wrap feathers
    /// controls or animate a color.
    const KIT_ALLOWED: &[Allowed] = &[
        Allowed { path: "src/theme.rs", rule: "color-literal", why: "builds the theme" },
        Allowed { path: "src/theme.rs", rule: "palette", why: "builds the theme" },
        Allowed { path: "src/theme.rs", rule: "feathers-color", why: "puts the kit's tokens on feathers' slots" },
        Allowed { path: "src/desktop.rs", rule: "color-literal", why: "reads the desktop's accent color" },
        Allowed { path: "src/lint.rs", rule: "*", why: "names what the rules find" },
        Allowed { path: "src/buttons.rs", rule: "feathers-color", why: "wraps feathers' button: its caption token" },
        Allowed { path: "src/slider.rs", rule: "feathers-color", why: "wraps feathers' slider: its bar and track" },
        Allowed { path: "src/accordion.rs", rule: "feathers-color", why: "wraps feathers' panes: their borders" },
        Allowed { path: "src/scrollbar.rs", rule: "raw-color", why: "fades the scrollbar's own color in and out" },
        Allowed { path: "src/skeleton.rs", rule: "raw-color", why: "pulses a token's color" },
    ];
}
