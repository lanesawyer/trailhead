//! The run a connector's `main` hands itself to: what the server gives it (its settings, secrets, state file, the
//! server's address and a token), the sync it runs, and the feeds it hands back.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::Value;
use trailhead_model::connector::{FeedSet, HandedOver, env};

use crate::{FeedRecord, Http, Manifest, Secrets, Settings, State};

/// What a connector's sync fails with: anything that can be said in words.
pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// What a run gives the sync: its settings and secrets, the host-checked [`Http`], its saved [`State`], and the way to
/// hand a feed over.
pub struct Cx {
    manifest: Arc<Manifest>,
    settings: Settings,
    secrets: Secrets,
    http: Http,
    state: Arc<Mutex<State>>,
    sink: Sink,
}

/// Where handed-over feeds go: the server, or a test's list.
#[derive(Clone)]
pub(crate) enum Sink {
    Server(ServerClient),
    Collect(Arc<Mutex<BTreeMap<String, Vec<FeedRecord>>>>),
}

impl Cx {
    pub(crate) fn new(
        manifest: Arc<Manifest>,
        settings: Settings,
        secrets: Secrets,
        http: Http,
        state: State,
        sink: Sink,
    ) -> Cx {
        Cx { manifest, settings, secrets, http, state: Arc::new(Mutex::new(state)), sink }
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn secrets(&self) -> &Secrets {
        &self.secrets
    }

    pub fn http(&self) -> &Http {
        &self.http
    }

    /// The state saved by the last run that succeeded. Changes are saved when this run succeeds, and dropped when it
    /// fails. Don't hold it across an `.await`.
    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Hands the feed's whole current set to the server, checked against the manifest's fields first. Whatever the
    /// server holds from the feed and the set lacks is removed, so hand over every record each time.
    pub async fn hand_over(&self, feed: &str, records: Vec<FeedRecord>) -> Result<HandedOver, Error> {
        self.manifest.check_feed(feed, &records)?;
        match &self.sink {
            Sink::Server(client) => client.hand_over(&self.manifest.id, feed, records).await,
            Sink::Collect(feeds) => {
                let n = records.len();
                feeds.lock().unwrap_or_else(PoisonError::into_inner).insert(feed.to_string(), records);
                Ok(HandedOver { feed: feed.to_string(), records: n, apps: Vec::new() })
            }
        }
    }
}

/// The connector's client for the Trailhead server: it reaches only the address the server gave it, with the run's
/// token, and does nothing but hand over feeds. Separate from [`Http`], which reaches only the declared hosts.
#[derive(Clone)]
pub(crate) struct ServerClient {
    client: reqwest::Client,
    url: String,
    token: String,
}

impl ServerClient {
    fn new(url: &str, token: &str) -> Result<ServerClient, Error> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(120))
            .build()?;
        Ok(ServerClient { client, url: url.trim_end_matches('/').to_string(), token: token.to_string() })
    }

    async fn hand_over(&self, id: &str, feed: &str, records: Vec<FeedRecord>) -> Result<HandedOver, Error> {
        let url = format!("{}/v1/connectors/{id}/feeds/{feed}", self.url);
        let answer = self
            .client
            .put(&url)
            .bearer_auth(&self.token)
            .json(&FeedSet { records })
            .send()
            .await
            .map_err(|e| format!("handing over feed `{feed}`: the server didn't answer: {}", e.without_url()))?;
        let status = answer.status();
        let body = answer.bytes().await.map_err(|e| format!("handing over feed `{feed}`: {}", e.without_url()))?;
        if !status.is_success() {
            let why = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(String::from))
                .unwrap_or_else(|| String::from_utf8_lossy(&body).chars().take(300).collect());
            return Err(format!("handing over feed `{feed}`: the server refused it ({status}): {why}").into());
        }
        Ok(serde_json::from_slice(&body).map_err(|e| format!("handing over feed `{feed}`: the answer: {e}"))?)
    }
}

/// Runs a connector: reads what the server gave it from the environment, runs `sync`, saves its state if it
/// succeeded, and exits: 0 when it did, else 1 after saying why on stderr in plain words. A connector's whole `main`:
///
/// ```ignore
/// fn main() {
///     trailhead_connector::run(include_str!("../connector.toml"), |cx| async move {
///         let items: Vec<serde_json::Value> = cx.http().get("https://api.example.com/items").send().await?.json()?;
///         let records = Vec::new(); // map `items` to records
///         cx.hand_over("items", records).await?;
///         Ok(())
///     })
/// }
/// ```
pub fn run<F, Fut>(manifest: &str, sync: F) -> !
where
    F: FnOnce(Cx) -> Fut,
    Fut: Future<Output = Result<(), Error>>,
{
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(r) => r,
        Err(e) => fail(&format!("couldn't start: {e}")),
    };
    match runtime.block_on(run_with_env(manifest, |name| std::env::var(name).ok(), sync)) {
        Ok(()) => std::process::exit(0),
        Err(e) => fail(&e.to_string()),
    }
}

fn fail(why: &str) -> ! {
    eprintln!("{why}");
    std::process::exit(1)
}

/// [`run`] without the process: what the server gave it is read with `lookup` (an environment variable's value by
/// name), and the outcome comes back. The state is saved when `sync` succeeds.
pub async fn run_with_env<F, Fut>(manifest: &str, lookup: impl Fn(&str) -> Option<String>, sync: F) -> Result<(), Error>
where
    F: FnOnce(Cx) -> Fut,
    Fut: Future<Output = Result<(), Error>>,
{
    let manifest = Arc::new(Manifest::parse(manifest)?);
    let need = |name: &str| {
        lookup(name)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| format!("{name} is not set: a connector is run by the Trailhead server, which sets it"))
    };
    let settings: Value =
        serde_json::from_str(&need(env::SETTINGS)?).map_err(|e| format!("{} is not JSON: {e}", env::SETTINGS))?;
    let settings = Settings::new(&manifest, settings)?;
    let secrets = Secrets::from_lookup(&manifest, &lookup)?;
    let state = State::load(PathBuf::from(need(env::STATE)?))?;
    let server = ServerClient::new(&need(env::URL)?, &need(env::TOKEN)?)?;
    let http = Http::new(&manifest)?;
    let cx = Cx::new(manifest, settings, secrets, http, state, Sink::Server(server));
    run_sync(cx, sync).await?.save()?;
    Ok(())
}

/// Runs `sync`, and gives back the state it left when it succeeded.
pub(crate) async fn run_sync<F, Fut>(cx: Cx, sync: F) -> Result<State, Error>
where
    F: FnOnce(Cx) -> Fut,
    Fut: Future<Output = Result<(), Error>>,
{
    let state = cx.state.clone();
    sync(cx).await?;
    let state = state.lock().unwrap_or_else(PoisonError::into_inner);
    Ok(state.clone())
}
