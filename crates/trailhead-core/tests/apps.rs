mod common;

use common::Env;
use serde_json::{Value, json};
use trailhead_core::app_data::{Declaration, Fields, Outcome};
use trailhead_core::{
    AppEventFilter, AppWrite, Ctx, Error, EventFilter, NewRecord, Principal, RecordChange, RecordPage, Store,
    TokenOptions, TokenPatch,
};

const SHAPE: &str = r#"
[collections.snoozes]
fields = { item = "string", until = "timestamp", note = "string?" }

[collections.pull_requests]
from = "github.pull_requests"
"#;

fn feeds(feed: &str) -> Option<Fields> {
    (feed == "github.pull_requests").then(|| [("title".to_string(), "string".parse().unwrap())].into())
}

fn declaration(text: &str) -> Declaration {
    Declaration::parse(text).unwrap().with_feeds(feeds).unwrap()
}

async fn env() -> Env {
    let env = Env::new().await;
    env.store.register_app(&env.human, "cairn", declaration(SHAPE)).await.unwrap();
    env
}

fn snooze(item: &str) -> Value {
    json!({ "item": item, "until": "2026-11-23T09:00:00Z" })
}

fn new(item: &str) -> NewRecord {
    NewRecord { collection: "snoozes".into(), key: None, data: snooze(item) }
}

fn invalid<T: std::fmt::Debug>(r: trailhead_core::Result<T>, says: &str) {
    match r {
        Err(Error::Invalid(msg)) => assert!(msg.contains(says), "{msg:?} doesn't say {says:?}"),
        other => panic!("expected invalid ({says}), got {other:?}"),
    }
}

fn forbidden<T: std::fmt::Debug>(r: trailhead_core::Result<T>) {
    assert!(matches!(r, Err(Error::Forbidden(_))), "{r:?}");
}

#[test]
fn declarations_parse_and_say_what_is_wrong() {
    let d = Declaration::parse(SHAPE).unwrap();
    assert_eq!(d.collections["snoozes"].fields.len(), 3);
    assert!(d.collections["snoozes"].fields["note"].optional);
    assert_eq!(d.collections["pull_requests"].from.as_deref(), Some("github.pull_requests"));
    assert!(d.collections["pull_requests"].fields.is_empty());
    assert!(d.check().unwrap_err().0.contains("shape of feed `github.pull_requests` isn't known"));
    declaration(SHAPE).check().unwrap();
    assert!(Declaration::parse("").unwrap().collections.is_empty());

    let refused = |text: &str, says: &str| {
        let e = Declaration::parse(text).unwrap_err().0;
        assert!(e.contains(says), "{e:?} doesn't say {says:?}");
    };
    refused("[collections.a]\nfields = { x = \"string\" }\nfrom = \"g.f\"", "both `fields` and `from`");
    refused("[collections.a]\n", "needs `fields");
    refused("[collections.Snoozes]\nfields = { x = \"string\" }", "collection name `Snoozes`");
    refused("[collections.a]\nfields = { Item = \"string\" }", "field name `Item`");
    refused("[collections.a]\nfields = { x = \"time\" }", "field `x`: `time` isn't a field type");
    refused("[collections.a]\nfields = { x = \"string?[]\" }", "string[]?");
    refused("[collections.a]\nfields = { x = 1 }", "its type is a string");
    refused("[collections.a]\nfields = {}", "has no fields");
    refused("[collections.a]\nfrom = \"github\"", "<connector>.<feed>");
    refused("[collections.a]\nfields = { x = \"string\" }\nshape = 1", "unknown key `shape`");
    refused("[things.a]\n", "unknown table `things`");
    refused("[collections.a\n", "collections.toml");
    let e = Declaration::parse("[collections.a]\nfrom = \"jira.issues\"").unwrap().with_feeds(feeds).unwrap_err();
    assert!(e.0.contains("no feed `jira.issues`"), "{e:?}");
}

#[tokio::test]
async fn writes_are_checked_against_the_declaration() {
    let env = env().await;
    let (s, h) = (&env.store, &env.human);
    invalid(
        s.create_record(h, "cairn", NewRecord { data: json!({"item": "x"}), ..new("x") }).await,
        "`until` is missing",
    );
    invalid(
        s.create_record(
            h,
            "cairn",
            NewRecord { data: json!({"item": "x", "until": "2026-11-23T09:00:00Z", "who": 1}), ..new("x") },
        )
        .await,
        "unknown field `who`",
    );
    invalid(
        s.create_record(
            h,
            "cairn",
            NewRecord { data: json!({"item": 3, "until": "2026-11-23T09:00:00Z"}), ..new("x") },
        )
        .await,
        "`item` should be a string",
    );
    let gone = s.create_record(h, "cairn", NewRecord { collection: "nope".into(), ..new("x") }).await;
    assert!(matches!(gone, Err(Error::NotFound(_))), "{gone:?}");
    invalid(s.create_record(h, "Cairn", new("x")).await, "app's id");
    assert!(matches!(s.create_record(h, "other", new("x")).await, Err(Error::NotFound(_))));

    let r = s
        .create_record(
            h,
            "cairn",
            NewRecord { data: json!({"item": "x", "until": "2026-11-23T09:00:00Z", "note": null}), ..new("x") },
        )
        .await
        .unwrap();
    assert_eq!(r.version, 1);
    assert_eq!(r.created_by, h.user_id);
    assert!(!r.data.contains_key("note"));
    assert_eq!(s.get_record(h, "cairn", r.id).await.unwrap(), r);
}

#[tokio::test]
async fn updates_replace_or_merge_and_check_the_version() {
    let env = env().await;
    let (s, h) = (&env.store, &env.human);
    let r = s.create_record(h, "cairn", new("a")).await.unwrap();
    let w = s.update_record(h, "cairn", r.id, RecordChange::Merge(json!({"note": "later"})), Some(1)).await.unwrap();
    assert_eq!((w.outcome, w.record.version), (Outcome::Changed, 2));
    assert_eq!(w.record.data["item"], "a");
    let w = s.update_record(h, "cairn", r.id, RecordChange::Merge(json!({"note": null})), None).await.unwrap();
    assert!(!w.record.data.contains_key("note"));
    invalid(
        s.update_record(h, "cairn", r.id, RecordChange::Merge(json!({"item": null})), None).await,
        "`item` is missing",
    );
    let w = s.update_record(h, "cairn", r.id, RecordChange::Replace(snooze("b")), Some(3)).await.unwrap();
    assert_eq!((w.outcome, w.record.version), (Outcome::Changed, 4));
    let same = s.update_record(h, "cairn", r.id, RecordChange::Replace(snooze("b")), None).await.unwrap();
    assert_eq!((same.outcome, same.record.version), (Outcome::Unchanged, 4));

    let stale = s.update_record(h, "cairn", r.id, RecordChange::Replace(snooze("c")), Some(1)).await;
    assert!(matches!(stale, Err(Error::VersionConflict { expected: 1, current: 4 })), "{stale:?}");
    let stale = s.delete_record(h, "cairn", r.id, Some(2)).await;
    assert!(matches!(stale, Err(Error::VersionConflict { expected: 2, current: 4 })), "{stale:?}");
    s.delete_record(h, "cairn", r.id, Some(4)).await.unwrap();
    assert!(matches!(s.get_record(h, "cairn", r.id).await, Err(Error::NotFound(_))));
}

#[tokio::test]
async fn putting_by_key_writes_only_what_changes() {
    let env = env().await;
    let (s, h) = (&env.store, &env.human);
    let put = |item: &'static str| s.put_record(h, "cairn", "snoozes", "lanesawyer/trailhead#123", snooze(item));
    let first = put("a").await.unwrap();
    assert_eq!((first.outcome, first.record.version), (Outcome::Created, 1));
    let events = s.latest_app_event_id(h, "cairn").await.unwrap();

    let again = put("a").await.unwrap();
    assert_eq!((again.outcome, again.record.version, again.record.id), (Outcome::Unchanged, 1, first.record.id));
    assert_eq!(s.latest_app_event_id(h, "cairn").await.unwrap(), events);

    let changed = put("b").await.unwrap();
    assert_eq!((changed.outcome, changed.record.version, changed.record.id), (Outcome::Changed, 2, first.record.id));
    assert_eq!(s.latest_app_event_id(h, "cairn").await.unwrap(), events + 1);
    let by_key = s.get_record_by_key(h, "cairn", "snoozes", "lanesawyer/trailhead#123").await.unwrap();
    assert_eq!(by_key, changed.record);

    let taken =
        s.create_record(h, "cairn", NewRecord { key: Some("lanesawyer/trailhead#123".into()), ..new("c") }).await;
    assert!(matches!(taken, Err(Error::AlreadyExists(_))), "{taken:?}");
    invalid(s.put_record(h, "cairn", "snoozes", "", snooze("a")).await, "key");
}

#[tokio::test]
async fn replacing_a_set_makes_the_collection_match() {
    let env = env().await;
    let (s, h) = (&env.store, &env.human);
    s.create_record(h, "cairn", new("unkeyed")).await.unwrap();
    let set = |items: &[(&str, &str)]| items.iter().map(|(k, i)| (k.to_string(), snooze(i))).collect::<Vec<_>>();
    let c = s.replace_set(h, "cairn", "snoozes", set(&[("a", "1"), ("b", "2")])).await.unwrap();
    assert_eq!((c.created, c.changed, c.removed, c.unchanged), (2, 0, 1, 0));
    let before = s.latest_app_event_id(h, "cairn").await.unwrap();
    let c = s.replace_set(h, "cairn", "snoozes", set(&[("a", "1"), ("b", "2")])).await.unwrap();
    assert_eq!((c.created, c.changed, c.removed, c.unchanged), (0, 0, 0, 2));
    assert_eq!(s.latest_app_event_id(h, "cairn").await.unwrap(), before);
    let c = s.replace_set(h, "cairn", "snoozes", set(&[("b", "3"), ("c", "4")])).await.unwrap();
    assert_eq!((c.created, c.changed, c.removed, c.unchanged), (1, 1, 1, 0));
    let keys: Vec<_> = s
        .list_records(h, "cairn", "snoozes", RecordPage::default())
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.key.unwrap())
        .collect();
    assert_eq!(keys, ["b", "c"]);

    invalid(s.replace_set(h, "cairn", "snoozes", set(&[("a", "1"), ("a", "2")])).await, "twice");
    let bad = vec![("d".to_string(), json!({"item": "x"}))];
    invalid(s.replace_set(h, "cairn", "snoozes", bad).await, "`until` is missing");
    assert_eq!(s.list_records(h, "cairn", "snoozes", RecordPage::default()).await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_batch_is_all_or_nothing() {
    let env = env().await;
    let (s, h) = (&env.store, &env.human);
    let kept = s.create_record(h, "cairn", new("kept")).await.unwrap();
    let ctx = h.next_batch();
    let failed = s
        .write_records(
            &ctx,
            "cairn",
            vec![
                AppWrite::Create(new("x")),
                AppWrite::Delete { id: kept.id, expected_version: None },
                AppWrite::Update { id: kept.id, change: RecordChange::Replace(snooze("y")), expected_version: None },
            ],
        )
        .await;
    assert!(matches!(failed, Err(Error::NotFound(_))), "{failed:?}");
    assert_eq!(s.list_records(h, "cairn", "snoozes", RecordPage::default()).await.unwrap(), vec![kept.clone()]);

    let written = s
        .write_records(
            &ctx,
            "cairn",
            vec![
                AppWrite::Create(new("x")),
                AppWrite::Put { collection: "snoozes".into(), key: "k".into(), data: snooze("k") },
                AppWrite::Update {
                    id: kept.id,
                    change: RecordChange::Merge(json!({"note": "n"})),
                    expected_version: Some(1),
                },
            ],
        )
        .await
        .unwrap();
    let outcomes: Vec<_> = written.iter().map(|w| w.outcome).collect();
    assert_eq!(outcomes, [Outcome::Created, Outcome::Created, Outcome::Changed]);
    let events =
        s.list_app_events(h, "cairn", AppEventFilter { after_id: Some(1), ..Default::default() }).await.unwrap();
    assert_eq!(events.len(), 3);
    assert!(events.iter().all(|e| e.batch_id == ctx.batch_id));

    let report = s.undo_app_batch(h, "cairn", ctx.batch_id).await.unwrap();
    assert_eq!(report.reverted.len(), 3);
    assert_eq!(s.list_records(h, "cairn", "snoozes", RecordPage::default()).await.unwrap().len(), 1);
    assert!(!s.get_record(h, "cairn", kept.id).await.unwrap().data.contains_key("note"));
}

#[tokio::test]
async fn events_list_and_undo_each_kind_of_change() {
    let env = env().await;
    let (s, h) = (&env.store, &env.human);
    let r = s.create_record(&h.next_batch(), "cairn", new("a")).await.unwrap();
    let w = s.update_record(&h.next_batch(), "cairn", r.id, RecordChange::Replace(snooze("b")), None).await.unwrap();
    let events =
        s.list_app_events(h, "cairn", AppEventFilter { record: Some(r.id), ..Default::default() }).await.unwrap();
    let kinds: Vec<_> = events.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, ["record.created", "record.updated"]);
    assert_eq!(events[1].before.as_ref().unwrap()["data"]["item"], "a");
    assert_eq!(events[1].after.as_ref().unwrap()["data"]["item"], "b");
    assert_eq!(
        (events[1].actor_id, events[1].on_behalf_of, events[1].collection.as_str()),
        (h.user_id, h.user_id, "snoozes")
    );

    let conflict = s.undo_app_event(h, "cairn", events[0].id).await;
    assert!(
        matches!(conflict, Err(Error::UndoConflict { later_event, .. }) if later_event == events[1].id),
        "{conflict:?}"
    );

    let undo = s.undo_app_event(h, "cairn", events[1].id).await.unwrap();
    let back = s.get_record(h, "cairn", r.id).await.unwrap();
    assert_eq!((back.data["item"].as_str(), back.version), (Some("a"), w.record.version + 1));
    let undone = s
        .list_app_events(h, "cairn", AppEventFilter { after_id: Some(events[1].id), ..Default::default() })
        .await
        .unwrap();
    assert_eq!((undone[0].kind.as_str(), undone[0].undoes), ("record.reverted", Some(events[1].id)));
    assert_eq!(undo.reverted[0].event_id, Some(undone[0].id));

    s.undo_app_event(h, "cairn", undone[0].id).await.unwrap();
    assert_eq!(s.get_record(h, "cairn", r.id).await.unwrap().data["item"], "b");

    let deleted = s.delete_record(h, "cairn", r.id, None).await.unwrap();
    let last = s.latest_app_event_id(h, "cairn").await.unwrap();
    s.undo_app_event(h, "cairn", last).await.unwrap();
    let restored = s.get_record(h, "cairn", r.id).await.unwrap();
    assert_eq!((restored.data, restored.version), (deleted.data, deleted.version + 1));

    let created = s.create_record(h, "cairn", new("c")).await.unwrap();
    s.undo_app_event(h, "cairn", s.latest_app_event_id(h, "cairn").await.unwrap()).await.unwrap();
    assert!(matches!(s.get_record(h, "cairn", created.id).await, Err(Error::NotFound(_))));
    assert!(matches!(s.undo_app_event(h, "cairn", 9999).await, Err(Error::NotFound(_))));

    env.advance(91 * 24 * 3600);
    let old = s.undo_app_event(h, "cairn", events[0].id).await;
    assert!(matches!(old, Err(Error::UndoWindowExpired(_))), "{old:?}");
}

#[tokio::test]
async fn undo_wont_take_a_key_another_record_has_now() {
    let env = env().await;
    let (s, h) = (&env.store, &env.human);
    let first = s.put_record(h, "cairn", "snoozes", "k", snooze("a")).await.unwrap().record;
    s.delete_record(h, "cairn", first.id, None).await.unwrap();
    let deleted = s.latest_app_event_id(h, "cairn").await.unwrap();
    s.put_record(h, "cairn", "snoozes", "k", snooze("b")).await.unwrap();
    let r = s.undo_app_event(h, "cairn", deleted).await;
    assert!(matches!(r, Err(Error::AlreadyExists(_))), "{r:?}");
}

#[tokio::test]
async fn reverting_an_actor_skips_what_others_changed_and_runs_over_every_app() {
    let env = env().await;
    let (s, h) = (&env.store, &env.human);
    s.register_app(h, "notes", declaration("[collections.notes]\nfields = { text = \"string\" }")).await.unwrap();
    let bot = env.agent("bot").await.with_apps(vec!["cairn".into(), "notes".into()]);
    let since = env.now();
    let a = s.create_record(&bot.next_batch(), "cairn", new("a")).await.unwrap();
    let b = s.create_record(&bot.next_batch(), "cairn", new("b")).await.unwrap();
    let n = s
        .create_record(
            &bot.next_batch(),
            "notes",
            NewRecord { collection: "notes".into(), key: None, data: json!({"text": "hi"}) },
        )
        .await
        .unwrap();
    s.update_record(h, "cairn", b.id, RecordChange::Merge(json!({"note": "mine"})), None).await.unwrap();

    forbidden(s.revert_app_actor(&bot, "cairn", bot.actor_id(), since).await);
    forbidden(s.revert_actor_in_apps(&bot, bot.actor_id(), since).await);
    let reports = s.revert_actor_in_apps(h, bot.actor_id(), since).await.unwrap();
    let apps: Vec<_> = reports.iter().map(|(app, _)| app.as_str()).collect();
    assert_eq!(apps, ["cairn", "notes"]);
    let cairn = &reports[0].1;
    assert_eq!(cairn.reverted.iter().map(|r| r.record_id).collect::<Vec<_>>(), [a.id]);
    assert_eq!(cairn.skipped.iter().map(|r| r.record_id).collect::<Vec<_>>(), [b.id]);
    assert!(cairn.skipped[0].later_event.is_some());
    assert!(matches!(s.get_record(h, "cairn", a.id).await, Err(Error::NotFound(_))));
    assert!(s.get_record(h, "cairn", b.id).await.is_ok());
    assert!(matches!(s.get_record(h, "notes", n.id).await, Err(Error::NotFound(_))));
}

#[tokio::test]
async fn a_shape_changes_only_by_addition_once_it_holds_records() {
    let env = env().await;
    let (s, h) = (&env.store, &env.human);
    let with = |extra: &str| declaration(&format!("{SHAPE}\n{extra}"));
    let changed = SHAPE.replace("until = \"timestamp\"", "until = \"day\"");
    s.register_app(h, "cairn", declaration(&changed)).await.unwrap();
    s.register_app(h, "cairn", declaration(SHAPE)).await.unwrap();

    s.create_record(h, "cairn", new("a")).await.unwrap();
    invalid(s.register_app(h, "cairn", declaration(&changed)).await, "was `timestamp` and is now `day`");
    let required = SHAPE.replace("note = \"string?\"", "note = \"string?\", due = \"day\"");
    invalid(s.register_app(h, "cairn", declaration(&required)).await, "new field `snoozes.due` isn't optional");
    let dropped = SHAPE.replace(", note = \"string?\"", "");
    invalid(s.register_app(h, "cairn", declaration(&dropped)).await, "`snoozes.note` is gone");
    invalid(s.register_app(h, "cairn", declaration("")).await, "collection `snoozes` is gone");

    let optional = SHAPE.replace("note = \"string?\"", "note = \"string?\", due = \"day?\"");
    s.register_app(h, "cairn", declaration(&optional)).await.unwrap();
    invalid(
        s.register_app(h, "cairn", with("[collections.more]\nfields = { x = \"integer\" }")).await,
        "`snoozes.due` is gone",
    );
    let both = format!("{optional}\n[collections.more]\nfields = {{ x = \"integer\" }}");
    s.register_app(h, "cairn", declaration(&both)).await.unwrap();
    let decl = s.app_declaration(h, "cairn").await.unwrap();
    assert!(decl.collections.contains_key("more") && decl.collections["snoozes"].fields.contains_key("due"));
}

#[tokio::test]
async fn tokens_reach_only_the_apps_they_name() {
    let env = env().await;
    let (s, h) = (&env.store, &env.human);
    let r = s.create_record(h, "cairn", new("a")).await.unwrap();

    let (_, _, none) = s.create_agent_with_token(h, "plain", TokenOptions::default()).await.unwrap();
    let plain = s.authenticate(&none).await.unwrap();
    forbidden(s.get_record(&plain, "cairn", r.id).await);
    forbidden(s.create_record(&plain, "cairn", new("b")).await);
    forbidden(s.list_app_events(&plain, "cairn", AppEventFilter::default()).await);
    forbidden(s.create_record(&env.agent("fresh").await, "cairn", new("b")).await);

    let opts = TokenOptions { apps: Some(vec!["cairn".into()]), ..Default::default() };
    let (agent, token, secret) = s.create_agent_with_token(h, "reader", opts).await.unwrap();
    assert_eq!(token.apps, Some(vec!["cairn".to_string()]));
    let reader = s.authenticate(&secret).await.unwrap();
    s.get_record(&reader, "cairn", r.id).await.unwrap();
    forbidden(s.get_record(&reader, "notes", r.id).await);

    let wider = TokenOptions { apps: Some(vec!["cairn".into(), "notes".into()]), ..Default::default() };
    forbidden(s.create_agent_with_token(&reader, "sub", wider).await);
    let narrow = TokenOptions { apps: Some(vec!["cairn".into()]), ..Default::default() };
    s.create_agent_with_token(&reader, "sub", narrow).await.unwrap();
    let bad = TokenOptions { apps: Some(vec!["Not An App".into()]), ..Default::default() };
    invalid(s.create_token_with(h, Principal::Agent(agent.id), "bad", bad).await, "app's id");

    let moved = TokenPatch { apps: Some(Some(vec!["notes".into()])), ..Default::default() };
    assert_eq!(s.update_token(h, token.id, moved).await.unwrap().apps, Some(vec!["notes".to_string()]));
    let reader = s.authenticate(&secret).await.unwrap();
    forbidden(s.get_record(&reader, "cairn", r.id).await);

    let (_, mine) = s.create_token(h, Principal::User(h.user_id), "me").await.unwrap();
    let me = s.authenticate(&mine).await.unwrap();
    assert!(me.apps.is_none());
    s.get_record(&me, "cairn", r.id).await.unwrap();
    let projects = TokenOptions { projects: Some(vec!["AUTH".into()]), ..Default::default() };
    let (_, scoped) = s.create_token_with(h, Principal::User(h.user_id), "scoped", projects).await.unwrap();
    forbidden(s.get_record(&s.authenticate(&scoped).await.unwrap(), "cairn", r.id).await);
}

#[tokio::test]
async fn a_fed_collection_is_written_only_from_its_feed() {
    let env = env().await;
    let (s, h) = (&env.store, &env.human);
    let pr = |t: &str| json!({ "title": t });
    let fed = NewRecord { collection: "pull_requests".into(), key: None, data: pr("x") };
    forbidden(s.create_record(h, "cairn", fed).await);
    forbidden(s.put_record(h, "cairn", "pull_requests", "a", pr("x")).await);
    forbidden(s.replace_set(h, "cairn", "pull_requests", vec![("a".into(), pr("x"))]).await);
    invalid(
        s.replace_feed_set(h, "cairn", "snoozes", vec![("a".into(), snooze("x"))]).await,
        "isn't filled from a feed",
    );

    let c = s.replace_feed_set(h, "cairn", "pull_requests", vec![("a".into(), pr("x"))]).await.unwrap();
    assert_eq!(c.created, 1);
    let r = s.get_record_by_key(h, "cairn", "pull_requests", "a").await.unwrap();
    forbidden(s.update_record(h, "cairn", r.id, RecordChange::Replace(pr("y")), None).await);
    forbidden(s.delete_record(h, "cairn", r.id, None).await);
    let batch = s.write_records(h, "cairn", vec![AppWrite::Delete { id: r.id, expected_version: None }]).await;
    forbidden(batch);
    forbidden(s.undo_app_event(h, "cairn", s.latest_app_event_id(h, "cairn").await.unwrap()).await);
}

#[tokio::test]
async fn app_writes_leave_the_tasks_database_alone() {
    let env = env().await;
    let (s, h) = (&env.store, &env.human);
    let before = s.latest_event_id(h).await.unwrap();
    let tasks_events = s.list_events(h, EventFilter { limit: Some(1000), ..Default::default() }).await.unwrap().len();
    let r = s.create_record(h, "cairn", new("a")).await.unwrap();
    s.update_record(h, "cairn", r.id, RecordChange::Replace(snooze("b")), None).await.unwrap();
    s.undo_app_event(h, "cairn", 2).await.unwrap();
    assert_eq!(s.latest_event_id(h).await.unwrap(), before);
    assert_eq!(
        s.list_events(h, EventFilter { limit: Some(1000), ..Default::default() }).await.unwrap().len(),
        tasks_events
    );
    let conn = env.raw_conn().await;
    let mut rows =
        conn.query("SELECT count(*) FROM sqlite_master WHERE name IN ('records', 'meta')", ()).await.unwrap();
    assert_eq!(rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(), 0);
}

#[tokio::test]
async fn reopening_finds_the_records_and_the_declaration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trailhead.db");
    let store = Store::open_local(&path).await.unwrap();
    let (ws, user) = store.bootstrap_local("Lane").await.unwrap();
    let h = Ctx::user(ws.id, user.id);
    store.register_app(&h, "cairn", declaration(SHAPE)).await.unwrap();
    let r = store.put_record(&h, "cairn", "snoozes", "k", snooze("a")).await.unwrap().record;
    drop(store);
    assert!(dir.path().join("apps/cairn.db").exists());

    let store = Store::open_local(&path).await.unwrap();
    assert_eq!(store.app_declaration(&h, "cairn").await.unwrap(), declaration(SHAPE));
    assert_eq!(store.get_record(&h, "cairn", r.id).await.unwrap(), r);
    assert_eq!(store.latest_app_event_id(&h, "cairn").await.unwrap(), 1);
    let again = store.put_record(&h, "cairn", "snoozes", "k", snooze("a")).await.unwrap();
    assert_eq!(again.outcome, Outcome::Unchanged);

    let elsewhere = Ctx::user(uuid::Uuid::now_v7(), user.id);
    assert!(matches!(store.get_record(&elsewhere, "cairn", r.id).await, Err(Error::NotFound(_))));
    let memory = Store::open_local(":memory:").await.unwrap();
    invalid(memory.register_app(&h, "cairn", declaration(SHAPE)).await, "keeps no app data");
}

#[tokio::test]
async fn records_page_in_the_order_they_were_made() {
    let env = env().await;
    let (s, h) = (&env.store, &env.human);
    let mut ids = Vec::new();
    for i in 0..5 {
        ids.push(s.create_record(h, "cairn", new(&i.to_string())).await.unwrap().id);
    }
    let page = |after, limit| s.list_records(h, "cairn", "snoozes", RecordPage { after, limit: Some(limit) });
    let first: Vec<_> = page(None, 2).await.unwrap().into_iter().map(|r| r.id).collect();
    assert_eq!(first, ids[..2]);
    let rest: Vec<_> = page(Some(ids[1]), 10).await.unwrap().into_iter().map(|r| r.id).collect();
    assert_eq!(rest, ids[2..]);
    assert!(matches!(s.list_records(h, "cairn", "nope", RecordPage::default()).await, Err(Error::NotFound(_))));
}

#[tokio::test]
async fn a_page_says_which_event_it_was_read_at_and_apps_are_listed_by_reach() {
    let env = env().await;
    let (s, h) = (&env.store, &env.human);
    s.register_app(h, "notes", declaration("[collections.notes]\nfields = { text = \"string\" }")).await.unwrap();
    let (records, at) = s.list_records_at(h, "cairn", "snoozes", RecordPage::default()).await.unwrap();
    assert_eq!((records.len(), at), (0, 0));
    s.create_record(h, "cairn", new("a")).await.unwrap();
    s.create_record(h, "cairn", new("b")).await.unwrap();
    let (records, at) = s.list_records_at(h, "cairn", "snoozes", RecordPage::default()).await.unwrap();
    assert_eq!((records.len(), at), (2, 2));

    let ids = |apps: Vec<(String, Declaration)>| apps.into_iter().map(|(id, _)| id).collect::<Vec<_>>();
    assert_eq!(ids(s.list_apps(h).await.unwrap()), ["cairn", "notes"]);
    let bot = env.agent("bot").await;
    assert!(s.list_apps(&bot).await.unwrap().is_empty());
    assert_eq!(ids(s.list_apps(&bot.with_apps(vec!["notes".into()])).await.unwrap()), ["notes"]);
    let elsewhere = Ctx::user(uuid::Uuid::now_v7(), h.user_id);
    assert!(s.list_apps(&elsewhere).await.unwrap().is_empty());
}
