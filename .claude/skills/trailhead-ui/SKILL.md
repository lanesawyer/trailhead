---
name: trailhead-ui
description: 'Build or change the Trailhead UI kit (clients/trailhead-ui), or build an app view from it (Lookout, the explorer, any Bevy app using the kit): use a widget or grow one, feathers controls, theme tokens, named sizes and spacing, Lucide icons, layering and input, in-place updates. Use when: adding or restyling a button, dropdown, field, panel, menu, modal or any shared widget, or anything under clients/trailhead-ui/src.'
argument-hint: 'What the widget or view does'
---

# Trailhead UI kit

The kit's rules and its catalogue live in the kit itself, so that an app pinned to any revision reads the ones that
match it:

- **`clients/trailhead-ui/GUIDE.md`**: what the kit is and who changes it, every widget and how it is updated in
  place, colors, sizes, spacing, icons and layers, the rules that came from real bugs, recipes, and the lint. It is
  the source of truth.
- **`clients/trailhead-shell/GUIDE.md`**: an app's contract with the shell (`ShellApp`, `ActiveApp`, where to draw,
  the `LocalStore`, the connection, testing an app, the example app).

**Read the kit's guide before touching the kit or any view built on it**, and the shell's before touching an app's
contract with the shell. Lookout's own part (its tokens, card, column and views) is in the `lookout-ui` skill; the
design is in `docs/factory/ui.md#look-and-components`.

What follows is only about working on the kit in this repo.

## Keep the guides true

A change to the kit changes its guide in the same commit: a new or changed widget, signature, token, size, spacing
name, icon or layer goes in the catalogue and the tables, and a new rule goes in with its reason and its task. The
same holds for the shell's contract and `clients/trailhead-shell/GUIDE.md`. An agent in an apps repo has only the
guides and the code at its pin, so a guide that lags the code teaches the wrong thing.

Much of the kit is ported from Lane's `bevy-data-explorer`; on this machine it is at
`~/Dev/bevy-data-explorer/src/widgets/` (and `src/app/theme.rs`). Porting a widget from there is better than writing a
new one.

## Check it

No headless test builds what the binary builds, and a factory sandbox can't open a window.

- `cargo clippy -p trailhead-ui --all-targets -- -D warnings` and `cargo test -p trailhead-ui`, plus the same for each
  crate that uses the change (`trailhead-shell`, `trailhead-app-example`, `trailhead-lookout`, `trailhead-desktop`).
  These are not default members, so plain `cargo test` skips them; CI's `ui` job runs them.
- The kit's lint runs over the kit from `src/lint.rs`'s tests (`lint::KIT`), over Lookout from
  `clients/trailhead-lookout/tests/design_system.rs`, over the shell from `clients/trailhead-shell/tests/design_system.rs`
  and over the example app from `clients/trailhead-app-example/tests/design_system.rs`. A rule change is checked
  against all four.
- With a display, run the Trailhead shell's window: `timeout 15 cargo run -p trailhead-desktop`, and look for panics
  and `bevy_feathers::theme: Theme color ... not found` in the log.
- When layout or input misbehaves, measure rather than guess (the kit's guide, section 4, "Look before you fix"). Never
  drive a person's mouse to test it.
- In a factory sandbox, say in your summary which parts nobody has seen yet. Every `clients/**` PR gets a human look
  before it merges.
