//! The shell's window: [`Shell`] is what a binary builds and runs. It owns what every app shares: the window, titled
//! Trailhead and wearing its icon ([`crate::icon`]), the theme and the choice of light or dark, the kit's plugins, and the [`ShellPlugin`](crate::ShellPlugin) that holds
//! the apps and their connection. A binary names its apps and how to connect, and nothing else.

use std::path::PathBuf;
use std::time::Duration;

use bevy::prelude::*;
use bevy::winit::{UpdateMode, WinitSettings};
use trailhead_model::shell::BUILD_ENV;
use trailhead_ui::{Appearance, ThemeChoice, ThemePlugin, UiPlugin};

use crate::icon::{IconUiPlugin, primary_window};
use crate::net::NetConfig;
use crate::place::{PlaceFile, keep_place};
use crate::{ShellApp, ShellPlugin};

/// Where the theme picked is kept for the next launch: a file holding `light`, `dark` or `desktop`.
#[derive(Resource, Clone, Debug)]
pub struct ThemeFile(pub PathBuf);

impl ThemeFile {
    /// The choice kept last time, or the desktop's when there is none.
    pub fn load(&self) -> ThemeChoice {
        std::fs::read_to_string(&self.0).map_or(ThemeChoice::Desktop, |text| theme_from_word(&text))
    }
}

/// A theme choice as it is kept.
pub fn theme_word(choice: ThemeChoice) -> &'static str {
    match choice {
        ThemeChoice::Desktop => "desktop",
        ThemeChoice::Light => "light",
        ThemeChoice::Dark => "dark",
    }
}

/// The choice a kept word means; anything else is the desktop's.
pub fn theme_from_word(word: &str) -> ThemeChoice {
    ThemeChoice::ALL.into_iter().find(|c| theme_word(*c) == word.trim()).unwrap_or_default()
}

/// Keeps the theme picked for the next launch.
fn keep_theme(appearance: Res<Appearance>, file: Res<ThemeFile>, mut kept: Local<Option<ThemeChoice>>) {
    if *kept == Some(appearance.choice) {
        return;
    }
    // The first look is at what was loaded, which is already kept.
    if kept.replace(appearance.choice).is_none() {
        return;
    }
    if let Some(dir) = file.0.parent()
        && let Err(e) =
            std::fs::create_dir_all(dir).and_then(|()| std::fs::write(&file.0, theme_word(appearance.choice)))
    {
        warn!(error = %e, "couldn't keep the theme picked");
    }
}

/// The shell as a binary runs it: a window holding the apps given, in the order the rail shows them.
pub struct Shell {
    apps: ShellPlugin,
    dir: PathBuf,
}

impl Shell {
    /// A shell that keeps what it remembers on this machine under `dir`: the theme picked (`theme`) and what each
    /// app keeps in its [`LocalStore`](crate::LocalStore) (`apps/`).
    pub fn new(dir: PathBuf) -> Self {
        Shell { apps: ShellPlugin::new(), dir }
    }

    /// Adds an app, after the ones already given.
    #[must_use]
    pub fn with(mut self, app: impl ShellApp) -> Self {
        self.apps = self.apps.with(app);
        self
    }

    /// Connects to the Trailhead server through `config`, dialing `target`, or the control plane dialed last time
    /// without one ([`ShellPlugin::connecting`]).
    #[must_use]
    pub fn connecting(mut self, config: NetConfig, target: Option<String>) -> Self {
        self.apps = self.apps.connecting(config, target);
        self
    }

    /// Signs in with this user token at startup.
    #[must_use]
    pub fn signing_in_with(mut self, token: Option<String>) -> Self {
        self.apps = self.apps.signing_in_with(token);
        self
    }

    /// Opens the window and runs until it is closed. A shell the launcher started ([`BUILD_ENV`] names its build)
    /// offers to restart onto a newer build. An exit with an error code ends the process with that code, since the
    /// launcher reads it ([`RESTART_CODE`](trailhead_model::shell::RESTART_CODE) asks for the newest build) and a
    /// binary's `main` drops what this returns.
    pub fn run(self) -> AppExit {
        let theme_file = ThemeFile(self.dir.join("theme"));
        let build = std::env::var(BUILD_ENV).ok().filter(|b| !b.is_empty());
        let apps = self.apps.built_as(build).store_in(self.dir.join("apps")).windowed();
        let place = PlaceFile::load(self.dir.join("window"));
        let mut window = primary_window();
        if let Some(kept) = place.kept() {
            kept.apply(&mut window);
        }
        let exit = App::new()
            .add_plugins(DefaultPlugins.set(WindowPlugin { primary_window: Some(window), ..default() }))
            .insert_resource(place)
            // Update on input, and often enough for news from a background thread to show, instead of redrawing
            // every frame.
            .insert_resource(WinitSettings {
                focused_mode: UpdateMode::reactive(Duration::from_millis(100)),
                unfocused_mode: UpdateMode::reactive_low_power(Duration::from_secs(1)),
            })
            .add_plugins(ThemePlugin { theme: apps.theme(), choice: theme_file.load(), follow_desktop: true })
            .insert_resource(theme_file)
            .add_plugins(UiPlugin)
            .add_plugins(IconUiPlugin)
            .add_plugins(apps)
            .add_systems(Update, (keep_theme, keep_place))
            .run();
        if let AppExit::Error(code) = exit {
            std::process::exit(code.get().into());
        }
        exit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_theme_is_kept_as_a_word_and_anything_else_is_the_desktops() {
        for choice in ThemeChoice::ALL {
            assert_eq!(theme_from_word(theme_word(choice)), choice);
        }
        assert_eq!(theme_from_word("light\n"), ThemeChoice::Light);
        assert_eq!(theme_from_word("sepia"), ThemeChoice::Desktop);
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(ThemeFile(dir.path().join("theme")).load(), ThemeChoice::Desktop);
    }

    #[test]
    fn a_theme_picked_is_kept_and_the_one_loaded_is_not_written_again() {
        let dir = tempfile::tempdir().unwrap();
        let file = ThemeFile(dir.path().join("kept").join("theme"));
        let mut app = App::new();
        app.insert_resource(Appearance::new(file.load())).insert_resource(file.clone()).add_systems(Update, keep_theme);
        app.update();
        assert!(!file.0.exists(), "what was loaded is already kept");
        app.world_mut().resource_mut::<Appearance>().choice = ThemeChoice::Light;
        app.update();
        assert_eq!(file.load(), ThemeChoice::Light);
    }
}
