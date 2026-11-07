# Lookout

Lookout is the Trailhead factory's Bevy client (see `docs/factory/ui.md`). It is a workspace member but not a default member, so plain `cargo build` and `cargo test` skip it. Build it with `-p trailhead-lookout`, or use `--workspace`.

```sh
cargo run -p trailhead-lookout      # opens the window
cargo test -p trailhead-lookout     # headless, no display needed
```

Linux only for now.

## Connecting

Lookout reaches your factory over iroh, by the control plane's endpoint id (printed in `trailhead factory run`'s log as `factory ready endpoint=…`).

1. Start Lookout. While it isn't live, a panel along the bottom has a field for the factory's endpoint id. Paste it and press Connect.
2. The factory refuses a Lookout it doesn't know. The panel shows this Lookout's `trailhead factory allow <id>` command with a Copy button; run it where the factory runs.
3. Lookout signs in with the user token the CLI saved for that server. If there isn't one, or it's refused, a token field appears: paste a user token (not an agent's) and press Sign in.

Lookout remembers the factory and the token, so later starts connect by themselves. Without the window you can also run `lookout <endpoint id>`, set `LOOKOUT_TOKEN` to sign in with a token, and set `LOOKOUT_QUEUE` for the task new tasks go under (default: the first queue the factory reports, else `TRAIL-102`).

## System packages

Bevy's `audio`, `bevy_gilrs` and Wayland features are off, and `bevy` has `default-features = false`, so building needs no alsa, udev or wayland headers. Building needs only a Rust toolchain and a C compiler (already needed by the rest of the workspace). Tests need no display or GPU.

Running the window needs these at runtime:

- X11 (or XWayland on a Wayland desktop): `libx11`, `libxcursor`, `libxi`, `libxkbcommon-x11`. Winit loads them with `dlopen`.
- A Vulkan driver (`mesa-vulkan-drivers` on Debian and Ubuntu) for wgpu.

Wayland-native windows are off because `wayland-sys` needs `libwayland-dev` to build; turn on Bevy's `wayland` feature if you want it.
