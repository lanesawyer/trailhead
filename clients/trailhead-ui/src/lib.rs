//! The Trailhead UI kit: every control and surface an app's views are built from, one widget to a file. It knows
//! nothing of Trailhead; Lookout, the factory's Bevy app, is its first user.
//!
//! The rules (use a widget or grow one, theme tokens, named sizes and spacing, icons, layers) are in the `lookout-ui`
//! skill and `docs/factory/ui.md#look-and-components`. Much of this is ported from Lane's bevy-data-explorer. A
//! widget never imports a view module.
//!
//! Which widget for what:
//! - A button with words (and maybe an icon): [`button`]. Icon only, with a hint: [`tool_button`].
//! - A button that opens a menu: [`icon_menu`], then fill its popup; [`close_menu_holding`] when an item is done.
//! - A small chip with an icon and a word: [`badge`].
//! - A text input, one line or several: [`text_input`], inside a [`field`].
//! - A choice from a fixed set: [`select`], and observe [`Picked`].
//! - Text: [`text`], [`text_dim`], [`text_toned`] at a [`size`]. A button's caption: [`button_text`].
//! - An icon in text: [`icon_text`]; as a button's caption: [`button_icon`].
//! - A panel over the content: [`panel`], with a [`layer`]. Inside it: [`section`], [`row`], [`field`].
//! - A panel's or modal's title row with its close X: [`panel_header`]; whoever owns the panel answers the close.
//! - A dot and a few words of state: [`chip`].
//! - A form or dialog: a [`Modal`] marker type and [`open_modal`]; fill its [`ModalBody`], buttons in its [`ModalFooter`].
//! - A hint over anything: put a [`Hint`] on it.
//! - Colors: [`token`] (the kit's) and `bevy::feathers::tokens` (the controls'). An app adds its own tokens with
//!   [`paint`] on top of [`theme_props`].
//! - An area that scrolls both ways (Shift + wheel goes sideways): [`ScrollBoth`]; one that scrolls down only,
//!   inside it: [`ScrollDown`].
//! - A scrollbar for a scrolling area: [`scrollbar`], vertical or horizontal; it hides when the area fits.
//! - Children that follow a list of keyed view models without respawning: [`Keyed`].
//! - Gaps, padding, margins: [`space`]. Corners: [`radius`]. Panel widths and offsets: [`extent`].
//! - Pages picked from a row (or column) of tabs: a [`tab_strip`] holding [`tab`]s; observe
//!   `ValueChange<Option<Entity>>` on the strip and set its `SelectedTab`.
//! - A section that opens and closes: [`spawn_accordion`], a pane or a group by [`SectionLevel`]; fill its body, add
//!   controls to its header with [`spawn_header_button`].
//! - Text cut to a width with an ellipsis: [`truncate_to_width`], measured by the same estimate as [`width_of`].
//! - Writing a `Node` or `Text` only when it changes: [`patch_node`], [`set_display`], [`set_text`].
//!
//! The kit's per-frame systems run in [`UiPlace`] in `Update`; order an app's own systems around it.

use bevy::prelude::*;

mod accordion;
mod badge;
mod buttons;
mod chip;
mod containers;
mod hint;
mod icons;
mod input;
mod keyed;
pub mod layer;
mod menu;
mod modal;
mod panel_header;
mod patch;
mod scroll;
mod scrollbar;
mod select;
mod spacing;
mod tabs;
mod text;
mod theme;
mod truncate;

pub use accordion::{
    ACCORDION_INDENT, Accordion, AccordionCaret, AccordionParts, AccordionTitle, SectionLevel, spawn_accordion,
    spawn_header_button,
};
pub use badge::badge;
pub use buttons::{button, button_text, danger_button, tool_button};
pub use chip::{ChipDot, ChipEdit, ChipWords, chip, dot};
pub use containers::{field, panel, row, section};
pub use hint::Hint;
pub use icons::{Icon, button_icon, icon_text};
pub use input::text_input;
pub use keyed::Keyed;
pub use menu::{MENU_WIDTH, Menu, close_menu_holding, icon_menu};
pub use modal::{
    MAX_HEIGHT_VH, Modal, ModalBody, ModalClosed, ModalFooter, ModalLocked, ModalPlugin, ModalRoot, ModalSlots,
    open_modal,
};
pub use panel_header::panel_header;
pub use patch::{display, patch_node, set_display, set_text};
pub use scroll::{ScrollBoth, ScrollDown, scroll_body, scroll_frame};
pub use scrollbar::{SCROLLBAR_WIDTH, scrollbar};
pub use select::{Picked, SelectEdit, SelectOptions, select};
pub use spacing::{check as spacing_check, extent, radius, space};
pub use tabs::{TabCaption, fit_tabs, style_tabs, tab, tab_strip};
pub use text::{size, text, text_dim, text_toned};
pub use theme::{paint, props as theme_props, theme, token};
pub use truncate::{truncate_to_width, width_of};

/// Where the kit's per-frame systems run in `Update`: opening and closing accordions, cutting titles and captions to
/// fit, painting tabs. An app's systems that spawn or change widgets go before it.
#[derive(SystemSet, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct UiPlace;

/// What the widgets need at run time: the embedded icon font, the hint, modals, and the systems in [`UiPlace`].
pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((icons::IconsPlugin, hint::HintPlugin, ModalPlugin))
            .add_observer(scroll::on_scroll)
            .add_systems(PostUpdate, scrollbar::hide_when_fits.after(bevy::ui::UiSystems::Layout))
            .add_systems(
                Update,
                (
                    accordion::sync_accordions,
                    accordion::pad_accordion_headers,
                    accordion::truncate_accordion_titles,
                    tabs::fit_tabs,
                    tabs::style_tabs,
                )
                    .chain()
                    .in_set(UiPlace),
            );
    }
}
