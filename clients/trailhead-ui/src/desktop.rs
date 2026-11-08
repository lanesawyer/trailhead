//! What the desktop says about how it looks: its accent color, and on Linux whether it is dark.
//!
//! Each platform keeps the accent somewhere different, and each is read by asking the tool the platform ships rather
//! than through a binding to its API:
//!
//! - Linux: the XDG desktop portal's `org.freedesktop.appearance` settings, which GNOME and KDE both answer, through
//!   `busctl` or else `gdbus`. The portal also says whether the desktop prefers dark, which the window doesn't always
//!   (winit on Wayland can't tell).
//! - macOS: `AppleAccentColor` in the global defaults, one of eight fixed colors. Unset means "multicolor", which
//!   leaves the choice to the app.
//! - Windows: the DWM `AccentColor` value in the registry.
//!
//! Asked off the main thread, since a session bus with no portal on it can take seconds to say so. Only read, never
//! watched: a change on the desktop reaches the app at its next launch.
//!
//! Ported from bevy-data-explorer's `app/accent.rs`.

use std::process::Command;

use bevy::prelude::*;
use bevy::window::WindowTheme;

/// What the desktop answered.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Desktop {
    /// Its accent, if one is set and it could be asked.
    pub accent: Option<Color>,
    /// Light or dark, where the platform says so here rather than on the window.
    pub theme: Option<WindowTheme>,
}

/// Asks the desktop. Blocks for up to a few seconds.
pub(crate) fn ask() -> Desktop {
    if cfg!(target_os = "macos") {
        let accent = run("defaults", &["read", "-g", "AppleAccentColor"])
            .and_then(|out| out.trim().parse().ok())
            .and_then(apple_accent);
        Desktop { accent, theme: None }
    } else if cfg!(windows) {
        let accent = run("reg", &["query", r"HKCU\Software\Microsoft\Windows\DWM", "/v", "AccentColor"])
            .and_then(|out| windows_accent(&out));
        Desktop { accent, theme: None }
    } else {
        Desktop {
            accent: portal("accent-color").and_then(|out| portal_accent(&out)),
            theme: portal("color-scheme").and_then(|out| portal_scheme(&out)),
        }
    }
}

/// The portal's `org.freedesktop.appearance` setting `key`, as `busctl` or else `gdbus` prints it.
fn portal(key: &str) -> Option<String> {
    const PORTAL: [&str; 3] =
        ["org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.portal.Settings"];
    run(
        "busctl",
        &[
            "--user",
            "--timeout=2",
            "call",
            PORTAL[0],
            PORTAL[1],
            PORTAL[2],
            "ReadOne",
            "ss",
            "org.freedesktop.appearance",
            key,
        ],
    )
    .or_else(|| {
        let method = format!("{}.ReadOne", PORTAL[2]);
        run(
            "gdbus",
            &[
                "call",
                "--session",
                "--timeout=2",
                "--dest",
                PORTAL[0],
                "--object-path",
                PORTAL[1],
                "--method",
                &method,
                "org.freedesktop.appearance",
                key,
            ],
        )
    })
}

/// What `program` printed, if it ran and succeeded.
fn run(program: &str, args: &[&str]) -> Option<String> {
    let mut command = Command::new(program);
    command.args(args);
    // Without this a windowed app flashes a console for the moment `reg` runs.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let output = command.output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The numbers in a portal answer, as either tool prints it.
fn numbers(out: &str) -> Vec<f32> {
    out.split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).filter_map(|word| word.parse().ok()).collect()
}

/// The portal's accent: `v (ddd) 0.56 0.25 0.67` from `busctl`, `(<<(0.56, 0.25, 0.67)>>,)` from `gdbus`. A component
/// outside 0 to 1 means no accent is set.
fn portal_accent(out: &str) -> Option<Color> {
    let [r, g, b] = numbers(out)[..] else { return None };
    [r, g, b].iter().all(|part| (0.0..=1.0).contains(part)).then(|| Color::srgb(r, g, b))
}

/// The portal's color scheme: `v u 1` from `busctl`, `(<<uint32 1>>,)` from `gdbus`. 1 prefers dark, 2 prefers
/// light, 0 has no preference.
fn portal_scheme(out: &str) -> Option<WindowTheme> {
    // The value is the last number; gdbus's type name (`uint32`) has digits of its own before it.
    match numbers(out).last() {
        Some(1.0) => Some(WindowTheme::Dark),
        Some(2.0) => Some(WindowTheme::Light),
        _ => None,
    }
}

/// The registry's `0xAABBGGRR`, as `reg query` prints it.
fn windows_accent(out: &str) -> Option<Color> {
    let hex = out.split_whitespace().find_map(|word| word.strip_prefix("0x"))?;
    let abgr = u32::from_str_radix(hex, 16).ok()?;
    let [r, g, b, _] = abgr.to_le_bytes();
    Some(Color::srgb_u8(r, g, b))
}

/// macOS's accent by its number, in the system's own colors. Graphite is -1; "multicolor" writes nothing at all, and
/// so never gets here.
fn apple_accent(index: i32) -> Option<Color> {
    let (r, g, b) = match index {
        -1 => (140, 140, 140),
        0 => (255, 59, 48),
        1 => (255, 149, 0),
        2 => (255, 204, 0),
        3 => (40, 205, 65),
        4 => (0, 122, 255),
        5 => (175, 82, 222),
        6 => (255, 45, 85),
        _ => return None,
    };
    Some(Color::srgb_u8(r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_portals_accent_is_read_from_either_tool() {
        let purple = Color::srgb(0.568627, 0.254902, 0.67451);
        assert_eq!(portal_accent("v (ddd) 0.568627 0.254902 0.67451\n"), Some(purple));
        assert_eq!(portal_accent("(<<(0.568627, 0.254902, 0.67451)>>,)\n"), Some(purple));
        assert_eq!(portal_accent("v (ddd) -1 -1 -1\n"), None, "unset");
        assert_eq!(portal_accent(""), None);
    }

    #[test]
    fn the_portals_color_scheme_is_read_from_either_tool() {
        assert_eq!(portal_scheme("v u 1\n"), Some(WindowTheme::Dark));
        assert_eq!(portal_scheme("v u 2\n"), Some(WindowTheme::Light));
        assert_eq!(portal_scheme("v u 0\n"), None, "no preference");
        assert_eq!(portal_scheme("(<<uint32 2>>,)\n"), Some(WindowTheme::Light));
        assert_eq!(portal_scheme(""), None);
    }

    #[test]
    fn windows_keeps_it_backwards() {
        let out =
            "\nHKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\DWM\n    AccentColor    REG_DWORD    0xffd77800\n\n";
        assert_eq!(windows_accent(out), Some(Color::srgb_u8(0x00, 0x78, 0xd7)));
        assert_eq!(windows_accent("nothing here"), None);
    }

    #[test]
    fn macos_names_its_accents_by_number() {
        assert_eq!(apple_accent(4), Some(Color::srgb_u8(0, 122, 255)));
        assert_eq!(apple_accent(9), None);
    }
}
