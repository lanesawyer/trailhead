mod common;

use common::Env;
use trailhead_core::{Error, NewTask, Status, TaskPatch};

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn racing_agents_never_share_a_claim() {
    let env = Env::new().await;
    for round in 0..10 {
        let task = env.task(&format!("contested {round}")).await;
        let mut agents = Vec::new();
        for i in 0..16 {
            agents.push(env.agent(&format!("agent-{round}-{i}")).await);
        }
        let handles: Vec<_> = agents
            .into_iter()
            .map(|ctx| {
                let store = env.store.clone();
                let key = task.key.clone();
                tokio::spawn(async move { store.claim(&ctx, &key, None).await })
            })
            .collect();
        let mut won = 0;
        for h in handles {
            match h.await.unwrap() {
                Ok(_) => won += 1,
                Err(Error::ClaimHeld { .. } | Error::InvalidTransition { .. }) => {}
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
        assert_eq!(won, 1, "round {round}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_edits_hit_version_conflicts() {
    let env = Env::new().await;
    let task = env.task("edit me").await;
    let handles: Vec<_> = (0..12)
        .map(|i| {
            let store = env.store.clone();
            let ctx = env.human.next_batch();
            let key = task.key.clone();
            let version = task.version;
            tokio::spawn(async move {
                let patch = TaskPatch { title: Some(format!("edit {i}")), ..Default::default() };
                store.update_task(&ctx, &key, patch, Some(version)).await
            })
        })
        .collect();
    let mut ok = 0;
    for h in handles {
        match h.await.unwrap() {
            Ok(t) => {
                ok += 1;
                assert_eq!(t.version, task.version + 1);
            }
            Err(Error::VersionConflict { expected, current }) => {
                assert_eq!(expected, task.version);
                assert_eq!(current, task.version + 1);
            }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
    assert_eq!(ok, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn bulk_revert_racing_human_edits_stays_consistent() {
    for _ in 0..5 {
        let env = Env::new().await;
        let agent = env.agent("rogue").await;
        let since = env.now();
        let mut tasks = Vec::new();
        for i in 0..8 {
            let new = NewTask { project: "AUTH".into(), title: format!("junk {i}"), ..Default::default() };
            tasks.push(env.store.create_task(&agent.next_batch(), new).await.unwrap());
        }
        let revert = {
            let store = env.store.clone();
            let ctx = env.human.next_batch();
            let agent_id = agent.actor_id();
            tokio::spawn(async move { store.revert_actor(&ctx, agent_id, since).await.unwrap() })
        };
        let edits: Vec<_> = tasks
            .iter()
            .map(|t| {
                let store = env.store.clone();
                let ctx = env.human.next_batch();
                let key = t.key.clone();
                tokio::spawn(async move {
                    let patch = TaskPatch { title: Some("kept by human".into()), ..Default::default() };
                    store.update_task(&ctx, &key, patch, None).await
                })
            })
            .collect();
        let report = revert.await.unwrap();
        for (task, edit) in tasks.iter().zip(edits) {
            let skipped = report.skipped.iter().any(|s| s.entity_id == task.id.to_string());
            let current = env.store.get_task(&env.human, &task.key).await;
            match edit.await.unwrap() {
                Ok(_) => {
                    assert!(skipped, "{} was edited by a human but reverted anyway", task.key);
                    assert_eq!(current.unwrap().title, "kept by human");
                }
                Err(Error::NotFound(_)) => {
                    assert!(!skipped);
                    assert!(current.is_err(), "{} should be deleted", task.key);
                }
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claim_then_finish_round_trip_under_load() {
    let env = Env::new().await;
    for i in 0..30 {
        env.task(&format!("work {i}")).await;
    }
    let workers: Vec<_> = (0..6).map(|i| format!("worker-{i}")).collect();
    let mut handles = Vec::new();
    for name in workers {
        let ctx = env.agent(&name).await;
        let store = env.store.clone();
        handles.push(tokio::spawn(async move {
            let mut done = 0;
            loop {
                let ready = store.ready(&ctx, None, Some(5)).await.unwrap();
                if ready.is_empty() {
                    return done;
                }
                for t in ready {
                    if store.claim(&ctx.next_batch(), &t.key, None).await.is_ok() {
                        let patch = TaskPatch { status: Some(Status::Done), ..Default::default() };
                        store.update_task(&ctx.next_batch(), &t.key, patch, None).await.unwrap();
                        done += 1;
                        break;
                    }
                }
            }
        }));
    }
    let mut total = 0;
    for h in handles {
        total += h.await.unwrap();
    }
    assert_eq!(total, 30);
}
