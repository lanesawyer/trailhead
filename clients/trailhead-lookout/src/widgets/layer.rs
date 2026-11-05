//! Which surface is over which, for both drawing and clicks.
//!
//! The board fills the window and is spawned after the overlays, so an overlay without a `GlobalZIndex` loses every
//! click to it even though it draws on top (TRAIL-776). Every overlay takes its layer from here.

/// The board and the header bar.
pub const BOARD: i32 = 0;
/// The connect panel along the bottom while not live.
pub const CONNECT: i32 = 1;
/// The inspector down the right.
pub const INSPECTOR: i32 = 2;
/// The task forms (new, edit, comment, answer).
pub const FORM: i32 = 10;
/// Menu and dropdown popups, over whatever opened them.
pub const MENU: i32 = 20;
/// The hint over a hovered button, over everything.
pub const HINT: i32 = 30;
