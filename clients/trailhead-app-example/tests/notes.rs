//! The example app's notes against an in-process server: its `collections.toml` declared, the notes already there
//! loaded, and a note added and removed from the app reaching the server and coming back through the mirror.

use serde_json::json;
use trailhead_app_example::notes::{AddNote, NOTES, Note, RemoveNote};
use trailhead_app_example::{EXAMPLE, Example};
use trailhead_shell::ShellPlugin;
use trailhead_shell::records::Record;
use trailhead_shell::testing::server::{Server, live, records_caught_up};
use trailhead_shell::testing::until;

fn texts(app: &mut bevy::prelude::App) -> Vec<String> {
    let world = app.world_mut();
    let mut texts: Vec<String> = world.query::<&Record<Note>>().iter(world).map(|n| n.data.text.clone()).collect();
    texts.sort();
    texts
}

#[tokio::test(flavor = "multi_thread")]
async fn notes_added_and_removed_in_the_app_reach_the_server_and_come_back() {
    let server = Server::new().await;
    server.declare_app(&Example).await;
    server.client().create_record("example", NOTES, None, json!({"text": "from elsewhere"})).await.unwrap();

    let mut shell = live(&server, ShellPlugin::new().with(Example)).await;
    records_caught_up(&mut shell, &server, EXAMPLE).await;
    assert_eq!(texts(&mut shell), ["from elsewhere"]);

    shell.world_mut().trigger(AddNote("eggs".into()));
    until(&mut shell, "eggs", |a| has(a, "eggs")).await;
    let elsewhere = {
        let world = shell.world_mut();
        world.query::<&Record<Note>>().iter(world).find(|n| n.data.text == "from elsewhere").unwrap().id
    };
    shell.world_mut().trigger(RemoveNote(elsewhere));
    until(&mut shell, "removed", |a| !has(a, "from elsewhere")).await;
    assert_eq!(texts(&mut shell), ["eggs"]);

    let stored = server.client().list_records("example", NOTES, None, None).await.unwrap();
    let on_server: Vec<_> = stored.records.iter().map(|r| r.data["text"].clone()).collect();
    assert_eq!(on_server, [json!("eggs")]);
}

fn has(app: &bevy::prelude::App, text: &str) -> bool {
    app.world().iter_entities().any(|e| e.get::<Record<Note>>().is_some_and(|n| n.data.text == text))
}
