mod common;

use common::Env;
use trailhead_core::{Error, StoredResponse, idempotency_fingerprint};

fn response(body: &str) -> StoredResponse {
    StoredResponse { status: 201, headers: vec![("etag".into(), "\"1\"".into())], body: body.as_bytes().to_vec() }
}

#[tokio::test]
async fn second_request_while_first_runs_is_in_flight() {
    let env = Env::new().await;
    let fp = idempotency_fingerprint("POST", "/v1/tasks", b"{}");
    assert_eq!(env.store.idempotency_begin(&env.human, "k", &fp).await.unwrap(), None);
    let err = env.store.idempotency_begin(&env.human, "k", &fp).await.unwrap_err();
    assert!(matches!(err, Error::IdempotencyKeyInFlight), "{err}");
    let other = idempotency_fingerprint("POST", "/v1/tasks", b"{\"x\":1}");
    let err = env.store.idempotency_begin(&env.human, "k", &other).await.unwrap_err();
    assert!(matches!(err, Error::IdempotencyKeyReused), "{err}");

    env.store.idempotency_complete(&env.human, "k", &response("done")).await.unwrap();
    assert_eq!(env.store.idempotency_begin(&env.human, "k", &fp).await.unwrap(), Some(response("done")));
}

#[tokio::test]
async fn abandoned_and_stale_reservations_free_the_key() {
    let env = Env::new().await;
    let fp = idempotency_fingerprint("POST", "/v1/tasks", b"{}");
    env.store.idempotency_begin(&env.human, "k", &fp).await.unwrap();
    env.store.idempotency_abandon(&env.human, "k").await.unwrap();
    assert_eq!(env.store.idempotency_begin(&env.human, "k", &fp).await.unwrap(), None);

    env.advance(6 * 60);
    assert_eq!(env.store.idempotency_begin(&env.human, "k", &fp).await.unwrap(), None);
}

#[tokio::test]
async fn keys_expire_after_a_day() {
    let env = Env::new().await;
    let fp = idempotency_fingerprint("POST", "/v1/tasks", b"{}");
    env.store.idempotency_begin(&env.human, "k", &fp).await.unwrap();
    env.store.idempotency_complete(&env.human, "k", &response("old")).await.unwrap();
    env.advance(23 * 3600);
    assert_eq!(env.store.idempotency_begin(&env.human, "k", &fp).await.unwrap(), Some(response("old")));
    env.advance(2 * 3600);
    let other = idempotency_fingerprint("POST", "/v1/projects", b"{}");
    assert_eq!(env.store.idempotency_begin(&env.human, "k", &other).await.unwrap(), None);
}
