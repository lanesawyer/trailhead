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
//! - A small chip with an icon and a word: [`badge`], changed in place with [`BadgeEdit`].
//! - A text input, one line or several: [`text_input`], inside a [`field`]. Enter in a one-line one triggers
//!   [`TextEntered`].
//! - A text edited where it is read (a title, a description): [`inline_text`]; a sidebar label and choice:
//!   [`inline_field`]. Observe [`InlineEdited`] / [`InlineChosen`]; the caller answers `confirm` or `fail`.
//! - A choice from a fixed set: [`select`], and observe [`Picked`]. A list floating under a field without moving what
//!   comes after (a search's suggestions): [`dropdown_list`], of [`dropdown_item`]s.
//! - Text: [`text`], [`text_dim`], [`text_toned`] at a [`size`]; one line cut to fit its row: [`text_truncated`]. A
//!   button's caption: [`button_text`].
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
//!   [`sidebar_footer`], with a [`sidebar_card`] above it for something to keep an eye on; mark the page shown on its
//!   [`NavItem`], and change a [`nav_item_noted`] with [`NavEdit`]. Pages fill [`beside_sidebar`].
//! - The apps of a window that holds several, down its left edge: [`rail`], holding a [`rail_item`] each and a
//!   [`rail_foot`] for what the window says about itself; an app with no sidebar fills [`beside_rail`].
//! - Colors: a meaning from [`token`], on a node as a [`BackgroundTone`], [`BorderTone`] or [`TextTone`]. A surface (a
//!   panel, a card, a hint): a [`Surface`]. An app builds on [`Theme::new`]: [`Theme::alias`] a token of its own to a
//!   meaning, [`Theme::paint`] only colors that encode data, and adds [`ThemePlugin`] with it beside [`UiPlugin`].
//! - An area that scrolls both ways (Shift + wheel goes sideways): [`ScrollBoth`]; one that scrolls down only,
//!   inside it: [`ScrollDown`].
//! - A scrollbar for a scrolling area: [`scrollbar`], vertical or horizontal; it hides when the area fits.
//! - Children that follow a list of keyed view models without respawning: [`Keyed`].
//! - A list a person puts in their own order by dragging (or Alt and an arrow): [`ReorderList`] on it,
//!   [`ReorderItem`] on each item, and observe [`Reordered`].
//! - Gaps, padding, margins: [`space`]. Corners: [`radius`]. Borders: [`stroke`]. Panel widths and offsets: [`extent`].
//! - Pages picked from a row (or column) of tabs: a [`tab_strip`] holding [`tab`]s; observe
//!   `ValueChange<Option<Entity>>` on the strip and set its `SelectedTab`. A settings modal with pages: its tabs down
//!   the side with [`spawn_settings_tabs`], under a [`tab_heading`] per group, and each page a [`settings_page`].
//! - A section that opens and closes: [`spawn_accordion`], a pane or a group by [`SectionLevel`]; fill its body, add
//!   controls to its header with [`spawn_header_button`].
//! - Text cut to a width with an ellipsis: [`truncate_to_width`], measured by the same estimate as [`width_of`]; or
//!   [`Truncated`] on a text, cut to whatever room its row leaves it.
//! - A number picked on a track: [`slider`]. Its value is written back for you.
//! - A search box with a hint while empty and a clear button: [`spawn_search_field`], or
//!   [`spawn_search_field_filling`] to fill a sidebar; match items with
//!   [`matches_search`]. A text field straight on a pane or menu goes in a [`field_well`] to be seen.
//! - A detail modal's body, a main column beside a sidebar of facts: [`spawn_detail_panes`], in a modal
//!   [`extent::DETAIL_WIDTH`] wide.
//! - A button that opens a URL in the browser: [`link_button`], or icon-only [`link_tool_button`].
//! - A line saying how something went, with an icon for its [`Tone`]: [`notice`], then write a [`Notice`] onto it;
//!   an empty message hides it. One that says something from the start: [`notice_with`].
//! - Placeholder bars where content is on its way: [`skeleton`].
//! - Rows of items under column headings, a row pressable as a whole: [`table_head`] and [`table_row`]s, by
//!   [`TableColumn`].
//! - How far along something is: [`progress_bar`]; set its [`Progress`], and its [`ProgressTone`] to recolor it. With
//!   a label, a value and a note: [`meter`], updated through its [`MeterPart`]s.
//! - A setting that is on or off: [`toggle_switch`]; observe `ValueChange<bool>` on it.
//! - A list of short values a person adds to and takes from (directories): [`value_list`]; observe [`ValuesEdited`]
//!   on it and set its [`ValueList`].
//! - A pick among things a person should see (small previews), a row of pictures with captions: [`spawn_choice_tile`]s,
//!   the one chosen marked with [`ChoiceTile`].
//! - Something that moves: [`ease_in`] and [`ease_out`] over real time, and a background faded with
//!   [`BackgroundFade`].
//! - Writing a `Node` or `Text` only when it changes: [`patch_node`], [`set_display`], [`set_text`].
//! - How big the whole UI is drawn (Ctrl and `=`, `-`, `0`): [`TextSize`], changed with [`ChangeTextSize`].
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
mod check_circle;
mod chip;
mod choice_tile;
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
#[cfg(test)]
mod layout_test;
mod link;
pub mod lint;
mod menu;
mod modal;
mod motion;
mod nav;
mod notice;
mod panel_header;
mod patch;
mod progress;
mod reorder;
mod scroll;
mod scrollbar;
mod search;
mod select;
mod settings;
mod skeleton;
mod slider;
mod spacing;
mod table;
mod tabs;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
mod text;
mod text_size;
mod theme;
mod toggle;
mod truncate;
mod value_list;

pub use accordion::{
    ACCORDION_INDENT, Accordion, AccordionCaret, AccordionParts, AccordionTitle, Fold, SectionLevel, spawn_accordion,
    spawn_header_button,
};
pub use appearance::{Appearance, ThemeChoice, ThemeColors, ThemePlugin};
pub use badge::{BadgeEdit, BadgeIcon, BadgeLabel, badge};
pub use bubble::{Side, bubble, bubble_head};
pub use button_group::{ButtonGroup, button_group};
pub use buttons::{ButtonKind, button, button_text, tool_button};
pub use check_circle::{CheckCircle, CheckMark, CheckRing, check_circle};
pub use chip::{ChipDot, ChipEdit, ChipWords, chip, dot};
pub use choice_tile::{ChoiceCaption, ChoicePicture, ChoiceTile, ChoiceTileParts, spawn_choice_tile};
pub use containers::{dropdown_item, dropdown_list, field, panel, row, section, section_frame};
pub use detail::{DetailMain, DetailPanes, DetailSide, spawn_detail_panes};
pub use field_well::field_well;
pub use hint::{Hint, HintBounds};
pub use hover::HoverTone;
pub use icons::{Icon, button_icon, icon_text};
pub use inline::{
    InlineChosen, InlineEdited, InlineField, InlineMode, InlinePlugin, InlineText, inline_field, inline_text,
    inline_text_sized,
};
pub use input::{InitialText, TextEntered, text_input, text_input_sized};
pub use keyed::Keyed;
pub use link::{ExternalLink, link_button, link_tool_button};
pub use menu::{MENU_WIDTH, Menu, close_menu_holding, menu_item, spawn_icon_menu};
pub use modal::{
    MAX_HEIGHT_VH, Modal, ModalBody, ModalClosed, ModalFocus, ModalFooter, ModalLocked, ModalPlugin, ModalRoot,
    ModalSlots, open_modal,
};
pub use motion::{BackgroundFade, ease_in, ease_out};
pub use nav::{
    BesideRail, BesideSidebar, NavEdit, NavItem, NavPart, SidebarEdge, SidebarWidth, beside_rail, beside_sidebar,
    nav_heading, nav_heading_action, nav_item, nav_item_noted, rail, rail_foot, rail_item, sidebar, sidebar_card,
    sidebar_footer,
};
pub use notice::{Notice, Tone, notice, notice_with};
pub use panel_header::{PanelTitle, PanelTools, panel_header};
pub use patch::{display, patch_node, set_display, set_text};
pub use progress::{MeterPart, Progress, ProgressTone, meter, progress_bar};
pub use reorder::{DRAG_THRESHOLD, Lifted, RELEASE_SLACK, ReorderItem, ReorderList, ReorderPlugin, Reordered};
pub use scroll::{ScrollBoth, ScrollDown, scroll_body, scroll_body_node, scroll_frame, scroll_frame_node};
pub use scrollbar::{SCROLLBAR_GAP, SCROLLBAR_INSET, SCROLLBAR_WIDTH, has_room as has_scrollbar_room, scrollbar};
pub use search::{
    ClearSearch, SearchField, SearchHint, matches_search, spawn_search_field, spawn_search_field_filling,
};
pub use select::{Picked, SelectEdit, SelectIcons, SelectOptions, SelectTones, select, select_iconed, select_toned};
pub use settings::{
    SettingNote, SettingsSection, SettingsTabs, setting, setting_toggle, settings_page, spawn_settings_section,
    spawn_settings_tabs,
};
pub use skeleton::{SkeletonBar, skeleton};
pub use slider::slider;
pub use spacing::{extent, radius, space, stroke};
pub use table::{CellAlign, ColumnWidth, TableColumn, table_head, table_row};
pub use tabs::{TabCaption, tab, tab_heading, tab_strip};
pub use text::{Size, size, text, text_dim, text_mono, text_toned, text_truncated};
pub use text_size::{ChangeTextSize, TextSize, TextSizePlugin};
pub use theme::{BackgroundTone, BorderTone, Surface, TextTone, Theme, Token, palette, token};
pub use toggle::toggle_switch;
pub use truncate::{Truncated, truncate_to_width, width_of};
pub use value_list::{
    AddValue, RemoveValue, ValueField, ValueLine, ValueLines, ValueList, ValueListEmpty, ValuesEdited, value_list,
};

/// Where the kit's per-frame systems run in `Update`: opening and closing accordions, cutting titles and captions to
/// fit, painting tabs. An app's systems that spawn or change widgets go before it.
#[derive(SystemSet, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct UiPlace;

/// What the widgets need at run time: the embedded icon font, the hint, modals, sliders that write their value back,
/// reordering by dragging, the [`TextSize`] and its keys, and the systems in [`UiPlace`].
pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<TextSizePlugin>() {
            app.add_plugins(TextSizePlugin);
        }
        app.add_plugins((icons::IconsPlugin, hint::HintPlugin, ModalPlugin, InlinePlugin, ReorderPlugin))
            .init_resource::<nav::SidebarWidth>()
            .add_observer(nav::start_edge_drag)
            .add_observer(nav::drag_edge)
            .add_observer(nav::end_edge_drag)
            .add_observer(scroll::on_scroll)
            .add_observer(button_group::on_option)
            .add_observer(bevy::ui_widgets::slider_self_update)
            .add_systems(Update, (progress::fill_progress, progress::tone_progress).in_set(UiPlace))
            .add_systems(Update, select::fit_select_captions.in_set(UiPlace).after(select::show_chosen_icons))
            .add_systems(
                PostUpdate,
                (scroll::clamp_scroll_positions, scrollbar::show_scrollbars).after(bevy::ui::UiSystems::Layout),
            )
            .add_systems(PostUpdate, motion::fade_backgrounds.after(bevy::ui::UiSystems::Prepare))
            .add_systems(
                Update,
                (
                    accordion::sync_accordions,
                    accordion::pad_accordion_headers,
                    accordion::truncate_accordion_titles,
                    search::sync_search_hints,
                    slider::repaint_sliders.run_if(resource_exists_and_changed::<UiTheme>),
                    input::color_new_cursors.run_if(resource_exists::<UiTheme>),
                    input::fill_initial_text,
                    notice::sync_notices,
                    skeleton::pulse_skeletons.run_if(resource_exists::<UiTheme>),
                    tabs::fit_tabs,
                    tabs::style_tabs,
                    nav::paint_nav_items,
                    nav::fit_to_sidebar,
                    nav::show_edge_lines,
                    (value_list::sync_value_lists, value_list::paint_value_lines, truncate::fit_truncated).chain(),
                    button_group::paint_groups,
                    (hover::paint_hover_tones, check_circle::paint_check_circles, choice_tile::paint_choice_tiles),
                    buttons::dim_disabled_captions,
                    select::style_select_captions,
                    select::show_chosen_icons,
                )
                    .chain()
                    .in_set(UiPlace),
            );
    }
}
