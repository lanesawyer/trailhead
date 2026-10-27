//! Keeps a Sprite from pausing while the daemon works. A Sprite pauses about 30 seconds after its last inbound
//! activity, and the daemon's outbound iroh connection doesn't count, so without a hold a run is cut off mid-way.
//! Holds are tasks on the Sprite's management socket (`docs.sprites.dev/keeping-sprites-running`): a short expiry,
//! refreshed on a heartbeat and deleted when the work ends, so a crashed daemon frees the Sprite on its own.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

pub const SPRITE_API_SOCKET: &str = "/.sprite/api.sock";

const EXPIRE: &str = "5m";
const HEARTBEAT: Duration = Duration::from_secs(60);

pub struct SpriteHolds {
    socket: PathBuf,
    next: AtomicU64,
}

impl SpriteHolds {
    pub fn new(socket: impl Into<PathBuf>) -> Arc<SpriteHolds> {
        Arc::new(SpriteHolds { socket: socket.into(), next: AtomicU64::new(1) })
    }

    /// Holds the Sprite active until the guard drops.
    pub fn hold(self: &Arc<Self>) -> Hold {
        let name = format!("trailhead-daemon-{}", self.next.fetch_add(1, Ordering::Relaxed));
        let this = self.clone();
        let task = name.clone();
        let heartbeat = tokio::spawn(async move {
            loop {
                let body = format!(r#"{{"expire":"{EXPIRE}"}}"#);
                if let Err(e) = this.call("PUT", &format!("/v1/tasks/{task}"), Some(&body)).await {
                    tracing::warn!(error = %e, "couldn't hold the Sprite awake");
                }
                tokio::time::sleep(HEARTBEAT).await;
            }
        });
        Hold { holds: self.clone(), name, heartbeat }
    }

    /// One HTTP/1.1 request over the management socket; returns the status code.
    async fn call(&self, method: &str, path: &str, body: Option<&str>) -> std::io::Result<u16> {
        let mut stream = UnixStream::connect(&self.socket).await?;
        let body = body.unwrap_or("");
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: sprite\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(req.as_bytes()).await?;
        let mut res = Vec::new();
        stream.read_to_end(&mut res).await?;
        let status = String::from_utf8_lossy(&res).split_whitespace().nth(1).and_then(|s| s.parse().ok());
        status.ok_or_else(|| std::io::Error::other("no HTTP status from the Sprite API"))
    }
}

pub struct Hold {
    holds: Arc<SpriteHolds>,
    name: String,
    heartbeat: tokio::task::JoinHandle<()>,
}

impl Drop for Hold {
    fn drop(&mut self) {
        self.heartbeat.abort();
        let (holds, path) = (self.holds.clone(), format!("/v1/tasks/{}", self.name));
        tokio::spawn(async move {
            if let Err(e) = holds.call("DELETE", &path, None).await {
                tracing::warn!(error = %e, "couldn't release the Sprite hold; it expires on its own");
            }
        });
    }
}
