//! The credentials half of the gateway: the part of Fly's gateway API the daemon uses. A request in origin form
//! (not a proxy request) is addressed to the gateway itself: `GET /v1/gateway/list` lists the connections the
//! calling sandbox may use, and `/v1/gateway/<provider>/<id>/...` is forwarded to that connection's upstream with
//! the stored credential added and any the sandbox sent removed. Responses stream as they arrive, with no total
//! timeout.

use std::net::IpAddr;
use std::sync::Arc;

use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use trailhead_factory::{AuthScheme, GatewayConnection, Labels};

use super::{Gateway, policy, respond_with};

/// The largest request body forwarded.
const MAX_BODY: usize = 32 * 1024 * 1024;

/// Headers that carry a credential, or that identify the hop rather than the request; none are forwarded.
const NOT_FORWARDED: &[&str] = &[
    "host",
    "content-length",
    "connection",
    "keep-alive",
    "transfer-encoding",
    "te",
    "trailer",
    "upgrade",
    "expect",
    "authorization",
    "proxy-authorization",
    "proxy-connection",
    "x-api-key",
    "api-key",
    "x-auth-token",
    "x-goog-api-key",
    "x-amz-security-token",
    "cookie",
];

struct ApiRequest {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
}

/// Whether a request head is addressed to the gateway itself (origin form) rather than a proxy request.
pub(super) fn is_api_request(head: &[u8]) -> bool {
    head.split(|b| *b == b' ').nth(1).is_some_and(|t| t.starts_with(b"/"))
}

fn parse(head: &[u8]) -> Result<ApiRequest, &'static str> {
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
    let mut headers = Vec::new();
    for l in lines {
        if l.is_empty() {
            break;
        }
        let (name, value) = l.split_once(':').ok_or("bad header")?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }
    Ok(ApiRequest { method: method.to_string(), target: target.to_string(), headers })
}

/// A `Host` header value that's safe to put in a URL we hand back.
fn plausible_host(h: &str) -> bool {
    !h.is_empty() && h.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'))
}

impl Gateway {
    /// Answers one gateway API request. `head` is the request head, `rest` any bytes read past it.
    pub(super) async fn api(
        self: &Arc<Self>,
        client: &mut TcpStream,
        head: &[u8],
        rest: Vec<u8>,
    ) -> std::io::Result<()> {
        let local = client.local_addr()?;
        let req = match parse(head) {
            Ok(r) => r,
            Err(why) => return respond_with(client, 400, "text/plain", why).await,
        };
        let header = |name: &str| req.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str());
        let Some(labels) = self.labels_at(local.ip()) else {
            return respond_with(client, 403, "text/plain", "no sandbox is registered on this network").await;
        };
        let (path, query) = match req.target.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (req.target.as_str(), None),
        };

        if path == "/v1/gateway/list" {
            if !req.method.eq_ignore_ascii_case("GET") {
                return respond_with(client, 405, "text/plain", "use GET").await;
            }
            let authority =
                header("host").filter(|h| plausible_host(h)).map_or_else(|| local.to_string(), String::from);
            let body = self.list(&labels, &authority);
            return respond_with(client, 200, "application/json", &body.to_string()).await;
        }

        let Some(tail) = path.strip_prefix("/v1/gateway/") else {
            return respond_with(client, 404, "text/plain", "not a gateway path").await;
        };
        let mut parts = tail.splitn(3, '/');
        let (Some(provider), Some(id)) = (parts.next(), parts.next()) else {
            return respond_with(client, 404, "text/plain", "expected /v1/gateway/<provider>/<id>/...").await;
        };
        let upstream_path = format!("/{}", parts.next().unwrap_or(""));
        let Some(conn) = self.connection(provider, id) else {
            return respond_with(client, 404, "text/plain", "no such connection").await;
        };
        if let Err(why) = policy::check(provider, &labels, &req.method, &upstream_path) {
            return respond_with(client, 403, "text/plain", &why).await;
        }
        if header("transfer-encoding").is_some() {
            return respond_with(client, 411, "text/plain", "send a Content-Length instead of chunking").await;
        }
        let length = match header("content-length").map(|v| v.parse::<usize>()) {
            None => 0,
            Some(Ok(n)) if n <= MAX_BODY => n,
            Some(Ok(_)) => return respond_with(client, 413, "text/plain", "the request body is too large").await,
            Some(Err(_)) => return respond_with(client, 400, "text/plain", "bad Content-Length").await,
        };
        let mut body = rest;
        if body.len() < length {
            let have = body.len();
            body.resize(length, 0);
            client.read_exact(&mut body[have..]).await?;
        }
        body.truncate(length);

        let mut url = format!("{}{}", conn.upstream.trim_end_matches('/'), upstream_path);
        if let Some(q) = query {
            url.push('?');
            url.push_str(q);
        }
        self.forward(client, &conn, &req, url, body).await
    }

    fn labels_at(&self, local: IpAddr) -> Option<Labels> {
        self.inner().sandboxes.values().find(|s| s.address == local).map(|s| s.labels.clone())
    }

    fn connection(&self, provider: &str, id: &str) -> Option<GatewayConnection> {
        self.inner().connections.get(&(provider.to_string(), id.to_string())).cloned()
    }

    fn list(&self, labels: &Labels, authority: &str) -> serde_json::Value {
        let inner = self.inner();
        let connections: Vec<_> = inner
            .connections
            .values()
            .filter(|c| policy::grants(&c.provider, labels))
            .map(|c| {
                json!({
                    "provider": c.provider,
                    "id": c.id,
                    "gateway_base_url": format!("http://{authority}/v1/gateway/{}/{}", c.provider, c.id),
                })
            })
            .collect();
        json!({ "connections": connections })
    }

    async fn forward(
        &self,
        client: &mut TcpStream,
        conn: &GatewayConnection,
        req: &ApiRequest,
        url: String,
        body: Vec<u8>,
    ) -> std::io::Result<()> {
        let Ok(method) = reqwest::Method::from_bytes(req.method.as_bytes()) else {
            return respond_with(client, 400, "text/plain", "bad method").await;
        };
        let mut headers = HeaderMap::new();
        for (name, value) in &req.headers {
            if NOT_FORWARDED.contains(&name.as_str()) {
                continue;
            }
            if let (Ok(n), Ok(v)) = (HeaderName::from_bytes(name.as_bytes()), HeaderValue::from_str(value)) {
                headers.append(n, v);
            }
        }
        let credential = match conn.auth {
            AuthScheme::Bearer => {
                HeaderValue::from_str(&format!("Bearer {}", conn.credential.expose())).map(|v| (AUTHORIZATION, v))
            }
            AuthScheme::XApiKey => {
                HeaderValue::from_str(conn.credential.expose()).map(|v| (HeaderName::from_static("x-api-key"), v))
            }
        };
        let Ok((name, mut value)) = credential else {
            tracing::warn!(provider = %conn.provider, id = %conn.id, "the stored credential isn't a valid header value");
            return respond_with(client, 502, "text/plain", "the gateway's credential for this connection is unusable")
                .await;
        };
        value.set_sensitive(true);
        headers.insert(name, value);

        let mut upstream = self.http.request(method.clone(), url).headers(headers);
        if !body.is_empty() {
            upstream = upstream.body(body);
        }
        let mut resp = match upstream.send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(provider = %conn.provider, id = %conn.id, error = %e.without_url(), "upstream request failed");
                return respond_with(client, 502, "text/plain", "couldn't reach the upstream").await;
            }
        };

        let status = resp.status();
        let bodiless = method == reqwest::Method::HEAD || matches!(status.as_u16(), 204 | 304);
        let length = resp.headers().get("content-length").cloned();
        let mut out = format!("HTTP/1.1 {} {}\r\n", status.as_u16(), status.canonical_reason().unwrap_or(""));
        for (name, value) in resp.headers() {
            if matches!(
                name.as_str(),
                "connection" | "keep-alive" | "transfer-encoding" | "te" | "trailer" | "upgrade" | "proxy-connection"
            ) {
                continue;
            }
            out.push_str(name.as_str());
            out.push_str(": ");
            out.push_str(&String::from_utf8_lossy(value.as_bytes()));
            out.push_str("\r\n");
        }
        let chunked = length.is_none() && !bodiless;
        if chunked {
            out.push_str("transfer-encoding: chunked\r\n");
        }
        out.push_str("connection: close\r\n\r\n");
        client.write_all(out.as_bytes()).await?;
        client.flush().await?;
        if bodiless {
            return client.shutdown().await;
        }
        loop {
            match resp.chunk().await {
                Ok(Some(chunk)) if chunk.is_empty() => {}
                Ok(Some(chunk)) => {
                    if chunked {
                        client.write_all(format!("{:x}\r\n", chunk.len()).as_bytes()).await?;
                        client.write_all(&chunk).await?;
                        client.write_all(b"\r\n").await?;
                    } else {
                        client.write_all(&chunk).await?;
                    }
                    client.flush().await?;
                }
                Ok(None) => break,
                // Cut the connection without a terminator so the client sees the stream broke.
                Err(e) => {
                    tracing::warn!(provider = %conn.provider, error = %e.without_url(), "upstream stream broke");
                    return Ok(());
                }
            }
        }
        if chunked {
            client.write_all(b"0\r\n\r\n").await?;
        }
        client.shutdown().await
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::time::Duration;

    use tokio::net::TcpListener;
    use tokio::sync::{Notify, mpsc};
    use trailhead_factory::{GatewayRequest, GatewayResponse, Secret};

    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> Labels {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn worker() -> Labels {
        labels(&[("role", "worker"), ("repo", "lane/trail")])
    }

    fn reviewer() -> Labels {
        labels(&[("role", "reviewer"), ("repo", "lane/trail")])
    }

    /// Reads one request (head and Content-Length body) from `s`.
    async fn read_request(s: &mut TcpStream) -> (String, Vec<u8>) {
        let (head, mut body) = super::super::read_head(s).await.unwrap().unwrap();
        let head = String::from_utf8(head).unwrap();
        let length = head
            .lines()
            .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap()))
            .unwrap_or(0);
        let have = body.len();
        body.resize(length, 0);
        s.read_exact(&mut body[have..]).await.unwrap();
        (head, body)
    }

    /// A fake upstream that answers every request with a small JSON body and reports what it was sent.
    async fn upstream() -> (String, mpsc::UnboundedReceiver<(String, Vec<u8>)>) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = l.accept().await.unwrap();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let seen = read_request(&mut s).await;
                    let _ = tx.send(seen);
                    let body = r#"{"ok":true}"#;
                    let resp = format!(
                        "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = s.write_all(resp.as_bytes()).await;
                });
            }
        });
        (url, rx)
    }

    async fn gateway(labels: Labels) -> (Arc<Gateway>, SocketAddr) {
        let gw = Gateway::new();
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(gw.clone().serve_proxy(listener));
        let r = gw
            .handle(GatewayRequest::Register { name: "a".into(), address: "127.0.0.1".parse().unwrap(), labels })
            .await;
        assert_eq!(r, GatewayResponse::Ok);
        (gw, SocketAddr::from(([127, 0, 0, 1], port)))
    }

    async fn connect(gw: &Gateway, provider: &str, id: &str, upstream: &str, auth: AuthScheme, credential: &str) {
        let r = gw
            .handle(GatewayRequest::SetConnection(GatewayConnection {
                provider: provider.into(),
                id: id.into(),
                upstream: upstream.into(),
                auth,
                credential: Secret::new(credential),
            }))
            .await;
        assert_eq!(r, GatewayResponse::Ok);
    }

    /// Sends a request to the gateway and returns the status and the body.
    async fn call(gw: SocketAddr, method: &str, target: &str, extra: &[&str], body: &str) -> (u16, String) {
        let mut s = TcpStream::connect(gw).await.unwrap();
        let mut req = format!("{method} {target} HTTP/1.1\r\nHost: trailhead-gateway:8080\r\n");
        for h in extra {
            req.push_str(h);
            req.push_str("\r\n");
        }
        if !body.is_empty() {
            req.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        req.push_str("\r\n");
        req.push_str(body);
        s.write_all(req.as_bytes()).await.unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        let status = out[9..12].parse().unwrap();
        let body = out.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
        (status, body)
    }

    async fn list(gw: SocketAddr) -> Vec<(String, String)> {
        let (status, body) = call(gw, "GET", "/v1/gateway/list", &[], "").await;
        assert_eq!(status, 200, "{body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        v["connections"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| (c["provider"].as_str().unwrap().to_string(), c["gateway_base_url"].as_str().unwrap().to_string()))
            .collect()
    }

    #[tokio::test]
    async fn the_list_depends_on_the_role() {
        let (gw, addr) = gateway(worker()).await;
        connect(&gw, "github", "gh1", "https://api.github.com", AuthScheme::Bearer, "ghp_secret").await;
        connect(&gw, "anthropic", "an1", "https://api.anthropic.com", AuthScheme::XApiKey, "sk-secret").await;
        connect(&gw, "mystery", "m1", "https://example.com", AuthScheme::Bearer, "x").await;
        let got = list(addr).await;
        assert_eq!(
            got,
            vec![
                ("anthropic".to_string(), "http://trailhead-gateway:8080/v1/gateway/anthropic/an1".to_string()),
                ("github".to_string(), "http://trailhead-gateway:8080/v1/gateway/github/gh1".to_string()),
            ]
        );

        let register =
            |labels| GatewayRequest::Register { name: "a".into(), address: "127.0.0.1".parse().unwrap(), labels };
        // No role, an unknown role, and a GitHub connection with no repo to scope it to.
        gw.handle(register(Labels::new())).await;
        assert!(list(addr).await.is_empty());
        gw.handle(register(labels(&[("role", "pooled"), ("repo", "lane/trail")]))).await;
        assert!(list(addr).await.is_empty());
        gw.handle(register(labels(&[("role", "reviewer")]))).await;
        assert_eq!(list(addr).await.len(), 1);
        gw.handle(register(reviewer())).await;
        assert_eq!(list(addr).await.len(), 2);

        // Removing the label is the kill switch.
        gw.handle(GatewayRequest::Unregister { name: "a".into() }).await;
        let (status, _) = call(addr, "GET", "/v1/gateway/list", &[], "").await;
        assert_eq!(status, 403);
    }

    #[tokio::test]
    async fn swaps_the_sandboxs_credentials_for_the_stored_one() {
        let (url, mut seen) = upstream().await;
        let (gw, addr) = gateway(worker()).await;
        connect(&gw, "github", "gh1", &url, AuthScheme::Bearer, "ghp_real").await;
        connect(&gw, "anthropic", "an1", &url, AuthScheme::XApiKey, "sk-real").await;

        let (status, body) = call(
            addr,
            "POST",
            "/v1/gateway/github/gh1/repos/lane/trail/git/blobs?x=1",
            &[
                "Authorization: Bearer placeholder",
                "X-Api-Key: placeholder",
                "Cookie: a=b",
                "Accept: application/json",
                "Content-Type: application/json",
            ],
            r#"{"content":"hi"}"#,
        )
        .await;
        assert_eq!((status, body.as_str()), (201, r#"{"ok":true}"#));
        let (head, body) = seen.recv().await.unwrap();
        let lower = head.to_ascii_lowercase();
        assert!(head.starts_with("POST /repos/lane/trail/git/blobs?x=1 HTTP/1.1"), "{head}");
        assert!(lower.contains("authorization: bearer ghp_real"), "{head}");
        assert!(!lower.contains("placeholder") && !lower.contains("x-api-key") && !lower.contains("cookie"), "{head}");
        assert!(lower.contains("accept: application/json"), "{head}");
        assert!(!lower.contains("trailhead-gateway"), "{head}");
        assert_eq!(body, br#"{"content":"hi"}"#);

        let (status, _) = call(
            addr,
            "POST",
            "/v1/gateway/anthropic/an1/v1/messages",
            &[
                "Authorization: Bearer sk-ant-oat-subscription",
                "x-api-key: placeholder",
                "anthropic-version: 2023-06-01",
            ],
            "{}",
        )
        .await;
        assert_eq!(status, 201);
        let (head, _) = seen.recv().await.unwrap();
        let lower = head.to_ascii_lowercase();
        assert!(lower.contains("x-api-key: sk-real"), "{head}");
        assert!(lower.contains("anthropic-version: 2023-06-01"), "{head}");
        assert!(!lower.contains("authorization") && !lower.contains("placeholder"), "{head}");
    }

    #[tokio::test]
    async fn applies_the_path_policy() {
        let (url, mut seen) = upstream().await;
        let (gw, addr) = gateway(worker()).await;
        connect(&gw, "github", "gh1", &url, AuthScheme::Bearer, "t").await;
        let go = |method: &'static str, path: &'static str| async move {
            call(addr, method, &format!("/v1/gateway/github/gh1{path}"), &[], "").await
        };

        for (m, p) in [
            ("POST", "/repos/lane/trail/git/trees"),
            ("PATCH", "/repos/lane/trail/git/refs/heads/x"),
            ("GET", "/repos/lane/trail/git/ref/heads/x"),
            ("POST", "/repos/lane/trail/pulls"),
            ("GET", "/repos/lane/trail/pulls/3/files"),
            ("POST", "/repos/lane/trail/issues/3/comments"),
        ] {
            assert_eq!(go(m, p).await.0, 201, "{m} {p}");
            seen.recv().await.unwrap();
        }
        for (m, p) in [
            ("PUT", "/repos/lane/trail/pulls/3/merge"),
            ("DELETE", "/repos/lane/trail/git/refs/heads/x"),
            ("GET", "/repos/lane/other/git/trees"),
            ("GET", "/repos/lane/trail"),
            ("GET", "/repos/lane/trail-evil/pulls"),
            ("GET", "/user"),
            ("GET", "/repos/lane/trail/pulls/../../other/pulls"),
            ("GET", "/repos/lane/trail/pulls/%2e%2e/x"),
        ] {
            let (status, why) = go(m, p).await;
            assert_eq!(status, 403, "{m} {p}");
            assert!(!why.is_empty());
        }
        assert_eq!(go("GET", "/repos/lane/trail/pulls").await.0, 201);
        assert_eq!(call(addr, "GET", "/v1/gateway/github/nope/user", &[], "").await.0, 404);
        assert_eq!(call(addr, "GET", "/v2/other", &[], "").await.0, 404);
        // Nothing refused reached the upstream.
        seen.recv().await.unwrap();
        assert!(seen.try_recv().is_err());

        gw.handle(GatewayRequest::Register {
            name: "a".into(),
            address: "127.0.0.1".parse().unwrap(),
            labels: reviewer(),
        })
        .await;
        for (m, p) in [
            ("GET", "/repos/lane/trail/pulls/3/files"),
            ("GET", "/repos/lane/trail/contents/README.md"),
            ("POST", "/repos/lane/trail/issues/3/comments"),
            ("POST", "/repos/lane/trail/pulls/3/reviews"),
        ] {
            assert_eq!(go(m, p).await.0, 201, "{m} {p}");
        }
        for (m, p) in [
            ("POST", "/repos/lane/trail/pulls"),
            ("POST", "/repos/lane/trail/git/blobs"),
            ("PUT", "/repos/lane/trail/pulls/3/merge"),
            ("PATCH", "/repos/lane/trail/pulls/3"),
            ("POST", "/repos/lane/trail/issues"),
        ] {
            assert_eq!(go(m, p).await.0, 403, "{m} {p}");
        }
    }

    #[tokio::test]
    async fn no_role_means_no_access() {
        let (url, mut seen) = upstream().await;
        let (gw, addr) = gateway(labels(&[("repo", "lane/trail")])).await;
        connect(&gw, "github", "gh1", &url, AuthScheme::Bearer, "t").await;
        let (status, why) = call(addr, "GET", "/v1/gateway/github/gh1/repos/lane/trail/pulls", &[], "").await;
        assert_eq!(status, 403);
        assert!(why.contains("no role"), "{why}");
        assert!(seen.try_recv().is_err());
    }

    #[tokio::test]
    async fn streams_a_chunked_response_as_it_arrives() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let gate = Arc::new(Notify::new());
        let g = gate.clone();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            read_request(&mut s).await;
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await
                .unwrap();
            s.write_all(b"6\r\nfirst \r\n").await.unwrap();
            s.flush().await.unwrap();
            g.notified().await;
            s.write_all(b"6\r\nsecond\r\n0\r\n\r\n").await.unwrap();
        });
        let (gw, addr) = gateway(worker()).await;
        connect(&gw, "anthropic", "an1", &url, AuthScheme::XApiKey, "k").await;

        let mut s = TcpStream::connect(addr).await.unwrap();
        s.write_all(b"POST /v1/gateway/anthropic/an1/v1/messages HTTP/1.1\r\nHost: g\r\nContent-Length: 2\r\n\r\n{}")
            .await
            .unwrap();
        // The first chunk arrives while the upstream is still holding the second back.
        let mut got = Vec::new();
        let mut buf = [0u8; 256];
        tokio::time::timeout(Duration::from_secs(5), async {
            while !String::from_utf8_lossy(&got).contains("first ") {
                let n = s.read(&mut buf).await.unwrap();
                assert!(n > 0, "closed early");
                got.extend_from_slice(&buf[..n]);
            }
        })
        .await
        .expect("the first chunk wasn't streamed");
        assert!(!String::from_utf8_lossy(&got).contains("second"));
        gate.notify_one();
        let mut rest = String::new();
        s.read_to_string(&mut rest).await.unwrap();
        let all = format!("{}{rest}", String::from_utf8_lossy(&got));
        assert!(all.starts_with("HTTP/1.1 200"), "{all}");
        assert!(all.to_ascii_lowercase().contains("transfer-encoding: chunked"), "{all}");
        assert!(all.contains("second") && all.ends_with("0\r\n\r\n"), "{all}");
    }

    #[tokio::test]
    async fn credentials_are_redacted_and_kept_off_the_state() {
        let gw = Gateway::new();
        let conn = GatewayConnection {
            provider: "github".into(),
            id: "gh1".into(),
            upstream: "https://api.github.com".into(),
            auth: AuthScheme::Bearer,
            credential: Secret::new("ghp_hunter2"),
        };
        let req = GatewayRequest::SetConnection(conn);
        assert!(!format!("{req:?}").contains("hunter2"));
        assert_eq!(gw.handle(req).await, GatewayResponse::Ok);
        let state = gw.state();
        assert_eq!(state.connections.len(), 1);
        assert!(!format!("{state:?}").contains("hunter2"));
        assert!(!serde_json::to_string(&state).unwrap().contains("hunter2"));

        let bad = GatewayConnection {
            provider: "a/b".into(),
            id: "x".into(),
            upstream: "https://x".into(),
            auth: AuthScheme::Bearer,
            credential: Secret::new("s"),
        };
        assert!(matches!(gw.handle(GatewayRequest::SetConnection(bad)).await, GatewayResponse::Error { .. }));
        gw.handle(GatewayRequest::RemoveConnection { provider: "github".into(), id: "gh1".into() }).await;
        assert!(gw.state().connections.is_empty());
    }
}
