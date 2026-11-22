//! Testing a connector with no server, no network and no token: its sync runs against recorded answers, and what it
//! handed over comes back.
//!
//! ```ignore
//! # async fn sync(cx: trailhead_connector::Cx) -> Result<(), trailhead_connector::Error> { Ok(()) }
//! # async fn t() {
//! use trailhead_connector::testing::Test;
//!
//! let ran = Test::new(include_str!("../connector.toml"))
//!     .settings(serde_json::json!({ "repos": ["a/b"] }))
//!     .secret("token", "not-a-real-token")
//!     .replay_dir("tests/replay")
//!     .run(sync)
//!     .await
//!     .unwrap();
//! assert_eq!(ran.feed("pull_requests").len(), 2);
//! # }
//! ```

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;

use crate::run::{Cx, Error, Sink, run_sync};
use crate::{FeedRecord, Http, Manifest, Recorded, Secrets, Settings, State};

enum Answers {
    None,
    Entries(Vec<Recorded>),
    Dir(PathBuf),
}

/// One run of a connector's sync, set up by a test.
pub struct Test {
    manifest: String,
    settings: Value,
    secrets: Vec<(String, String)>,
    answers: Answers,
    state: Option<PathBuf>,
}

/// What a test run handed over, by feed, and the state it left.
#[derive(Debug)]
pub struct Ran {
    pub feeds: BTreeMap<String, Vec<FeedRecord>>,
    pub state: State,
}

impl Ran {
    /// What the run handed over of `feed`; empty when it handed none.
    pub fn feed(&self, feed: &str) -> &[FeedRecord] {
        self.feeds.get(feed).map_or(&[], Vec::as_slice)
    }
}

impl Test {
    /// A run of the connector `connector.toml` declares, with no settings, secrets or recorded answers yet.
    pub fn new(manifest: &str) -> Test {
        Test {
            manifest: manifest.to_string(),
            settings: Value::Null,
            secrets: Vec::new(),
            answers: Answers::None,
            state: None,
        }
    }

    /// The settings, as the JSON object the server would hold.
    pub fn settings(mut self, settings: Value) -> Test {
        self.settings = settings;
        self
    }

    pub fn secret(mut self, name: &str, value: &str) -> Test {
        self.secrets.push((name.to_string(), value.to_string()));
        self
    }

    /// Answers from these recorded responses ([`Http::replay`]).
    pub fn replay(mut self, entries: Vec<Recorded>) -> Test {
        self.answers = Answers::Entries(entries);
        self
    }

    /// Answers from the `*.json` files of a directory ([`Http::replay_dir`]).
    pub fn replay_dir(mut self, dir: impl Into<PathBuf>) -> Test {
        self.answers = Answers::Dir(dir.into());
        self
    }

    /// Starts from the state saved in this file, as a run after an earlier one does. The file is never written: the
    /// state the run leaves comes back in [`Ran::state`].
    pub fn state_file(mut self, path: impl Into<PathBuf>) -> Test {
        self.state = Some(path.into());
        self
    }

    /// Runs `sync`, failing as the real run would: a setting or secret missing, a host not declared, an answer not
    /// recorded, a record that doesn't match its feed.
    pub async fn run<F, Fut>(self, sync: F) -> Result<Ran, Error>
    where
        F: FnOnce(Cx) -> Fut,
        Fut: Future<Output = Result<(), Error>>,
    {
        let manifest = Arc::new(Manifest::parse(&self.manifest)?);
        let settings = Settings::new(&manifest, self.settings)?;
        let secrets = Secrets::from_pairs(&manifest, self.secrets.iter().map(|(n, v)| (n.as_str(), v.as_str())))?;
        let http = match self.answers {
            Answers::None => Http::replay(&manifest, Vec::new())?,
            Answers::Entries(entries) => Http::replay(&manifest, entries)?,
            Answers::Dir(dir) => Http::replay_dir(&manifest, dir)?,
        };
        let state = match self.state {
            Some(path) => State::load(path)?,
            None => State::load(std::env::temp_dir().join("trailhead-connector-test-never-written.json"))?,
        };
        let feeds = Arc::new(Mutex::new(BTreeMap::new()));
        let cx = Cx::new(manifest, settings, secrets, http, state, Sink::Collect(feeds.clone()));
        let state = run_sync(cx, sync).await?;
        let feeds = std::mem::take(&mut *feeds.lock().unwrap_or_else(PoisonError::into_inner));
        Ok(Ran { feeds, state })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::fixtures::GITHUB;

    async fn sync(cx: Cx) -> Result<(), Error> {
        let repos = cx.settings().list("repos")?.unwrap_or_default();
        let token = cx.secrets().get("token")?.to_string();
        let mut records = Vec::new();
        for repo in repos {
            let url = format!("https://api.github.com/repos/{repo}/pulls");
            let pulls: Vec<Value> = cx.http().get(&url).bearer(&token).send().await?.error_for_status()?.json()?;
            for p in pulls {
                let data = json!({"title": p["title"], "url": p["html_url"], "author": p["user"]["login"],
                    "draft": p["draft"], "updated": p["updated_at"], "reviewers": []});
                records.push(FeedRecord::new(format!("{repo}#{}", p["number"]), data.as_object().unwrap().clone()));
            }
        }
        cx.state().set("seen", &records.len())?;
        cx.hand_over("pull_requests", records).await?;
        Ok(())
    }

    fn pulls() -> Recorded {
        let body = json!([{"number": 1, "title": "Fix", "html_url": "https://github.com/a/b/pull/1",
            "user": {"login": "lane"}, "draft": false, "updated_at": "2026-11-22T10:00:00Z"}]);
        Recorded::new("GET", "https://api.github.com/repos/a/b/pulls", 200, &body.to_string())
    }

    #[tokio::test]
    async fn a_sync_runs_on_replayed_answers_and_its_feeds_come_back() {
        let ran = Test::new(GITHUB)
            .settings(json!({"repos": ["a/b"]}))
            .secret("token", "t")
            .replay(vec![pulls()])
            .run(sync)
            .await
            .unwrap();
        let prs = ran.feed("pull_requests");
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0].key, "a/b#1");
        assert_eq!(prs[0].data["author"], "lane");
        assert_eq!(ran.state.get::<usize>("seen").unwrap(), Some(1));
        assert!(ran.feed("issues").is_empty());
    }

    #[tokio::test]
    async fn a_test_run_fails_as_the_real_one_would() {
        let no_secret = Test::new(GITHUB).settings(json!({"repos": ["a/b"]})).run(sync).await.unwrap_err();
        assert!(no_secret.to_string().contains("secret `token`"), "{no_secret}");
        let no_setting = Test::new(GITHUB).secret("token", "t").run(sync).await.unwrap_err();
        assert!(no_setting.to_string().contains("`repos` is required"), "{no_setting}");
        let unrecorded =
            Test::new(GITHUB).settings(json!({"repos": ["c/d"]})).secret("token", "t").replay(vec![pulls()]);
        let e = unrecorded.run(sync).await.unwrap_err().to_string();
        assert!(e.contains("no recorded answer"), "{e}");
    }

    #[tokio::test]
    async fn a_record_that_doesnt_match_its_feed_is_refused_before_it_is_sent() {
        let e = Test::new(GITHUB)
            .settings(json!({"repos": []}))
            .secret("token", "t")
            .run(async |cx: Cx| {
                let bad = FeedRecord::new("k", json!({"title": 1}).as_object().unwrap().clone());
                cx.hand_over("pull_requests", vec![bad]).await?;
                Ok(())
            })
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("feed `pull_requests`, record `k`"), "{e}");
    }
}
