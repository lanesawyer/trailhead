mod common;

use common::Env;
use jiff::SignedDuration;
use trailhead_core::{
    CommentKind, DEFAULT_REMINDER_WINDOW, DUE_SOON, Entity, Error, EventFilter, NewComment, NewNotification, NewTask,
    Notification, NotificationAction, NotificationFilter, OVERDUE, ProjectPatch, QUESTION, REVIEW, SOURCE_SERVER,
    Status, TaskPatch,
};

async fn all(env: &Env) -> Vec<Notification> {
    env.store.list_notifications(&env.human, NotificationFilter::default()).await.unwrap()
}

async fn unread(env: &Env) -> Vec<Notification> {
    env.store.list_notifications(&env.human, NotificationFilter { unread: true, ..Default::default() }).await.unwrap()
}

fn from_app(title: &str) -> NewNotification {
    NewNotification {
        source: "ticklist".into(),
        kind: "ticklist.streak".into(),
        title: title.into(),
        ..Default::default()
    }
}

fn status(s: Status) -> TaskPatch {
    TaskPatch { status: Some(s), ..Default::default() }
}

#[tokio::test]
async fn one_is_raised_listed_read_unread_and_resolved() {
    let env = Env::new().await;
    let task = env.task("Ship it").await;
    let new = NewNotification {
        body: "  Seven days in a row.  ".into(),
        subject: Some("streak".into()),
        task: Some(task.key.to_string()),
        actions: vec![NotificationAction { label: "See it".into(), url: "https://example.com/streak".into() }],
        ..from_app(" A streak ")
    };
    let n = env.store.raise_notification(&env.human, new).await.unwrap();
    assert_eq!((n.source.as_str(), n.kind.as_str()), ("ticklist", "ticklist.streak"));
    assert_eq!((n.title.as_str(), n.body.as_str()), ("A streak", "Seven days in a row."));
    assert_eq!((n.task_id, n.task_key.as_deref()), (Some(task.id), Some(task.key.as_str())));
    assert_eq!(n.raised_by, env.human.user_id);
    assert!(n.is_unread());
    assert_eq!(all(&env).await, vec![n.clone()]);
    assert_eq!(env.store.unread_notifications(&env.human).await.unwrap(), 1);

    env.advance(5);
    let read = env.store.mark_notification(&env.human, n.id, true).await.unwrap();
    assert!(read.read_at.is_some_and(|at| at > n.created_at));
    assert!(unread(&env).await.is_empty());
    assert_eq!(all(&env).await, vec![read.clone()], "a read one stays in the list");
    assert_eq!(
        env.store.mark_notification(&env.human, n.id, true).await.unwrap(),
        read,
        "reading twice changes nothing"
    );

    let again = env.store.mark_notification(&env.human, n.id, false).await.unwrap();
    assert_eq!(again.read_at, None);
    assert_eq!(unread(&env).await.len(), 1);

    let resolved = env.store.resolve_notification(&env.human, n.id).await.unwrap();
    assert_eq!(resolved.resolved_at, read.read_at);
    assert!(unread(&env).await.is_empty(), "a resolved one no longer asks for the person");
    assert_eq!(all(&env).await, vec![resolved]);

    let kinds: Vec<String> = env
        .store
        .list_events(&env.human, EventFilter { kinds: vec!["notification.".into()], ..Default::default() })
        .await
        .unwrap()
        .into_iter()
        .inspect(|e| assert_eq!((e.entity, e.task_id), (Entity::Notification, None)))
        .map(|e| e.kind)
        .collect();
    assert_eq!(kinds, ["notification.raised", "notification.read", "notification.unread", "notification.resolved"]);
}

#[tokio::test]
async fn what_is_raised_is_checked() {
    let env = Env::new().await;
    let bad = [
        NewNotification { source: " ".into(), ..from_app("t") },
        NewNotification { kind: String::new(), ..from_app("t") },
        from_app(""),
        from_app("two\nlines"),
        NewNotification {
            actions: vec![NotificationAction { label: "Run".into(), url: "file:///bin/sh".into() }],
            ..from_app("t")
        },
    ];
    for new in bad {
        let err = env.store.raise_notification(&env.human, new.clone()).await.unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{new:?}: {err:?}");
    }
    let missing = NewNotification { task: Some("AUTH-999".into()), ..from_app("t") };
    assert!(matches!(env.store.raise_notification(&env.human, missing).await, Err(Error::NotFound(_))));
    assert!(all(&env).await.is_empty());
}

#[tokio::test]
async fn they_page_back_newest_first_and_all_are_read_at_once() {
    let env = Env::new().await;
    let mut ids = Vec::new();
    for i in 0..5 {
        ids.push(env.store.raise_notification(&env.human.next_batch(), from_app(&format!("n{i}"))).await.unwrap().id);
    }
    let titles = |ns: &[Notification]| ns.iter().map(|n| n.title.clone()).collect::<Vec<_>>();
    let page = |before, limit| NotificationFilter { before, limit: Some(limit), ..Default::default() };
    let first = env.store.list_notifications(&env.human, page(None, 2)).await.unwrap();
    assert_eq!(titles(&first), ["n4", "n3"]);
    let next = env.store.list_notifications(&env.human, page(Some(first[1].id), 10)).await.unwrap();
    assert_eq!(titles(&next), ["n2", "n1", "n0"]);

    env.store.resolve_notification(&env.human, ids[0]).await.unwrap();
    assert_eq!(
        env.store.mark_all_notifications_read(&env.human).await.unwrap(),
        4,
        "the resolved one asked for nobody"
    );
    assert_eq!(env.store.unread_notifications(&env.human).await.unwrap(), 0);
    assert!(all(&env).await.iter().all(|n| n.read_at.is_some()));
    assert_eq!(env.store.mark_all_notifications_read(&env.human).await.unwrap(), 0);
}

#[tokio::test]
async fn only_the_person_reads_and_a_scoped_token_sees_none() {
    let env = Env::new().await;
    let agent = env.agent("worker").await;
    let n = env.store.raise_notification(&agent, from_app("from an agent")).await.unwrap();
    assert_eq!(n.raised_by, agent.actor_id());
    assert!(matches!(env.store.mark_notification(&agent, n.id, true).await, Err(Error::Forbidden(_))));
    assert!(matches!(env.store.mark_all_notifications_read(&agent).await, Err(Error::Forbidden(_))));
    assert_eq!(env.store.list_notifications(&agent, NotificationFilter::default()).await.unwrap().len(), 1);
    env.store.resolve_notification(&agent, n.id).await.unwrap();

    let scoped = agent.clone().scoped_to(Vec::new());
    assert!(env.store.list_notifications(&scoped, NotificationFilter::default()).await.unwrap().is_empty());
    assert_eq!(env.store.unread_notifications(&scoped).await.unwrap(), 0);
    assert!(matches!(env.store.resolve_notification(&scoped, n.id).await, Err(Error::Forbidden(_))));
}

#[tokio::test]
async fn a_raiser_resolves_by_subject() {
    let env = Env::new().await;
    let about = |subject: &str| NewNotification { subject: Some(subject.into()), ..from_app("t") };
    env.store.raise_notification(&env.human, about("run-1")).await.unwrap();
    env.store.raise_notification(&env.human, about("run-1")).await.unwrap();
    let other = env.store.raise_notification(&env.human, about("run-2")).await.unwrap();
    let elsewhere = NewNotification { source: "lookout".into(), ..about("run-1") };
    let theirs = env.store.raise_notification(&env.human, elsewhere).await.unwrap();

    assert_eq!(env.store.resolve_notifications_about(&env.human, "ticklist", "run-1").await.unwrap(), 2);
    let open: Vec<_> = unread(&env).await.into_iter().map(|n| n.id).collect();
    assert_eq!(open, [theirs.id, other.id]);
    assert_eq!(env.store.resolve_notifications_about(&env.human, "ticklist", "run-1").await.unwrap(), 0);
}

#[tokio::test]
async fn an_agents_question_raises_one_and_its_answer_resolves_it() {
    let env = Env::new().await;
    let agent = env.agent("worker").await;
    let task = env.task("Pick a colour").await;
    env.store.claim(&agent, &task.key, None).await.unwrap();
    let q = env
        .store
        .add_comment(&agent.next_batch(), &task.key, NewComment::new(CommentKind::Question, "Red or blue?"))
        .await
        .unwrap();

    let raised = unread(&env).await;
    assert_eq!(raised.len(), 1);
    let n = &raised[0];
    assert_eq!((n.source.as_str(), n.kind.as_str()), (SOURCE_SERVER, QUESTION));
    assert_eq!(n.title, format!("{}: worker asks about Pick a colour", task.key));
    assert_eq!((n.body.as_str(), n.task_id), ("Red or blue?", Some(task.id)));
    assert_eq!(n.raised_by, agent.actor_id());

    env.store.resolve_comment(&env.human.next_batch(), q.id, Some("Blue".into())).await.unwrap();
    assert!(unread(&env).await.is_empty());
    let after = all(&env).await;
    assert!(after[0].resolved_at.is_some() && after[0].read_at.is_none(), "answered elsewhere, never marked read");
}

#[tokio::test]
async fn a_persons_question_and_one_put_to_an_agent_raise_none() {
    let env = Env::new().await;
    let agent = env.agent("worker").await;
    let lead = env.agent("lead").await;
    let task = env.task("Pick a colour").await;
    env.store.add_comment(&env.human, &task.key, NewComment::new(CommentKind::Question, "Why?")).await.unwrap();
    let to_lead =
        NewComment { addressed_to: Some(lead.actor_id()), ..NewComment::new(CommentKind::Question, "Which lib?") };
    env.store.add_comment(&agent, &task.key, to_lead).await.unwrap();
    env.store.add_comment(&agent, &task.key, NewComment::new(CommentKind::Note, "A note")).await.unwrap();
    assert!(all(&env).await.is_empty());

    let to_person =
        NewComment { addressed_to: Some(env.human.user_id), ..NewComment::new(CommentKind::Question, "Red?") };
    env.store.add_comment(&agent, &task.key, to_person).await.unwrap();
    assert_eq!(unread(&env).await.len(), 1);
}

#[tokio::test]
async fn a_finished_or_deleted_task_resolves_everything_about_it() {
    let env = Env::new().await;
    for end in ["done", "cancelled", "deleted"] {
        let task = env.task(end).await;
        let about = NewNotification { task: Some(task.key.to_string()), ..from_app("about it") };
        env.store.raise_notification(&env.human.next_batch(), about).await.unwrap();
        assert_eq!(unread(&env).await.len(), 1, "{end}");
        let ctx = env.human.next_batch();
        match end {
            "done" => env.store.update_task(&ctx, &task.key, status(Status::Done), None).await.unwrap(),
            "cancelled" => env.store.update_task(&ctx, &task.key, status(Status::Cancelled), None).await.unwrap(),
            _ => env.store.delete_task(&ctx, &task.key, None).await.unwrap(),
        };
        assert!(unread(&env).await.is_empty(), "{end}");
    }
    let unrelated = env.store.raise_notification(&env.human, from_app("about nothing")).await.unwrap();
    let task = env.task("another").await;
    env.store.update_task(&env.human.next_batch(), &task.key, status(Status::Done), None).await.unwrap();
    assert_eq!(unread(&env).await, vec![unrelated]);
}

#[tokio::test]
async fn a_review_only_the_person_can_give_raises_one_until_it_is_given() {
    let env = Env::new().await;
    let agent = env.agent("worker").await;
    let task = env.task("Build it").await;
    env.store.claim(&agent, &task.key, None).await.unwrap();
    env.store.update_task(&agent.next_batch(), &task.key, status(Status::InReview), None).await.unwrap();
    let raised = unread(&env).await;
    assert_eq!(raised.len(), 1);
    assert_eq!(raised[0].kind, REVIEW);
    assert_eq!(raised[0].title, format!("{} is waiting for your review: Build it", task.key));

    env.store.update_task(&env.human.next_batch(), &task.key, status(Status::Approved), None).await.unwrap();
    assert!(unread(&env).await.is_empty());

    // Where an agent may approve, the review is its to give, and whoever runs it says when a person is needed.
    env.agent("lead").await;
    let patch = ProjectPatch { reviewers: Some(vec!["lead".to_string()]), ..Default::default() };
    env.store.update_project(&env.human, "AUTH", patch, None).await.unwrap();
    let other = env.task("Build more").await;
    env.store.claim(&agent, &other.key, None).await.unwrap();
    env.store.update_task(&agent.next_batch(), &other.key, status(Status::InReview), None).await.unwrap();
    assert!(unread(&env).await.is_empty());

    // A person sending their own work for review tells themselves nothing.
    let mine = env.task("Mine").await;
    env.store.update_task(&env.human.next_batch(), &mine.key, status(Status::InReview), None).await.unwrap();
    assert!(unread(&env).await.is_empty());
}

#[tokio::test]
async fn a_reminder_raises_one_and_overdue_takes_due_soons_place() {
    let env = Env::new().await;
    let new =
        NewTask { title: "Ship".into(), due_at: Some(env.at_in(SignedDuration::from_hours(2))), ..Default::default() };
    let task = env.task_with(new).await;
    let fired = env.store.emit_due_reminders(&env.human.next_batch(), DEFAULT_REMINDER_WINDOW).await.unwrap();
    assert_eq!(fired.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(), [DUE_SOON], "only the reminders come back");
    let soon = unread(&env).await;
    assert_eq!(soon.len(), 1);
    assert_eq!((soon[0].kind.as_str(), soon[0].title.clone()), (DUE_SOON, format!("{} is due soon: Ship", task.key)));

    env.advance(3 * 3600);
    env.store.emit_due_reminders(&env.human.next_batch(), DEFAULT_REMINDER_WINDOW).await.unwrap();
    let late = unread(&env).await;
    assert_eq!(late.iter().map(|n| n.kind.as_str()).collect::<Vec<_>>(), [OVERDUE]);
    assert_eq!(all(&env).await.len(), 2);

    // A new deadline makes the old warning about nothing.
    let later = TaskPatch { due_at: Some(Some(env.at_in(SignedDuration::from_hours(72)))), ..Default::default() };
    env.store.update_task(&env.human.next_batch(), &task.key, later, None).await.unwrap();
    assert!(unread(&env).await.is_empty());
}

#[tokio::test]
async fn undo_leaves_notifications_alone() {
    let env = Env::new().await;
    let agent = env.agent("worker").await;
    let task = env.task("Pick").await;
    let ask = agent.next_batch();
    env.store.add_comment(&ask, &task.key, NewComment::new(CommentKind::Question, "Red?")).await.unwrap();
    let n = unread(&env).await.remove(0);

    // Undoing the question takes the comment back; what the person was told stays told, and isn't a conflict.
    let report = env.store.undo_batch(&env.human.next_batch(), ask.batch_id).await.unwrap();
    assert!(report.skipped.is_empty(), "{report:?}");
    assert_eq!(all(&env).await.len(), 1);

    let read = env.human.next_batch();
    env.store.mark_notification(&read, n.id, true).await.unwrap();
    let report = env.store.undo_batch(&env.human.next_batch(), read.batch_id).await.unwrap();
    assert!(report.reverted.is_empty() && report.skipped.is_empty(), "{report:?}");
    assert!(all(&env).await[0].read_at.is_some());

    let changes = env
        .store
        .list_events(&env.human, EventFilter { changes_only: true, limit: Some(500), ..Default::default() })
        .await
        .unwrap();
    assert!(changes.iter().all(|e| e.entity != Entity::Notification));
}

#[tokio::test]
async fn how_the_person_is_told_is_kept_and_theirs_to_set() {
    use trailhead_core::{Focus, NotificationLevel, NotificationSettings, QuietHours};
    let env = Env::new().await;
    assert_eq!(env.store.notification_settings(&env.human).await.unwrap(), NotificationSettings::default());

    let mut new = NotificationSettings { sound: false, counts: false, ..Default::default() };
    new.levels.insert(" ticklist ".into(), NotificationLevel::Bell);
    new.quiet_hours = Some(QuietHours { start: 22 * 60, end: 7 * 60 });
    new.focus = Some(Focus { until: None });
    let saved = env.store.set_notification_settings(&env.human, new).await.unwrap();
    assert_eq!(saved.level_of("ticklist"), NotificationLevel::Bell);
    assert_eq!(env.store.notification_settings(&env.human).await.unwrap(), saved);

    let agent = env.agent("worker").await;
    assert_eq!(env.store.notification_settings(&agent).await.unwrap(), saved, "anyone may read them");
    let reset = env.store.set_notification_settings(&agent, NotificationSettings::default()).await;
    assert!(matches!(reset, Err(Error::Forbidden(_))));
    let bad = NotificationSettings { quiet_hours: Some(QuietHours { start: 0, end: 2000 }), ..Default::default() };
    assert!(matches!(env.store.set_notification_settings(&env.human, bad).await, Err(Error::Invalid(_))));
    assert_eq!(env.store.notification_settings(&env.human).await.unwrap(), saved);
}
