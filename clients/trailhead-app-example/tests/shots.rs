//! The example app drawn to PNGs, one for each view and theme, named `<app>-<view>-<theme>`: the reference an app's
//! shots test is copied from. `#[ignore]`d, since it needs a render adapter: `mise run shots` runs every app's, and
//! says where the pictures went.

use trailhead_app_example::{Example, Press};
use trailhead_shell::ShellPlugin;
use trailhead_shell::testing::shot::{ShotApp, ShotOptions, ShotTheme, shot};
use trailhead_shell::testing::{Other, settle};

/// Saves the view as it is now, and fails on a frame with nothing drawn in it.
fn save(shot: &mut ShotApp, view: &str) {
    let picture = shot.picture();
    let name = format!("example-{view}-{}", shot.options().theme.word());
    assert_eq!(picture.size(), shot.options().pixels(), "{name} is the size asked for");
    assert!(picture.colors() > 16, "{name} has only {} colors: nothing was drawn", picture.colors());
    println!("{}", picture.save(&name).display());
}

#[test]
#[ignore = "needs a render adapter: run with `mise run shots`"]
fn shots() {
    // With a second app, so the rail shows as it does in a shell holding more than one.
    let shell = ShellPlugin::new().with(Example).with(Other);
    let mut shot = shot(shell, ShotOptions { size: (800, 480), ..ShotOptions::default() });
    for _ in 0..3 {
        shot.world_mut().trigger(Press);
    }
    settle(&mut shot);
    save(&mut shot, "page");
    shot.set_theme(ShotTheme::Light);
    save(&mut shot, "page");
}
