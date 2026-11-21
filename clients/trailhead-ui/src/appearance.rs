//! Which theme an app wears, and where that comes from.
//!
//! By default the desktop's: light or dark from the window ([`WindowTheme`], followed through `WindowThemeChanged`),
//! or from the desktop portal where the window can't tell, and in the desktop's accent color, asked once at startup
//! (`desktop.rs`). Until the desktop answers, the theme is dark in feathers' accent.
//!
//! Following stops once the person picks light or dark ([`ThemeChoice`]): a preference read from elsewhere is a good
//! default and a bad override. The app keeps [`Appearance::choice`] across launches and hands it to [`ThemePlugin`];
//! picking [`ThemeChoice::Desktop`] hands the theme back. The accent is followed either way.
//!
//! Ported from bevy-data-explorer's `app/theme.rs`.

use bevy::feathers::FeathersPlugins;
use bevy::feathers::theme::UiTheme;
use bevy::prelude::*;
use bevy::tasks::{IoTaskPool, Task, block_on, poll_once};
use bevy::window::{WindowTheme, WindowThemeChanged};

use crate::desktop::{self, Desktop};
use crate::icons::Icon;
use crate::theme::{Theme, token};

/// Where the theme's light or dark comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemeChoice {
    /// Whatever the desktop prefers.
    #[default]
    Desktop,
    Light,
    Dark,
}

impl ThemeChoice {
    /// In the order they are offered.
    pub const ALL: [ThemeChoice; 3] = [ThemeChoice::Light, ThemeChoice::Dark, ThemeChoice::Desktop];

    /// The choice as a word, for a button.
    pub fn label(self) -> &'static str {
        match self {
            ThemeChoice::Desktop => "Desktop",
            ThemeChoice::Light => "Light",
            ThemeChoice::Dark => "Dark",
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            ThemeChoice::Desktop => Icon::Monitor,
            ThemeChoice::Light => Icon::Sun,
            ThemeChoice::Dark => Icon::Moon,
        }
    }
}

/// The theme being worn: the person's choice, and what the desktop says. Set [`Appearance::choice`] to change it.
#[derive(Resource, Debug, Clone, Default, PartialEq)]
pub struct Appearance {
    pub choice: ThemeChoice,
    /// Light or dark, as the desktop prefers, once it has said.
    desktop: Option<WindowTheme>,
    /// The desktop's accent, once it has answered with one.
    accent: Option<Color>,
}

impl Appearance {
    pub fn new(choice: ThemeChoice) -> Self {
        Self { choice, ..default() }
    }

    /// Whether the theme worn is the dark one: the choice, else the desktop's, else dark.
    pub fn is_dark(&self) -> bool {
        match self.choice {
            ThemeChoice::Light => false,
            ThemeChoice::Dark => true,
            ThemeChoice::Desktop => self.desktop != Some(WindowTheme::Light),
        }
    }

    /// `theme` as it is worn now.
    pub fn wear(&self, theme: &Theme) -> Theme {
        let theme = if self.is_dark() { theme.clone() } else { theme.light() };
        match self.accent {
            Some(accent) => theme.with_accent(accent),
            None => theme,
        }
    }

    fn follow(&mut self, desktop: WindowTheme) {
        if self.desktop != Some(desktop) {
            self.desktop = Some(desktop);
        }
    }
}

/// The theme an app painted, which [`Appearance`] says how to wear.
#[derive(Resource, Clone)]
struct Painted(Theme);

/// The question put to the desktop at startup, until it answers.
#[derive(Resource)]
struct Asking(Task<Desktop>);

/// Feathers' controls and the theme they're drawn in, worn as [`Appearance`] says. An app adds this and
/// [`super::UiPlugin`]; without a theme every color is missing, and feathers draws missing ones in fuchsia.
pub struct ThemePlugin {
    pub theme: Theme,
    /// The person's choice, as the app kept it from last time.
    pub choice: ThemeChoice,
    /// Whether to ask the desktop for its accent and light or dark, and follow the window's. Off, `choice` and the
    /// theme's own accent are final: nothing asynchronous changes them, which a drawing to a file needs.
    pub follow_desktop: bool,
}

impl ThemePlugin {
    /// `theme`, following the desktop.
    pub fn new(theme: Theme) -> Self {
        Self { theme, choice: ThemeChoice::Desktop, follow_desktop: true }
    }

    /// `theme` worn as `choice` says and in its own accent, the desktop never asked.
    pub fn fixed(theme: Theme, choice: ThemeChoice) -> Self {
        Self { theme, choice, follow_desktop: false }
    }
}

impl Plugin for ThemePlugin {
    fn build(&self, app: &mut App) {
        let appearance = Appearance::new(self.choice);
        let worn = appearance.wear(&self.theme);
        app.add_plugins(FeathersPlugins)
            .insert_resource(ClearColor(worn.color(&token::SURFACE_WINDOW)))
            .insert_resource(worn.resource())
            .insert_resource(appearance)
            .insert_resource(Painted(self.theme.clone()));
        if self.follow_desktop {
            app.add_systems(Startup, ask_the_desktop)
                .add_systems(Update, (seed_from_window, follow_window, take_the_answer, wear).chain());
        } else {
            app.add_systems(Update, wear);
        }
    }
}

fn ask_the_desktop(mut commands: Commands) {
    commands.insert_resource(Asking(IoTaskPool::get().spawn(async { desktop::ask() })));
}

/// The desktop's answer, once it lands: its accent, and light or dark where the window didn't say.
fn take_the_answer(asking: Option<ResMut<Asking>>, mut appearance: ResMut<Appearance>, mut commands: Commands) {
    let Some(mut asking) = asking else { return };
    let Some(answer) = block_on(poll_once(&mut asking.0)) else { return };
    commands.remove_resource::<Asking>();
    match answer.accent {
        Some(color) => info!("wearing the desktop's accent, {:?}", color.to_srgba()),
        None => info!("the desktop has no accent to wear"),
    }
    appearance.accent = answer.accent;
    if appearance.desktop.is_none()
        && let Some(theme) = answer.theme
    {
        appearance.follow(theme);
    }
}

/// The window's theme as the platform reported it when the window was made, which it carries only if the platform
/// says. Read once: later changes come as `WindowThemeChanged`, and the window's own field isn't updated with them.
fn seed_from_window(windows: Query<&Window>, mut appearance: ResMut<Appearance>, mut seeded: Local<bool>) {
    if *seeded {
        return;
    }
    if let Some(theme) = windows.iter().find_map(|window| window.window_theme) {
        *seeded = true;
        appearance.follow(theme);
    }
}

fn follow_window(mut changed: MessageReader<WindowThemeChanged>, mut appearance: ResMut<Appearance>) {
    for event in changed.read() {
        appearance.follow(event.theme);
    }
}

/// Put the theme on: feathers repaints everything carrying a token when [`UiTheme`] changes, and the window clears
/// to its background, which shows wherever no node is drawn.
fn wear(appearance: Res<Appearance>, painted: Res<Painted>, mut theme: ResMut<UiTheme>, mut clear: ResMut<ClearColor>) {
    // A change in the very first frame counts too: the desktop can answer before that frame's update runs, and
    // nothing changes the appearance again after it (TRAIL-1615).
    if appearance.is_changed() {
        let worn = appearance.wear(&painted.0);
        clear.0 = worn.color(&token::SURFACE_WINDOW);
        *theme = worn.resource();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token;

    #[test]
    fn an_answer_that_lands_in_the_first_frame_is_worn() {
        let painted = Theme::default();
        let appearance = Appearance::new(ThemeChoice::Desktop);
        let before = appearance.wear(&painted);
        let accent = Color::srgb(0.57, 0.25, 0.67);
        let mut app = App::new();
        app.insert_resource(ClearColor(before.color(&token::SURFACE_WINDOW)))
            .insert_resource(before.clone().resource())
            .insert_resource(appearance)
            .insert_resource(Painted(painted.clone()))
            .add_systems(Update, (take_the_answer, wear).chain());
        let answered = IoTaskPool::get_or_init(Default::default)
            .spawn(async move { Desktop { accent: Some(accent), theme: None } });
        while !answered.is_finished() {
            std::thread::yield_now();
        }
        app.insert_resource(Asking(answered));
        app.update();
        let mut wanted = Appearance::new(ThemeChoice::Desktop);
        wanted.accent = Some(accent);
        let wanted = wanted.wear(&painted);
        assert_ne!(wanted.color(&token::FILL_ACCENT), before.color(&token::FILL_ACCENT));
        assert_eq!(app.world().resource::<UiTheme>().color(&token::FILL_ACCENT.0), wanted.color(&token::FILL_ACCENT));
    }

    fn lightness(color: Color) -> f32 {
        Oklcha::from(color).lightness
    }

    #[test]
    fn the_desktop_is_followed_until_a_choice_is_made() {
        let mut appearance = Appearance::default();
        assert!(appearance.is_dark(), "dark until the desktop says otherwise");
        appearance.follow(WindowTheme::Light);
        assert!(!appearance.is_dark());
        appearance.choice = ThemeChoice::Dark;
        assert!(appearance.is_dark());
        appearance.follow(WindowTheme::Light);
        assert!(appearance.is_dark(), "a chosen theme isn't taken back");
        appearance.choice = ThemeChoice::Desktop;
        assert!(!appearance.is_dark(), "handed back, it wears the desktop's again");
    }

    #[test]
    fn a_light_desktop_wears_the_light_theme_in_its_accent() {
        let purple = Color::srgb(0.568627, 0.254902, 0.67451);
        let appearance = Appearance { desktop: Some(WindowTheme::Light), accent: Some(purple), ..default() };
        let worn = appearance.wear(&Theme::new());
        assert!(lightness(worn.color(&token::SURFACE_PANEL)) > 0.5);
        let hue = Oklcha::from(worn.color(&token::FILL_ACCENT)).hue;
        assert!((hue - Oklcha::from(purple).hue).abs() < 1.0);
    }

    #[test]
    fn a_new_choice_repaints_the_theme() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(Painted(Theme::new()))
            .insert_resource(Theme::new().resource())
            .init_resource::<Appearance>()
            .init_resource::<ClearColor>()
            .add_systems(Update, wear);
        app.update();
        let panel = |app: &App| app.world().resource::<UiTheme>().color(&token::SURFACE_PANEL.0);
        assert!(lightness(panel(&app)) < 0.5);
        app.world_mut().resource_mut::<Appearance>().choice = ThemeChoice::Light;
        app.update();
        assert!(lightness(panel(&app)) > 0.5);
        assert!(lightness(app.world().resource::<ClearColor>().0) > 0.5, "the window behind it turns too");
    }
}
