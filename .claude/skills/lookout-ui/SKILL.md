---
name: lookout-ui
description: 'Build or change UI in Lookout (clients/trailhead-lookout), the factory''s Bevy app: its theme tokens, task card, board column, views and how to check them. Builds on the trailhead-ui skill, which has the kit rules. Use when: adding or changing a view, the card, a column, a status or priority token, or anything under clients/trailhead-lookout/src.'
argument-hint: 'What the UI does, and which view it lives in'
---

# Lookout UI

**Read the kit's guide first**, `clients/trailhead-ui/GUIDE.md`: the rules for the kit Lookout is built from (use a
widget or grow one, feathers, theme tokens, named sizes and spacing, icons, layers, in-place updates, plugins) and the
catalogue of its widgets. Lookout is an app in the Trailhead shell, so its contract with the shell is in
`clients/trailhead-shell/GUIDE.md`. The `trailhead-ui` skill has what is about working on the kit in this repo. This
file is only what is Lookout's own. The design is in `docs/factory/ui.md#look-and-components`.

## Where things go

- Lookout's `src/widgets/mod.rs` re-exports the kit, so views import `crate::widgets`, and adds the widgets only
  Lookout has: the task card, the board column, the page panel, the status and priority tokens. A widget that isn't
  about tasks or the board goes in the kit (`clients/trailhead-ui/src/`), not here.
- **Lookout's colors** (a background and a text tone per status, priorities, grades, the trail map's sandbox states
  and agents) are categorical tokens painted onto the kit's theme with `Theme::paint`, defined once where Lookout's
  theme is built (`widgets/theme.rs`) and applied from `ShellApp::paint`. Everything else (panels, cards, borders,
  hovers, warnings) names the kit's semantic tokens; the card is a `Surface::Raised`. A Lookout color that means what
  a kit token means is an alias (`PRIORITY_1` is `STATUS_DANGER`); `no_color_is_painted_that_a_meaning_already_has`
  fails on a repeat.
- **Lookout never imports `bevy::feathers`**, not even in a test. What it needs from feathers it gets from the kit;
  when the kit lacks it, grow the kit (the kit's guide, section 4, "Feathers stays in the kit"). The lint's
  `no-feathers` rule finds any `feathers::` or `bevy_feathers` in its code, tests or `Cargo.toml`.
- **`tests/design_system.rs`** runs the kit's lint (`trailhead_ui::lint::APP`, the kit's guide, section 6) over
  Lookout's sources, plus Lookout's own `status-label` rule (a status is shown through `status_label`). Its `ALLOWED`
  list says which file may break which rule and why.
- **Cards and columns:** a card shows key, title (clamped to two lines), small badges and icon-button actions; never a
  handoff or question body in full, which belongs to the inspector. A column that can hold hundreds of cards builds
  only what's on screen.
- **Layers:** the board fills the window and is spawned after the overlays; order is board, connect panel, inspector,
  then forms and menus on top.

## Tests

- `tests/in_place.rs` fails on a new wholesale redraw (`despawn_children`, a loop despawning `child`, a `redraw*`
  function that despawns) unless it is in its `ALLOWED` list. Each view carries a headless test that an update leaves
  every other entity the same `Entity` (see `header_tests` in `board/header.rs`).
- Pure functions that turn the mirror into a view model (`board::build`, `inspector::build`) carry most of the logic
  tests.

## Check it in the window

- `cargo clippy -p trailhead-lookout --all-targets -- -D warnings` and `cargo test -p trailhead-lookout`.
- With a display: `timeout 15 cargo run -p trailhead-desktop` (the Trailhead window, which holds Lookout) and look
  for panics and `bevy_feathers::theme: Theme color ... not found` in the log. The rest of the checking advice is in
  the `trailhead-ui` skill, "Check it", and the kit's guide, section 6.
