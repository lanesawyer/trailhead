mod common;

use common::Env;
use trailhead_core::{
    CommentKind, Ctx, Error, EventFilter, GradeFilter, NewComment, NewProject, NewTask, Principal, ProjectPatch,
    TaskFilter, TaskPatch, TokenOptions, TokenPatch,
};

struct Scoped {
    env: Env,
    /// An agent whose token only covers AUTH.
    bot: Ctx,
    /// A task in WEB, outside the bot's scope.
    web: String,
}

async fn setup() -> Scoped {
    let env = Env::new().await;
    env.store
        .create_project(&env.human, NewProject { key: "WEB".into(), name: "Web".into(), ..Default::default() })
        .await
        .unwrap();
    let agent = env.store.create_agent(&env.human, "bot").await.unwrap();
    let opts =
        TokenOptions { projects: Some(vec!["auth".into()]), rate_limit_per_minute: Some(30), ..Default::default() };
    let (token, secret) =
        env.store.create_token_with(&env.human, Principal::Agent(agent.id), "bot", opts).await.unwrap();
    assert_eq!(token.rate_limit_per_minute, Some(30));
    let (bot, authed) = env.store.authenticate_token(&secret).await.unwrap();
    assert_eq!(authed.id, token.id);
    assert_eq!(bot.agent_id, Some(agent.id));
    let web = env.task_with(NewTask { project: "WEB".into(), title: "web thing".into(), ..Default::default() }).await;
    Scoped { env, bot, web: web.key }
}

fn hidden<T: std::fmt::Debug>(r: trailhead_core::Result<T>) {
    assert!(matches!(r, Err(Error::NotFound(_))), "{r:?}");
}

fn forbidden<T: std::fmt::Debug>(r: trailhead_core::Result<T>) {
    assert!(matches!(r, Err(Error::Forbidden(_))), "{r:?}");
}

#[tokio::test]
async fn reads_outside_scope_look_missing() {
    let Scoped { env, bot, web } = setup().await;
    let auth = env.task("auth thing").await;
    let s = &env.store;
    hidden(s.get_task(&bot, &web).await);
    hidden(s.task_detail(&bot, &web).await);
    hidden(s.list_comments(&bot, &web).await);
    hidden(s.blockers(&bot, &web).await);
    hidden(s.get_project(&bot, "WEB").await);
    hidden(s.list_events(&bot, EventFilter { task: Some(web.clone()), ..Default::default() }).await);
    hidden(s.list_tasks(&bot, TaskFilter { project: Some("WEB".into()), ..Default::default() }).await);
    hidden(s.ready(&bot, Some("WEB"), None, GradeFilter::Any).await);

    let keys = |ts: Vec<trailhead_core::Task>| ts.into_iter().map(|t| t.key).collect::<Vec<_>>();
    assert_eq!(keys(s.list_tasks(&bot, TaskFilter::default()).await.unwrap()), vec![auth.key.clone()]);
    assert_eq!(keys(s.ready(&bot, None, None, GradeFilter::Any).await.unwrap()), vec![auth.key.clone()]);
    assert_eq!(keys(s.search(&bot, "thing", None).await.unwrap()), vec![auth.key.clone()]);
    assert_eq!(keys(s.search(&env.human, "thing", None).await.unwrap()).len(), 2);
    let projects: Vec<_> = s.list_projects(&bot, false).await.unwrap().into_iter().map(|p| p.key).collect();
    assert_eq!(projects, vec!["AUTH"]);

    let events = s.list_events(&bot, EventFilter::default()).await.unwrap();
    assert!(!events.is_empty());
    assert!(events.iter().all(|e| e.task_key.as_deref() != Some(web.as_str()) && e.entity_id != web));
    assert!(events.iter().all(|e| !(e.kind == "project.created" && e.after.as_ref().unwrap()["key"] == "WEB")));
}

#[tokio::test]
async fn writes_outside_scope_are_rejected() {
    let Scoped { env, bot, web } = setup().await;
    let auth = env.task("auth thing").await;
    let s = &env.store;
    let patch = TaskPatch { title: Some("x".into()), ..Default::default() };
    hidden(s.update_task(&bot, &web, patch.clone(), None).await);
    hidden(s.delete_task(&bot, &web, None).await);
    hidden(s.claim(&bot, &web, None).await);
    hidden(s.release(&bot, &web).await);
    let note = NewComment::new(CommentKind::Note, "hi");
    hidden(s.add_comment(&bot, &web, note.clone()).await);
    let c = s.add_comment(&env.human, &web, note.clone()).await.unwrap();
    hidden(s.resolve_comment(&bot, c.id, None).await);

    // Naming a project outright is forbidden rather than hidden.
    let new = NewTask { project: "WEB".into(), title: "sneaky".into(), ..Default::default() };
    forbidden(s.create_task(&bot, new).await);
    forbidden(s.update_project(&bot, "WEB", ProjectPatch { name: Some("x".into()), ..Default::default() }, None).await);
    let p = NewProject { key: "NEW".into(), name: "New".into(), ..Default::default() };
    forbidden(s.create_project(&bot, p).await);
    // A scoped agent may create sub-agents, but their tokens can't reach past its scope.
    let kid = s.create_agent(&bot, "child").await.unwrap();
    let wider = trailhead_core::TokenOptions { projects: Some(vec!["WEB".into()]), ..Default::default() };
    forbidden(s.create_token_with(&bot, trailhead_core::Principal::Agent(kid.id), "k", wider).await);

    // Parents and dependencies can't reach out of scope either.
    let child = NewTask { project: "AUTH".into(), title: "c".into(), parent: Some(web.clone()), ..Default::default() };
    hidden(s.create_task(&bot, child).await);
    hidden(s.add_dependency(&bot, &web, &auth.key).await);
    hidden(s.add_dependency(&bot, &auth.key, &web).await);

    // In scope still works.
    s.update_task(&bot, &auth.key, patch, None).await.unwrap();
    s.claim(&bot, &auth.key, None).await.unwrap();
    s.add_comment(&bot, &auth.key, note).await.unwrap();
}

#[tokio::test]
async fn out_of_scope_blockers_are_redacted() {
    let Scoped { env, bot, web } = setup().await;
    let auth = env.task("auth thing").await;
    env.store.add_dependency(&env.human, &web, &auth.key).await.unwrap();
    assert!(env.store.ready(&bot, None, None, GradeFilter::Any).await.unwrap().is_empty());
    let detail = env.store.task_detail(&bot, &auth.key).await.unwrap();
    assert_eq!(detail.blocked_by, vec!["(out of scope)"]);
    assert!(env.store.blockers(&bot, &auth.key).await.unwrap().is_empty());
    match env.store.claim(&bot, &auth.key, None).await {
        Err(Error::BlockedBy(keys)) => assert_eq!(keys, vec!["(out of scope)"]),
        other => panic!("{other:?}"),
    }
    assert_eq!(env.store.task_detail(&env.human, &auth.key).await.unwrap().blocked_by, vec![web]);
}

#[tokio::test]
async fn scoped_undo_stays_in_scope() {
    let Scoped { env, bot, web } = setup().await;
    let human_batch = env.human.next_batch();
    let t = env
        .store
        .update_task(&human_batch, &web, TaskPatch { title: Some("renamed".into()), ..Default::default() }, None)
        .await
        .unwrap();
    forbidden(env.store.undo_batch(&bot, human_batch.batch_id).await);
    let ev = env.store.list_events(&env.human, EventFilter { newest_first: true, ..Default::default() }).await.unwrap();
    hidden(env.store.undo_event(&bot, ev[0].id).await);
    assert_eq!(env.store.get_task(&env.human, &t.key).await.unwrap().title, "renamed");

    let mine = bot.next_batch();
    let auth =
        env.store.create_task(&mine, NewTask { project: "AUTH".into(), title: "a".into(), ..Default::default() });
    auth.await.unwrap();
    env.store.undo_batch(&bot, mine.batch_id).await.unwrap();
}

#[tokio::test]
async fn token_options_are_validated() {
    let env = Env::new().await;
    let s = &env.store;
    let me = Principal::User(env.human.user_id);
    let bad = TokenOptions { projects: Some(vec!["NOPE".into()]), ..Default::default() };
    hidden(s.create_token_with(&env.human, me, "x", bad).await);
    let zero = TokenOptions { rate_limit_per_minute: Some(0), ..Default::default() };
    assert!(matches!(s.create_token_with(&env.human, me, "x", zero).await, Err(Error::Invalid(_))));
    let empty = TokenOptions { projects: Some(vec![]), ..Default::default() };
    let (t, secret) = s.create_token_with(&env.human, me, "x", empty).await.unwrap();
    assert_eq!(t.projects, None);
    assert!(!s.authenticate(&secret).await.unwrap().is_scoped());

    let scoped = TokenOptions { projects: Some(vec!["AUTH".into()]), ..Default::default() };
    let (_, secret) = s.create_token_with(&env.human, me, "x", scoped).await.unwrap();
    let ctx = s.authenticate(&secret).await.unwrap();
    assert!(ctx.is_human() && ctx.is_scoped());
    forbidden(s.create_token(&ctx, me, "y").await);
}

#[tokio::test]
async fn tokens_can_be_rescoped_without_reissuing() {
    let Scoped { env, bot, .. } = setup().await;
    let s = &env.store;
    let token = s.list_tokens(&env.human).await.unwrap().into_iter().find(|t| t.name == "bot").unwrap();

    let widen = TokenPatch { projects: Some(Some(vec!["WEB".into(), "AUTH".into()])), ..Default::default() };
    // Only the human who owns it can change it, never the agent holding it.
    forbidden(s.update_token(&bot, token.id, widen.clone()).await);
    let t = s.update_token(&env.human, token.id, widen).await.unwrap();
    assert_eq!(t.projects.as_ref().map(Vec::len), Some(2));
    assert_eq!(t.rate_limit_per_minute, Some(30), "omitted fields stay");

    let lift = TokenPatch { projects: Some(None), rate_limit_per_minute: Some(None) };
    let t = s.update_token(&env.human, token.id, lift).await.unwrap();
    assert_eq!((t.projects, t.rate_limit_per_minute), (None, None));
    let t = s.update_token(&env.human, token.id, TokenPatch { projects: Some(Some(vec![])), ..Default::default() });
    assert_eq!(t.await.unwrap().projects, None, "an empty list means every project too");

    let zero = TokenPatch { rate_limit_per_minute: Some(Some(0)), ..Default::default() };
    assert!(matches!(s.update_token(&env.human, token.id, zero).await, Err(Error::Invalid(_))));
    let nope = TokenPatch { projects: Some(Some(vec!["NOPE".into()])), ..Default::default() };
    hidden(s.update_token(&env.human, token.id, nope).await);

    // A scoped human token can't change tokens, and revoked tokens stay revoked.
    let me = Principal::User(env.human.user_id);
    let opts = TokenOptions { projects: Some(vec!["AUTH".into()]), ..Default::default() };
    let (_, secret) = s.create_token_with(&env.human, me, "narrow", opts).await.unwrap();
    let narrow = s.authenticate(&secret).await.unwrap();
    forbidden(s.update_token(&narrow, token.id, TokenPatch::default()).await);
    s.revoke_token(&env.human, token.id).await.unwrap();
    hidden(s.update_token(&env.human, token.id, TokenPatch::default()).await);
}

#[tokio::test]
async fn a_changed_scope_applies_on_the_next_request() {
    let env = Env::new().await;
    let s = &env.store;
    let me = Principal::User(env.human.user_id);
    let opts =
        TokenOptions { projects: Some(vec!["AUTH".into()]), rate_limit_per_minute: Some(10), ..Default::default() };
    let (token, secret) = s.create_token_with(&env.human, me, "x", opts).await.unwrap();
    assert!(s.authenticate(&secret).await.unwrap().is_scoped());
    let lift = TokenPatch { projects: Some(None), rate_limit_per_minute: Some(Some(100)) };
    s.update_token(&env.human, token.id, lift).await.unwrap();
    let (ctx, t) = s.authenticate_token(&secret).await.unwrap();
    assert!(!ctx.is_scoped());
    assert_eq!(t.rate_limit_per_minute, Some(100));
}
