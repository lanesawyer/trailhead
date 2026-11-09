//! The colors the kit wears, in one place.
//!
//! Colors come in three layers (`docs/factory/ui.md`, "Colors"). The [`palette`] is raw colors, named only where a
//! theme is built. The [`token`]s are meanings (a surface, text, the border, a fill, a status), which views and widgets
//! name. Each one sits on one of feathers' semantic slots where feathers has one, so repainting a meaning with
//! [`Theme::set`] repaints the kit's widgets and feathers' controls together. A widget's own token is an
//! [`Theme::alias`] of a meaning; an app's colors that encode data (a status column, a priority) are categorical, each
//! [`Theme::paint`]ed on a slot of its own. A view names a [`Token`], never a color, so a missing token shows up as
//! fuchsia rather than as a wrong color nobody notices.
//!
//! Feathers' dark theme is the base. Its hovers are widened (as bevy-data-explorer found, feathers' own are too faint
//! to read as a response) on the semantic slots its controls share, so every control hovers alike.
//!
//! A theme is painted dark, and worn light too ([`Theme::light`]) and in the desktop's accent ([`Theme::with_accent`]);
//! which one is worn is [`super::Appearance`]'s. Feathers has only a dark theme, so the light one is the dark one
//! turned over, as bevy-data-explorer does it: a near-neutral color keeps its distance from the background and swaps
//! which side of it it is on, and a color that means something (a status, a priority) keeps its hue and is darkened to
//! hold its own on a pale background. The accent's shades (feathers' own, and those set with [`Theme::set_accent`])
//! stay as they are in either theme, and take the desktop's accent when it has one.
//!
//! Feathers stays inside the kit: an app names [`Token`], [`Theme`], [`palette`], [`Surface`] and the tone components
//! here, never feathers' own, so the kit is the one place that changes when feathers does.

use std::collections::HashSet;

use bevy::app::Propagate;
use bevy::ecs::lifecycle::HookContext;
use bevy::ecs::world::DeferredWorld;
use bevy::feathers::dark_theme::create_dark_theme;
use bevy::feathers::palette as feathers_palette;
use bevy::feathers::theme::{
    SemanticToken, SurfaceLevel, ThemeBackgroundColor, ThemeBorderColor, ThemeContext, ThemeProps, ThemeTextColor,
    ThemeToken, UiTheme,
};
use bevy::feathers::tokens::{self, semantic};
use bevy::prelude::*;

/// A named color in the theme. A view puts one on a node with [`BackgroundTone`], [`BorderTone`] or [`TextTone`].
#[derive(Clone, PartialEq, Eq, Hash, Default, Debug)]
pub struct Token(pub(crate) ThemeToken);

impl Token {
    /// A token called `name`. Prefix it with the app's name (`lookout.priority.1`) so it can't collide with the kit's.
    pub const fn new(name: &'static str) -> Self {
        Self(ThemeToken::new_static(name))
    }
}

impl std::fmt::Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// The kit's semantic tokens: what a color is for. Views and widgets name these; [`Theme::set`] repaints one.
pub mod token {
    use super::Token;

    /// The window itself, behind everything: what shows where nothing is drawn.
    pub const SURFACE_WINDOW: Token = Token::new("ui.surface.window");
    /// A panel over the window: an inspector, a sidebar, a modal's body.
    pub const SURFACE_PANEL: Token = Token::new("ui.surface.panel");
    /// Something raised on a panel: a card, someone else's message, a field well.
    pub const SURFACE_RAISED: Token = Token::new("ui.surface.raised");
    /// Something floating over the rest: a hint, a menu, a popup.
    pub const SURFACE_FLOATING: Token = Token::new("ui.surface.floating");

    /// Text.
    pub const TEXT: Token = Token::new("ui.text");
    /// Text secondary to what's beside it.
    pub const TEXT_DIM: Token = Token::new("ui.text.dim");
    /// Text of something that can't be used right now.
    pub const TEXT_DISABLED: Token = Token::new("ui.text.disabled");

    /// A border, and the rule between sections or beside a sidebar.
    pub const BORDER: Token = Token::new("ui.border");

    /// A solid fill that stands off any surface: a badge, a placeholder bar, a normal button.
    pub const FILL_SOLID: Token = Token::new("ui.fill.solid");
    pub const FILL_SOLID_HOVER: Token = Token::new("ui.fill.solid.hover");
    pub const FILL_SOLID_PRESSED: Token = Token::new("ui.fill.solid.pressed");
    /// Something pressable with no fill until the pointer is on it: a plain button, a tab, a row.
    pub const FILL_ITEM: Token = Token::new("ui.fill.item");
    pub const FILL_ITEM_HOVER: Token = Token::new("ui.fill.item.hover");
    pub const FILL_ITEM_PRESSED: Token = Token::new("ui.fill.item.pressed");
    /// The accent: the one call to action, the tab picked, the reader's own message.
    pub const FILL_ACCENT: Token = Token::new("ui.fill.accent");
    pub const FILL_ACCENT_HOVER: Token = Token::new("ui.fill.accent.hover");
    pub const FILL_ACCENT_PRESSED: Token = Token::new("ui.fill.accent.pressed");
    /// Where text is typed.
    pub const FILL_FIELD: Token = Token::new("ui.fill.field");
    pub const FILL_FIELD_HOVER: Token = Token::new("ui.fill.field.hover");
    pub const FILL_FIELD_PRESSED: Token = Token::new("ui.fill.field.pressed");

    /// Something that went well: a passed gate, a live connection.
    pub const STATUS_OK: Token = Token::new("ui.status.ok");
    /// Something that wants a person: an open question, a refusal to look at.
    pub const STATUS_WARN: Token = Token::new("ui.status.warn");
    /// Something destructive (cancel, revert) or failed (a failing gate).
    pub const STATUS_DANGER: Token = Token::new("ui.status.danger");
    /// Something under way, or worth knowing: news that isn't good or bad.
    pub const STATUS_INFO: Token = Token::new("ui.status.info");
    /// Each status faintly, for the box of a message in it.
    pub const STATUS_OK_SUBTLE: Token = Token::new("ui.status.ok.subtle");
    pub const STATUS_WARN_SUBTLE: Token = Token::new("ui.status.warn.subtle");
    pub const STATUS_DANGER_SUBTLE: Token = Token::new("ui.status.danger.subtle");
    pub const STATUS_INFO_SUBTLE: Token = Token::new("ui.status.info.subtle");

    /// Every semantic token, for a test that each resolves.
    pub const ALL: [Token; 28] = [
        SURFACE_WINDOW,
        SURFACE_PANEL,
        SURFACE_RAISED,
        SURFACE_FLOATING,
        TEXT,
        TEXT_DIM,
        TEXT_DISABLED,
        BORDER,
        FILL_SOLID,
        FILL_SOLID_HOVER,
        FILL_SOLID_PRESSED,
        FILL_ITEM,
        FILL_ITEM_HOVER,
        FILL_ITEM_PRESSED,
        FILL_ACCENT,
        FILL_ACCENT_HOVER,
        FILL_ACCENT_PRESSED,
        FILL_FIELD,
        FILL_FIELD_HOVER,
        FILL_FIELD_PRESSED,
        STATUS_OK,
        STATUS_WARN,
        STATUS_DANGER,
        STATUS_INFO,
        STATUS_OK_SUBTLE,
        STATUS_WARN_SUBTLE,
        STATUS_DANGER_SUBTLE,
        STATUS_INFO_SUBTLE,
    ];
}

/// The base colors the kit's tokens are painted from, for an app's own colors to sit beside them. Named only where a
/// theme is built.
pub mod palette {
    use bevy::color::Color;
    use bevy::feathers::palette;

    /// The darkest surface: behind everything else.
    pub const GRAY_0: Color = palette::GRAY_0;
    /// A panel.
    pub const GRAY_1: Color = palette::GRAY_1;
    /// Something raised on a panel: a card.
    pub const GRAY_2: Color = palette::GRAY_2;
    /// A border, a badge.
    pub const GRAY_3: Color = palette::GRAY_3;
    pub const WHITE: Color = palette::WHITE;
    /// The primary button's blue.
    pub const ACCENT: Color = palette::ACCENT;
}

/// The kit's tokens that sit on one of feathers' semantic slots, so feathers' controls wear the same colors.
const ON_FEATHERS: [(Token, SemanticToken); 20] = [
    (token::SURFACE_WINDOW, semantic::SURFACE_WINDOW),
    (token::SURFACE_PANEL, semantic::SURFACE_PANE_BODY),
    (token::SURFACE_RAISED, semantic::SURFACE_GROUP),
    (token::SURFACE_FLOATING, semantic::SURFACE_MENU),
    (token::TEXT, semantic::TEXT_DEFAULT),
    (token::TEXT_DIM, semantic::TEXT_DIM),
    (token::TEXT_DISABLED, semantic::TEXT_DISABLED),
    (token::BORDER, semantic::BORDER_DEFAULT),
    (token::FILL_SOLID, semantic::FILL_SOLID_DEFAULT),
    (token::FILL_SOLID_HOVER, semantic::FILL_SOLID_HOVER),
    (token::FILL_SOLID_PRESSED, semantic::FILL_SOLID_PRESSED),
    (token::FILL_ITEM, semantic::FILL_ITEM_DEFAULT),
    (token::FILL_ITEM_HOVER, semantic::FILL_ITEM_HOVER),
    (token::FILL_ITEM_PRESSED, semantic::FILL_ITEM_PRESSED),
    (token::FILL_ACCENT, semantic::FILL_ACCENT_DEFAULT),
    (token::FILL_ACCENT_HOVER, semantic::FILL_ACCENT_HOVER),
    (token::FILL_ACCENT_PRESSED, semantic::FILL_ACCENT_PRESSED),
    (token::FILL_FIELD, semantic::FILL_FIELD_DEFAULT),
    (token::FILL_FIELD_HOVER, semantic::FILL_FIELD_HOVER),
    (token::FILL_FIELD_PRESSED, semantic::FILL_FIELD_PRESSED),
];

/// How strongly a status tints the box of a message in it.
const SUBTLE_ALPHA: f32 = 0.12;

/// A theme: every [`Token`] and its color.
#[derive(Clone)]
pub struct Theme {
    props: ThemeProps,
    /// The semantic slots holding a shade of the accent.
    accent: HashSet<SemanticToken>,
    /// The tokens painted a color of their own: an app's categorical colors.
    painted: Vec<Token>,
}

/// How saturated a color may be and still count as neutral chrome, which turns over in the light theme.
const NEUTRAL_CHROMA: f32 = 0.03;
/// How far a color's hue may be from feathers' accent and still be one of its shades rather than a color of its own.
const ACCENT_HUE_SPREAD: f32 = 25.0;
/// An accent this light or lighter, such as a yellow, takes dark text rather than white.
const PALE_ACCENT: f32 = 0.75;
/// How much darker a color that means something is in the light theme, so it reads on a pale background.
const LIGHT_DARKENING: f32 = 0.22;
/// The darkest such a color gets, so a dark one doesn't turn black.
const LIGHT_FLOOR: f32 = 0.35;
/// How far apart two colors' channels may be and still count as the same color.
const SAME_COLOR: f32 = 1e-3;

/// Text that doesn't turn over in the light theme, because what it is written on doesn't: a button keeps its pale
/// label over a mid gray, and the primary button's sits on the accent.
const STAYS_PUT: [ThemeToken; 6] = [
    tokens::BUTTON_TEXT,
    tokens::BUTTON_TEXT_DISABLED,
    tokens::BUTTON_PRIMARY_TEXT,
    tokens::BUTTON_PRIMARY_TEXT_DISABLED,
    tokens::SLIDER_TEXT,
    tokens::SLIDER_TEXT_DISABLED,
];

/// Text drawn on the accent itself, which turns dark on a pale accent.
const ON_ACCENT: [ThemeToken; 2] = [tokens::BUTTON_PRIMARY_TEXT, tokens::CHECKBOX_MARK];

impl Theme {
    /// Feathers' dark theme with the kit's tokens on its slots: the start of an app's own theme.
    pub fn new() -> Self {
        let mut props = create_dark_theme();
        // Feathers' own shades of its accent, found by their hue before anything else is painted.
        let accent = props.semantic_base.iter().filter(|(_, c)| is_accent_shade(**c)).map(|(s, _)| s.clone()).collect();
        for (token, slot) in ON_FEATHERS {
            props.token_assignments.insert(token.0, slot);
        }
        let mut theme = Self { props, accent, painted: Vec::new() };

        // One border, the same gray everywhere, where feathers' is warm.
        theme.set(&token::BORDER, palette::GRAY_3);
        // Floating things are the lightest surface, so a hint or menu stands off the panel under it.
        theme.set(&token::SURFACE_FLOATING, palette::GRAY_3.darker(0.05));
        // Feathers' hovers are a shade of the surface, too faint to read as a response; the kit's are one color that
        // shows on every surface, and a press is the accent.
        let hover = palette::GRAY_3.lighter(0.14);
        let pressed = palette::ACCENT.darker(0.12);
        theme.set(&token::FILL_SOLID_HOVER, hover);
        theme.set_accent(&token::FILL_SOLID_PRESSED, pressed);
        theme.set(&token::FILL_ITEM_HOVER, hover);
        theme.set_accent(&token::FILL_ITEM_PRESSED, pressed);
        theme.set_accent(&token::FILL_ACCENT_HOVER, palette::ACCENT.lighter(0.08));

        let statuses = [
            (token::STATUS_OK, token::STATUS_OK_SUBTLE, Color::oklch(0.75, 0.14, 150.0)),
            (token::STATUS_WARN, token::STATUS_WARN_SUBTLE, Color::oklch(0.80, 0.14, 75.0)),
            (token::STATUS_DANGER, token::STATUS_DANGER_SUBTLE, Color::oklch(0.68, 0.17, 25.0)),
            (token::STATUS_INFO, token::STATUS_INFO_SUBTLE, Color::srgb(0.45, 0.68, 0.95)),
        ];
        for (status, subtle, color) in statuses {
            theme.set(&status, color);
            theme.set(&subtle, color.with_alpha(SUBTLE_ALPHA));
        }
        theme
    }

    /// Repaint the meaning `token` is on, on every surface: every token aliased to it follows. A token with no slot
    /// yet gets one named after it.
    pub fn set(&mut self, token: &Token, color: Color) {
        let slot = self.slot(token, color);
        self.accent.remove(&slot);
    }

    /// [`Theme::set`] a shade of [`palette::ACCENT`] (the accent itself, or a step lighter or darker), which takes the
    /// desktop's accent along with feathers' own.
    pub fn set_accent(&mut self, token: &Token, color: Color) {
        let slot = self.slot(token, color);
        self.accent.insert(slot);
    }

    /// Have `token` wear whatever `of` wears, now and after either theme turns: a widget's own token on a meaning.
    ///
    /// # Panics
    ///
    /// When `of` isn't in the theme, which a theme built at startup finds at once.
    pub fn alias(&mut self, token: &Token, of: &Token) {
        let slot = self.props.token_assignments.get(&of.0).unwrap_or_else(|| panic!("{of} isn't in the theme")).clone();
        self.props.token_assignments.insert(token.0.clone(), slot);
    }

    /// Give `token` a color of its own, on a slot named after it, so changing it repaints nothing else: for an app's
    /// categorical colors, which encode data (a status column, a priority) rather than mean something to the kit.
    pub fn paint(&mut self, token: &Token, color: Color) {
        let own = self.own_slot(&token.0, color);
        self.accent.remove(&own);
        self.painted.retain(|t| t != token);
        self.painted.push(token.clone());
    }

    /// Set the color of the slot `token` is on, giving it one named after it if it has none. A color set this way
    /// holds on every surface, so feathers' overrides of the slot go.
    fn slot(&mut self, token: &Token, color: Color) -> SemanticToken {
        let slot = match self.props.token_assignments.get(&token.0) {
            Some(slot) => slot.clone(),
            None => {
                let own = SemanticToken::new(token.0.to_string().into());
                self.props.token_assignments.insert(token.0.clone(), own.clone());
                own
            }
        };
        self.props.semantic_base.insert(slot.clone(), color);
        for colors in self.props.semantic_overrides.values_mut() {
            colors.remove(&slot);
        }
        slot
    }

    fn own_slot(&mut self, token: &ThemeToken, color: Color) -> SemanticToken {
        let own = SemanticToken::new(token.to_string().into());
        self.props.token_assignments.insert(token.clone(), own.clone());
        self.props.semantic_base.insert(own.clone(), color);
        own
    }

    /// The color `token` is painted, fuchsia when it isn't in the theme.
    pub fn color(&self, token: &Token) -> Color {
        self.props.color(&token.0)
    }

    /// The color `token` is painted on `surface`, as something inside that surface wears it.
    pub fn color_on(&self, token: &Token, surface: Surface) -> Color {
        self.props.context_color(&token.0, surface.level())
    }

    /// Whether `token` is in the theme, so a test can check every token resolves.
    pub fn has(&self, token: &Token) -> bool {
        self.props.token_assignments.contains_key(&token.0)
    }

    /// Each painted token whose color is another painted token's or a semantic token's, as `painted = other`. Such a
    /// color has a meaning already: alias it (or name the meaning) rather than paint the same color twice.
    pub fn repeated_colors(&self) -> Vec<String> {
        let mut repeated = Vec::new();
        let meanings = token::ALL;
        for (i, painted) in self.painted.iter().enumerate() {
            let color = self.color(painted);
            let others = self.painted[i + 1..].iter().chain(meanings.iter());
            for other in others.filter(|other| *other != painted) {
                if same_color(color, self.color(other)) {
                    repeated.push(format!("{painted} = {other}"));
                }
            }
        }
        repeated
    }

    /// The same theme for a light desktop: the near-neutrals turned over, the colors that mean something darkened,
    /// the accent and the text on unmoving surfaces ([`STAYS_PUT`]) left alone. A first cut to look at and adjust,
    /// not a designed palette.
    pub fn light(&self) -> Self {
        let mut theme = self.clone();
        let kept: Vec<SemanticToken> = STAYS_PUT
            .iter()
            .map(|token| {
                let color = theme.props.color(token);
                theme.own_slot(token, color)
            })
            .collect();
        let accent = theme.accent.clone();
        for (slot, color) in colors_mut(&mut theme.props) {
            if !kept.contains(slot) && !accent.contains(slot) {
                *color = to_light(*color);
            }
        }
        theme
    }

    /// The same theme in `accent` wherever it wears feathers' own.
    pub fn with_accent(&self, accent: Color) -> Self {
        let mut theme = self.clone();
        let slots = theme.accent.clone();
        for (slot, color) in colors_mut(&mut theme.props) {
            if slots.contains(slot) {
                *color = onto_accent(*color, accent);
            }
        }
        if Oklcha::from(accent).lightness >= PALE_ACCENT {
            for token in &ON_ACCENT {
                theme.own_slot(token, feathers_palette::BLACK);
            }
        }
        theme
    }

    pub(crate) fn resource(self) -> UiTheme {
        UiTheme(self.props)
    }
}

/// Whether `a` and `b` are the same color, to the eye.
fn same_color(a: Color, b: Color) -> bool {
    let (a, b) = (a.to_srgba().to_f32_array(), b.to_srgba().to_f32_array());
    a.iter().zip(b).all(|(a, b)| (a - b).abs() < SAME_COLOR)
}

/// Every color in a theme, on any surface.
fn colors_mut(props: &mut ThemeProps) -> impl Iterator<Item = (&SemanticToken, &mut Color)> {
    props.semantic_base.iter_mut().chain(props.semantic_overrides.values_mut().flat_map(|colors| colors.iter_mut()))
}

/// Whether `color` is one of feathers' accent's shades: saturated, and near its hue.
fn is_accent_shade(color: Color) -> bool {
    let (from, shade) = (Oklcha::from(feathers_palette::ACCENT), Oklcha::from(color));
    let hue_gap = (shade.hue - from.hue + 180.0).rem_euclid(360.0) - 180.0;
    shade.chroma > NEUTRAL_CHROMA && hue_gap.abs() <= ACCENT_HUE_SPREAD
}

/// A shade of feathers' accent moved onto `accent`: it keeps its step lighter or darker and takes the new hue and
/// saturation.
fn onto_accent(color: Color, accent: Color) -> Color {
    let (from, to, shade) = (Oklcha::from(feathers_palette::ACCENT), Oklcha::from(accent), Oklcha::from(color));
    Color::from(Oklcha {
        lightness: (shade.lightness + to.lightness - from.lightness).clamp(0.0, 1.0),
        chroma: shade.chroma * to.chroma / from.chroma,
        hue: to.hue,
        alpha: shade.alpha,
    })
}

/// `color` in the light theme: a near-neutral turned over, anything else darkened.
fn to_light(color: Color) -> Color {
    let oklch = Oklcha::from(color);
    let lightness = if oklch.chroma <= NEUTRAL_CHROMA {
        1.0 - oklch.lightness
    } else {
        (oklch.lightness - LIGHT_DARKENING).max(LIGHT_FLOOR.min(oklch.lightness))
    };
    Color::from(Oklcha { lightness, ..oklch })
}

impl Default for Theme {
    fn default() -> Self {
        Self::new()
    }
}

/// A node's background in `Token`'s color, repainted when the theme changes. Insert it again to change the token.
#[derive(Component, Clone, Default, PartialEq, Debug)]
#[component(immutable, on_insert = background_tone)]
pub struct BackgroundTone(pub Token);

/// A node's border in `Token`'s color, all four sides. Insert it again to change the token.
#[derive(Component, Clone, Default, PartialEq, Debug)]
#[component(immutable, on_insert = border_tone)]
pub struct BorderTone(pub Token);

/// A text's color, `Token`'s, on the text itself rather than inherited. Insert it again to change the token.
#[derive(Component, Clone, Default, PartialEq, Debug)]
#[component(immutable, on_insert = text_tone)]
pub struct TextTone(pub Token);

fn background_tone(mut world: DeferredWorld, context: HookContext) {
    let token = world.get::<BackgroundTone>(context.entity).map(|t| t.0.0.clone()).unwrap_or_default();
    world.commands().entity(context.entity).insert(ThemeBackgroundColor(token));
}

fn border_tone(mut world: DeferredWorld, context: HookContext) {
    let token = world.get::<BorderTone>(context.entity).map(|t| t.0.0.clone()).unwrap_or_default();
    world.commands().entity(context.entity).insert(ThemeBorderColor(token));
}

fn text_tone(mut world: DeferredWorld, context: HookContext) {
    let token = world.get::<TextTone>(context.entity).map(|t| t.0.0.clone()).unwrap_or_default();
    world.commands().entity(context.entity).insert(ThemeTextColor(token));
}

/// A surface: its background in its level's token, and the level passed down to everything inside it, so a control
/// on it (a field, a solid fill) takes the color that shows on this surface. Insert it again to change the level.
#[derive(Component, Clone, Copy, PartialEq, Eq, Default, Debug)]
#[component(immutable, on_insert = surface)]
pub enum Surface {
    /// The window, behind everything.
    #[default]
    Window,
    /// A panel over the window: an inspector, a sidebar.
    Panel,
    /// Raised on a panel: a card, a field well.
    Raised,
    /// Floating over the rest: a hint, a menu.
    Floating,
}

impl Surface {
    /// The token the surface's background wears.
    pub fn token(self) -> Token {
        match self {
            Surface::Window => token::SURFACE_WINDOW,
            Surface::Panel => token::SURFACE_PANEL,
            Surface::Raised => token::SURFACE_RAISED,
            Surface::Floating => token::SURFACE_FLOATING,
        }
    }

    fn level(self) -> SurfaceLevel {
        match self {
            Surface::Window => SurfaceLevel::Base,
            Surface::Panel => SurfaceLevel::Higher,
            Surface::Raised => SurfaceLevel::Highest,
            Surface::Floating => SurfaceLevel::Floating,
        }
    }
}

fn surface(mut world: DeferredWorld, context: HookContext) {
    let surface = world.get::<Surface>(context.entity).copied().unwrap_or_default();
    world
        .commands()
        .entity(context.entity)
        .insert((BackgroundTone(surface.token()), Propagate(ThemeContext(surface.level()))));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kit_token_resolves_rather_than_falling_back_to_fuchsia() {
        let theme = Theme::new();
        for t in token::ALL {
            assert!(theme.has(&t), "{t} isn't in the theme");
        }
    }

    #[test]
    fn the_kits_tokens_and_feathers_controls_share_a_slot() {
        let mut theme = Theme::new();
        theme.set(&token::FILL_SOLID, Color::WHITE);
        assert_eq!(theme.color(&Token(tokens::BUTTON_BG)), Color::WHITE, "a normal button wears fill.solid");
        assert_eq!(theme.color(&Token(tokens::BUTTON_PLAIN_BG_HOVER)), theme.color(&token::FILL_ITEM_HOVER));
        assert_eq!(theme.color(&Token(tokens::BUTTON_PRIMARY_BG)), theme.color(&token::FILL_ACCENT));
    }

    #[test]
    fn an_alias_follows_its_meaning_and_a_painted_token_stays_its_own() {
        const MINE: Token = Token::new("app.mine");
        const OWN: Token = Token::new("app.own");
        let mut theme = Theme::new();
        theme.alias(&MINE, &token::STATUS_DANGER);
        theme.paint(&OWN, theme.color(&token::STATUS_OK));
        theme.set(&token::STATUS_DANGER, Color::WHITE);
        theme.set(&token::STATUS_OK, Color::BLACK);
        assert_eq!(theme.color(&MINE), Color::WHITE);
        assert_ne!(theme.color(&OWN), Color::BLACK);
        assert_eq!(theme.resource().color(&MINE.0), Color::WHITE);
    }

    #[test]
    fn a_painted_color_that_repeats_a_meaning_is_found() {
        const MINE: Token = Token::new("app.mine");
        const OTHER: Token = Token::new("app.other");
        let mut theme = Theme::new();
        theme.paint(&MINE, Color::oklch(0.5, 0.1, 10.0));
        theme.paint(&OTHER, Color::oklch(0.5, 0.1, 200.0));
        assert!(theme.repeated_colors().is_empty(), "{:?}", theme.repeated_colors());
        theme.paint(&OTHER, palette::GRAY_2);
        assert!(theme.repeated_colors().contains(&"app.other = ui.surface.raised".to_string()));
    }

    #[test]
    fn a_hover_shows_on_every_surface() {
        // Feathers' hovers are a shade of the surface; the kit's are set on every surface, so its overrides go.
        let theme = Theme::new();
        for surface in [Surface::Window, Surface::Panel, Surface::Raised, Surface::Floating] {
            for (rest, hover) in
                [(token::FILL_SOLID, token::FILL_SOLID_HOVER), (surface.token(), token::FILL_ITEM_HOVER)]
            {
                let gap = lightness(theme.color_on(&hover, surface)) - lightness(theme.color_on(&rest, surface));
                assert!(gap.abs() > 0.05, "{hover} on {surface:?} is {gap:.3} from {rest}");
            }
        }
    }

    #[test]
    fn a_field_on_a_raised_surface_is_lifted_off_it() {
        // Feathers fills a field in the panel's gray; on a raised surface it takes a lighter one.
        let theme = Theme::new();
        let field = |surface| lightness(theme.color_on(&token::FILL_FIELD, surface));
        assert!(field(Surface::Raised) > lightness(theme.color(&token::SURFACE_RAISED)));
        assert!(field(Surface::Raised) > field(Surface::Window));
    }

    fn lightness(color: Color) -> f32 {
        Oklcha::from(color).lightness
    }

    #[test]
    fn the_light_theme_turns_the_chrome_over_and_keeps_text_readable() {
        let (dark, light) = (Theme::new(), Theme::new().light());
        for surface in [token::SURFACE_WINDOW, token::SURFACE_PANEL, token::SURFACE_RAISED] {
            assert!(lightness(dark.color(&surface)) < 0.5 && lightness(light.color(&surface)) > 0.5, "{surface}");
        }
        assert!(lightness(light.color(&token::TEXT)) < 0.5);
        let gap = lightness(light.color(&token::TEXT)) - lightness(light.color(&token::SURFACE_PANEL));
        assert!(gap.abs() > 0.3, "text has to stay readable: {gap}");
    }

    #[test]
    fn text_on_a_background_that_moves_moves_with_it_and_a_buttons_label_stays() {
        let light = Theme::new().light();
        for text in [tokens::MENUITEM_TEXT, tokens::TEXT_INPUT_TEXT] {
            assert!(lightness(light.color(&Token(text.clone()))) < 0.5, "{text} stayed pale over a pale background");
        }
        assert!(lightness(light.color(&Token(tokens::BUTTON_TEXT))) > 0.9);
    }

    #[test]
    fn colors_that_mean_something_keep_their_hue_and_darken_in_the_light_theme() {
        let (dark, light) = (Theme::new(), Theme::new().light());
        for tone in [token::STATUS_WARN, token::STATUS_DANGER, token::STATUS_OK, token::STATUS_INFO] {
            let (d, l) = (Oklcha::from(dark.color(&tone)), Oklcha::from(light.color(&tone)));
            assert!((d.hue - l.hue).abs() < 1.0 && l.lightness < d.lightness, "{tone}");
        }
        // The accent says "this one" in either theme.
        assert_eq!(light.color(&token::FILL_ACCENT), palette::ACCENT);
    }

    #[test]
    fn an_accent_reaches_every_shade_of_feathers_own_and_the_kits_and_nothing_else() {
        let purple = Color::srgb(0.568627, 0.254902, 0.67451);
        let (plain, theme) = (Theme::new(), Theme::new().with_accent(purple));
        let hue = |color: Color| Oklcha::from(color).hue;
        for t in
            [token::FILL_ACCENT, token::FILL_ACCENT_HOVER, token::FILL_ITEM_PRESSED, Token(tokens::CHECKBOX_BG_CHECKED)]
        {
            assert!((hue(theme.color(&t)) - hue(purple)).abs() < 1.0, "{t}");
        }
        for t in [token::SURFACE_PANEL, token::TEXT, token::FILL_SOLID, token::STATUS_INFO, token::STATUS_OK] {
            assert_eq!(theme.color(&t), plain.color(&t), "{t}");
        }
        let primary = |t: Token| lightness(theme.color(&t));
        assert!(primary(token::FILL_ACCENT_HOVER) > primary(token::FILL_ACCENT), "hover still lifts");
    }

    #[test]
    fn a_pale_accent_takes_dark_text() {
        let on_accent = |accent| lightness(Theme::new().with_accent(accent).color(&Token(tokens::BUTTON_PRIMARY_TEXT)));
        assert!(on_accent(Color::srgb_u8(255, 204, 0)) < 0.2);
        assert!(on_accent(Color::srgb_u8(0, 122, 255)) > 0.95);
    }

    #[test]
    fn an_apps_token_painted_a_neutral_turns_over_too() {
        const MINE: Token = Token::new("app.mine");
        let mut theme = Theme::new();
        theme.paint(&MINE, palette::GRAY_0);
        assert!(lightness(theme.light().color(&MINE)) > 0.5);
    }

    #[test]
    fn a_tone_component_puts_feathers_on_the_node_and_follows_a_new_token() {
        let mut world = World::new();
        let node = world
            .spawn((BackgroundTone(token::STATUS_OK), BorderTone(token::BORDER), TextTone(token::STATUS_WARN)))
            .id();
        world.flush();
        assert!(world.get::<ThemeBackgroundColor>(node).unwrap().0 == token::STATUS_OK.0);
        assert!(world.get::<ThemeBorderColor>(node).unwrap().0 == token::BORDER.0);
        assert!(world.get::<ThemeTextColor>(node).unwrap().0 == token::STATUS_WARN.0);
        world.entity_mut(node).insert(BackgroundTone(token::STATUS_DANGER));
        world.flush();
        assert!(world.get::<ThemeBackgroundColor>(node).unwrap().0 == token::STATUS_DANGER.0);
    }

    #[test]
    fn a_surface_wears_its_token_and_tells_what_is_inside_it() {
        let mut world = World::new();
        let card = world.spawn(Surface::Raised).id();
        world.flush();
        assert_eq!(world.get::<BackgroundTone>(card).unwrap().0, token::SURFACE_RAISED);
        assert_eq!(world.get::<Propagate<ThemeContext>>(card).unwrap().0, ThemeContext(SurfaceLevel::Highest));
    }
}
