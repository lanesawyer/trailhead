//! The API tunnel: a daemon reaches the Trailhead server through the control plane over [`API_ALPN`].

use std::net::SocketAddr;
use std::sync::Arc;

use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointId};
use tokio::net::{TcpListener, TcpStream};
use trailhead_factory::Result;

use crate::{API_ALPN, Peers, io_err, pipe};

/// The control plane's end of the API tunnel.
#[derive(Debug, Clone)]
pub struct ApiTunnel {
    server: SocketAddr,
    peers: Peers,
}

impl ApiTunnel {
    pub fn new(server: SocketAddr, peers: Peers) -> ApiTunnel {
        ApiTunnel { server, peers }
    }
}

impl ProtocolHandler for ApiTunnel {
    async fn accept(&self, conn: Connection) -> std::result::Result<(), AcceptError> {
        if !self.peers.lock().unwrap().contains(&conn.remote_id()) {
            tracing::warn!(peer = %conn.remote_id(), "refusing an API tunnel from an unknown peer");
            conn.close(1u32.into(), b"unknown peer");
            return Ok(());
        }
        while let Ok((send, recv)) = conn.accept_bi().await {
            let server = self.server;
            tokio::spawn(async move {
                match TcpStream::connect(server).await {
                    Ok(tcp) => pipe(send, recv, tcp).await,
                    Err(e) => tracing::warn!(%server, error = %e, "can't reach the Trailhead server"),
                }
            });
        }
        Ok(())
    }
}

/// The daemon's end of the API tunnel: a loopback port whose connections reach the Trailhead server, each as a
/// stream on one long-lived connection to the control plane, redialed if it drops.
pub async fn open_api_tunnel(endpoint: Endpoint, control_plane: EndpointId) -> Result<SocketAddr> {
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
                            let c = endpoint.connect(control_plane, API_ALPN).await.map_err(io_err)?;
                            *guard = Some(c.clone());
                            c
                        }
                    };
                    drop(guard);
                    c.open_bi().await.map_err(io_err)
                };
                match streams.await {
                    Ok((send, recv)) => pipe(send, recv, tcp).await,
                    Err(e) => tracing::warn!(error = %e, "API tunnel to the control plane failed"),
                }
            });
        }
    });
    Ok(addr)
}
