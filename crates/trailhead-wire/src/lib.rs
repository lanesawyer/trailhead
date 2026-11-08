//! The factory's iroh protocols, shared by the control plane, the sandbox daemon and Lookout, with no harness, code
//! host or server dependency.
//!
//! - The daemon contract (`trailhead/daemon/0`): the control plane dials each sandbox's daemon by an endpoint id it
//!   derives from the sandbox name ([`sandbox_key`]), so it needs no address book and can reconnect after a restart.
//!   [`daemon::RemoteDaemon`] is the control plane's side; the daemon's side is in `trailhead-daemon`.
//! - The API tunnel ([`API_ALPN`], [`api`]): a daemon reaches Trailhead by dialing the control plane, which forwards
//!   each stream to the server's loopback port. Only daemons this control plane started get in, and the server still
//!   checks every request's bearer token.
//! - Lookout's `trailhead/ui/0` ([`ui`]).
//!
//! One bidirectional stream carries one request; frames are a 4-byte big-endian length and a JSON message.

pub mod api;
pub mod daemon;
pub mod ui;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use hmac::{Hmac, KeyInit, Mac};
use iroh::endpoint::{ReadExactError, RecvStream, SendStream};
use iroh::{EndpointId, SecretKey};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use trailhead_factory::{Error, Result, SandboxId};

pub const ALPN: &[u8] = b"trailhead/daemon/0";

/// Daemon to control plane: a byte stream to the Trailhead server.
pub const API_ALPN: &[u8] = b"trailhead/api/0";

/// The daemons allowed through the API tunnel: the ones this control plane started or dialed.
pub type Peers = Arc<Mutex<HashSet<EndpointId>>>;

const MAX_FRAME: usize = 16 * 1024 * 1024;

/// A sandbox daemon's iroh key, derived from the control plane's master key and the sandbox name.
pub fn sandbox_key(master: &[u8; 32], sandbox: &SandboxId) -> SecretKey {
    let mut mac = Hmac::<Sha256>::new_from_slice(master).expect("HMAC takes any key length");
    mac.update(b"trailhead daemon key\0");
    mac.update(sandbox.0.as_bytes());
    SecretKey::from_bytes(&mac.finalize().into_bytes().into())
}

pub fn parse_secret_key(hex_key: &str) -> Result<SecretKey> {
    let bytes: [u8; 32] = hex::decode(hex_key.trim())
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| Error::Config("secret_key must be 32 bytes of hex".into()))?;
    Ok(SecretKey::from_bytes(&bytes))
}

pub fn parse_endpoint_id(hex_id: &str) -> Result<EndpointId> {
    hex_id.trim().parse().map_err(|e| Error::Config(format!("endpoint id {hex_id}: {e}")))
}

/// The identity of a daemon binary: the hex SHA-256 of its bytes.
pub fn build_id(binary: &[u8]) -> String {
    hex::encode(Sha256::digest(binary))
}

pub fn io_err(e: impl std::fmt::Display) -> Error {
    Error::Other(format!("daemon connection: {e}"))
}

pub async fn write_frame<T: Serialize>(send: &mut SendStream, msg: &T) -> Result<()> {
    let body = serde_json::to_vec(msg).map_err(io_err)?;
    let len = u32::try_from(body.len()).map_err(io_err)?;
    send.write_all(&len.to_be_bytes()).await.map_err(io_err)?;
    send.write_all(&body).await.map_err(io_err)
}

/// The next frame, or `None` when the peer finished the stream cleanly.
pub async fn read_frame<T: DeserializeOwned>(recv: &mut RecvStream) -> Result<Option<T>> {
    let mut len = [0u8; 4];
    match recv.read_exact(&mut len).await {
        Ok(_) => {}
        Err(ReadExactError::FinishedEarly(0)) => return Ok(None),
        Err(e) => return Err(io_err(e)),
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(io_err(format!("frame of {len} bytes")));
    }
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body).await.map_err(io_err)?;
    serde_json::from_slice(&body).map(Some).map_err(io_err)
}

/// Reads frames on their own task, since `read_frame` isn't safe to cancel inside a `select!`.
pub fn frames<T: DeserializeOwned + Send + 'static>(mut recv: RecvStream) -> mpsc::UnboundedReceiver<Result<T>> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            match read_frame(&mut recv).await {
                Ok(Some(msg)) => {
                    if tx.send(Ok(msg)).is_err() {
                        return;
                    }
                }
                Ok(None) => return,
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            }
        }
    });
    rx
}

/// Copies bytes both ways between an iroh stream and a TCP connection until both sides finish.
pub(crate) async fn pipe(mut send: SendStream, mut recv: RecvStream, tcp: TcpStream) {
    let (mut tcp_read, mut tcp_write) = tcp.into_split();
    let inbound = async {
        let _ = tokio::io::copy(&mut recv, &mut tcp_write).await;
        let _ = tcp_write.shutdown().await;
    };
    let outbound = async {
        let _ = tokio::io::copy(&mut tcp_read, &mut send).await;
        let _ = send.finish();
    };
    tokio::join!(inbound, outbound);
}
