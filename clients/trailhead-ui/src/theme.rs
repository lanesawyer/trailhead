//! The colors the kit wears, in one place.
//!
//! Feathers' dark theme is the base. Its button hover is widened (as bevy-data-explorer found, feathers' own is too
//! faint to read as a response), and the kit's base palette is added as tokens: the surfaces panels sit on and the
//! tones for badges, hints and warnings. A view names a [`Token`], never a color, so the theme is one edit and a missing
//! token shows up as fuchsia rather than as a wrong color nobody notices.
//!
//! An app adds its own colors on top: declare its [`Token`]s, start from [`Theme::new`], [`Theme::paint`] each one,
//! and hand it to [`ThemePlugin`]. Lookout does this for its board, cards, priorities and status columns.
//!
//! A theme is painted dark, and worn light too ([`Theme::light`]) and in the desktop's accent ([`Theme::with_accent`]);
//! which one is worn is [`super::Appearance`]'s. Feathers has only a dark theme, so the light one is the dark one
//! turned over, as bevy-data-explorer does it: a near-neutral color keeps its distance from the background and swaps
//! which side of it it is on, and a color that means something (a status, a priority, a warning) keeps its hue and is
//! darkened to hold its own on a pale background. The accent's shades (feathers' own, and those painted with
//! [`Theme::paint_accent`]) stay as they are in either theme, and take the desktop's accent when it has one.
//!
//! Feathers stays inside the kit: an app names [`Token`], [`Theme`], [`palette`] and the tone components here, never
//! feathers' own, so the kit is the one place that changes when feathers does.

use std::collections::HashSet;

use bevy::ecs::lifecycle::HookContext;
use bevy::ecs::world::DeferredWorld;
use bevy::feathers::dark_theme::create_dark_theme;
use bevy::feathers::theme::{
    SemanticToken, ThemeBackgroundColor, ThemeBorderColor, ThemeProps, ThemeTextColor, ThemeToken, UiTheme,
};
use bevy::feathers::{palette as feathers_palette, tokens};
use bevy::prelude::*;

/// A named color in the theme. A view puts one on a node with [`BackgroundTone`], [`BorderTone`] or [`TextTone`].
#[derive(Clone, PartialEq, Eq, Hash, Default, Debug)]
pub struct Token(pub(crate) ThemeToken);

impl Token {
    /// A token called `name`. Prefix it with the app's name (`lookout.card.bg`) so it can't collide with the kit's.
    pub const fn new(name: &'static str) -> Self {
        Self(ThemeToken::new_static(name))
    }
}

impl std::fmt::Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// The kit's tokens, and the controls' that a view names too.
pub mod token {
    use bevy::feathers::tokens;

    use super::Token;

    /// A panel over the app's content: an inspector, a connect panel, a form.
    pub const PANEL_BG: Token = Token::new("ui.panel.bg");
    /// A border, and the rule between sections of a panel.
    pub const DIVIDER: Token = Token::new("ui.divider");
    /// A small badge's background.
    pub const BADGE_BG: Token = Token::new("ui.badge.bg");
    /// The hint shown over a hovered tool button.
    pub const HINT_BG: Token = Token::new("ui.hint.bg");
    /// A message in a conversation from someone else.
    pub const BUBBLE_BG: Token = Token::new("ui.bubble.bg");
    /// A message in a conversation from the reader.
    pub const BUBBLE_MINE_BG: Token = Token::new("ui.bubble.mine.bg");

    /// Something that wants a person: an open question, a refusal to look at.
    pub const WARN: Token = Token::new("ui.warn");
    /// Something destructive (cancel, revert) or failed (a failing gate).
    pub const DANGER: Token = Token::new("ui.danger");
    /// Something that went well: a passed gate, a live connection.
    pub const OK: Token = Token::new("ui.ok");
    /// Something under way, or worth knowing: a notice that isn't good or bad news.
    pub const INFO: Token = Token::new("ui.info");
    /// A placeholder bar where content is on its way, at its brightest.
    pub const SKELETON: Token = Token::new("ui.skeleton");

    /// The window itself, behind everything: what shows where nothing is drawn.
    pub const WINDOW_BG: Token = Token(tokens::WINDOW_BG);
    /// The window's text.
    pub const TEXT: Token = Token(tokens::TEXT_MAIN);
    /// Text secondary to what's beside it.
    pub const TEXT_DIM: Token = Token(tokens::TEXT_DIM);
    /// A button's caption.
    pub const BUTTON_TEXT: Token = Token(tokens::BUTTON_TEXT);
    /// A plain button's background under the pointer, for an app to check against the surfaces its plain buttons sit
    /// on: it has to show on each.
    pub const BUTTON_PLAIN_BG_HOVER: Token = Token(tokens::BUTTON_PLAIN_BG_HOVER);
    /// A normal button's background, the token every control's theme is checked by.
    pub const BUTTON_BG: Token = Token(tokens::BUTTON_BG);
}

/// The base colors the kit's tokens are painted from, for an app's own tokens to sit beside them.
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

/// A theme: every [`Token`] and its color.
#[derive(Clone)]
pub struct Theme {
    props: ThemeProps,
    /// The semantic slots holding a shade of the accent.
    accent: HashSet<SemanticToken>,
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
    /// Feathers' dark theme with the kit's tokens added: the start of an app's own theme.
    pub fn new() -> Self {
        let props = create_dark_theme();
        // Feathers' own shades of its accent, found by their hue before anything else is painted.
        let accent = props.semantic_base.iter().filter(|(_, c)| is_accent_shade(**c)).map(|(s, _)| s.clone()).collect();
        let mut theme = Self { props, accent };
        theme.paint(&Token(tokens::BUTTON_BG_HOVER), feathers_palette::GRAY_3.lighter(0.14));
        theme.paint_accent(&Token(tokens::BUTTON_BG_PRESSED), feathers_palette::ACCENT.darker(0.12));
        theme.paint_accent(&Token(tokens::BUTTON_PRIMARY_BG_HOVER), feathers_palette::ACCENT.lighter(0.08));
        // Plain buttons (icon buttons on cards and lists) hover like normal ones: feathers' plain hover is a shade of
        // the surface they sit on, so on a card it can't be seen.
        theme.paint(&token::BUTTON_PLAIN_BG_HOVER, feathers_palette::GRAY_3.lighter(0.14));
        theme.paint_accent(&Token(tokens::BUTTON_PLAIN_BG_PRESSED), feathers_palette::ACCENT.darker(0.12));

        theme.paint(&token::PANEL_BG, palette::GRAY_1);
        theme.paint(&token::DIVIDER, palette::GRAY_3);
        theme.paint(&token::BADGE_BG, palette::GRAY_3);
        theme.paint(&token::HINT_BG, palette::GRAY_3.darker(0.05));
        theme.paint(&token::BUBBLE_BG, palette::GRAY_2);
        theme.paint_accent(&token::BUBBLE_MINE_BG, palette::ACCENT);

        theme.paint(&token::WARN, Color::oklch(0.80, 0.14, 75.0));
        theme.paint(&token::DANGER, Color::oklch(0.68, 0.17, 25.0));
        theme.paint(&token::OK, Color::oklch(0.75, 0.14, 150.0));
        theme.paint(&token::INFO, Color::srgb(0.45, 0.68, 0.95));
        theme.paint(&token::SKELETON, palette::GRAY_3);
        theme
    }

    /// Give `token` a color of its own, on a semantic slot named after it, so changing it repaints nothing else.
    pub fn paint(&mut self, token: &Token, color: Color) {
        let own = self.own_slot(&token.0, color);
        self.accent.remove(&own);
    }

    /// [`Theme::paint`] a shade of [`palette::ACCENT`] (the accent itself, or a step lighter or darker), which takes
    /// the desktop's accent along with feathers' own.
    pub fn paint_accent(&mut self, token: &Token, color: Color) {
        let own = self.own_slot(&token.0, color);
        self.accent.insert(own);
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

    /// Whether `token` is in the theme, so a test can check every token resolves.
    pub fn has(&self, token: &Token) -> bool {
        self.props.token_assignments.contains_key(&token.0)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kit_token_resolves_rather_than_falling_back_to_fuchsia() {
        let theme = Theme::new();
        let all = [
            token::PANEL_BG,
            token::DIVIDER,
            token::BADGE_BG,
            token::HINT_BG,
            token::BUBBLE_BG,
            token::BUBBLE_MINE_BG,
            token::WARN,
            token::DANGER,
            token::OK,
            token::INFO,
            token::SKELETON,
            token::TEXT,
            token::TEXT_DIM,
            token::BUTTON_TEXT,
            token::BUTTON_BG,
            token::WINDOW_BG,
            token::BUTTON_PLAIN_BG_HOVER,
        ];
        for t in all {
            assert!(theme.has(&t), "{t} isn't in the theme");
        }
    }

    #[test]
    fn an_app_paints_its_own_token_on_top_of_the_kits() {
        const MINE: Token = Token::new("app.mine");
        let mut theme = Theme::new();
        theme.paint(&MINE, Color::WHITE);
        assert_eq!(theme.color(&MINE), Color::WHITE);
        assert_eq!(theme.resource().color(&MINE.0), Color::WHITE);
    }

    fn lightness(color: Color) -> f32 {
        Oklcha::from(color).lightness
    }

    #[test]
    fn the_light_theme_turns_the_chrome_over_and_keeps_text_readable() {
        let (dark, light) = (Theme::new(), Theme::new().light());
        for surface in [Token(tokens::WINDOW_BG), token::PANEL_BG] {
            assert!(lightness(dark.color(&surface)) < 0.5 && lightness(light.color(&surface)) > 0.5, "{surface}");
        }
        assert!(lightness(light.color(&token::TEXT)) < 0.5);
        let gap = lightness(light.color(&token::TEXT)) - lightness(light.color(&token::PANEL_BG));
        assert!(gap.abs() > 0.3, "text has to stay readable: {gap}");
    }

    #[test]
    fn text_on_a_background_that_moves_moves_with_it_and_a_buttons_label_stays() {
        let light = Theme::new().light();
        for text in [tokens::MENUITEM_TEXT, tokens::TEXT_INPUT_TEXT] {
            assert!(lightness(light.color(&Token(text.clone()))) < 0.5, "{text} stayed pale over a pale background");
        }
        assert!(lightness(light.color(&token::BUTTON_TEXT)) > 0.9);
    }

    #[test]
    fn colors_that_mean_something_keep_their_hue_and_darken_in_the_light_theme() {
        let (dark, light) = (Theme::new(), Theme::new().light());
        for tone in [token::WARN, token::DANGER, token::OK, token::INFO] {
            let (d, l) = (Oklcha::from(dark.color(&tone)), Oklcha::from(light.color(&tone)));
            assert!((d.hue - l.hue).abs() < 1.0 && l.lightness < d.lightness, "{tone}");
        }
        // The accent says "this one" in either theme.
        assert_eq!(light.color(&Token(tokens::BUTTON_PRIMARY_BG)), feathers_palette::ACCENT);
        assert_eq!(light.color(&token::BUBBLE_MINE_BG), palette::ACCENT);
    }

    #[test]
    fn an_accent_reaches_every_shade_of_feathers_own_and_the_kits_and_nothing_else() {
        let purple = Color::srgb(0.568627, 0.254902, 0.67451);
        let (plain, theme) = (Theme::new(), Theme::new().with_accent(purple));
        let hue = |color: Color| Oklcha::from(color).hue;
        for t in [tokens::BUTTON_PRIMARY_BG, tokens::BUTTON_PRIMARY_BG_HOVER, tokens::CHECKBOX_BG_CHECKED] {
            assert!((hue(theme.color(&Token(t.clone()))) - hue(purple)).abs() < 1.0, "{t}");
        }
        assert!((hue(theme.color(&token::BUBBLE_MINE_BG)) - hue(purple)).abs() < 1.0);
        for t in [token::PANEL_BG, token::TEXT, token::BUTTON_BG, token::INFO, token::OK] {
            assert_eq!(theme.color(&t), plain.color(&t), "{t}");
        }
        let primary = |t: ThemeToken| lightness(theme.color(&Token(t)));
        assert!(primary(tokens::BUTTON_PRIMARY_BG_HOVER) > primary(tokens::BUTTON_PRIMARY_BG), "hover still lifts");
    }

    #[test]
    fn a_pale_accent_takes_dark_text() {
        let on_accent = |accent| lightness(Theme::new().with_accent(accent).color(&Token(tokens::BUTTON_PRIMARY_TEXT)));
        assert!(on_accent(Color::srgb_u8(255, 204, 0)) < 0.2);
        assert!(on_accent(Color::srgb_u8(0, 122, 255)) > 0.95);
    }

    #[test]
    fn an_apps_token_painted_after_a_neutral_one_turns_over_too() {
        const MINE: Token = Token::new("app.mine");
        let mut theme = Theme::new();
        theme.paint(&MINE, palette::GRAY_0);
        assert!(lightness(theme.light().color(&MINE)) > 0.5);
    }

    #[test]
    fn a_tone_component_puts_feathers_on_the_node_and_follows_a_new_token() {
        let mut world = World::new();
        let node = world.spawn((BackgroundTone(token::OK), BorderTone(token::DIVIDER), TextTone(token::WARN))).id();
        world.flush();
        assert!(world.get::<ThemeBackgroundColor>(node).unwrap().0 == token::OK.0);
        assert!(world.get::<ThemeBorderColor>(node).unwrap().0 == token::DIVIDER.0);
        assert!(world.get::<ThemeTextColor>(node).unwrap().0 == token::WARN.0);
        world.entity_mut(node).insert(BackgroundTone(token::DANGER));
        world.flush();
        assert!(world.get::<ThemeBackgroundColor>(node).unwrap().0 == token::DANGER.0);
    }
}
