//! The kit's `run` against a real server in this process: what a connector hands over lands in the app collection
//! filled from its feed, and its state is saved.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};
use trailhead_connector::{Cx, Error, FeedRecord, run_with_env};
use trailhead_core::app_data::Declaration;
use trailhead_core::connector::{Manifest, env};
use trailhead_core::{Ctx, Principal, RecordPage, Store, TokenOptions};
use trailhead_server::{AppState, router};

const MANIFEST: &str = r#"
id = "notes"
name = "Notes"
every = "5m"
hosts = ["notes.example.com"]
secrets = ["token"]

[settings.folder]
kind = "string"
required = true

[feeds.notes]
fields = { title = "string", words = "integer" }
"#;

async fn sync(cx: Cx) -> Result<(), Error> {
    let folder = cx.settings().string("folder")?.unwrap_or_default().to_string();
    assert_eq!(cx.secrets().get("token")?, "s3cret");
    let runs: u32 = cx.state().get("runs")?.unwrap_or(0);
    let note = |key: &str, words: u32| {
        FeedRecord::new(key, json!({"title": format!("{folder}/{key}"), "words": words}).as_object().unwrap().clone())
    };
    let handed = cx.hand_over("notes", vec![note("a", 10), note("b", 20 + runs)]).await?;
    assert_eq!(handed.apps.len(), 1);
    cx.state().set("runs", &(runs + 1))?;
    Ok(())
}

#[tokio::test]
async fn a_connectors_run_hands_its_feed_to_the_server_and_the_app_shows_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_local(&dir.path().join("t.db")).await.unwrap();
    let (ws, user) = store.bootstrap_local("lane").await.unwrap();
    let me = Ctx::user(ws.id, user.id);
    let declaration: Declaration = serde_json::from_value(json!({"collections": {
        "notes": {"from": "notes.notes", "fields": {"title": "string", "words": "integer"}}
    }}))
    .unwrap();
    store.register_app(&me, "jot", declaration).await.unwrap();
    let agent = store.connector_agent(&me, "notes").await.unwrap();
    let (_, token) =
        store.create_token_with(&me, Principal::Agent(agent.id), "run", TokenOptions::default()).await.unwrap();

    let state = AppState::new(store);
    state.connectors.insert(Manifest::parse(MANIFEST).unwrap(), None);
    let state = Arc::new(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await });

    let state_file = dir.path().join("connectors/notes/state.json");
    let vars: HashMap<String, String> = [
        (env::URL.to_string(), url),
        (env::TOKEN.to_string(), token),
        (env::SETTINGS.to_string(), json!({"folder": "inbox"}).to_string()),
        (env::STATE.to_string(), state_file.display().to_string()),
        ("TRAILHEAD_CONNECTOR_SECRET_TOKEN".to_string(), "s3cret".to_string()),
    ]
    .into();
    let lookup = |name: &str| vars.get(name).cloned();
    run_with_env(MANIFEST, lookup, sync).await.unwrap();
    run_with_env(MANIFEST, lookup, sync).await.unwrap();

    let records = state.store.list_records(&me, "jot", "notes", RecordPage::default()).await.unwrap();
    let shown: Vec<(Option<&str>, &Value, &Value)> =
        records.iter().map(|r| (r.key.as_deref(), &r.data["title"], &r.data["words"])).collect();
    assert_eq!(shown, [(Some("a"), &json!("inbox/a"), &json!(10)), (Some("b"), &json!("inbox/b"), &json!(21))]);
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&state_file).unwrap()).unwrap();
    assert_eq!(saved, json!({"runs": 2}));

    let failing = |cx: Cx| async move {
        cx.state().set("runs", &99)?;
        Err::<(), Error>("the service is down".into())
    };
    let e = run_with_env(MANIFEST, lookup, failing).await.unwrap_err();
    assert_eq!(e.to_string(), "the service is down");
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&state_file).unwrap()).unwrap();
    assert_eq!(saved, json!({"runs": 2}), "a failed run's state isn't saved");

    let refused = run_with_env(
        MANIFEST,
        |name: &str| if name == env::TOKEN { Some("th_nope".into()) } else { lookup(name) },
        sync,
    )
    .await;
    let e = refused.unwrap_err().to_string();
    assert!(e.starts_with("handing over feed `notes`: the server refused it (401"), "{e}");
}
