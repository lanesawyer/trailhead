//! The Connectors page's logic against an in-process server: the connectors it knows listed, a connector's edits
//! saved with only what changed and read back from the server, a save the server refuses said in its words with
//! what was typed kept, and Sync now held back, with the reason, for a connector that can't run.
//!
//! The test server runs no connectors (it builds no apps repo), so a connector is registered with it by hand, as the
//! server's own tests do, and none of its runs can start here.

use serde_json::json;
use trailhead_model::connector::Manifest;
use trailhead_shell::ShellPlugin;
use trailhead_shell::connector_settings::{
    ConnectorSettings, Load, LoadConnectors, SaveConnector, Saving, SyncConnector, Syncing, Typed,
};
use trailhead_shell::testing::Other;
use trailhead_shell::testing::server::{LiveShell, Server, live};
use trailhead_shell::testing::until;

const HELLO: &str = r#"
id = "hello"
name = "Hello"
every = "5m"
hosts = ["example.com"]
secrets = ["token"]

[settings.greeting]
kind = "string"
label = "Greeting"
required = true

[settings.loud]
kind = "boolean"

[feeds.items]
fields = { title = "string" }
"#;

fn settings(shell: &LiveShell) -> &ConnectorSettings {
    shell.world().resource::<ConnectorSettings>()
}

async fn loaded(shell: &mut LiveShell) {
    shell.world_mut().trigger(LoadConnectors);
    until(shell, "the connectors read", |a| a.world().resource::<ConnectorSettings>().load == Load::Loaded).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connector_is_listed_saved_and_refused_as_the_server_says() {
    let server = Server::new().await;
    server.state.connectors.insert(Manifest::parse(HELLO).unwrap(), None);
    let mut shell = live(&server, ShellPlugin::new().with(Other)).await;
    loaded(&mut shell).await;

    let hello = settings(&shell).info("hello").expect("the server's connector is listed").clone();
    assert_eq!((hello.name.as_str(), hello.enabled, hello.running), ("Hello", false, false));
    assert_eq!(hello.secrets[0].name, "token");
    assert!(!hello.secrets[0].set);
    assert!(hello.missing.iter().any(|m| m.contains("the setting `greeting`")), "{:?}", hello.missing);
    assert_eq!(settings(&shell).runs_connectors, Some(false), "the test server builds no apps repo");
    let gate = settings(&shell).sync_gate("hello", true, None);
    assert_eq!(gate, Err("It is off: turn it on and save first.".into()));

    {
        let mut s = shell.world_mut().resource_mut::<ConnectorSettings>();
        let draft = s.drafts.get_mut("hello").unwrap();
        draft.enabled = true;
        draft.values.insert("greeting".into(), Typed::Text("hi".into()));
    }
    shell.world_mut().trigger(SaveConnector("hello".into()));
    until(&mut shell, "the save answered", |a| !a.world().resource::<ConnectorSettings>().save_state("hello").saving)
        .await;
    assert_eq!(settings(&shell).save_state("hello"), Saving { saved: true, ..Default::default() });
    let held = server.client().show_connector("hello").await.unwrap();
    assert!(held.enabled);
    assert_eq!(held.settings.iter().find(|s| s.name == "greeting").unwrap().value, Some(json!("hi")));
    assert_eq!(settings(&shell).info("hello").unwrap(), &held, "the page shows what the server answered");
    assert!(!held.missing.iter().any(|m| m.contains("greeting")));
    let gate = settings(&shell).sync_gate("hello", true, None);
    assert_eq!(gate, Err("It can't run until it has what it needs.".into()), "its secret and a build are missing");
    shell.world_mut().trigger(SyncConnector("hello".into()));
    shell.update();
    assert_eq!(settings(&shell).sync_state("hello"), Syncing::Idle, "nothing is asked of a connector that can't run");

    // The connector's manifest changes under the page: `loud` is no longer declared, which the server refuses.
    server
        .state
        .connectors
        .insert(Manifest::parse(&HELLO.replace("[settings.loud]\nkind = \"boolean\"\n", "")).unwrap(), None);
    shell
        .world_mut()
        .resource_mut::<ConnectorSettings>()
        .drafts
        .get_mut("hello")
        .unwrap()
        .values
        .insert("loud".into(), Typed::Switch(true));
    shell.world_mut().trigger(SaveConnector("hello".into()));
    until(&mut shell, "the save refused", |a| {
        a.world().resource::<ConnectorSettings>().save_state("hello").refusal.is_some()
    })
    .await;
    let refusal = settings(&shell).save_state("hello").refusal.unwrap();
    assert_eq!(refusal, "invalid input: hello: `loud` is not a setting this connector declares");
    assert_eq!(settings(&shell).drafts["hello"].values["loud"], Typed::Switch(true), "what was typed stays");
}
