//! The Trailhead UI kit: every control and surface an app's views are built from, one widget to a file. It knows
//! nothing of Trailhead; Lookout, the factory's Bevy app, is its first user.
//!
//! The rules (use a widget or grow one, theme tokens, named sizes and spacing, icons, layers) are in the `trailhead-ui`
//! skill and `docs/factory/ui.md#look-and-components`. Much of this is ported from Lane's bevy-data-explorer. A
//! widget never imports a view module.
//!
//! A widget comes in one of three shapes, by what the caller does with it:
//! - A scene, `fn x(..) -> impl Scene`, spawned with `spawn_scene` or placed with `@x(..)`: most widgets.
//! - `fn spawn_x(commands, ..) -> XParts`, when the caller fills or keeps hold of what's inside (an accordion's body, a
//!   menu's popup, a detail modal's panes). It spawns unparented; put its root where it goes.
//! - A `Node` or bundle, when the caller extends it: a node to finish (`..beside_sidebar()`), or components to spawn
//!   beside its own marker ([`scroll_body`]).
//!
//! Which widget for what:
//! - A button with words (and maybe an icon): [`button`]. Icon only, with a hint: [`tool_button`]. How much it
//!   stands out: [`ButtonKind`].
//! - A button that opens a menu: [`spawn_icon_menu`], then fill its popup with [`menu_item`]s; [`close_menu_holding`] when
//!   something else in it is done.
//! - A small chip with an icon and a word: [`badge`].
//! - A text input, one line or several: [`text_input`], inside a [`field`].
//! - A text edited where it is read (a title, a description): [`inline_text`]; a sidebar label and choice:
//!   [`inline_field`]. Observe [`InlineEdited`] / [`InlineChosen`]; the caller answers `confirm` or `fail`.
//! - A choice from a fixed set: [`select`], and observe [`Picked`].
//! - Text: [`text`], [`text_dim`], [`text_toned`] at a [`size`]. A button's caption: [`button_text`].
//! - An icon in text: [`icon_text`]; as a button's caption: [`button_icon`].
//! - A panel over the content: [`panel`], with a [`layer`]. Inside it: [`section`], [`row`], [`field`].
//! - A panel's or modal's title row with its close X: [`panel_header`]; whoever owns the panel answers the close.
//! - A dot and a few words of state: [`chip`].
//! - A form or dialog: a [`Modal`] marker type and [`open_modal`]; fill its [`ModalBody`], buttons in its [`ModalFooter`].
//! - A hint over anything: put a [`Hint`] on it.
//! - Something pressable that isn't a button (a card, a row) answering the pointer: a [`HoverTone`] on it.
//! - A message in a conversation, the reader's on the right and others' on the left: [`bubble`] on a [`Side`], headed by
//!   [`bubble_head`].
//! - An app's sidebar down the left: [`sidebar`], holding [`nav_item`]s under [`nav_heading`]s and a
//!   [`sidebar_footer`]; mark the page shown on its [`NavItem`]. Pages fill [`beside_sidebar`].
//! - Colors: a meaning from [`token`], on a node as a [`BackgroundTone`], [`BorderTone`] or [`TextTone`]. A surface (a
//!   panel, a card, a hint): a [`Surface`]. An app builds on [`Theme::new`]: [`Theme::alias`] a token of its own to a
//!   meaning, [`Theme::paint`] only colors that encode data, and adds [`ThemePlugin`] with it beside [`UiPlugin`].
//! - An area that scrolls both ways (Shift + wheel goes sideways): [`ScrollBoth`]; one that scrolls down only,
//!   inside it: [`ScrollDown`].
//! - A scrollbar for a scrolling area: [`scrollbar`], vertical or horizontal; it hides when the area fits.
//! - Children that follow a list of keyed view models without respawning: [`Keyed`].
//! - Gaps, padding, margins: [`space`]. Corners: [`radius`]. Borders: [`stroke`]. Panel widths and offsets: [`extent`].
//! - Pages picked from a row (or column) of tabs: a [`tab_strip`] holding [`tab`]s; observe
//!   `ValueChange<Option<Entity>>` on the strip and set its `SelectedTab`.
//! - A section that opens and closes: [`spawn_accordion`], a pane or a group by [`SectionLevel`]; fill its body, add
//!   controls to its header with [`spawn_header_button`].
//! - Text cut to a width with an ellipsis: [`truncate_to_width`], measured by the same estimate as [`width_of`].
//! - A number picked on a track: [`slider`]. Its value is written back for you.
//! - A search box with a hint while empty and a clear button: [`spawn_search_field`], or
//!   [`spawn_search_field_filling`] to fill a sidebar; match items with
//!   [`matches_search`]. A text field straight on a pane or menu goes in a [`field_well`] to be seen.
//! - A detail modal's body, a main column beside a sidebar of facts: [`spawn_detail_panes`], in a modal
//!   [`extent::DETAIL_WIDTH`] wide.
//! - A button that opens a URL in the browser: [`link_button`].
//! - A line saying how something went, with an icon for its [`Tone`]: [`notice`], then write a [`Notice`] onto it;
//!   an empty message hides it.
//! - Placeholder bars where content is on its way: [`skeleton`].
//! - Writing a `Node` or `Text` only when it changes: [`patch_node`], [`set_display`], [`set_text`].
//!
//! The kit's per-frame systems run in [`UiPlace`] in `Update`; order an app's own systems around it.
//!
//! Feathers, which the controls are built on, stays inside the kit: an app names only what the kit exports, so a
//! change in feathers is a change here and nowhere else.
//!
//! [`lint`] keeps an app on all this: one test runs its rules over the app's sources.

use bevy::feathers::theme::UiTheme;
use bevy::prelude::*;

mod accordion;
mod appearance;
mod badge;
mod bubble;
mod button_group;
mod buttons;
mod chip;
mod containers;
mod desktop;
mod detail;
mod field_well;
mod hint;
mod hover;
mod icons;
mod inline;
mod input;
mod keyed;
pub mod layer;
mod link;
pub mod lint;
mod menu;
mod modal;
mod nav;
mod notice;
mod panel_header;
mod patch;
mod scroll;
mod scrollbar;
mod search;
mod select;
mod skeleton;
mod slider;
mod spacing;
mod tabs;
mod text;
mod theme;
mod truncate;

pub use accordion::{
    ACCORDION_INDENT, Accordion, AccordionCaret, AccordionParts, AccordionTitle, Fold, SectionLevel, spawn_accordion,
    spawn_header_button,
};
pub use appearance::{Appearance, ThemeChoice, ThemePlugin};
pub use badge::badge;
pub use bubble::{Side, bubble, bubble_head};
pub use button_group::{ButtonGroup, button_group};
pub use buttons::{ButtonKind, button, button_text, tool_button};
pub use chip::{ChipDot, ChipEdit, ChipWords, chip, dot};
pub use containers::{field, panel, row, section, section_frame};
pub use detail::{DetailMain, DetailPanes, DetailSide, spawn_detail_panes};
pub use field_well::field_well;
pub use hint::{Hint, HintBounds};
pub use hover::HoverTone;
pub use icons::{Icon, button_icon, icon_text};
pub use inline::{
    InlineChosen, InlineEdited, InlineField, InlineMode, InlinePlugin, InlineText, inline_field, inline_text,
    inline_text_sized,
};
pub use input::{text_input, text_input_sized};
pub use keyed::Keyed;
pub use link::{ExternalLink, link_button};
pub use menu::{MENU_WIDTH, Menu, close_menu_holding, menu_item, spawn_icon_menu};
pub use modal::{
    MAX_HEIGHT_VH, Modal, ModalBody, ModalClosed, ModalFocus, ModalFooter, ModalLocked, ModalPlugin, ModalRoot,
    ModalSlots, open_modal,
};
pub use nav::{
    BesideSidebar, NavItem, SidebarEdge, SidebarWidth, beside_sidebar, nav_heading, nav_item, nav_item_noted, sidebar,
    sidebar_footer,
};
pub use notice::{Notice, Tone, notice};
pub use panel_header::{PanelTitle, PanelTools, panel_header};
pub use patch::{display, patch_node, set_display, set_text};
pub use scroll::{ScrollBoth, ScrollDown, scroll_body, scroll_frame};
pub use scrollbar::{SCROLLBAR_EDGE, SCROLLBAR_WIDTH, scrollbar};
pub use search::{
    ClearSearch, SearchField, SearchHint, matches_search, spawn_search_field, spawn_search_field_filling,
};
pub use select::{Picked, SelectEdit, SelectIcons, SelectOptions, SelectTones, select, select_iconed, select_toned};
pub use skeleton::{SkeletonBar, skeleton};
pub use slider::slider;
pub use spacing::{extent, radius, space, stroke};
pub use tabs::{TabCaption, tab, tab_strip};
pub use text::{Size, size, text, text_dim, text_mono, text_toned};
pub use theme::{BackgroundTone, BorderTone, Surface, TextTone, Theme, Token, palette, token};
pub use truncate::{truncate_to_width, width_of};

/// Where the kit's per-frame systems run in `Update`: opening and closing accordions, cutting titles and captions to
/// fit, painting tabs. An app's systems that spawn or change widgets go before it.
#[derive(SystemSet, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct UiPlace;

/// What the widgets need at run time: the embedded icon font, the hint, modals, sliders that write their value back,
/// and the systems in [`UiPlace`].
pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((icons::IconsPlugin, hint::HintPlugin, ModalPlugin, InlinePlugin))
            .init_resource::<nav::SidebarWidth>()
            .add_observer(nav::start_edge_drag)
            .add_observer(nav::drag_edge)
            .add_observer(nav::end_edge_drag)
            .add_observer(scroll::on_scroll)
            .add_observer(button_group::on_option)
            .add_observer(bevy::ui_widgets::slider_self_update)
            .add_systems(PostUpdate, scrollbar::show_scrollbars.after(bevy::ui::UiSystems::Layout))
            .add_systems(
                Update,
                (
                    accordion::sync_accordions,
                    accordion::pad_accordion_headers,
                    accordion::truncate_accordion_titles,
                    search::sync_search_hints,
                    slider::repaint_sliders.run_if(resource_exists_and_changed::<UiTheme>),
                    input::color_new_cursors.run_if(resource_exists::<UiTheme>),
                    notice::sync_notices,
                    skeleton::pulse_skeletons.run_if(resource_exists::<UiTheme>),
                    tabs::fit_tabs,
                    tabs::style_tabs,
                    nav::paint_nav_items,
                    nav::fit_to_sidebar,
                    nav::show_edge_lines,
                    button_group::paint_groups,
                    hover::paint_hover_tones,
                    buttons::dim_disabled_captions,
                    select::style_select_captions,
                    select::show_chosen_icons,
                )
                    .chain()
                    .in_set(UiPlace),
            );
    }
}
