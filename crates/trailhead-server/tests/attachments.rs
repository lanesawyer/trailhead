mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use trailhead_client::creds::Cred;
use trailhead_core::MAX_ATTACHMENT_BYTES;
use trailhead_server::{AppState, open_local, router};

struct Api {
    app: Router,
    token: String,
    _dir: tempfile::TempDir,
}

struct Res {
    status: StatusCode,
    headers: HeaderMap,
    bytes: Vec<u8>,
}

impl Res {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.bytes).unwrap_or(Value::Null)
    }
}

/// Bytes the engine takes for a PNG: its signature and header chunk, then `fill`.
fn png(fill: u8, len: usize) -> Vec<u8> {
    let mut b = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0, 0, 13];
    b.extend(b"IHDR");
    b.extend(16u32.to_be_bytes());
    b.extend(9u32.to_be_bytes());
    b.extend([8, 6, 0, 0, 0]);
    b.resize(len.max(b.len()), fill);
    b
}

impl Api {
    async fn new() -> Api {
        let dir = tempfile::tempdir().unwrap();
        let cfg = common::local_config(dir.path());
        let (store, _) = open_local(&cfg).await.unwrap();
        let token = std::fs::read_to_string(cfg.creds.file(Cred::User)).unwrap().trim().to_string();
        let api = Api { app: router(Arc::new(AppState::new(store))), token, _dir: dir };
        let project = json!({"key": "AUTH", "name": "Auth"}).to_string();
        assert_eq!(api.send("POST", "/v1/projects", Some("application/json"), project.into(), &[]).await.status, 201);
        let task = json!({"project": "AUTH", "title": "Draw it"}).to_string();
        assert_eq!(api.send("POST", "/v1/tasks", Some("application/json"), task.into(), &[]).await.status, 201);
        api
    }

    async fn send(
        &self,
        method: &str,
        path: &str,
        content_type: Option<&str>,
        body: Vec<u8>,
        extra: &[(&str, &str)],
    ) -> Res {
        let mut req =
            Request::builder().method(method).uri(path).header(header::AUTHORIZATION, format!("Bearer {}", self.token));
        if let Some(ct) = content_type {
            req = req.header(header::CONTENT_TYPE, ct);
        }
        for (k, v) in extra {
            req = req.header(*k, *v);
        }
        let res = self.app.clone().oneshot(req.body(Body::from(body)).unwrap()).await.unwrap();
        let (status, headers) = (res.status(), res.headers().clone());
        Res { status, headers, bytes: res.into_body().collect().await.unwrap().to_bytes().to_vec() }
    }

    async fn upload(&self, query: &str, body: Vec<u8>, extra: &[(&str, &str)]) -> Res {
        self.send("POST", &format!("/v1/tasks/AUTH-1/attachments?{query}"), Some("image/png"), body, extra).await
    }
}

#[tokio::test]
async fn a_picture_goes_up_raw_and_comes_back_cacheable() {
    let api = Api::new().await;
    let bytes = png(7, 1_500_000);
    let up = api.upload("name=board-dark&role=after&set=run%207", bytes.clone(), &[]).await;
    assert_eq!(up.status, StatusCode::CREATED, "{}", up.json());
    let a = up.json();
    assert_eq!(
        (a["name"].as_str(), a["role"].as_str(), a["set"].as_str()),
        (Some("board-dark"), Some("after"), Some("run 7"))
    );
    assert_eq!((a["size"].as_i64(), a["width"].as_i64(), a["height"].as_i64()), (Some(1_500_000), Some(16), Some(9)));

    let list = api.send("GET", "/v1/tasks/AUTH-1/attachments", None, Vec::new(), &[]).await;
    assert_eq!(list.json(), json!([a]));
    let tasks = api.send("GET", "/v1/tasks?include=attachments", None, Vec::new(), &[]).await;
    assert_eq!(tasks.json()[0]["attachments"], json!([a]));
    let detail = api.send("GET", "/v1/tasks/AUTH-1", None, Vec::new(), &[]).await;
    assert_eq!(detail.json()["attachments"], json!([a]));

    let path = format!("/v1/attachments/{}", a["id"].as_str().unwrap());
    let read = api.send("GET", &path, None, Vec::new(), &[]).await;
    assert_eq!(read.status, StatusCode::OK);
    assert_eq!(read.bytes, bytes);
    assert_eq!(read.headers[header::CONTENT_TYPE], "image/png");
    let etag = format!("\"{}\"", a["sha256"].as_str().unwrap());
    assert_eq!(read.headers[header::ETAG], etag.as_str());
    assert!(read.headers[header::CACHE_CONTROL].to_str().unwrap().contains("immutable"));
    let again = api.send("GET", &path, None, Vec::new(), &[("if-none-match", &etag)]).await;
    assert_eq!(again.status, StatusCode::NOT_MODIFIED);
    assert!(again.bytes.is_empty());

    let removed = api.send("DELETE", &path, None, Vec::new(), &[]).await;
    assert_eq!(removed.status, StatusCode::OK);
    assert!(removed.json()["deleted_at"].is_string());
    assert_eq!(api.send("GET", &path, None, Vec::new(), &[]).await.status, StatusCode::NOT_FOUND);
    let list = api.send("GET", "/v1/tasks/AUTH-1/attachments", None, Vec::new(), &[]).await;
    assert_eq!(list.json(), json!([]));
}

#[tokio::test]
async fn a_bad_upload_is_refused_with_the_apis_error() {
    let api = Api::new().await;
    let too_big = api.upload("name=big", png(1, MAX_ATTACHMENT_BYTES + 1), &[]).await;
    assert_eq!(too_big.status, StatusCode::BAD_REQUEST);
    assert_eq!(too_big.json()["error"]["code"], "invalid");
    let keyed = api.upload("name=big", png(1, MAX_ATTACHMENT_BYTES + 1), &[("idempotency-key", "big")]).await;
    assert_eq!(keyed.json()["error"]["code"], "invalid");

    let not_png = api.upload("name=shot", b"<svg>not a png, whatever the header says</svg>".to_vec(), &[]).await;
    assert_eq!(not_png.json()["error"]["code"], "invalid", "{}", not_png.json());
    let jpeg = api.send("POST", "/v1/tasks/AUTH-1/attachments?name=shot", Some("image/jpeg"), png(1, 64), &[]).await;
    assert_eq!(jpeg.json()["error"]["code"], "invalid");
    assert_eq!(api.upload("role=after", png(1, 64), &[]).await.json()["error"]["code"], "invalid");
    assert_eq!(api.upload("name=x&role=during", png(1, 64), &[]).await.json()["error"]["code"], "invalid");
    let missing = api.send("POST", "/v1/tasks/AUTH-9/attachments?name=x", Some("image/png"), png(1, 64), &[]).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_upload_with_a_key_is_made_once() {
    let api = Api::new().await;
    let key = [("idempotency-key", "shot-1")];
    let first = api.upload("name=board", png(3, 2048), &key).await;
    assert_eq!(first.status, StatusCode::CREATED);
    let again = api.upload("name=board", png(3, 2048), &key).await;
    assert_eq!(again.headers["idempotent-replayed"], "true");
    assert_eq!(again.json(), first.json());
    let list = api.send("GET", "/v1/tasks/AUTH-1/attachments", None, Vec::new(), &[]).await;
    assert_eq!(list.json().as_array().unwrap().len(), 1);
    let other = api.upload("name=board", png(4, 2048), &key).await;
    assert_eq!(other.json()["error"]["code"], "idempotency_key_reused");
}
