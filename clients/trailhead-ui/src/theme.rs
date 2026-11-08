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
//! Feathers stays inside the kit: an app names [`Token`], [`Theme`], [`palette`] and the tone components here, never
//! feathers' own, so the kit is the one place that changes when feathers does.

use bevy::ecs::lifecycle::HookContext;
use bevy::ecs::world::DeferredWorld;
use bevy::feathers::FeathersPlugins;
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

    /// The window's text.
    pub const TEXT: Token = Token(tokens::TEXT_MAIN);
    /// Text secondary to what's beside it.
    pub const TEXT_DIM: Token = Token(tokens::TEXT_DIM);
    /// A button's caption.
    pub const BUTTON_TEXT: Token = Token(tokens::BUTTON_TEXT);
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
pub struct Theme(ThemeProps);

impl Theme {
    /// Feathers' dark theme with the kit's tokens added: the start of an app's own theme.
    pub fn new() -> Self {
        let mut theme = Self(create_dark_theme());
        theme.paint(&Token(tokens::BUTTON_BG_HOVER), feathers_palette::GRAY_3.lighter(0.14));
        theme.paint(&Token(tokens::BUTTON_BG_PRESSED), feathers_palette::ACCENT.darker(0.12));
        theme.paint(&Token(tokens::BUTTON_PRIMARY_BG_HOVER), feathers_palette::ACCENT.lighter(0.08));

        theme.paint(&token::PANEL_BG, palette::GRAY_1);
        theme.paint(&token::DIVIDER, palette::GRAY_3);
        theme.paint(&token::BADGE_BG, palette::GRAY_3);
        theme.paint(&token::HINT_BG, palette::GRAY_3.darker(0.05));

        theme.paint(&token::WARN, Color::oklch(0.80, 0.14, 75.0));
        theme.paint(&token::DANGER, Color::oklch(0.68, 0.17, 25.0));
        theme.paint(&token::OK, Color::oklch(0.75, 0.14, 150.0));
        theme.paint(&token::INFO, Color::srgb(0.45, 0.68, 0.95));
        theme.paint(&token::SKELETON, palette::GRAY_3);
        theme
    }

    /// Give `token` a color of its own, on a semantic slot named after it, so changing it repaints nothing else.
    pub fn paint(&mut self, token: &Token, color: Color) {
        let own = SemanticToken::new(token.0.to_string().into());
        self.0.token_assignments.insert(token.0.clone(), own.clone());
        self.0.semantic_base.insert(own, color);
    }

    /// The color `token` is painted, fuchsia when it isn't in the theme.
    pub fn color(&self, token: &Token) -> Color {
        self.0.color(&token.0)
    }

    /// Whether `token` is in the theme, so a test can check every token resolves.
    pub fn has(&self, token: &Token) -> bool {
        self.0.token_assignments.contains_key(&token.0)
    }

    pub(crate) fn resource(self) -> UiTheme {
        UiTheme(self.0)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::new()
    }
}

/// Feathers' controls and the theme they're drawn in. An app adds this and [`super::UiPlugin`]; without a theme every
/// color is missing, and feathers draws missing ones in fuchsia.
pub struct ThemePlugin(pub Theme);

impl Plugin for ThemePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(FeathersPlugins).insert_resource(self.0.clone().resource());
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
            token::WARN,
            token::DANGER,
            token::OK,
            token::INFO,
            token::SKELETON,
            token::TEXT,
            token::TEXT_DIM,
            token::BUTTON_TEXT,
            token::BUTTON_BG,
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
