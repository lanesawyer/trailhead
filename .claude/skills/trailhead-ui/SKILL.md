---
name: trailhead-ui
description: 'Build or change the Trailhead UI kit (clients/trailhead-ui), or build an app view from it (Lookout, the explorer, any Bevy app using the kit): use a widget or grow one, feathers controls, theme tokens, named sizes and spacing, Lucide icons, layering and input, in-place updates. Use when: adding or restyling a button, dropdown, field, panel, menu, modal or any shared widget, or anything under clients/trailhead-ui/src.'
argument-hint: 'What the widget or view does'
---

# Trailhead UI kit

The kit is the `trailhead-ui` crate, `clients/trailhead-ui/src/`: every control and surface an app's views are built
from, one widget to a file, re-exported from its `lib.rs`. It knows nothing of Trailhead. These rules keep apps built
on it consistent. Most of them come from bugs that built cleanly and failed silently. Lookout's own part (its tokens,
card, column and views) is in the `lookout-ui` skill; the design is in `docs/factory/ui.md#look-and-components`.

## 1. Use the kit, or grow it

Before you write a `Node` with a background, a button, a dropdown or a text style in a view, look in the kit.

- **The kit has it:** call it. Don't copy its insides into the view and tweak them.
- **It nearly fits:** add an option to the widget (a variant, a prop) and use that. Two similar buttons, cards or
  fields that drift apart are a bug.
- **It doesn't exist:** add a widget file to the kit, built on feathers, with a doc comment saying what it's for. Then
  use it in the view. A widget never imports a view module. A widget that is about one app's domain (Lookout's task
  card and board column) lives in that app, not the kit.
- **Layouts that repeat are widgets too.** A panel header, a section with a heading, a label-and-value row, a form's
  button bar: if more than one surface has or will have it, it's a widget in the kit, even while only one uses it.
  Composing kit pieces inline the same way in two views drifts just like two hand-made buttons (TRAIL-861). Before
  building one, check the other views for the same shape.

Much of the kit is ported from Lane's `bevy-data-explorer` (`~/Dev/bevy-data-explorer/src/widgets/`, also on GitHub at
lanesawyer/bevy-data-explorer). Porting a widget from there is better than writing a new one.

## 2. Build on feathers inside the kit, and prefer upstream

**Feathers stays in the kit.** Only `clients/trailhead-ui` imports `bevy::feathers`; an app (Lookout, the explorer)
names only what the kit exports. When a view needs something feathers has and the kit doesn't expose (a token, a
control, a component to query in a test), grow the kit: wrap it in a kit type or function and export that. Never
re-export a feathers type as it is, and never reach past the kit from an app, even in a test. The kit's API is the
seam that keeps a feathers upgrade to one crate.

`bevy_feathers` 0.20 has buttons (`ButtonVariant`: normal, primary, plain; the kit's `ButtonKind`), tool buttons, menu buttons and popups,
`FeathersSelect` for dropdowns, text inputs, checkboxes, toggles, sliders, scrollbars, list views, and panes. The kit
wraps these; it doesn't draw its own where feathers has one. Where feathers lacks something or has a bug, work around
it in one kit widget with a comment saying what upstream is missing, and prefer upstreaming the fix over growing a
private copy.

- **A fixed set of choices** (a filter, a priority, a status) is a dropdown (`FeathersSelect`), never a button that
  cycles through values.
- **A button that opens a menu** shows a chevron. One that acts when pressed doesn't.
- **One primary button per surface** ("New task", "Save"). Everything else is normal or an icon-only tool button with a
  tooltip.

## 3. Colors, sizes, spacing, icons

- **Colors are theme tokens**, a kit `Token` on a node as `BackgroundTone`, `TextTone` or `BorderTone`, never
  `Color::srgb(..)` literals. The kit's base palette is in `clients/trailhead-ui/src/theme.rs`; an app's own colors
  are `Token`s it paints onto `Theme::new()` with `Theme::paint` (from the kit's `palette` or its own colors), defined
  once where the theme is built. The tone components are immutable: insert a new one to change the token. A missing token renders fuchsia
  (TRAIL-777): if you see magenta, a token isn't in the theme.
- **Paint dark; the kit derives light and the accent.** `Theme::light` turns near-neutrals over and darkens colors
  that mean something; `Theme::with_accent` moves the accent's shades onto the desktop's. Paint a shade of
  `palette::ACCENT` with `Theme::paint_accent`, or it stays feathers' blue. A color written as a literal follows
  neither, so it stays dark in a light theme.
- **Text sizes** come from the kit's `size` scale and **gaps, padding and margins** from its `space` scale, named for
  what they're for. A test fails on a literal. If no name fits, add one with a doc comment.
- **Icons** are `Icon` variants (Lucide glyphs from the embedded font). The text font has no icon glyphs, so anything
  else shows as a box or a question mark. Adding an icon is one variant; take its codepoint from the same
  `lucide-static` release the font came from.
- **Long text is clamped or truncated** on cards and in lists. Full handoffs, questions and descriptions belong to a
  detail panel.

## 4. Layering and input

- **A full-window surface is spawned after the overlays**, so an overlay without a `GlobalZIndex` loses every click to
  it, even though it draws on top (TRAIL-776). Use the kit's layer constants: the app's main surface at the bottom,
  panels above it, then forms and menus on top.
- **A node is never smaller than its content unless you say so.** `width/height: percent(100)` loses to the content's
  size in Taffy, so a full-window panel full of cards grew past the window and nothing inside it could overflow, so
  nothing scrolled (TRAIL-836). A panel or column that should hold its children to a size and let them scroll needs
  `min_width: px(0)` / `min_height: px(0)` (and its scrolling child `overflow: scroll_*()`). `Overflow::clip()` alone
  doesn't do it.
- **Scrolling goes through the kit's `ScrollBoth` / `ScrollDown`,** whose one observer walks up from what the pointer
  is over to the nearest of each. Don't add a second wheel observer per area: a bubbling `PointerScroll` keeps naming
  the entity it was fired on, so an ancestor's handler never sees its own entity.
- **A surface is spawned once and updated in place** (TRAIL-880). Despawning a parent's children and spawning them
  again on a change is a bug, however small the surface: the dropdown you just used is despawned under you, the
  header flashes, scroll positions go. The only despawns are opening and closing the surface itself (a panel, a
  modal). On a change, set what changed: a select's chosen row and, only when its options changed, its rows
  (`SelectEdit`; never while its popup is open), a button's caption, a chip's tone and words (`ChipEdit`), a
  disabled state. Lists that gain, lose and reorder items go through `widgets::keyed`. Each app guards this with a
  test (Lookout's is in the `lookout-ui` skill).
- Presses arrive as `Activate` events (from `bevy::ui_widgets`, which feathers builds on), not `Interaction`.
- `system_clipboard` must stay in the Bevy features, or no text field can paste.

## 5. Plugins and tests

- **Logic and drawing are separate plugins.** `XPlugin` holds the state and observers and runs under `MinimalPlugins`
  in tests; `XUiPlugin` spawns and draws, and only `main.rs` adds it.
- **The app wires its own dependencies** in `main.rs` (`DefaultPlugins`, the kit's `ThemePlugin` with its `Theme`,
  which brings feathers' plugins, the kit's `UiPlugin`, winit settings). A plugin never adds `StatesPlugin` or other app-level plugins for its tests' sake; the test app adds what
  `MinimalPlugins` lacks (TRAIL-774).
- **Test the logic headless.** Pure functions that turn state into a view model carry most of the tests.

## 6. Check it

No headless test builds what `main.rs` builds, and a factory sandbox can't open a window.

- `cargo clippy -p trailhead-ui --all-targets -- -D warnings` and `cargo test -p trailhead-ui`, plus the same for the
  app that uses the change.
- With a display, run the app and look for panics and `bevy_feathers::theme: Theme color ... not found` in the log.
- When layout or input misbehaves, measure rather than guess: log `ComputedNode` `size` and `content_size` (times
  `inverse_scale_factor` for logical pixels) of the nodes involved, and what an observer receives. A temporary
  `info!(target: "probe", ..)` you remove before committing. Never drive a person's mouse to test it.
- In a factory sandbox, say in your summary which parts nobody has seen yet. Every `clients/**` PR gets a human look
  before it merges.
