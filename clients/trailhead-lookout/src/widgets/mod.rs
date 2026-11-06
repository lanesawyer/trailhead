//! Lookout's component kit: every control and surface the views are built from, one widget to a file.
//!
//! The rules (use a widget or grow one, theme tokens, named sizes and spacing, icons, layers) are in the `lookout-ui`
//! skill and `docs/factory/ui.md#look-and-components`. Much of this is ported from Lane's bevy-data-explorer. A
//! widget never imports a view module.
//!
//! Which widget for what:
//! - A button with words (and maybe an icon): [`button`]. Icon only, with a hint: [`tool_button`].
//! - A button that opens a menu: [`icon_menu`], then fill its popup; [`close_menu_holding`] when an item is done.
//! - A choice from a fixed set: [`select`], and observe [`Picked`].
//! - Text: [`text`], [`text_dim`], [`text_toned`] at a [`size`]. A button's caption: [`button_text`].
//! - An icon in text: [`icon_text`]; as a button's caption: [`button_icon`].
//! - A chip of small text: [`badge`]. A task's card: [`card`], with [`card_title`], [`card_under`] and [`card_row`].
//! - A panel over the board: [`panel`], with a [`layer`]. Inside it: [`section`], [`row`], [`field`].
//! - A dot and a few words of state: [`chip`].
//! - A hint over anything: put a [`Hint`] on it.
//! - Colors: [`token`] (Lookout's) and `bevy::feathers::tokens` (the controls'); a status column's: [`status_token`].
//! - An area that scrolls both ways (Shift + wheel goes sideways): [`ScrollBoth`]; one that scrolls down only,
//!   inside it: [`ScrollDown`].
//! - A scrollbar for a scrolling area: [`scrollbar`], vertical or horizontal; it hides when the area fits.
//! - A board column: [`column_shell`], [`column_header`], [`column_frame`], [`column_body`], [`column_scrollbar`]; [`column_empty`] for none.
//! - Children that follow a list of keyed view models without respawning: [`Keyed`].
//! - Gaps, padding, margins: [`space`]. Corners: [`radius`].

use bevy::prelude::*;

mod badge;
mod buttons;
mod card;
mod chip;
mod column;
mod containers;
mod hint;
mod icons;
mod keyed;
pub mod layer;
mod menu;
mod scroll;
mod scrollbar;
mod select;
mod spacing;
mod text;
mod theme;

pub use badge::badge;
pub use buttons::{button, button_text, tool_button};
pub use card::{TITLE_LINES, card, card_row, card_title, card_under};
pub use chip::chip;
pub use column::{
    COLUMN_GUTTER, COLUMN_WIDTH, ColumnCount, column_body, column_empty, column_frame, column_header, column_scrollbar,
    column_shell,
};
pub use containers::{field, panel, row, section};
pub use hint::Hint;
pub use icons::{Icon, button_icon, icon_text};
pub use keyed::Keyed;
pub use menu::{MENU_WIDTH, Menu, close_menu_holding, icon_menu};
pub use scroll::{ScrollBoth, ScrollDown};
pub use scrollbar::{SCROLLBAR_WIDTH, scrollbar};
pub use select::{Picked, select};
pub use spacing::{radius, space};
pub use text::{size, text, text_dim, text_toned};
pub use theme::{props as theme_props, status_token, theme, token};

/// What the widgets need at run time: the embedded icon font, and the hint.
pub struct WidgetsPlugin;

impl Plugin for WidgetsPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((icons::IconsPlugin, hint::HintPlugin))
            .add_observer(scroll::on_scroll)
            .add_systems(PostUpdate, scrollbar::hide_when_fits.after(bevy::ui::UiSystems::Layout));
    }
}
