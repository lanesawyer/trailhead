mod common;

use common::Env;
use trailhead_core::{Error, NewProject, NewTask, Principal, TokenOptions};

#[tokio::test]
async fn agents_create_and_revoke_their_own_sub_agents() {
    let env = Env::new().await;
    env.store
        .create_project(&env.human, NewProject { key: "WEB".into(), name: "Web".into(), ..Default::default() })
        .await
        .unwrap();
    let parent = env.store.create_agent(&env.human, "openclaw").await.unwrap();
    let opts = TokenOptions { projects: Some(vec!["AUTH".into()]), rate_limit_per_minute: Some(10) };
    let (_, secret) = env.store.create_token_with(&env.human, Principal::Agent(parent.id), "oc", opts).await.unwrap();
    let (oc, _) = env.store.authenticate_token(&secret).await.unwrap();

    // Explicit creation, named under the parent, acting for the same human.
    let kid = env.store.create_agent(&oc, "researcher").await.unwrap();
    assert_eq!(kid.name, "openclaw/researcher");
    assert_eq!(kid.parent_agent_id, Some(parent.id));
    assert_eq!(kid.owner_user_id, env.human.user_id);
    assert!(matches!(env.store.create_agent(&oc, "researcher").await, Err(Error::AlreadyExists(_))));
    assert!(matches!(env.store.create_agent(&oc, "bad/name").await, Err(Error::Invalid(_))));

    // Its token inherits the parent's projects and rate limit, and can't exceed them.
    let (tok, kid_secret) =
        env.store.create_token_with(&oc, Principal::Agent(kid.id), "r", TokenOptions::default()).await.unwrap();
    assert_eq!(tok.rate_limit_per_minute, Some(10));
    assert!(tok.projects.as_ref().is_some_and(|p| p.len() == 1));
    let wider = TokenOptions { rate_limit_per_minute: Some(100), ..Default::default() };
    assert!(matches!(
        env.store.create_token_with(&oc, Principal::Agent(kid.id), "r2", wider).await,
        Err(Error::Forbidden(_))
    ));
    let other_project = TokenOptions { projects: Some(vec!["WEB".into()]), ..Default::default() };
    assert!(matches!(
        env.store.create_token_with(&oc, Principal::Agent(kid.id), "r3", other_project).await,
        Err(Error::Forbidden(_))
    ));

    // The sub-agent's work is attributed to it.
    let (kid_ctx, _) = env.store.authenticate_token(&kid_secret).await.unwrap();
    let t = env
        .store
        .create_task(&kid_ctx, NewTask { project: "AUTH".into(), title: "by kid".into(), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(t.created_by, kid.id);

    // Agents can't reach outside their own subtree.
    let other = env.store.create_agent(&env.human, "other").await.unwrap();
    assert!(matches!(
        env.store.create_token_with(&oc, Principal::Agent(other.id), "x", TokenOptions::default()).await,
        Err(Error::Forbidden(_))
    ));
    assert!(matches!(env.store.revoke_agent(&kid_ctx, "openclaw").await, Err(Error::Forbidden(_))));
    assert!(matches!(
        env.store.create_token_with(&oc, Principal::User(env.human.user_id), "me", TokenOptions::default()).await,
        Err(Error::Forbidden(_))
    ));

    // Revoking the parent revokes everything under it.
    let grandkid = env.store.create_agent(&kid_ctx, "notes").await.unwrap();
    assert_eq!(grandkid.name, "openclaw/researcher/notes");
    env.store.revoke_agent(&env.human, "openclaw").await.unwrap();
    assert!(matches!(env.store.authenticate_token(&kid_secret).await, Err(Error::Unauthorized)));
    assert!(matches!(env.store.authenticate_token(&secret).await, Err(Error::Unauthorized)));
}
