//! Where the control plane gets the daemon binary it uploads to sandboxes (`docs/factory/operating.md`).
//!
//! `--daemon` wins when given. Otherwise the control plane downloads the release asset that matches its own
//! version, checks it against the published SHA-256 and caches it in the data dir. A version with no release (a dev
//! build) falls back to a local build, [`LOCAL_BUILD`]. Only a missing release falls back: a
//! failed download or a checksum mismatch is an error, so a corrupted or truncated binary never reaches a sandbox.

use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

/// The target the release workflow builds the daemon for, which is what Sprites run.
pub const TARGET: &str = "x86_64-unknown-linux-musl";
/// Where `mise run factory` and `cargo build --release -p trailhead-daemon` leave a local build.
pub const LOCAL_BUILD: &str = "target/release/trailhead-daemon";
/// Where GitHub serves release assets from.
pub const GITHUB: &str = "https://github.com";

const TIMEOUT: Duration = Duration::from_secs(120);

/// Which release to fetch the daemon from.
pub struct Release {
    /// The server, without a trailing slash: [`GITHUB`], or a local one in tests.
    pub base_url: String,
    /// `owner/repo`.
    pub repo: String,
    /// The control plane's own version, like `0.2.0`.
    pub version: String,
}

impl Release {
    /// The asset's name: `trailhead-daemon-v0.2.0-x86_64-unknown-linux-musl`.
    pub fn asset(&self) -> String {
        format!("trailhead-daemon-v{}-{TARGET}", self.version)
    }

    fn url(&self, name: &str) -> String {
        format!("{}/{}/releases/download/v{}/{name}", self.base_url, self.repo, self.version)
    }
}

/// The daemon binary to upload to sandboxes. `cache_dir` is where downloads are kept.
pub async fn resolve(
    over: Option<&Path>,
    fallback: &Path,
    release: &Release,
    cache_dir: &Path,
) -> Result<Vec<u8>, String> {
    if let Some(path) = over {
        return read_local(path);
    }
    match fetch(release, cache_dir).await {
        Ok(binary) => Ok(binary),
        Err(Fetch::NoRelease) => {
            let asset = release.asset();
            tracing::warn!("no release asset {asset}; using the local build {}", fallback.display());
            read_local(fallback)
                .map_err(|e| format!("no release asset {asset} for this version, and {e}; build one or pass --daemon"))
        }
        Err(Fetch::Failed(e)) => Err(e),
    }
}

fn read_local(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("daemon binary {}: {e}", path.display()))
}

enum Fetch {
    /// The release or its asset doesn't exist (HTTP 404).
    NoRelease,
    Failed(String),
}

impl From<String> for Fetch {
    fn from(e: String) -> Self {
        Fetch::Failed(e)
    }
}

async fn fetch(release: &Release, cache_dir: &Path) -> Result<Vec<u8>, Fetch> {
    let name = release.asset();
    let (binary_path, sum_path) = (cache_dir.join(&name), cache_dir.join(format!("{name}.sha256")));
    if let (Ok(binary), Ok(sum)) = (std::fs::read(&binary_path), std::fs::read_to_string(&sum_path))
        && parse_checksum(&sum).is_ok_and(|want| digest(&binary) == want)
    {
        tracing::info!(path = %binary_path.display(), "using the cached daemon {name}");
        return Ok(binary);
    }

    let http = reqwest::Client::builder().timeout(TIMEOUT).build().map_err(|e| e.to_string())?;
    let sum = get(&http, &release.url(&format!("{name}.sha256"))).await?;
    let sum_text = String::from_utf8(sum).map_err(|_| format!("{name}.sha256 isn't text"))?;
    let want = parse_checksum(&sum_text)?;
    let binary = get(&http, &release.url(&name)).await?;
    let got = digest(&binary);
    if got != want {
        return Err(Fetch::Failed(format!(
            "{name} doesn't match its published checksum (got {got}, want {want}); refusing it"
        )));
    }

    // The binary lands first and the checksum last, so a half-written cache never passes the check above.
    let cached = std::fs::create_dir_all(cache_dir)
        .and_then(|()| {
            let tmp = cache_dir.join(format!("{name}.tmp"));
            std::fs::write(&tmp, &binary)?;
            std::fs::rename(&tmp, &binary_path)?;
            std::fs::write(&sum_path, &sum_text)
        })
        .map_err(|e| format!("{}: {e}", cache_dir.display()));
    match cached {
        Ok(()) => tracing::info!(path = %binary_path.display(), "downloaded and cached the daemon {name}"),
        Err(e) => tracing::warn!("couldn't cache the daemon: {e}"),
    }
    Ok(binary)
}

async fn get(http: &reqwest::Client, url: &str) -> Result<Vec<u8>, Fetch> {
    let response = http.get(url).send().await.map_err(|e| format!("{url}: {e}"))?;
    match response.status() {
        s if s.is_success() => Ok(response.bytes().await.map_err(|e| format!("{url}: {e}"))?.to_vec()),
        reqwest::StatusCode::NOT_FOUND => Err(Fetch::NoRelease),
        s => Err(Fetch::Failed(format!("{url}: HTTP {s}"))),
    }
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The hex digest from `sha256sum` output (`<hex>  <name>`).
fn parse_checksum(text: &str) -> Result<String, String> {
    let sum = text.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
    if sum.len() == 64 && sum.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(sum)
    } else {
        Err(format!("not a SHA-256 checksum: {:?}", text.trim()))
    }
}

/// `<data dir>/factory/daemon`, where downloads are cached.
pub fn cache_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("factory").join("daemon")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::Router;
    use axum::http::StatusCode;
    use axum::routing::get;

    use super::*;

    const BINARY: &[u8] = b"\x7fELF pretend daemon";

    /// Serves the asset and its checksum (`sum`) for 0.2.0, counting requests.
    async fn serve(sum: String, binary: &'static [u8]) -> (String, Arc<AtomicUsize>) {
        let hits = Arc::new(AtomicUsize::new(0));
        let (h1, h2) = (hits.clone(), hits.clone());
        let base = "/o/r/releases/download/v0.2.0/trailhead-daemon-v0.2.0-x86_64-unknown-linux-musl";
        let app = Router::new()
            .route(
                base,
                get(move || async move {
                    h1.fetch_add(1, Ordering::SeqCst);
                    binary
                }),
            )
            .route(
                &format!("{base}.sha256"),
                get(move || async move {
                    h2.fetch_add(1, Ordering::SeqCst);
                    sum
                }),
            )
            .fallback(|| async { StatusCode::NOT_FOUND });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, hits)
    }

    fn release(base_url: String, version: &str) -> Release {
        Release { base_url, repo: "o/r".into(), version: version.into() }
    }

    fn sha256sum(bytes: &[u8]) -> String {
        format!("{}  trailhead-daemon-v0.2.0-x86_64-unknown-linux-musl\n", digest(bytes))
    }

    #[tokio::test]
    async fn downloads_verifies_and_caches() {
        let (url, hits) = serve(sha256sum(BINARY), BINARY).await;
        let dir = tempfile::tempdir().unwrap();
        let release = release(url, "0.2.0");

        assert_eq!(resolve(None, Path::new(LOCAL_BUILD), &release, dir.path()).await.unwrap(), BINARY);
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        // The second start is a cache hit: nothing is fetched.
        assert_eq!(resolve(None, Path::new(LOCAL_BUILD), &release, dir.path()).await.unwrap(), BINARY);
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_corrupted_cache_entry_is_fetched_again() {
        let (url, hits) = serve(sha256sum(BINARY), BINARY).await;
        let dir = tempfile::tempdir().unwrap();
        let release = release(url, "0.2.0");
        resolve(None, Path::new(LOCAL_BUILD), &release, dir.path()).await.unwrap();
        std::fs::write(dir.path().join(release.asset()), b"tampered").unwrap();

        assert_eq!(resolve(None, Path::new(LOCAL_BUILD), &release, dir.path()).await.unwrap(), BINARY);
        assert_eq!(hits.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn a_checksum_mismatch_refuses_the_binary() {
        let (url, _) = serve(sha256sum(b"something else"), BINARY).await;
        let dir = tempfile::tempdir().unwrap();

        let err = resolve(None, Path::new(LOCAL_BUILD), &release(url, "0.2.0"), dir.path()).await.unwrap_err();
        assert!(err.contains("doesn't match its published checksum"), "{err}");
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none(), "nothing is cached");
    }

    #[tokio::test]
    async fn a_version_with_no_release_falls_back_to_the_local_build_or_says_so() {
        let (url, _) = serve(sha256sum(BINARY), BINARY).await;
        let dir = tempfile::tempdir().unwrap();

        let err = resolve(None, &dir.path().join("missing"), &release(url.clone(), "0.3.0-dev"), dir.path())
            .await
            .unwrap_err();
        assert!(err.contains("no release asset") && err.contains("--daemon"), "{err}");

        let local = dir.path().join("local");
        std::fs::write(&local, b"local build").unwrap();
        let cache = tempfile::tempdir().unwrap();
        assert_eq!(resolve(None, &local, &release(url, "0.3.0-dev"), cache.path()).await.unwrap(), b"local build");
        assert!(std::fs::read_dir(cache.path()).unwrap().next().is_none(), "nothing is cached");
    }

    #[tokio::test]
    async fn the_daemon_flag_overrides_the_release() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("daemon");
        std::fs::write(&local, b"local build").unwrap();

        // The server is never contacted: nothing listens on this address.
        let release = release("http://127.0.0.1:1".into(), "0.2.0");
        assert_eq!(resolve(Some(&local), Path::new(LOCAL_BUILD), &release, dir.path()).await.unwrap(), b"local build");
    }

    #[test]
    fn parses_sha256sum_output() {
        let sum = "a".repeat(64);
        assert_eq!(parse_checksum(&format!("{sum}  file\n")).unwrap(), sum);
        assert!(parse_checksum("<html>").is_err());
    }
}
