//! `trailhead app install`: the window as a macOS app, `~/Applications/Trailhead.app`, so it starts from Spotlight,
//! Launchpad or the Dock with Trailhead's icon instead of from a terminal. The bundle only runs `trailhead app`; the
//! window it starts is the one that command builds or finds. (On Linux the desktop entry does this:
//! `mise run shell-install`.)

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::app_notice::LOG_ENV;

/// Made by `cargo run -p trailhead-shell --example make_icon`, whose test fails when it drifts from the drawing.
const ICON: &[u8] = include_bytes!("../../assets/trailhead.icns");
const BUNDLE: &str = "Trailhead.app";
const EXECUTABLE: &str = "trailhead-launcher";
/// Where the launcher's output goes, under the home directory: an app started from the Dock has no terminal.
const LOG: &str = "Library/Logs/Trailhead/launcher.log";

pub(crate) fn install() -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!(
            "`trailhead app install` makes a macOS app; on Linux the desktop entry is installed from the repo with `mise run shell-install`"
        );
    }
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
    let cli = std::env::current_exe().context("finding this binary")?;
    let path = std::env::var("PATH").unwrap_or_default();
    let app = bundle(&home.join("Applications"), &cli, &path)?;
    println!(
        "installed {}\nIt runs `{} app` with this terminal's PATH, and writes what that prints to ~/{LOG}.\nStart the window once from a terminal first: signing in needs one. When the app can't start the window it says why in a dialog.",
        app.display(),
        cli.display()
    );
    Ok(())
}

/// Writes the bundle into `applications`, replacing one already there, and returns its path. The bundle's executable
/// is a script that runs `cli app` with `path` as its PATH: an app started from the Dock gets the system's bare PATH,
/// with no cargo on it to build the window with.
fn bundle(applications: &Path, cli: &Path, path: &str) -> Result<PathBuf> {
    let app = applications.join(BUNDLE);
    let (macos, resources) = (app.join("Contents/MacOS"), app.join("Contents/Resources"));
    let script = script(cli, path)?;
    for dir in [&macos, &resources] {
        std::fs::create_dir_all(dir).with_context(|| dir.display().to_string())?;
    }
    write(&app.join("Contents/Info.plist"), INFO.as_bytes())?;
    write(&resources.join("trailhead.icns"), ICON)?;
    let launcher = macos.join(EXECUTABLE);
    write(&launcher, script.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))
            .with_context(|| launcher.display().to_string())?;
    }
    Ok(app)
}

fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).with_context(|| path.display().to_string())
}

fn script(cli: &Path, path: &str) -> Result<String> {
    let cli = cli.to_str().with_context(|| format!("{} is not UTF-8", cli.display()))?;
    // Both go in single quotes, where only a single quote means anything to the shell.
    for value in [cli, path] {
        if value.contains('\'') || value.contains('\n') {
            bail!("{value:?} has a character the launcher script can't hold as it is");
        }
    }
    Ok(format!(
        "#!/bin/sh\n# Written by `trailhead app install`; run it again after moving the CLI.\nexport PATH='{path}'\nlog=\"$HOME/{LOG}\"\nmkdir -p \"$(dirname \"$log\")\"\nexport {LOG_ENV}=\"$log\"\nexec '{cli}' app >>\"$log\" 2>&1\n"
    ))
}

/// No `LSUIElement`, though the launcher has no window of its own: macOS gives the window the launcher starts this
/// bundle's identity, so the window is what wears the name and icon in the Dock and the menu bar, and with
/// `LSUIElement` it had neither (seen on a Mac, 2026-11-21).
const INFO: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>Trailhead</string>
    <key>CFBundleDisplayName</key>
    <string>Trailhead</string>
    <key>CFBundleIdentifier</key>
    <string>dev.trailhead.launcher</string>
    <key>CFBundleExecutable</key>
    <string>trailhead-launcher</string>
    <key>CFBundleIconFile</key>
    <string>trailhead</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleVersion</key>
    <string>1</string>
    <key>CFBundleShortVersionString</key>
    <string>1</string>
</dict>
</plist>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundle_runs_the_cli_with_the_installing_terminals_path_and_wears_the_icon() {
        let dir = tempfile::tempdir().unwrap();
        let app =
            bundle(dir.path(), Path::new("/Users/me/.cargo/bin/trailhead"), "/Users/me/.cargo/bin:/usr/bin").unwrap();
        assert_eq!(app, dir.path().join("Trailhead.app"));
        let script = std::fs::read_to_string(app.join("Contents/MacOS").join(EXECUTABLE)).unwrap();
        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(script.contains("export PATH='/Users/me/.cargo/bin:/usr/bin'\n"));
        assert!(script.contains("exec '/Users/me/.cargo/bin/trailhead' app >>\"$log\" 2>&1\n"));
        assert!(script.contains("export TRAILHEAD_APP_LOG=\"$log\"\n"), "the launcher learns nobody reads its output");
        let info = std::fs::read_to_string(app.join("Contents/Info.plist")).unwrap();
        assert!(info.contains(&format!("<string>{EXECUTABLE}</string>")), "the plist names the script");
        assert!(!info.contains("LSUIElement"), "an agent app's window gets no Dock tile or menu bar");
        assert_eq!(&std::fs::read(app.join("Contents/Resources/trailhead.icns")).unwrap()[..4], b"icns");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(app.join("Contents/MacOS").join(EXECUTABLE)).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "the script is executable");
        }
    }

    #[test]
    fn installing_again_replaces_the_script() {
        let dir = tempfile::tempdir().unwrap();
        bundle(dir.path(), Path::new("/old/trailhead"), "/usr/bin").unwrap();
        let app = bundle(dir.path(), Path::new("/new/trailhead"), "/usr/bin").unwrap();
        let script = std::fs::read_to_string(app.join("Contents/MacOS").join(EXECUTABLE)).unwrap();
        assert!(script.contains("'/new/trailhead'") && !script.contains("/old/"));
    }

    #[test]
    fn a_path_the_script_cant_quote_is_refused_and_nothing_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let refused = bundle(dir.path(), Path::new("/Users/o'brien/trailhead"), "/usr/bin");
        assert!(refused.unwrap_err().to_string().contains("can't hold"));
        assert!(!dir.path().join(BUNDLE).exists());
    }
}
