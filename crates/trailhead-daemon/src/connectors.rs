//! This sandbox's connections, as its gateway lists them: GitHub's API through the Sprite's GitHub connector or the
//! Docker gateway's, and, on Docker, git over smart-HTTP for cloning private repos.
//!
//! The list is asked for when a run needs it, with a timeout and a few tries, and never once at startup: a gateway
//! that wasn't answering yet when the daemon started used to leave it with no code host for as long as it ran, and
//! every publish it refused became a question for a person (TRAIL-1448).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::OnceCell;
use trailhead_codehost_github::{GitHub, Identity};
use trailhead_daemon::FindRemote;
use trailhead_factory::{
    BranchUpdate, ChangedFile, Ci, CiState, CodeHost, Commit, Error, MergeOutcome, NewPr, OpenPr, PrRef, PrState,
    RateLimit, Repo, Result,
};

const TRIES: u32 = 3;
const TIMEOUT: Duration = Duration::from_secs(10);
const BETWEEN_TRIES: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub struct Gateway {
    list: String,
    http: reqwest::Client,
    between_tries: Duration,
}

impl Gateway {
    pub fn new(list: String) -> Gateway {
        Gateway { list, http: reqwest::Client::new(), between_tries: BETWEEN_TRIES }
    }

    /// The base URL of the `provider` connection, if this sandbox is granted one. A list that can't be read is
    /// `Unreachable`, which callers retry and never take for a refusal.
    async fn connection(&self, provider: &str) -> Result<Option<String>> {
        let mut tried = 0;
        loop {
            tried += 1;
            match self.read().await {
                Ok(list) => {
                    let conns = list["connections"].as_array().map(Vec::as_slice).unwrap_or_default();
                    let found = conns.iter().find(|c| c["provider"] == provider);
                    return Ok(found.and_then(|c| c["gateway_base_url"].as_str().map(str::to_string)));
                }
                Err(e) if tried < TRIES => {
                    tracing::warn!(error = %e, tried, "couldn't list this sandbox's connectors; trying again");
                    tokio::time::sleep(self.between_tries).await;
                }
                Err(e) => return Err(Error::Unreachable(format!("couldn't list this sandbox's connectors: {e}"))),
            }
        }
    }

    async fn read(&self) -> std::result::Result<serde_json::Value, reqwest::Error> {
        self.http.get(&self.list).timeout(TIMEOUT).send().await?.error_for_status()?.json().await
    }

    /// Where a clone comes from: the gateway's git connection when there is one, else github.com, anonymously.
    pub fn remote(&self) -> FindRemote {
        let gateway = self.clone();
        Arc::new(move || {
            let gateway = gateway.clone();
            Box::pin(async move {
                Ok(match gateway.connection("git").await? {
                    Some(url) => {
                        tracing::info!(%url, "cloning through the git connection");
                        url
                    }
                    None => "https://github.com".into(),
                })
            })
        })
    }
}

/// GitHub through the gateway's connector, found on first use and kept once found. Without a connector every call
/// fails with the reason, and the next call looks again.
pub struct GatewayCodeHost {
    gateway: Gateway,
    author: Identity,
    found: OnceCell<GitHub>,
}

impl GatewayCodeHost {
    pub fn new(gateway: Gateway, author: Identity) -> GatewayCodeHost {
        GatewayCodeHost { gateway, author, found: OnceCell::new() }
    }

    async fn host(&self) -> Result<&GitHub> {
        self.found
            .get_or_try_init(|| async {
                let url = self
                    .gateway
                    .connection("github")
                    .await?
                    .ok_or_else(|| Error::Forbidden("no GitHub connector is granted to this sandbox".into()))?;
                tracing::info!(%url, "using the GitHub connector");
                Ok(GitHub::gateway(url).with_author(self.author.clone()))
            })
            .await
    }
}

#[async_trait]
impl CodeHost for GatewayCodeHost {
    async fn publish(&self, repo: &Repo, start: &str, branch: &str, commits: &[Commit]) -> Result<()> {
        self.host().await?.publish(repo, start, branch, commits).await
    }
    async fn open_pr(&self, repo: &Repo, pr: NewPr) -> Result<PrRef> {
        self.host().await?.open_pr(repo, pr).await
    }
    async fn diff(&self, pr: &PrRef) -> Result<String> {
        self.host().await?.diff(pr).await
    }
    async fn comment(&self, pr: &PrRef, body: &str) -> Result<u64> {
        self.host().await?.comment(pr, body).await
    }
    async fn edit_comment(&self, pr: &PrRef, id: u64, body: &str) -> Result<()> {
        self.host().await?.edit_comment(pr, id, body).await
    }
    async fn merge(&self, pr: &PrRef, head: &str) -> Result<MergeOutcome> {
        self.host().await?.merge(pr, head).await
    }
    async fn pr_state(&self, pr: &PrRef) -> Result<PrState> {
        self.host().await?.pr_state(pr).await
    }
    async fn behind(&self, pr: &PrRef) -> Result<bool> {
        self.host().await?.behind(pr).await
    }
    async fn update_branch(&self, pr: &PrRef) -> Result<BranchUpdate> {
        self.host().await?.update_branch(pr).await
    }
    async fn carries_verdict(&self, pr: &PrRef, reviewed: &str) -> Result<bool> {
        self.host().await?.carries_verdict(pr, reviewed).await
    }
    async fn open_prs(&self, repo: &Repo, base: &str) -> Result<Vec<OpenPr>> {
        self.host().await?.open_prs(repo, base).await
    }
    async fn title(&self, pr: &PrRef) -> Result<String> {
        self.host().await?.title(pr).await
    }
    async fn files(&self, pr: &PrRef) -> Result<Vec<ChangedFile>> {
        self.host().await?.files(pr).await
    }
    async fn head(&self, pr: &PrRef) -> Result<String> {
        self.host().await?.head(pr).await
    }
    async fn ci(&self, pr: &PrRef) -> Result<Ci> {
        self.host().await?.ci(pr).await
    }
    async fn ci_at(&self, pr: &PrRef, sha: &str) -> Result<CiState> {
        self.host().await?.ci_at(pr, sha).await
    }
    async fn ci_logs(&self, pr: &PrRef, checks: &[String]) -> Result<Vec<(String, String)>> {
        self.host().await?.ci_logs(pr, checks).await
    }
    fn rate_limit(&self) -> Option<RateLimit> {
        self.found.get().and_then(CodeHost::rate_limit)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::http::StatusCode;
    use axum::routing::get;

    use super::*;

    /// A gateway whose list fails `failing` times, then answers with `list`.
    async fn gateway(failing: usize, list: serde_json::Value) -> (Gateway, Arc<AtomicUsize>) {
        let asked = Arc::new(AtomicUsize::new(0));
        let count = asked.clone();
        let app = axum::Router::new().route(
            "/list",
            get(move || {
                let n = count.fetch_add(1, Ordering::SeqCst);
                let list = list.clone();
                async move { if n < failing { Err(StatusCode::BAD_GATEWAY) } else { Ok(axum::Json(list)) } }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/list", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (Gateway { between_tries: Duration::ZERO, ..Gateway::new(url) }, asked)
    }

    fn author() -> Identity {
        Identity { name: "Trailhead Factory".into(), email: "factory@trailhead.invalid".into() }
    }

    #[tokio::test]
    async fn a_list_that_fails_at_first_is_asked_for_again() {
        let granted = serde_json::json!({ "connections": [{ "provider": "git", "gateway_base_url": "http://git" }] });
        let (gateway, asked) = gateway(2, granted).await;
        assert_eq!((gateway.remote())().await.unwrap(), "http://git");
        assert_eq!(asked.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_list_that_cant_be_read_fails_that_call_as_unreachable_and_the_next_call_looks_again() {
        let granted = serde_json::json!({ "connections": [{ "provider": "github", "gateway_base_url": "http://gh" }] });
        let (gateway, asked) = gateway(TRIES as usize, granted).await;
        let host = GatewayCodeHost::new(gateway, author());
        assert!(matches!(host.host().await, Err(Error::Unreachable(_))), "not a refusal: worth another try");
        assert!(host.host().await.is_ok(), "the gateway answers now");
        let before = asked.load(Ordering::SeqCst);
        assert!(host.host().await.is_ok());
        assert_eq!(asked.load(Ordering::SeqCst), before, "found once, it's kept");
    }

    #[tokio::test]
    async fn a_connection_is_picked_by_its_provider() {
        let list = serde_json::json!({ "connections": [
            { "provider": "github", "gateway_base_url": "http://gw/github" },
            { "provider": "git", "gateway_base_url": "http://gw/git" },
            { "provider": "other" },
        ] });
        let (gateway, _) = gateway(0, list).await;
        assert_eq!(gateway.connection("git").await.unwrap().as_deref(), Some("http://gw/git"));
        assert_eq!(gateway.connection("github").await.unwrap().as_deref(), Some("http://gw/github"));
        assert_eq!(gateway.connection("other").await.unwrap(), None);
        assert_eq!(gateway.connection("missing").await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_sandbox_with_no_connector_is_refused_and_clones_anonymously() {
        let (gateway, _) = gateway(0, serde_json::json!({ "connections": [] })).await;
        assert_eq!((gateway.remote())().await.unwrap(), "https://github.com");
        let host = GatewayCodeHost::new(gateway, author());
        assert!(matches!(host.host().await, Err(Error::Forbidden(why)) if why.contains("no GitHub connector")));
    }
}
