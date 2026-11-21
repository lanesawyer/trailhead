# Lookout

Lookout is the Trailhead factory's Bevy client (see `docs/factory/ui.md`), an app in the Trailhead shell (`clients/trailhead-shell`, see `docs/app-platform.md`). This crate is a library: the window is the shell's, and `trailhead-desktop` (`clients/trailhead-desktop`) builds the `trailhead-shell` binary that holds Lookout. Both are workspace members but not default members, so plain `cargo build` and `cargo test` skip them. Build them with `-p`, or use `--workspace`.

```sh
cargo run -p trailhead-desktop      # opens the Trailhead window, with Lookout in it
cargo test -p trailhead-lookout     # headless, no display needed
mise run shell-install-standalone   # installs trailhead-shell with a desktop entry and icons
                                    # (with `[shell] repo` set, `mise run shell-install` adds one for `trailhead app`)
```

Linux only for now.

## Connecting

The Trailhead window reaches your server over iroh, by the factory control plane's endpoint id (printed in `trailhead factory run`'s log as `factory ready endpoint=…`). The connection is the shell's, shared by every app in the window.

1. Start `trailhead-shell`. While it isn't live, a panel along the bottom has a field for the server's endpoint id. Paste it and press Connect.
2. The control plane refuses a window it doesn't know. The panel shows this window's `trailhead factory allow <id>` command with a Copy button; run it where the factory runs.
3. The window signs in with the user token the CLI saved for that server. If there isn't one, or it's refused, a token field appears: paste a user token (not an agent's) and press Sign in.

It remembers the control plane (in `shell` under Trailhead's config directory, with its endpoint key, the theme and each app's store; `TRAILHEAD_SHELL_DIR` moves it) and saves the token where the CLI keeps its tokens, so later starts connect by themselves. You can also run `trailhead-shell <endpoint id>`, set `TRAILHEAD_SHELL_TOKEN` to sign in with a token, and set `LOOKOUT_QUEUE` for the task Lookout's new tasks go under (default: the first queue the factory reports, else `TRAIL-102`).

## System packages

Bevy's `audio`, `bevy_gilrs` and Wayland features are off, and `bevy` has `default-features = false`, so building needs no alsa, udev or wayland headers. Building needs only a Rust toolchain and a C compiler (already needed by the rest of the workspace). Tests need no display or GPU.

Running the window needs these at runtime:

- X11 (or XWayland on a Wayland desktop): `libx11`, `libxcursor`, `libxi`, `libxkbcommon-x11`. Winit loads them with `dlopen`.
- A Vulkan driver (`mesa-vulkan-drivers` on Debian and Ubuntu) for wgpu.

Wayland-native windows are off because `wayland-sys` needs `libwayland-dev` to build; turn on Bevy's `wayland` feature if you want it.
