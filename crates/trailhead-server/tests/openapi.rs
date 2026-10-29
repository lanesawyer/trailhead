mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use trailhead_server::{AppState, open_local, router};

/// Every (method, path) registered in `routes::v1()`. Add new routes here.
const ROUTES: &[(&str, &str)] = &[
    ("get", "/v1/health"),
    ("get", "/v1/openapi.json"),
    ("get", "/v1/me"),
    ("get", "/v1/projects"),
    ("post", "/v1/projects"),
    ("get", "/v1/projects/{project}"),
    ("patch", "/v1/projects/{project}"),
    ("get", "/v1/tasks"),
    ("post", "/v1/tasks"),
    ("get", "/v1/tasks/{task}"),
    ("patch", "/v1/tasks/{task}"),
    ("delete", "/v1/tasks/{task}"),
    ("post", "/v1/tasks/{task}/claim"),
    ("post", "/v1/tasks/{task}/renew"),
    ("post", "/v1/tasks/{task}/release"),
    ("get", "/v1/tasks/{task}/comments"),
    ("post", "/v1/tasks/{task}/comments"),
    ("get", "/v1/tasks/{task}/blockers"),
    ("post", "/v1/tasks/{task}/blockers"),
    ("delete", "/v1/tasks/{task}/blockers/{blocker}"),
    ("get", "/v1/tasks/{task}/history"),
    ("post", "/v1/comments/{id}/resolve"),
    ("get", "/v1/ready"),
    ("get", "/v1/search"),
    ("get", "/v1/events"),
    ("get", "/v1/events/stream"),
    ("post", "/v1/undo"),
    ("post", "/v1/revert"),
    ("get", "/v1/agents"),
    ("post", "/v1/agents"),
    ("get", "/v1/claims"),
    ("post", "/v1/agents/{agent}/revoke"),
    ("get", "/v1/tokens"),
    ("post", "/v1/tokens"),
    ("get", "/v1/webhooks"),
    ("post", "/v1/webhooks"),
    ("delete", "/v1/webhooks/{id}"),
    ("get", "/v1/webhooks/{id}/deliveries"),
    ("delete", "/v1/tokens/{id}"),
    ("patch", "/v1/tokens/{id}"),
];

async fn fetch_doc() -> Value {
    let dir = tempfile::tempdir().unwrap();
    let cfg = common::local_config(dir.path());
    let (store, _) = open_local(&cfg).await.unwrap();
    let app = router(Arc::new(AppState::new(store)));
    let req = Request::builder().uri("/v1/openapi.json").body(Body::empty()).unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "served without a token");
    assert_eq!(res.headers()["content-type"], "application/json");
    serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[tokio::test]
async fn openapi_documents_every_route() {
    let doc = fetch_doc().await;
    assert!(doc["openapi"].as_str().unwrap().starts_with("3."));
    let paths = doc["paths"].as_object().unwrap();
    for (method, path) in ROUTES {
        assert!(paths.get(*path).and_then(|p| p.get(*method)).is_some(), "{method} {path} is not documented");
    }
    let documented: usize = paths.values().map(|p| p.as_object().unwrap().len()).sum();
    assert_eq!(documented, ROUTES.len(), "documented operations not in ROUTES");
}

#[tokio::test]
async fn openapi_describes_auth_errors_and_concurrency() {
    let doc = fetch_doc().await;
    assert_eq!(doc["components"]["securitySchemes"]["bearer"]["scheme"], "bearer");
    assert_eq!(doc["security"][0]["bearer"], serde_json::json!([]));
    assert_eq!(doc["paths"]["/v1/health"]["get"]["security"], serde_json::json!([{}]));

    let codes = &doc["components"]["schemas"]["ErrorDetail"]["properties"]["code"]["enum"];
    for code in ["claim_held", "version_conflict", "blocked_by", "not_found", "unauthorized"] {
        assert!(codes.as_array().unwrap().iter().any(|c| c == code), "{code} missing");
    }

    let patch = &doc["paths"]["/v1/tasks/{task}"]["patch"];
    let params = patch["parameters"].as_array().unwrap();
    assert!(params.iter().any(|p| p["name"] == "If-Match" && p["in"] == "header"));
    assert!(patch["responses"]["412"].is_object());
    assert!(patch["responses"]["200"]["headers"]["ETag"].is_object());
    assert!(patch["responses"]["200"]["headers"]["trailhead-batch"].is_object());
    assert!(patch["responses"]["401"].is_object());

    let claim = &doc["paths"]["/v1/tasks/{task}/claim"]["post"]["responses"]["200"];
    assert_eq!(claim["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/ClaimResult");
    let task = &doc["components"]["schemas"]["Task"]["properties"];
    assert_eq!(task["id"]["format"], "uuid");
    assert!(task["created_at"]["format"] == "date-time", "{}", task["created_at"]);
    assert!(doc["paths"]["/v1/events/stream"]["get"]["responses"]["200"]["content"]["text/event-stream"].is_object());
}

#[tokio::test]
async fn writes_document_idempotency_keys() {
    let doc = fetch_doc().await;
    let post = &doc["paths"]["/v1/tasks"]["post"];
    let params: Vec<&str> = post["parameters"].as_array().unwrap().iter().filter_map(|p| p["name"].as_str()).collect();
    assert!(params.contains(&"Idempotency-Key"), "{params:?}");
    assert!(post["responses"]["422"].is_object());
    assert!(post["responses"]["429"].is_object());
    let get = &doc["paths"]["/v1/ready"]["get"];
    assert!(get["parameters"].as_array().is_none_or(|p| p.iter().all(|p| p["name"] != "Idempotency-Key")));
}

#[tokio::test]
async fn task_list_schema_describes_included_claims() {
    let doc = fetch_doc().await;
    let schemas = &doc["components"]["schemas"];
    let listed = serde_json::to_string(&schemas["ListedTask"]).unwrap();
    assert!(listed.contains("#/components/schemas/Task"), "{listed}");
    assert!(listed.contains("#/components/schemas/NamedClaim"), "{listed}");
    let named = serde_json::to_string(&schemas["NamedClaim"]).unwrap();
    assert!(named.contains("\"holder\"") && named.contains("#/components/schemas/Claim"), "{named}");
}

#[tokio::test]
async fn update_task_documents_a_nullable_grade() {
    let doc = fetch_doc().await;
    let grade = &doc["components"]["schemas"]["UpdateTask"]["properties"]["grade"];
    let text = serde_json::to_string(grade).unwrap();
    assert!(text.contains("integer") && text.contains("null"), "{text}");
    let create = &doc["components"]["schemas"]["CreateTask"]["properties"]["grade"];
    assert!(create.is_object(), "{create}");
    let params = doc["paths"]["/v1/ready"]["get"]["parameters"].as_array().unwrap();
    assert!(params.iter().any(|p| p["name"] == "grade"));
}
