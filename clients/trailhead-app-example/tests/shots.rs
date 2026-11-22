//! The example app drawn to PNGs, one for each view and theme, named `<app>-<view>-<theme>` (`<app>-<view>-150-<theme>`
//! at a text size of 150%): the reference an app's
//! shots test is copied from. `#[ignore]`d, since it needs a render adapter: `mise run shots` runs every app's, and
//! says where the pictures went.

use serde_json::json;
use trailhead_app_example::notes::NOTES;
use trailhead_app_example::{COUNT_SETTINGS, EXAMPLE, Example, Press};
use trailhead_shell::ShellPlugin;
use trailhead_shell::connection::Connection;
use trailhead_shell::fixture::record_json;
use trailhead_shell::settings::OpenSettings;
use trailhead_shell::testing::shot::{ShotApp, ShotOptions, ShotTheme, shot};
use trailhead_shell::testing::{Other, fill_records, set_connection, settle};
use trailhead_ui::TextSize;

/// Saves the view as it is now, and fails on a frame with nothing drawn in it.
fn save(shot: &mut ShotApp, view: &str) {
    let picture = shot.picture();
    let name = shot.options().name("example", view);
    assert_eq!(picture.size(), shot.options().pixels(), "{name} is the size asked for");
    assert!(picture.colors() > 16, "{name} has only {} colors: nothing was drawn", picture.colors());
    println!("{}", picture.save(&name).display());
}

#[test]
#[ignore = "needs a render adapter: run with `mise run shots`"]
fn shots() {
    // With a second app, so the rail shows as it does in a shell holding more than one.
    let shell = ShellPlugin::new().with(Example).with(Other);
    let mut shot = shot(shell, ShotOptions { size: (800, 560), ..ShotOptions::default() });
    for _ in 0..3 {
        shot.world_mut().trigger(Press);
    }
    set_connection(&mut shot, Connection::Live);
    let note = |id: &str, text: &str, at: &str| {
        record_json(json!({"id": id, "data": {"text": text}, "created_at": at, "updated_at": at}))
    };
    let notes = vec![
        note("n1", "Buy chalk", "2026-10-01T00:00:00Z"),
        note(
            "n2",
            "A note long enough to be cut short with an ellipsis where its row runs out of room",
            "2026-10-01T00:00:01Z",
        ),
        note("n3", "Call the gym about the bouldering wall", "2026-10-01T00:00:02Z"),
    ];
    fill_records(&mut shot, EXAMPLE, vec![(NOTES, notes)]);
    settle(&mut shot);
    save(&mut shot, "page");
    shot.set_theme(ShotTheme::Light);
    save(&mut shot, "page");

    // Ctrl and = up to 150%: everything larger, nothing cut off or out of place.
    shot.set_options(ShotOptions { theme: ShotTheme::Dark, text_size: TextSize::nearest(150), ..shot.options() });
    save(&mut shot, "page");

    // The app's page in the shell's settings.
    shot.set_options(ShotOptions { text_size: TextSize::DEFAULT, ..shot.options() });
    shot.world_mut().trigger(OpenSettings { page: Some(COUNT_SETTINGS) });
    settle(&mut shot);
    save(&mut shot, "settings");
}
