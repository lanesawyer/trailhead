---
name: lookout-ui
description: 'Build or change UI in Lookout (clients/trailhead-lookout), the factory''s Bevy app, the way it is meant to look and work: shared feathers widgets used everywhere, theme tokens, named sizes and spacing, Lucide icons, layering, and how to check it. Use when: adding or restyling a button, card, column, dropdown, field, panel, menu or view in Lookout, or anything under clients/trailhead-lookout/src or clients/trailhead-ui/src.'
argument-hint: 'What the UI does, and which view it lives in'
---

# Lookout UI

The design is in `docs/factory/ui.md#look-and-components`. These rules keep the app consistent. Most of them come from
bugs that built cleanly and failed silently.

## 1. Use the kit, or grow it

Every control and surface comes from the `trailhead-ui` kit crate, `clients/trailhead-ui/src/`, one widget to a file,
re-exported from its `lib.rs`. Lookout's `src/widgets/mod.rs` re-exports the kit (so views import `crate::widgets`) and
adds the widgets only Lookout has: the task card, the board column, the status and priority tokens. Before you write a
`Node` with a background, a button, a dropdown or a text style in a view, look there.
there.

- **The kit has it:** call it. Don't copy its insides into the view and tweak them.
- **It nearly fits:** add an option to the widget (a variant, a prop) and use that. Two similar buttons, cards or
  fields that drift apart are a bug.
- **It doesn't exist:** add a widget file to the kit (`clients/trailhead-ui/src/`; Lookout's `widgets/` only if it is about tasks or the board), built on feathers, with a doc comment saying what it's for.
  Then use it in the view. A widget never imports a view module.
- **Layouts that repeat are widgets too.** A panel header, a section with a heading, a label-and-value row, a form's
  button bar: if more than one surface has or will have it, it's a widget in the kit, even while only one uses it.
  Composing kit pieces inline the same way in two views drifts just like two hand-made buttons (TRAIL-861). Before
  building one, check the other views (inspector, forms, connect panel, modal) for the same shape.

Much of the kit is ported from Lane's `bevy-data-explorer` (`~/Dev/bevy-data-explorer/src/widgets/`, also on GitHub at
lanesawyer/bevy-data-explorer). Porting a widget from there is better than writing a new one.

## 2. Build on feathers

`bevy_feathers` 0.20 has buttons (`ButtonVariant`: normal, primary), tool buttons, menu buttons and popups,
`FeathersSelect` for dropdowns, text inputs, checkboxes, toggles, sliders, scrollbars, list views, and panes. The kit
wraps these; it doesn't draw its own where feathers has one.

- **A fixed set of choices** (a filter, a priority, a status) is a dropdown (`FeathersSelect`), never a button that
  cycles through values.
- **A button that opens a menu** shows a chevron. One that acts when pressed doesn't.
- **One primary button per surface** ("New task", "Save"). Everything else is normal or an icon-only tool button with a
  tooltip.

## 3. Colors, sizes, spacing, icons

- **Colors are theme tokens** (`ThemeBackgroundColor`, `ThemeTextColor`, `ThemeBorderColor`), never `Color::srgb(..)`
  literals. The kit's base palette is in `clients/trailhead-ui/src/theme.rs`; an app's own colors (Lookout's: one per status column, priorities) are tokens it paints onto that with `trailhead_ui::paint`, defined once where
  the theme is built. A missing token renders fuchsia (TRAIL-777): if you see magenta, a token isn't in the theme.
- **Text sizes** come from the kit's `size` scale and **gaps, padding and margins** from its `space` scale, named for
  what they're for. A test fails on a literal. If no name fits, add one with a doc comment.
- **Icons** are `Icon` variants (Lucide glyphs from the embedded font). The text font has no icon glyphs, so anything
  else shows as a box or a question mark. Adding an icon is one variant; take its codepoint from the same
  `lucide-static` release the font came from.
- **Long text is clamped or truncated** on cards and in lists. Full handoffs, questions and descriptions belong to the
  inspector.

## 4. Layering and input

- **The board fills the window and is spawned after the overlays**, so an overlay without a `GlobalZIndex` loses every
  click to it, even though it draws on top (TRAIL-776). Use the kit's layer constants: board, connect panel,
  inspector, then forms and menus on top.
- **A node is never smaller than its content unless you say so.** `width/height: percent(100)` loses to the content's
  size in Taffy, so a full-window panel full of cards grew past the window and nothing inside it could overflow, so
  nothing scrolled (TRAIL-836). A panel or column that should hold its children to a size and let them scroll needs
  `min_width: px(0)` / `min_height: px(0)` (and its scrolling child `overflow: scroll_*()`). `Overflow::clip()` alone
  doesn't do it.
- **Scrolling goes through the kit's `ScrollBoth` / `ScrollDown`,** whose one observer walks up from what the pointer
  is over to the nearest of each. Don't add a second wheel observer per area: a bubbling `PointerScroll` keeps naming
  the entity it was fired on, so an ancestor's handler never sees its own entity.
- Presses arrive as feathers `Activate` events, not `Interaction`.
- `system_clipboard` must stay in the Bevy features, or no text field can paste.

## 5. Plugins and tests

- **Logic and drawing are separate plugins.** `XPlugin` holds the state and observers and runs under `MinimalPlugins`
  in tests; `XUiPlugin` spawns and draws, and only `main.rs` adds it.
- **The app wires its own dependencies** in `main.rs` (`DefaultPlugins`, `FeathersPlugins`, the theme, winit
  settings). A plugin never adds `StatesPlugin` or other app-level plugins for its tests' sake; the test app adds what
  `MinimalPlugins` lacks (TRAIL-774).
- **Test the logic headless.** Pure functions that turn the mirror into a view model (`board::build`,
  `inspector::build`) carry most of the tests.

## 6. Check it in the window

No headless test builds what `main.rs` builds, and the sandbox can't open a window. Before calling UI work done:

- `cargo clippy -p trailhead-lookout --all-targets -- -D warnings` and `cargo test -p trailhead-lookout`.
- With a display: `timeout 15 cargo run -p trailhead-lookout` and look for panics and
  `bevy_feathers::theme: Theme color ... not found` in the log.
- When layout or input misbehaves, measure rather than guess: log `ComputedNode` `size` and `content_size` (times
  `inverse_scale_factor` for logical pixels) of the nodes involved, and what an observer receives. A temporary
  `info!(target: "probe", ..)` you remove before committing. Never drive a person's mouse to test it.
- In a factory sandbox, say in your summary which parts nobody has seen yet. Every `clients/**` PR gets a human look
  before it merges.
