//! `trailhead-daemon gateway`: the egress half of the Docker sandboxes' gateway
//! (`docs/factory/sandbox.md#the-gateway-credentials-and-egress-for-docker-sandboxes`).
//!
//! An HTTP forward proxy that tells sandboxes apart by the local address a request arrived on, and a control
//! socket that registers them and sets their allowlists. The request head is parsed by hand and the rest of each
//! connection is copied as bytes, so there's no HTTP dependency: a plain request is forwarded with
//! `Connection: close`, one request per connection, which keeps bodies and streamed responses untouched.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, UnixListener, UnixStream};
use tokio::sync::oneshot;
use trailhead_factory::{Egress, GatewayRequest, GatewayResponse, GatewaySandbox, GatewayState, Labels};

/// The longest request head the proxy reads.
const MAX_HEAD: usize = 64 * 1024;

/// Whether `domains` allow `host`: exact names and `*.` wildcards (subdomains only), trailing dots stripped from
/// both sides, case ignored. An empty list allows nothing.
pub fn allows(domains: &[String], host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    domains.iter().any(|d| {
        let d = d.trim_end_matches('.').to_ascii_lowercase();
        match d.strip_prefix("*.") {
            Some(suffix) => {
                !suffix.is_empty() && host.len() > suffix.len() + 1 && host.ends_with(&format!(".{suffix}"))
            }
            None => d == host,
        }
    })
}

struct Tunnel {
    id: u64,
    host: String,
    /// Dropping this tells the tunnel to stop.
    _kill: oneshot::Sender<()>,
    /// Resolves (as an error) once the tunnel has dropped its sockets.
    closed: oneshot::Receiver<()>,
}

struct Sandbox {
    address: IpAddr,
    labels: Labels,
    egress: Egress,
    refused: BTreeMap<String, u64>,
    tunnels: Vec<Tunnel>,
}

#[derive(Default)]
struct Inner {
    sandboxes: BTreeMap<String, Sandbox>,
    unregistered: BTreeMap<String, u64>,
    next_tunnel: u64,
}

/// What the tunnel task holds while it's open.
struct Lease {
    gateway: Arc<Gateway>,
    sandbox: String,
    id: u64,
    kill: oneshot::Receiver<()>,
    _done: oneshot::Sender<()>,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut inner = self.gateway.inner();
        if let Some(sb) = inner.sandboxes.get_mut(&self.sandbox) {
            sb.tunnels.retain(|t| t.id != self.id);
        }
    }
}

enum Verdict {
    Allowed(Lease),
    Refused,
}

#[derive(Default)]
pub struct Gateway {
    inner: Mutex<Inner>,
}

impl Gateway {
    pub fn new() -> Arc<Gateway> {
        Arc::new(Gateway::default())
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Decides a request from `local` for `host` and, if it's allowed, records the tunnel in the same step so a
    /// concurrent `set_egress` can't miss it.
    fn admit(self: &Arc<Self>, local: IpAddr, host: &str) -> Verdict {
        let mut inner = self.inner();
        let Some((name, sb)) = inner.sandboxes.iter_mut().find(|(_, s)| s.address == local) else {
            let seen = inner.unregistered.entry(local.to_string()).or_insert(0);
            *seen += 1;
            if *seen == 1 {
                tracing::warn!(%local, host, "refused a request from a network with no registered sandbox");
            }
            return Verdict::Refused;
        };
        if !allows(&sb.egress.domains, host) {
            let n = sb.refused.entry(host.trim_end_matches('.').to_ascii_lowercase()).or_insert(0);
            *n += 1;
            if *n == 1 {
                tracing::warn!(sandbox = %name, host, "refused; further refusals of this host are counted, not logged");
            }
            return Verdict::Refused;
        }
        let name = name.clone();
        let (kill_tx, kill) = oneshot::channel();
        let (done_tx, closed) = oneshot::channel();
        let id = inner.next_tunnel;
        inner.next_tunnel += 1;
        let sb = inner.sandboxes.get_mut(&name).expect("just found");
        sb.tunnels.push(Tunnel { id, host: host.trim_end_matches('.').to_ascii_lowercase(), _kill: kill_tx, closed });
        Verdict::Allowed(Lease { gateway: self.clone(), sandbox: name, id, kill, _done: done_tx })
    }

    /// Applies one control message.
    pub async fn handle(&self, req: GatewayRequest) -> GatewayResponse {
        match req {
            GatewayRequest::Register { name, address, labels } => {
                let mut inner = self.inner();
                if let Some((other, _)) = inner.sandboxes.iter().find(|(n, s)| s.address == address && **n != name) {
                    return GatewayResponse::Error { message: format!("{address} is already {other}'s network") };
                }
                match inner.sandboxes.get_mut(&name) {
                    Some(sb) => {
                        sb.address = address;
                        sb.labels = labels;
                    }
                    None => {
                        inner.sandboxes.insert(
                            name,
                            Sandbox {
                                address,
                                labels,
                                egress: Egress::none(),
                                refused: BTreeMap::new(),
                                tunnels: Vec::new(),
                            },
                        );
                    }
                }
                GatewayResponse::Ok
            }
            GatewayRequest::Unregister { name } => {
                let removed = self.inner().sandboxes.remove(&name);
                match removed {
                    Some(sb) => {
                        wait_closed(sb.tunnels).await;
                        GatewayResponse::Ok
                    }
                    None => GatewayResponse::Ok,
                }
            }
            GatewayRequest::SetEgress { name, egress } => {
                let doomed = {
                    let mut inner = self.inner();
                    let Some(sb) = inner.sandboxes.get_mut(&name) else {
                        return GatewayResponse::Error { message: format!("no sandbox named {name}") };
                    };
                    sb.egress = egress;
                    let (keep, doomed): (Vec<_>, Vec<_>) =
                        std::mem::take(&mut sb.tunnels).into_iter().partition(|t| allows(&sb.egress.domains, &t.host));
                    sb.tunnels = keep;
                    doomed
                };
                wait_closed(doomed).await;
                GatewayResponse::Ok
            }
            GatewayRequest::State => GatewayResponse::State(self.state()),
        }
    }

    pub fn state(&self) -> GatewayState {
        let inner = self.inner();
        GatewayState {
            sandboxes: inner
                .sandboxes
                .iter()
                .map(|(name, s)| GatewaySandbox {
                    name: name.clone(),
                    address: s.address,
                    labels: s.labels.clone(),
                    egress: s.egress.clone(),
                    refused: s.refused.clone(),
                    open_tunnels: s.tunnels.len(),
                })
                .collect(),
            unregistered_refusals: inner.unregistered.clone(),
        }
    }

    /// Serves the proxy on `listener` until the task is dropped.
    pub async fn serve_proxy(self: Arc<Self>, listener: TcpListener) {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let gw = self.clone();
                    tokio::spawn(async move {
                        if let Err(e) = gw.proxy(stream).await {
                            tracing::debug!(error = %e, "proxy connection ended");
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "proxy accept failed");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    }

    /// Serves the control protocol on `listener` until the task is dropped.
    pub async fn serve_control(self: Arc<Self>, listener: UnixListener) {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let gw = self.clone();
                    tokio::spawn(async move {
                        if let Err(e) = gw.control(stream).await {
                            tracing::debug!(error = %e, "control connection ended");
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "control accept failed");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    }

    async fn control(&self, stream: UnixStream) -> std::io::Result<()> {
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            let resp = match serde_json::from_str::<GatewayRequest>(&line) {
                Ok(req) => self.handle(req).await,
                Err(e) => GatewayResponse::Error { message: format!("bad request: {e}") },
            };
            let mut out = serde_json::to_vec(&resp).map_err(std::io::Error::other)?;
            out.push(b'\n');
            write.write_all(&out).await?;
        }
        Ok(())
    }

    async fn proxy(self: Arc<Self>, mut client: TcpStream) -> std::io::Result<()> {
        let local = client.local_addr()?.ip();
        let (head, rest) = match read_head(&mut client).await? {
            Some(h) => h,
            None => return Ok(()),
        };
        let req = match parse_head(&head) {
            Ok(r) => r,
            Err(why) => return respond(&mut client, 400, why).await,
        };
        match req {
            Request::Connect { host, port } => {
                let mut lease = match self.admit(local, &host) {
                    Verdict::Allowed(l) => l,
                    Verdict::Refused => return respond(&mut client, 403, "refused by the gateway").await,
                };
                let mut upstream = match connect(&host, port, &mut lease).await {
                    Some(Ok(s)) => s,
                    Some(Err(_)) => return respond(&mut client, 502, "couldn't reach the host").await,
                    None => return Ok(()),
                };
                client.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").await?;
                if !rest.is_empty() {
                    upstream.write_all(&rest).await?;
                }
                pipe(lease, client, upstream).await
            }
            Request::Plain { host, port, head } => {
                let mut lease = match self.admit(local, &host) {
                    Verdict::Allowed(l) => l,
                    Verdict::Refused => return respond(&mut client, 403, "refused by the gateway").await,
                };
                let mut upstream = match connect(&host, port, &mut lease).await {
                    Some(Ok(s)) => s,
                    Some(Err(_)) => return respond(&mut client, 502, "couldn't reach the host").await,
                    None => return Ok(()),
                };
                upstream.write_all(&head).await?;
                if !rest.is_empty() {
                    upstream.write_all(&rest).await?;
                }
                pipe(lease, client, upstream).await
            }
        }
    }
}

/// Waits until each tunnel has let go of its sockets.
async fn wait_closed(tunnels: Vec<Tunnel>) {
    for t in tunnels {
        drop(t._kill);
        let _ = t.closed.await;
    }
}

/// Connects to `host:port`, or gives up (`None`) if the tunnel is told to stop first.
async fn connect(host: &str, port: u16, lease: &mut Lease) -> Option<std::io::Result<TcpStream>> {
    tokio::select! {
        r = TcpStream::connect((host, port)) => Some(r),
        _ = &mut lease.kill => None,
    }
}

/// Copies both ways until either side closes or the tunnel is told to stop.
async fn pipe(mut lease: Lease, mut client: TcpStream, mut upstream: TcpStream) -> std::io::Result<()> {
    tokio::select! {
        r = tokio::io::copy_bidirectional(&mut client, &mut upstream) => r.map(|_| ()),
        _ = &mut lease.kill => Ok(()),
    }
}

async fn respond(client: &mut TcpStream, status: u16, why: &str) -> std::io::Result<()> {
    let reason = match status {
        400 => "Bad Request",
        403 => "Forbidden",
        _ => "Bad Gateway",
    };
    let msg = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{why}",
        why.len()
    );
    client.write_all(msg.as_bytes()).await?;
    client.shutdown().await
}

/// Reads up to the end of the request head; returns it and any bytes read past it.
async fn read_head<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Option<(Vec<u8>, Vec<u8>)>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = r.read(&mut chunk).await?;
        if n == 0 {
            return Ok(None);
        }
        // The terminator can straddle reads, so look from just before the new bytes.
        let from = buf.len().saturating_sub(3);
        buf.extend_from_slice(&chunk[..n]);
        if let Some(at) = buf[from..].windows(4).position(|w| w == b"\r\n\r\n") {
            let end = from + at + 4;
            let rest = buf.split_off(end);
            return Ok(Some((buf, rest)));
        }
        if buf.len() > MAX_HEAD {
            return Ok(None);
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Request {
    Connect {
        host: String,
        port: u16,
    },
    /// A plain request already rewritten to origin form, ready to send upstream.
    Plain {
        host: String,
        port: u16,
        head: Vec<u8>,
    },
}

/// Splits `host[:port]`, with `[v6]` brackets removed.
fn split_authority(a: &str, default_port: Option<u16>) -> Result<(String, u16), &'static str> {
    let (host, port) = if let Some(rest) = a.strip_prefix('[') {
        let (h, after) = rest.split_once(']').ok_or("bad host")?;
        (h, after.strip_prefix(':'))
    } else {
        match a.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (a, None),
        }
    };
    let port = match port {
        Some(p) => p.parse().map_err(|_| "bad port")?,
        None => default_port.ok_or("missing port")?,
    };
    if host.is_empty() {
        return Err("missing host");
    }
    Ok((host.to_string(), port))
}

fn parse_head(head: &[u8]) -> Result<Request, &'static str> {
    let text = std::str::from_utf8(head).map_err(|_| "request head isn't UTF-8")?;
    let mut lines = text.split("\r\n");
    let line = lines.next().ok_or("empty request")?;
    let mut parts = line.split(' ');
    let (Some(method), Some(target), Some(version), None) = (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err("bad request line");
    };
    if !version.starts_with("HTTP/1.") {
        return Err("unsupported HTTP version");
    }
    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = split_authority(target, None)?;
        return Ok(Request::Connect { host, port });
    }
    let rest = target
        .strip_prefix("http://")
        .or_else(|| target.get(..7).filter(|p| p.eq_ignore_ascii_case("http://")).map(|_| &target[7..]));
    let Some(rest) = rest else {
        return Err("this is a proxy: send an absolute-form request or CONNECT");
    };
    let (authority, path) = match rest.find(['/', '?']) {
        Some(i) if rest.as_bytes()[i] == b'/' => (&rest[..i], rest[i..].to_string()),
        Some(i) => (&rest[..i], format!("/{}", &rest[i..])),
        None => (rest, "/".to_string()),
    };
    let authority = authority.rsplit_once('@').map_or(authority, |(_, a)| a);
    let (host, port) = split_authority(authority, Some(80))?;
    let mut out = format!("{method} {path} {version}\r\nHost: {authority}\r\nConnection: close\r\n");
    for l in lines {
        if l.is_empty() {
            break;
        }
        let name = l.split_once(':').ok_or("bad header")?.0.trim().to_ascii_lowercase();
        if name.starts_with("proxy-") || matches!(name.as_str(), "host" | "connection" | "keep-alive") {
            continue;
        }
        out.push_str(l);
        out.push_str("\r\n");
    }
    out.push_str("\r\n");
    Ok(Request::Plain { host, port, head: out.into_bytes() })
}

/// Runs the gateway: the proxy on `listen`, the control socket at `socket`.
pub async fn run(listen: SocketAddr, socket: &Path) -> std::io::Result<()> {
    let proxy = TcpListener::bind(listen).await?;
    let _ = std::fs::remove_file(socket);
    let control = UnixListener::bind(socket)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    }
    tracing::info!(%listen, socket = %socket.display(), "trailhead-daemon gateway listening");
    let gw = Gateway::new();
    let p = tokio::spawn(gw.clone().serve_proxy(proxy));
    let c = tokio::spawn(gw.serve_control(control));
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = p => {}
        _ = c => {}
    }
    Ok(())
}

/// `gateway-ctl`: sends one request to the control socket and returns the response.
pub async fn ctl(socket: &Path, req: &GatewayRequest) -> std::io::Result<GatewayResponse> {
    let stream = UnixStream::connect(socket).await?;
    let (read, mut write) = stream.into_split();
    let mut line = serde_json::to_vec(req).map_err(std::io::Error::other)?;
    line.push(b'\n');
    write.write_all(&line).await?;
    let mut lines = BufReader::new(read).lines();
    let resp = lines
        .next_line()
        .await?
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "the gateway closed the socket"))?;
    serde_json::from_str(&resp).map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    fn list(d: &[&str]) -> Vec<String> {
        d.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn matching() {
        let d = list(&["dns.iroh.link", "*.relay.n0.iroh.link", "Example.COM."]);
        assert!(allows(&d, "dns.iroh.link"));
        assert!(allows(&d, "dns.iroh.link."));
        assert!(allows(&d, "usw1-1.relay.n0.iroh.link."));
        assert!(allows(&d, "a.b.relay.n0.iroh.link"));
        assert!(allows(&d, "example.com"));
        assert!(!allows(&d, "relay.n0.iroh.link"));
        assert!(!allows(&d, "evilrelay.n0.iroh.link"));
        assert!(!allows(&d, "iroh.link"));
        assert!(!allows(&d, ""));
        assert!(!allows(&[], "example.com"));
        assert!(!allows(&list(&["*"]), "example.com"));
    }

    #[test]
    fn parses_heads() {
        assert_eq!(
            parse_head(b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\n"),
            Ok(Request::Connect { host: "example.com".into(), port: 443 })
        );
        assert_eq!(
            parse_head(b"CONNECT [::1]:8443 HTTP/1.1\r\n\r\n"),
            Ok(Request::Connect { host: "::1".into(), port: 8443 })
        );
        let Ok(Request::Plain { host, port, head }) = parse_head(
            b"GET http://example.com:8080/a?b=1 HTTP/1.1\r\nProxy-Authorization: x\r\nProxy-Connection: keep-alive\r\nAccept: */*\r\nConnection: keep-alive\r\n\r\n",
        ) else {
            panic!("didn't parse");
        };
        assert_eq!((host.as_str(), port), ("example.com", 8080));
        assert_eq!(
            String::from_utf8(head).unwrap(),
            "GET /a?b=1 HTTP/1.1\r\nHost: example.com:8080\r\nConnection: close\r\nAccept: */*\r\n\r\n"
        );
        let Ok(Request::Plain { head, port, .. }) = parse_head(b"GET http://example.com?q=1 HTTP/1.1\r\n\r\n") else {
            panic!()
        };
        assert_eq!(port, 80);
        assert!(String::from_utf8(head).unwrap().starts_with("GET /?q=1 HTTP/1.1\r\n"));
        assert!(parse_head(b"GET /relative HTTP/1.1\r\n\r\n").is_err());
        assert!(parse_head(b"nonsense\r\n\r\n").is_err());
    }

    /// A gateway on 127.0.0.1 with one sandbox registered on that address and the given allowlist.
    async fn setup(domains: &[&str]) -> (Arc<Gateway>, SocketAddr) {
        let gw = Gateway::new();
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(gw.clone().serve_proxy(listener));
        let r = gw
            .handle(GatewayRequest::Register {
                name: "a".into(),
                address: "127.0.0.1".parse().unwrap(),
                labels: Labels::new(),
            })
            .await;
        assert_eq!(r, GatewayResponse::Ok);
        let r =
            gw.handle(GatewayRequest::SetEgress { name: "a".into(), egress: Egress { domains: list(domains) } }).await;
        assert_eq!(r, GatewayResponse::Ok);
        (gw, SocketAddr::from(([127, 0, 0, 1], port)))
    }

    /// An origin that echoes every byte it's sent, and for a plain request, the head it received.
    async fn origin() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = l.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    loop {
                        match s.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                if buf.starts_with(b"GET ") {
                                    let body = String::from_utf8_lossy(&buf[..n]).into_owned();
                                    let resp =
                                        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len());
                                    let _ = s.write_all(resp.as_bytes()).await;
                                    break;
                                }
                                if s.write_all(&buf[..n]).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                });
            }
        });
        port
    }

    async fn connect_via(proxy: SocketAddr, target: &str) -> (TcpStream, String) {
        let mut s = TcpStream::connect(proxy).await.unwrap();
        s.write_all(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n").as_bytes()).await.unwrap();
        let (head, _) = read_head(&mut s).await.unwrap().unwrap();
        let status = String::from_utf8(head).unwrap().lines().next().unwrap().to_string();
        (s, status)
    }

    #[tokio::test]
    async fn connect_tunnels_to_an_allowed_host() {
        let port = origin().await;
        let (_gw, proxy) = setup(&["127.0.0.1"]).await;
        let (mut s, status) = connect_via(proxy, &format!("127.0.0.1:{port}")).await;
        assert!(status.contains("200"), "{status}");
        s.write_all(b"ping").await.unwrap();
        let mut got = [0u8; 4];
        s.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"ping");
    }

    #[tokio::test]
    async fn plain_requests_are_rewritten_and_forwarded() {
        let port = origin().await;
        let (_gw, proxy) = setup(&["127.0.0.1"]).await;
        let mut s = TcpStream::connect(proxy).await.unwrap();
        s.write_all(
            format!("GET http://127.0.0.1:{port}/x?y=1 HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nProxy-Authorization: secret\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        assert!(out.starts_with("HTTP/1.1 200"), "{out}");
        assert!(out.contains("GET /x?y=1 HTTP/1.1"), "{out}");
        assert!(!out.to_ascii_lowercase().contains("proxy-authorization"), "{out}");
    }

    #[tokio::test]
    async fn refuses_hosts_off_the_list_and_unregistered_networks() {
        let port = origin().await;
        let (gw, proxy) = setup(&["example.com"]).await;
        let (_s, status) = connect_via(proxy, &format!("127.0.0.1:{port}")).await;
        assert!(status.contains("403"), "{status}");
        let mut s = TcpStream::connect(proxy).await.unwrap();
        s.write_all(format!("GET http://127.0.0.1:{port}/ HTTP/1.1\r\n\r\n").as_bytes()).await.unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        assert!(out.starts_with("HTTP/1.1 403"), "{out}");

        // Moving the sandbox to another address makes this one an unregistered network.
        gw.handle(GatewayRequest::Register {
            name: "a".into(),
            address: "10.9.9.9".parse().unwrap(),
            labels: Labels::new(),
        })
        .await;
        let (_s, status) = connect_via(proxy, "example.com:443").await;
        assert!(status.contains("403"), "{status}");
        let state = gw.state();
        assert_eq!(state.unregistered_refusals["127.0.0.1"], 1);
        assert_eq!(state.sandboxes[0].refused["127.0.0.1"], 2);
    }

    #[tokio::test]
    async fn narrowing_closes_tunnels_before_returning() {
        let port = origin().await;
        let (gw, proxy) = setup(&["127.0.0.1", "example.com"]).await;
        let (mut s, status) = connect_via(proxy, &format!("127.0.0.1:{port}")).await;
        assert!(status.contains("200"));
        assert_eq!(gw.state().sandboxes[0].open_tunnels, 1);

        // Still allowed: the tunnel survives.
        gw.handle(GatewayRequest::SetEgress { name: "a".into(), egress: Egress { domains: list(&["127.0.0.1"]) } })
            .await;
        s.write_all(b"hi").await.unwrap();
        let mut got = [0u8; 2];
        s.read_exact(&mut got).await.unwrap();

        gw.handle(GatewayRequest::SetEgress { name: "a".into(), egress: Egress { domains: list(&["example.com"]) } })
            .await;
        assert_eq!(gw.state().sandboxes[0].open_tunnels, 0);
        let mut rest = Vec::new();
        assert_eq!(s.read_to_end(&mut rest).await.unwrap(), 0);
        // And new requests are refused at once.
        let (_s, status) = connect_via(proxy, &format!("127.0.0.1:{port}")).await;
        assert!(status.contains("403"), "{status}");
    }

    #[tokio::test]
    async fn logs_a_refusal_once_per_host() {
        let (gw, _proxy) = setup(&[]).await;
        let local: IpAddr = "127.0.0.1".parse().unwrap();
        for _ in 0..3 {
            assert!(matches!(gw.admit(local, "dns.google."), Verdict::Refused));
        }
        assert!(matches!(gw.admit(local, "one.one.one.one"), Verdict::Refused));
        let refused = &gw.state().sandboxes[0].refused;
        assert_eq!(refused["dns.google"], 3);
        assert_eq!(refused["one.one.one.one"], 1);
    }

    #[tokio::test]
    async fn control_socket_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("gw.sock");
        let gw = Gateway::new();
        tokio::spawn(gw.clone().serve_control(UnixListener::bind(&socket).unwrap()));
        let labels: Labels = [("role".to_string(), "worker".to_string())].into();
        let register = GatewayRequest::Register {
            name: "sb".into(),
            address: "172.20.0.2".parse().unwrap(),
            labels: labels.clone(),
        };
        assert_eq!(ctl(&socket, &register).await.unwrap(), GatewayResponse::Ok);
        let egress = Egress { domains: list(&["github.com"]) };
        let set = GatewayRequest::SetEgress { name: "sb".into(), egress: egress.clone() };
        assert_eq!(ctl(&socket, &set).await.unwrap(), GatewayResponse::Ok);
        let GatewayResponse::State(state) = ctl(&socket, &GatewayRequest::State).await.unwrap() else { panic!() };
        assert_eq!(state.sandboxes.len(), 1);
        assert_eq!(state.sandboxes[0].labels, labels);
        assert_eq!(state.sandboxes[0].egress, egress);
        let missing = GatewayRequest::SetEgress { name: "nope".into(), egress };
        assert!(matches!(ctl(&socket, &missing).await.unwrap(), GatewayResponse::Error { .. }));
        let clash = GatewayRequest::Register {
            name: "other".into(),
            address: "172.20.0.2".parse().unwrap(),
            labels: Labels::new(),
        };
        assert!(matches!(ctl(&socket, &clash).await.unwrap(), GatewayResponse::Error { .. }));
        assert_eq!(ctl(&socket, &GatewayRequest::Unregister { name: "sb".into() }).await.unwrap(), GatewayResponse::Ok);
        let GatewayResponse::State(state) = ctl(&socket, &GatewayRequest::State).await.unwrap() else { panic!() };
        assert!(state.sandboxes.is_empty());
    }
}
