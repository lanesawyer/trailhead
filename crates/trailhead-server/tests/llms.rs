mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use tower::ServiceExt;
use trailhead_server::{AppState, open_local, router};

async fn get(app: &Router, path: &str) -> (StatusCode, String, String) {
    let res = app.clone().oneshot(Request::builder().uri(path).body(Body::empty()).unwrap()).await.unwrap();
    let status = res.status();
    let content_type = res.headers()[header::CONTENT_TYPE].to_str().unwrap().to_string();
    let body = String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    (status, content_type, body)
}

#[tokio::test]
async fn llms_txt_is_public_and_lists_every_error_code() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = common::local_config(dir.path());
    let (store, _) = open_local(&cfg).await.unwrap();
    let app = router(Arc::new(AppState::new(store)));

    let (status, content_type, text) = get(&app, "/llms.txt").await;
    assert_eq!(status, StatusCode::OK, "served without a token");
    assert!(content_type.starts_with("text/plain"), "{content_type}");
    assert!(text.starts_with("# Trailhead\n\n> "), "llms.txt opens with an H1 and a summary blockquote");

    let (_, _, doc) = get(&app, "/v1/openapi.json").await;
    let doc: serde_json::Value = serde_json::from_str(&doc).unwrap();
    let codes = doc["components"]["schemas"]["ErrorDetail"]["properties"]["code"]["enum"].as_array().unwrap();
    assert!(!codes.is_empty());
    for code in codes {
        let code = code.as_str().unwrap();
        assert!(text.contains(&format!("- `{code}` ")), "llms.txt doesn't document error code {code}");
    }
}
