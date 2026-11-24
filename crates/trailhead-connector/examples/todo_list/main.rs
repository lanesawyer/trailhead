//! The smallest whole connector: it reads one JSON list from the one host it declares and hands it over as the feed
//! `items`. A connector in an apps repo is laid out the same way, as `connectors/<id>/` with this file as
//! `src/main.rs`, its `connector.toml` beside `Cargo.toml`, and its recorded answers under `tests/replay/`.

use serde::Deserialize;
use serde_json::json;
use trailhead_connector::{Cx, Error, FeedRecord};

#[derive(Deserialize)]
struct Item {
    id: u64,
    title: String,
    done: bool,
    link: Option<String>,
}

fn main() {
    trailhead_connector::run(include_str!("connector.toml"), sync)
}

async fn sync(cx: Cx) -> Result<(), Error> {
    let list = cx.settings().string("list")?.unwrap_or_default();
    let url = format!("https://todos.example.com/lists/{list}/items");
    let mut request = cx.http().get(&url).bearer(cx.secrets().get("token")?);
    let etag: Option<String> = cx.state().get("etag")?;
    if let Some(etag) = &etag {
        request = request.if_none_match(etag);
    }
    let answer = request.send().await?.error_for_status()?;
    if answer.not_modified() {
        // Nothing changed since the last run, so the server already holds the whole set.
        return Ok(());
    }
    let items: Vec<Item> = answer.json()?;
    let records = items
        .into_iter()
        .map(|item| {
            let data = json!({ "title": item.title, "done": item.done, "url": item.link });
            FeedRecord::new(item.id.to_string(), data.as_object().cloned().unwrap_or_default())
        })
        .collect();
    cx.hand_over("items", records).await?;
    if let Some(etag) = answer.etag() {
        cx.state().set("etag", &etag)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use trailhead_connector::testing::Test;

    use super::*;

    fn test() -> Test {
        Test::new(include_str!("connector.toml"))
            .settings(json!({ "list": "home" }))
            .secret("token", "not-a-real-token")
            .replay_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/todo_list/replay"))
    }

    #[tokio::test]
    async fn hands_over_every_item_keyed_by_its_id() {
        let ran = test().run(sync).await.unwrap();
        let items = ran.feed("items");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].key, "1");
        assert_eq!(items[0].data["title"], "Buy milk");
        assert_eq!(items[1].data["url"], "https://todos.example.com/items/2");
        assert_eq!(ran.state.get::<String>("etag").unwrap().as_deref(), Some("\"v1\""));
    }

    #[tokio::test]
    async fn a_list_that_has_not_changed_hands_over_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state.json");
        std::fs::write(&state, r#"{"etag": "\"v1\""}"#).unwrap();
        let unchanged =
            trailhead_connector::Recorded::new("GET", "https://todos.example.com/lists/home/items", 304, "");
        let ran = test().replay(vec![unchanged]).state_file(&state).run(sync).await.unwrap();
        assert!(ran.feeds.is_empty());
    }
}
