//! A table: a row of headings over rows of cells, each column a fixed width or the room the others leave.
//!
//! Adapted from bevy-data-explorer's table (`view/table`) for a list short enough to build whole: the same look (dim
//! headings, numbers set flush right, a hairline under each row) without what a long list of records needs there
//! (rows built only while on screen, paging, columns dragged wider, sorting). A row is pressable as a whole: it sends
//! `Activate`, and lightens under the pointer.
//!
//! Spawn [`table_head`] over a column node, then a [`table_row`] per item in it, each given one cell per column. The
//! caller keeps the rows in step with its items (with [`super::Keyed`]) and updates what is in their cells in place.

use bevy::prelude::*;
use bevy::ui_widgets::Button;

use super::{BorderTone, HoverTone, size, space, stroke, text_dim, token};

/// How wide a column is.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ColumnWidth {
    /// This many pixels: a number, a key, a bar.
    Fixed(f32),
    /// What the fixed columns leave, shared among the columns that fill: a name.
    Fill,
}

/// Which side of its column a cell's content sits on.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum CellAlign {
    #[default]
    Start,
    /// Numbers, which read from their last digit.
    End,
}

/// A column: its heading, its width and which side its cells sit on.
#[derive(Clone, PartialEq, Debug)]
pub struct TableColumn {
    pub heading: String,
    pub width: ColumnWidth,
    pub align: CellAlign,
}

impl TableColumn {
    pub fn new(heading: impl Into<String>, width: ColumnWidth, align: CellAlign) -> Self {
        TableColumn { heading: heading.into(), width, align }
    }
}

/// How a cell for `column` is laid out: its width, how it grows and shrinks, and which side its content sits on.
#[derive(Clone, Copy, PartialEq, Debug)]
struct CellLayout {
    width: Val,
    flex_grow: f32,
    flex_shrink: f32,
    justify_content: JustifyContent,
}

fn cell_layout(column: &TableColumn) -> CellLayout {
    let (width, flex_grow, flex_shrink) = match column.width {
        ColumnWidth::Fixed(wide) => (px(wide), 0.0, 0.0),
        ColumnWidth::Fill => (Val::Auto, 1.0, 1.0),
    };
    let justify_content = match column.align {
        CellAlign::Start => JustifyContent::FlexStart,
        CellAlign::End => JustifyContent::FlexEnd,
    };
    CellLayout { width, flex_grow, flex_shrink, justify_content }
}

/// A cell for `column`, holding `content`.
fn cell(column: &TableColumn, content: Box<dyn SceneList>) -> Box<dyn SceneList> {
    let CellLayout { width, flex_grow, flex_shrink, justify_content } = cell_layout(column);
    Box::new(bsn_list! {
        Node {
            width: { width },
            flex_grow: { flex_grow },
            flex_shrink: { flex_shrink },
            min_width: px(0),
            overflow: Overflow::clip_x(),
            align_items: AlignItems::Center,
            justify_content: { justify_content },
            column_gap: { px(space::ICON_LABEL) },
        }
        Children [ { content } ]
    })
}

/// What every row, the headings' included, is laid out as: a line of cells with a rule under it.
fn row_frame() -> impl Scene {
    bsn! {
        Node {
            width: percent(100),
            align_items: AlignItems::Center,
            column_gap: { px(space::GROUPS) },
            padding: { UiRect::axes(px(space::CONTROL_INSET), px(space::LABEL_INSET)) },
            border: { UiRect::bottom(px(stroke::HAIRLINE)) },
        }
        BorderTone({ token::BORDER })
    }
}

/// The headings over `columns`, dim, with a rule under them.
pub fn table_head(columns: &[TableColumn]) -> impl Scene {
    let cells: Vec<Box<dyn SceneList>> = columns
        .iter()
        .map(|column| {
            let heading = column.heading.clone();
            cell(column, Box::new(bsn_list! { @text_dim(heading, size::SECONDARY) }))
        })
        .collect();
    bsn! {
        @row_frame()
        Children [ { cells } ]
    }
}

/// A row of `cells`, one per column of `columns` and in their order, pressable as a whole. A table sits on the window,
/// so the row rests in the window's color.
pub fn table_row(columns: &[TableColumn], cells: Vec<Box<dyn SceneList>>) -> impl Scene {
    let cells: Vec<Box<dyn SceneList>> =
        columns.iter().zip(cells).map(|(column, content)| cell(column, content)).collect();
    bsn! {
        @row_frame()
        Button
        HoverTone { rest: { token::SURFACE_WINDOW }, hover: { token::FILL_ITEM_HOVER } }
        Children [ { cells } ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fixed_column_holds_its_width_and_a_filling_one_takes_the_rest() {
        let fixed = cell_layout(&TableColumn::new("Open", ColumnWidth::Fixed(60.0), CellAlign::End));
        assert_eq!((fixed.width, fixed.flex_grow, fixed.flex_shrink), (px(60), 0.0, 0.0));
        assert_eq!(fixed.justify_content, JustifyContent::FlexEnd, "numbers sit flush right");
        let fill = cell_layout(&TableColumn::new("Name", ColumnWidth::Fill, CellAlign::Start));
        assert_eq!((fill.width, fill.flex_grow), (Val::Auto, 1.0));
    }
}
