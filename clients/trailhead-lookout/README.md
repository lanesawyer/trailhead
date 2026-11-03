# Lookout

Lookout is the Trailhead factory's Bevy client (see `docs/factory/ui.md`). It is a workspace member but not a default member, so plain `cargo build` and `cargo test` skip it. Build it with `-p trailhead-lookout`, or use `--workspace`.

```sh
cargo run -p trailhead-lookout      # opens the window
cargo test -p trailhead-lookout     # headless, no display needed
```

Linux only for now.

## System packages

Bevy's `audio`, `bevy_gilrs` and Wayland features are off, and `bevy` has `default-features = false`, so building needs no alsa, udev or wayland headers. Building needs only a Rust toolchain and a C compiler (already needed by the rest of the workspace). Tests need no display or GPU.

Running the window needs these at runtime:

- X11 (or XWayland on a Wayland desktop): `libx11`, `libxcursor`, `libxi`, `libxkbcommon-x11`. Winit loads them with `dlopen`.
- A Vulkan driver (`mesa-vulkan-drivers` on Debian and Ubuntu) for wgpu.

Wayland-native windows are off because `wayland-sys` needs `libwayland-dev` to build; turn on Bevy's `wayland` feature if you want it.
