//! The control plane's side of Lookout's transport (`docs/factory/ui.md#transport`): the `trailhead/ui/0` protocol on
//! the control plane's iroh endpoint, separate from the daemons' [`API_ALPN`](crate::net::API_ALPN) tunnel.
//!
//! Each stream opens with one framed [`UiRequest`] saying what it wants. [`UiRequest::Api`] turns the stream into a
//! byte pipe to the Trailhead server's loopback port, so the UI's HTTP client (and the `/v1/events/stream` event
//! stream) talks to a local port. The factory status snapshot and stream (TRAIL-692) are further requests.
//!
//! Admission is a persisted [`UiAllowlist`] of endpoint ids a person added; it decides who can connect. What they can
//! do is decided by the user's bearer token, which the server checks on every forwarded request.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointId};
use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, TcpStream};
use trailhead_factory::{Error, Result};

use crate::net::{io_err, parse_endpoint_id, pipe, read_frame, write_frame};

pub const UI_ALPN: &[u8] = b"trailhead/ui/0";

/// The first frame on every stream a UI opens.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum UiRequest {
    /// The rest of the stream is raw bytes to and from the Trailhead server.
    Api,
}

/// Why a stream was reset: the first frame wasn't a request this control plane knows.
const BAD_REQUEST: u32 = 2;

/// The endpoint ids allowed to use `trailhead/ui/0`, one per line in a file. It is read on every connection, so
/// `trailhead-factory allow` takes effect without restarting the control plane.
#[derive(Debug, Clone)]
pub struct UiAllowlist {
    path: PathBuf,
}

impl UiAllowlist {
    pub fn new(path: PathBuf) -> UiAllowlist {
        UiAllowlist { path }
    }

    pub fn list(&self) -> Result<Vec<EndpointId>> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(parse_endpoint_id)
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(Error::Other(format!("{}: {e}", self.path.display()))),
        }
    }

    pub fn contains(&self, id: &EndpointId) -> bool {
        match self.list() {
            Ok(ids) => ids.contains(id),
            Err(e) => {
                tracing::error!(error = %e, "can't read the UI allowlist; refusing");
                false
            }
        }
    }

    /// Whether `id` was newly added.
    pub fn add(&self, id: EndpointId) -> Result<bool> {
        let mut ids = self.list()?;
        if ids.contains(&id) {
            return Ok(false);
        }
        ids.push(id);
        self.save(&ids)?;
        Ok(true)
    }

    /// Whether `id` was there to remove.
    pub fn remove(&self, id: &EndpointId) -> Result<bool> {
        let mut ids = self.list()?;
        let before = ids.len();
        ids.retain(|i| i != id);
        if ids.len() == before {
            return Ok(false);
        }
        self.save(&ids)?;
        Ok(true)
    }

    fn save(&self, ids: &[EndpointId]) -> Result<()> {
        let fail = |e: std::io::Error| Error::Other(format!("{}: {e}", self.path.display()));
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(fail)?;
        }
        let text: String = ids.iter().map(|i| format!("{i}\n")).collect();
        // Write beside, then rename, so a crash can't leave half a list.
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, text).map_err(fail)?;
        std::fs::rename(&tmp, &self.path).map_err(fail)
    }
}

/// The control plane's end of `trailhead/ui/0`.
#[derive(Debug, Clone)]
pub struct UiProtocol {
    server: SocketAddr,
    allowed: Arc<UiAllowlist>,
}

impl UiProtocol {
    pub fn new(server: SocketAddr, allowed: UiAllowlist) -> UiProtocol {
        UiProtocol { server, allowed: Arc::new(allowed) }
    }
}

impl ProtocolHandler for UiProtocol {
    async fn accept(&self, conn: Connection) -> std::result::Result<(), AcceptError> {
        if !self.allowed.contains(&conn.remote_id()) {
            tracing::warn!(peer = %conn.remote_id(), "refusing a UI connection from an endpoint that isn't allowed");
            conn.close(1u32.into(), b"not allowed");
            return Ok(());
        }
        while let Ok((mut send, mut recv)) = conn.accept_bi().await {
            let server = self.server;
            tokio::spawn(async move {
                match read_frame::<UiRequest>(&mut recv).await {
                    Ok(Some(UiRequest::Api)) => match TcpStream::connect(server).await {
                        Ok(tcp) => pipe(send, recv, tcp).await,
                        Err(e) => {
                            tracing::warn!(%server, error = %e, "can't reach the Trailhead server");
                            let _ = send.reset(BAD_REQUEST.into());
                        }
                    },
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "bad UI request");
                        let _ = send.reset(BAD_REQUEST.into());
                    }
                }
            });
        }
        Ok(())
    }
}

/// A UI's end of the API forwarding: a loopback port whose connections reach the Trailhead server through the control
/// plane, each as a stream on one long-lived connection, redialed if it drops.
pub async fn open_ui_tunnel(endpoint: Endpoint, control_plane: EndpointId) -> Result<SocketAddr> {
    let listener = TcpListener::bind("127.0.0.1:0").await.map_err(io_err)?;
    let addr = listener.local_addr().map_err(io_err)?;
    let conn: Arc<tokio::sync::Mutex<Option<Connection>>> = Arc::default();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let (endpoint, conn) = (endpoint.clone(), conn.clone());
            tokio::spawn(async move {
                let streams = async {
                    let mut guard = conn.lock().await;
                    let live = guard.as_ref().filter(|c| c.close_reason().is_none()).cloned();
                    let c = match live {
                        Some(c) => c,
                        None => {
                            let c = endpoint.connect(control_plane, UI_ALPN).await.map_err(io_err)?;
                            *guard = Some(c.clone());
                            c
                        }
                    };
                    drop(guard);
                    let (mut send, recv) = c.open_bi().await.map_err(io_err)?;
                    write_frame(&mut send, &UiRequest::Api).await?;
                    Ok::<_, Error>((send, recv))
                };
                match streams.await {
                    Ok((send, recv)) => pipe(send, recv, tcp).await,
                    Err(e) => tracing::warn!(error = %e, "UI tunnel to the control plane failed"),
                }
            });
        }
    });
    Ok(addr)
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;

    use super::*;

    #[test]
    fn the_allowlist_persists_and_dedupes() {
        let dir = tempfile::tempdir().unwrap();
        let list = UiAllowlist::new(dir.path().join("factory").join("ui-allowed"));
        let (a, b) = (SecretKey::generate().public(), SecretKey::generate().public());
        assert!(list.list().unwrap().is_empty());
        assert!(list.add(a).unwrap());
        assert!(!list.add(a).unwrap());
        assert!(list.add(b).unwrap());
        let reread = UiAllowlist::new(dir.path().join("factory").join("ui-allowed"));
        assert_eq!(reread.list().unwrap(), vec![a, b]);
        assert!(reread.remove(&a).unwrap());
        assert!(!reread.remove(&a).unwrap());
        assert!(!reread.contains(&a) && reread.contains(&b));
    }

    #[test]
    fn requests_are_tagged_so_status_can_join_them() {
        assert_eq!(serde_json::to_string(&UiRequest::Api).unwrap(), r#"{"op":"api"}"#);
    }
}
