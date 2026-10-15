use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use trailhead_core::{CommentKind, Error, NewComment, NewProject, NewTask, webhook_signature};
use trailhead_server::creds::CredStore;
use trailhead_server::{AppState, LocalConfig, open_local, webhooks};

#[derive(Default)]
struct Receiver {
    status: AtomicU16,
    got: Mutex<Vec<(HeaderMap, Bytes)>>,
}

async fn receive(State(r): State<Arc<Receiver>>, headers: HeaderMap, body: Bytes) -> StatusCode {
    r.got.lock().unwrap().push((headers, body));
    StatusCode::from_u16(r.status.load(Ordering::SeqCst)).unwrap()
}

async fn receiver() -> (String, Arc<Receiver>) {
    let r = Arc::new(Receiver { status: AtomicU16::new(200), ..Default::default() });
    let app = Router::new().route("/hook", post(receive)).with_state(r.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, r)
}

fn header<'a>(h: &'a HeaderMap, name: &str) -> &'a str {
    h.get(name).and_then(|v| v.to_str().ok()).unwrap_or("")
}

#[tokio::test]
async fn delivers_signed_filtered_events_and_backs_off() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = LocalConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        db: dir.path().join("t.db"),
        creds: CredStore { config_dir: dir.path().into(), keyring: false },
        user_name: "lane".into(),
        pid_file: None,
    };
    let (store, ctx) = open_local(&cfg).await.unwrap();
    store
        .create_project(&ctx, NewProject { key: "AUTH".into(), name: "Auth".into(), ..Default::default() })
        .await
        .unwrap();
    let (url, rx) = receiver().await;
    let (hook, secret) = store.create_webhook(&ctx, &url, vec!["task.created".into()]).await.unwrap();
    assert!(secret.starts_with("whsec_"));

    let agent = store.create_agent(&ctx, "bot").await.unwrap();
    let agent_ctx = store.agent_ctx(ctx.workspace_id, agent.id).await.unwrap();
    assert!(matches!(store.create_webhook(&agent_ctx, &url, vec![]).await, Err(Error::Forbidden(_))));

    let task = store
        .create_task(&ctx.next_batch(), NewTask { project: "AUTH".into(), title: "first".into(), ..Default::default() })
        .await
        .unwrap();
    store
        .add_comment(
            &ctx.next_batch(),
            &task.key,
            NewComment { kind: CommentKind::Note, body: "skip me".into(), addressed_to: None },
        )
        .await
        .unwrap();

    let state = AppState::new(store);
    let http = reqwest::Client::new();
    webhooks::deliver_due(&state, &http).await;
    {
        let got = rx.got.lock().unwrap();
        // project.created predates the webhook, and comment.created doesn't match.
        assert_eq!(got.len(), 1);
        let (h, body) = &got[0];
        assert_eq!(header(h, "trailhead-event-kind"), "task.created");
        assert_eq!(header(h, "trailhead-webhook-id"), hook.id.to_string());
        let sig = header(h, "trailhead-signature");
        let t: i64 = sig.trim_start_matches("t=").split(',').next().unwrap().parse().unwrap();
        assert_eq!(sig, webhook_signature(&secret, t, body));
        let event: serde_json::Value = serde_json::from_slice(body).unwrap();
        assert_eq!(event["after"]["title"], "first");
    }
    let hooks = state.store.list_webhooks(&ctx).await.unwrap();
    let latest = state.store.latest_event_id(&ctx).await.unwrap();
    assert_eq!(hooks[0].cursor, latest, "skipped events move the cursor too");

    // A failing receiver is logged and backed off, not retried immediately.
    rx.status.store(500, Ordering::SeqCst);
    state
        .store
        .create_task(
            &ctx.next_batch(),
            NewTask { project: "AUTH".into(), title: "second".into(), ..Default::default() },
        )
        .await
        .unwrap();
    webhooks::deliver_due(&state, &http).await;
    webhooks::deliver_due(&state, &http).await;
    assert_eq!(rx.got.lock().unwrap().len(), 2, "one failed attempt, then backoff");
    let hooks = state.store.list_webhooks(&ctx).await.unwrap();
    assert_eq!(hooks[0].failure_count, 1);
    assert!(hooks[0].next_attempt_at.is_some());
    assert_eq!(hooks[0].cursor, latest, "the failed event stays queued");
    let log = state.store.webhook_deliveries(&ctx, hook.id, None).await.unwrap();
    assert_eq!(log[0].status_code, Some(500));
    assert_eq!(log[1].status_code, Some(200));
}
