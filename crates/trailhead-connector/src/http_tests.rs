use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect};
use axum::routing::get;

use crate::fixtures::GITHUB;
use crate::{Http, HttpError, Manifest, Recorded, RetryPolicy};

fn manifest(host: &str) -> Manifest {
    Manifest::parse(&GITHUB.replace("api.github.com", host)).unwrap()
}

const FAST: RetryPolicy = RetryPolicy { max_tries: 3, base: Duration::from_millis(1), cap: Duration::from_millis(20) };

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://127.0.0.1:{}", addr.port())
}

fn client() -> Http {
    Http::new_allowing_plain_http(&manifest("127.0.0.1")).with_retry(FAST)
}

#[tokio::test]
async fn sends_user_agent_auth_and_query() {
    async fn echo(headers: HeaderMap, axum::extract::RawQuery(q): axum::extract::RawQuery) -> String {
        let h = |n: &str| headers.get(n).map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
        format!("{}|{}|{}", h("user-agent"), h("authorization"), q.unwrap_or_default())
    }
    let base = serve(Router::new().route("/echo", get(echo))).await;
    let http = client();
    let body = http
        .get(&format!("{base}/echo"))
        .bearer("tok")
        .query("state", "open")
        .query("per_page", 50)
        .send()
        .await
        .unwrap()
        .text();
    let agent = format!("trailhead-connector/{} (github)", env!("CARGO_PKG_VERSION"));
    assert_eq!(body, format!("{agent}|Bearer tok|state=open&per_page=50"));
    let basic = http.get(&format!("{base}/echo")).basic("a", "b").send().await.unwrap().text();
    assert!(basic.contains("|Basic YTpi|"), "{basic}");
}

#[tokio::test]
async fn refuses_undeclared_hosts_and_plain_http() {
    let real = Http::new(&manifest("api.github.com")).unwrap();
    let e = real.get("https://evil.example.com/x").send().await.unwrap_err();
    assert!(matches!(e, HttpError::HostNotAllowed { ref host, .. } if host == "evil.example.com"), "{e}");
    assert!(matches!(real.get("http://api.github.com/x").send().await, Err(HttpError::NotHttps(_))));
    assert!(matches!(real.get("not a url").send().await, Err(HttpError::InvalidUrl(_))));
    assert!(matches!(
        real.get("https://api.github.com.evil.example/x").send().await,
        Err(HttpError::HostNotAllowed { .. })
    ));
}

#[tokio::test]
async fn does_not_follow_a_redirect_to_an_undeclared_host() {
    let base = serve(
        Router::new()
            .route("/out", get(|| async { Redirect::temporary("http://localhost:1/secret") }))
            .route("/in", get(|| async { Redirect::temporary("/ok") }))
            .route("/ok", get(|| async { "fine" })),
    )
    .await;
    let http = client();
    let e = http.get(&format!("{base}/out")).send().await.unwrap_err();
    assert!(matches!(e, HttpError::Transport { .. }), "{e}");
    assert!(e.to_string().contains("redirect"), "{e}");
    assert_eq!(http.get(&format!("{base}/in")).send().await.unwrap().text(), "fine");
}

#[tokio::test]
async fn retries_5xx_and_429_with_a_capped_retry_after() {
    #[derive(Clone)]
    struct Hits(Arc<AtomicUsize>);
    async fn flaky(State(h): State<Hits>) -> impl IntoResponse {
        match h.0.fetch_add(1, Ordering::SeqCst) {
            0 => (StatusCode::SERVICE_UNAVAILABLE, HeaderMap::new(), "down").into_response(),
            1 => {
                let mut headers = HeaderMap::new();
                headers.insert(header::RETRY_AFTER, "3600".parse().unwrap());
                (StatusCode::TOO_MANY_REQUESTS, headers, "slow down").into_response()
            }
            _ => "up".into_response(),
        }
    }
    let hits = Hits(Arc::default());
    let base = serve(Router::new().route("/flaky", get(flaky)).with_state(hits.clone())).await;
    let started = std::time::Instant::now();
    let r = client().get(&format!("{base}/flaky")).send().await.unwrap();
    assert_eq!((r.status, r.text().as_str()), (200, "up"));
    assert_eq!(hits.0.load(Ordering::SeqCst), 3);
    assert!(started.elapsed() < Duration::from_secs(5), "a Retry-After of an hour must be capped");
}

#[tokio::test]
async fn gives_up_after_the_last_try_and_does_not_retry_a_4xx() {
    let hits = Arc::new(AtomicUsize::new(0));
    let (a, b) = (hits.clone(), hits.clone());
    let base = serve(
        Router::new()
            .route(
                "/down",
                get(move || {
                    let a = a.clone();
                    async move {
                        a.fetch_add(1, Ordering::SeqCst);
                        StatusCode::BAD_GATEWAY
                    }
                }),
            )
            .route(
                "/missing",
                get(move || {
                    let b = b.clone();
                    async move {
                        b.fetch_add(100, Ordering::SeqCst);
                        (StatusCode::NOT_FOUND, "no such repo")
                    }
                }),
            ),
    )
    .await;
    let http = client();
    let r = http.get(&format!("{base}/down")).send().await.unwrap();
    assert_eq!(r.status, 502);
    assert_eq!(hits.load(Ordering::SeqCst), 3);
    let e = http.get(&format!("{base}/missing")).send().await.unwrap().error_for_status().unwrap_err();
    assert!(e.to_string().contains("404") && e.to_string().contains("no such repo"), "{e}");
    assert_eq!(hits.load(Ordering::SeqCst), 103);
}

#[tokio::test]
async fn conditional_requests_report_not_modified() {
    async fn thing(headers: HeaderMap) -> impl IntoResponse {
        if headers.get("if-none-match").is_some_and(|v| v == "\"v1\"") {
            return StatusCode::NOT_MODIFIED.into_response();
        }
        ([(header::ETAG, "\"v1\"")], axum::Json(serde_json::json!({"n": 1}))).into_response()
    }
    let base = serve(Router::new().route("/thing", get(thing))).await;
    let http = client();
    let first = http.get(&format!("{base}/thing")).send().await.unwrap();
    assert!(!first.not_modified());
    assert_eq!(first.json::<serde_json::Value>().unwrap()["n"], 1);
    let etag = first.etag().unwrap().to_string();
    let again = http.get(&format!("{base}/thing")).if_none_match(&etag).send().await.unwrap();
    assert!(again.not_modified());
    again.error_for_status().unwrap();
}

#[tokio::test]
async fn follows_link_headers_through_pages() {
    async fn page(axum::extract::Path(n): axum::extract::Path<u32>, headers: HeaderMap) -> impl IntoResponse {
        let host = headers.get(header::HOST).unwrap().to_str().unwrap().to_string();
        let mut out = HeaderMap::new();
        if n < 3 {
            let link = format!("<http://{host}/p/{}>; rel=\"next\", <http://{host}/p/9>; rel=\"last\"", n + 1);
            out.insert(header::LINK, link.parse().unwrap());
        }
        (out, axum::Json(vec![n]))
    }
    let base = serve(Router::new().route("/p/{n}", get(page))).await;
    let http = client();
    let pages = http.pages(http.get(&format!("{base}/p/1")), 10).await.unwrap();
    let got: Vec<Vec<u32>> = pages.iter().map(|p| p.json().unwrap()).collect();
    assert_eq!(got, [[1], [2], [3]]);
    assert_eq!(http.pages(http.get(&format!("{base}/p/1")), 2).await.unwrap().len(), 2);
}

#[test]
fn next_link_parsing() {
    let link = |h: &str| {
        let mut r = Recorded::new("GET", "https://api.github.com/a/b?page=1", 200, "");
        r = r.with_header("Link", h);
        r.into_response().unwrap().next_link()
    };
    assert_eq!(link("<https://x.test/n?page=2>; rel=\"next\"").as_deref(), Some("https://x.test/n?page=2"));
    assert_eq!(
        link("<https://x.test/p>; rel=\"prev\", <https://x.test/n>; rel=\"next\"").as_deref(),
        Some("https://x.test/n")
    );
    assert_eq!(link("</rel?page=2>; rel=next").as_deref(), Some("https://api.github.com/rel?page=2"));
    assert_eq!(link("<https://x.test/p>; rel=\"prev\""), None);
}

fn recorded(url: &str, body: &str) -> Recorded {
    Recorded::new("GET", url, 200, body)
}

#[tokio::test]
async fn replay_answers_from_recordings_and_fails_loudly_otherwise() {
    let m = manifest("api.github.com");
    let http = Http::replay(
        &m,
        vec![
            recorded("https://api.github.com/user", "{\"login\":\"lane\"}").with_header("ETag", "\"a\""),
            Recorded::new("GET", "https://api.github.com/seq", 200, "first"),
            Recorded::new("GET", "https://api.github.com/seq", 304, ""),
        ],
    )
    .unwrap();
    let r = http.get("https://api.github.com/user").bearer("x").send().await.unwrap();
    assert_eq!(r.json::<serde_json::Value>().unwrap()["login"], "lane");
    assert_eq!(r.etag(), Some("\"a\""));
    assert_eq!(http.get("https://api.github.com/seq").send().await.unwrap().text(), "first");
    assert!(http.get("https://api.github.com/seq").send().await.unwrap().not_modified());
    assert!(http.get("https://api.github.com/seq").send().await.unwrap().not_modified());

    let e = http.get("https://api.github.com/other").send().await.unwrap_err();
    assert!(matches!(e, HttpError::NotRecorded { .. }));
    assert!(e.to_string().contains("GET https://api.github.com/other") && e.to_string().contains("/user"), "{e}");
    assert!(http.post("https://api.github.com/user").send().await.is_err());
}

#[tokio::test]
async fn replay_still_enforces_the_host_and_scheme_rules() {
    let http = Http::replay(
        &manifest("api.github.com"),
        vec![recorded("https://evil.example.com/x", "x"), recorded("http://api.github.com/x", "x")],
    )
    .unwrap();
    assert!(matches!(http.get("https://evil.example.com/x").send().await, Err(HttpError::HostNotAllowed { .. })));
    assert!(matches!(http.get("http://api.github.com/x").send().await, Err(HttpError::NotHttps(_))));
}

#[tokio::test]
async fn replay_retries_recorded_failures() {
    let m = manifest("api.github.com");
    let http = Http::replay(
        &m,
        vec![Recorded::new("GET", "https://api.github.com/r", 503, ""), recorded("https://api.github.com/r", "ok")],
    )
    .unwrap()
    .with_retry(FAST);
    assert_eq!(http.get("https://api.github.com/r").send().await.unwrap().text(), "ok");
}

#[tokio::test]
async fn records_then_replays_a_directory() {
    async fn data() -> impl IntoResponse {
        ([(header::ETAG, "\"v9\""), (header::SET_COOKIE, "session=secret")], axum::Json(serde_json::json!({"n": 7})))
    }
    let base = serve(Router::new().route("/data", get(data)).route("/text", get(|| async { "plain" }))).await;
    let dir = tempfile::tempdir().unwrap();
    let recording = client().record_to(dir.path());
    let url = format!("{base}/data");
    recording.get(&url).send().await.unwrap();
    recording.get(&format!("{base}/text")).send().await.unwrap();

    let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert_eq!(files.len(), 2);
    for f in files {
        assert!(!std::fs::read_to_string(f.unwrap().path()).unwrap().contains("secret"));
    }

    let m = manifest("127.0.0.1");
    let replay = Http::replay_dir(&m, dir.path()).unwrap();
    let allowing = replay.allowing_plain_http();
    let r = allowing.get(&url).send().await.unwrap();
    assert_eq!((r.json::<serde_json::Value>().unwrap()["n"].as_i64(), r.etag()), (Some(7), Some("\"v9\"")));
    assert_eq!(allowing.get(&format!("{base}/text")).send().await.unwrap().text(), "plain");
}

#[test]
fn replay_dir_reads_hand_written_files_and_rejects_bad_ones() {
    let dir = tempfile::tempdir().unwrap();
    let m = manifest("api.github.com");
    std::fs::write(
        dir.path().join("a.json"),
        r#"[{"url": "https://api.github.com/a", "json": {"x": 1}}, {"url": "https://api.github.com/b", "status": 404, "body": "gone"}]"#,
    )
    .unwrap();
    assert!(Http::replay_dir(&m, dir.path()).is_ok());
    std::fs::write(dir.path().join("b.json"), r#"{"url": "https://api.github.com/c", "colour": 1}"#).unwrap();
    let e = Http::replay_dir(&m, dir.path()).unwrap_err().to_string();
    assert!(e.contains("b.json") && e.contains("colour"), "{e}");
}

#[test]
fn replay_entry_with_body_and_json_is_refused() {
    let both =
        Recorded { json: Some(serde_json::json!(1)), ..Recorded::new("GET", "https://api.github.com/a", 200, "x") };
    assert!(both.into_response().is_err());
}
