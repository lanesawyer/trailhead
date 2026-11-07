//! Lookout's widgets: the [`trailhead_ui`] kit, re-exported so views import from `crate::widgets` as they always
//! have, plus the two widgets only Lookout has (a task's [`card`] and a status [`column`](self#a-board-column)) and its
//! theme on top of the kit's.
//!
//! The rules (use a widget or grow one, theme tokens, named sizes and spacing, icons, layers) are in the `trailhead-ui`
//! skill and `docs/factory/ui.md#look-and-components`; which kit widget for what is in `trailhead_ui`'s docs. A widget
//! that isn't about tasks or the board belongs in the kit, not here. A widget never imports a view module.
//!
//! Lookout's own:
//! - A task's card: [`card`], with [`card_title`], [`card_under`], [`card_found`] and [`card_row`].
//! - A board column: [`column_shell`], [`column_header`], [`column_frame`], [`column_body`], [`column_scrollbar`];
//!   [`column_empty`] for none.
//! - Colors: [`token`] (the kit's and Lookout's), a status column's: [`status_token`]; a priority's: [`priority_tone`]; a grade's: [`grade_tone`].
//! - A status in words: [`status_label`]; every view shows a status through it.

mod card;
mod column;
mod theme;

pub use card::{TITLE_LINES, card, card_found, card_row, card_title, card_under};
pub use column::{
    COLUMN_WIDTH, ColumnCount, column_body, column_empty, column_frame, column_header, column_scrollbar, column_shell,
};
pub use theme::{grade_tone, priority_tone, props as theme_props, status_label, status_token, theme, token};
pub use trailhead_ui::*;
