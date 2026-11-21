mod common;

use common::Env;
use trailhead_core::{
    AttachmentRole, Entity, Error, EventFilter, MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS_PER_TASK, NewAttachment,
    NewProject,
};

/// Bytes the engine takes for a PNG: the signature and a header chunk, then `fill` to make them distinct and sized.
fn png(fill: u8, len: usize) -> Vec<u8> {
    let mut b = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0, 0, 13];
    b.extend(b"IHDR");
    b.extend(320u32.to_be_bytes());
    b.extend(200u32.to_be_bytes());
    b.extend([8, 6, 0, 0, 0]);
    b.resize(len.max(b.len()), fill);
    b
}

fn picture(name: &str, fill: u8) -> NewAttachment {
    NewAttachment::png(name, png(fill, 100))
}

async fn blob_count(env: &Env) -> i64 {
    let conn = env.raw_conn().await;
    let mut rows = conn.query("SELECT COUNT(*) FROM blobs", ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

#[tokio::test]
async fn a_picture_is_added_listed_read_removed_and_undone() {
    let env = Env::new().await;
    let task = env.task("Draw the board").await;
    let new = NewAttachment {
        role: Some(AttachmentRole::After),
        set: Some(" run-7 ".into()),
        ..NewAttachment::png(" board-dark ", png(1, 500))
    };
    let a = env.store.add_attachment(&env.human, task.key.as_str(), new).await.unwrap();
    assert_eq!((a.name.as_str(), a.set.as_deref(), a.role), ("board-dark", Some("run-7"), Some(AttachmentRole::After)));
    assert_eq!((a.size, a.width, a.height, a.content_type.as_str()), (500, 320, 200, "image/png"));
    assert_eq!(a.sha256.len(), 64);
    assert_eq!(a.created_by, env.human.user_id);

    assert_eq!(env.store.list_attachments(&env.human, task.key.as_str()).await.unwrap(), vec![a.clone()]);
    let detail = env.store.task_detail(&env.human, task.key.as_str()).await.unwrap();
    assert_eq!(detail.attachments, vec![a.clone()]);
    let (meta, bytes) = env.store.attachment_bytes(&env.human, a.id).await.unwrap();
    assert_eq!((meta, bytes), (a.clone(), png(1, 500)));

    // The event carries the metadata and the hash, never the bytes.
    let events = env
        .store
        .list_events(&env.human, EventFilter { kinds: vec!["attachment.".into()], ..Default::default() })
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!((events[0].entity, events[0].kind.as_str()), (Entity::Attachment, "attachment.created"));
    assert_eq!(events[0].task_id, Some(task.id));
    assert_eq!(events[0].after.as_ref().unwrap()["sha256"], a.sha256);
    assert!(events[0].after.as_ref().unwrap().get("bytes").is_none());

    let ctx = env.human.next_batch();
    let removed = env.store.remove_attachment(&ctx, a.id).await.unwrap();
    assert!(removed.deleted_at.is_some());
    assert!(env.store.list_attachments(&env.human, task.key.as_str()).await.unwrap().is_empty());
    assert!(matches!(env.store.attachment_bytes(&env.human, a.id).await, Err(Error::NotFound(_))));
    assert!(matches!(env.store.remove_attachment(&env.human.next_batch(), a.id).await, Err(Error::NotFound(_))));

    env.store.undo_batch(&env.human.next_batch(), ctx.batch_id).await.unwrap();
    assert_eq!(env.store.list_attachments(&env.human, task.key.as_str()).await.unwrap(), vec![a.clone()]);
    assert_eq!(env.store.attachment_bytes(&env.human, a.id).await.unwrap().1, png(1, 500));
}

#[tokio::test]
async fn undoing_an_add_takes_the_picture_off() {
    let env = Env::new().await;
    let task = env.task("t").await;
    let ctx = env.human.next_batch();
    let a = env.store.add_attachment(&ctx, task.key.as_str(), picture("shot", 1)).await.unwrap();
    env.store.undo_batch(&env.human.next_batch(), ctx.batch_id).await.unwrap();
    assert!(env.store.list_attachments(&env.human, task.key.as_str()).await.unwrap().is_empty());
    assert!(matches!(env.store.attachment_bytes(&env.human, a.id).await, Err(Error::NotFound(_))));
}

#[tokio::test]
async fn only_pngs_within_the_limits_are_taken() {
    let env = Env::new().await;
    let task = env.task("t").await;
    let key = task.key.as_str();
    let add = |new: NewAttachment| env.store.add_attachment(&env.human, key, new);

    let gif = NewAttachment::png("shot", b"GIF89a, which says it is a PNG but isn't one at all".to_vec());
    assert!(matches!(add(gif).await, Err(Error::Invalid(m)) if m.contains("PNG")));
    let jpeg = NewAttachment { content_type: "image/jpeg".into(), ..picture("shot", 1) };
    assert!(matches!(add(jpeg).await, Err(Error::Invalid(m)) if m.contains("image/png")));
    let with_params = NewAttachment { content_type: "Image/PNG; charset=binary".into(), ..picture("shot", 1) };
    add(with_params).await.unwrap();
    let big = NewAttachment::png("big", png(1, MAX_ATTACHMENT_BYTES + 1));
    assert!(matches!(add(big).await, Err(Error::Invalid(m)) if m.contains("at most")));
    assert!(matches!(add(picture("no spaces", 1)).await, Err(Error::Invalid(_))));
    let long_set = NewAttachment { set: Some("x".repeat(65)), ..picture("shot", 1) };
    assert!(matches!(add(long_set).await, Err(Error::Invalid(_))));
    assert_eq!(env.store.list_attachments(&env.human, key).await.unwrap().len(), 1);

    // 32 MiB a task: fifteen more full-size pictures fit beside the small one, the sixteenth doesn't.
    for i in 0..15u8 {
        add(NewAttachment::png(format!("full-{i}"), png(i, MAX_ATTACHMENT_BYTES))).await.unwrap();
    }
    let over = add(NewAttachment::png("full-last", png(99, MAX_ATTACHMENT_BYTES))).await;
    assert!(matches!(over, Err(Error::Invalid(m)) if m.contains("bytes together")));

    // 64 a task, counted on another task.
    let other = env.task("u").await;
    for i in 0..MAX_ATTACHMENTS_PER_TASK {
        env.store.add_attachment(&env.human, other.key.as_str(), picture(&format!("p{i}"), 7)).await.unwrap();
    }
    let full = env.store.add_attachment(&env.human, other.key.as_str(), picture("one-more", 7)).await;
    assert!(matches!(full, Err(Error::Invalid(m)) if m.contains("at most 64")));

    // Removing one makes room; undoing the removal once the room is taken again is refused.
    let first = env.store.list_attachments(&env.human, other.key.as_str()).await.unwrap()[0].clone();
    let ctx = env.human.next_batch();
    env.store.remove_attachment(&ctx, first.id).await.unwrap();
    env.store.add_attachment(&env.human, other.key.as_str(), picture("one-more", 7)).await.unwrap();
    assert!(matches!(env.store.undo_batch(&env.human.next_batch(), ctx.batch_id).await, Err(Error::Invalid(_))));
}

#[tokio::test]
async fn identical_bytes_are_stored_once_and_outlive_one_removal() {
    let env = Env::new().await;
    let task = env.task("t").await;
    let next = env.task("u").await;
    let after = NewAttachment { role: Some(AttachmentRole::After), set: Some("run-1".into()), ..picture("board", 3) };
    let before = NewAttachment { role: Some(AttachmentRole::Before), set: Some("run-2".into()), ..picture("board", 3) };
    let a = env.store.add_attachment(&env.human, task.key.as_str(), after).await.unwrap();
    let b = env.store.add_attachment(&env.human, next.key.as_str(), before).await.unwrap();
    assert_eq!(a.sha256, b.sha256);
    assert_eq!(blob_count(&env).await, 1);

    env.store.remove_attachment(&env.human, a.id).await.unwrap();
    assert_eq!(env.store.attachment_bytes(&env.human, b.id).await.unwrap().1, png(3, 100));
    env.store.add_attachment(&env.human, task.key.as_str(), picture("else", 4)).await.unwrap();
    assert_eq!(blob_count(&env).await, 2);

    let on = env.store.attachments_on(&env.human, &[task.id, next.id]).await.unwrap();
    assert_eq!(on[&task.id].iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["else"]);
    assert_eq!(on[&next.id], vec![b]);
}

#[tokio::test]
async fn whoever_may_comment_may_attach_and_agents_remove_only_their_own() {
    let env = Env::new().await;
    let task = env.task("t").await;
    let key = task.key.as_str();
    let factory = env.agent("factory").await;
    let sub = env.store.create_agent(&factory, "worker").await.unwrap();
    let worker = env.store.agent_ctx(env.human.workspace_id, sub.id).await.unwrap();
    let other = env.agent("other").await;

    let by_worker = env.store.add_attachment(&worker, key, picture("w", 1)).await.unwrap();
    assert_eq!((by_worker.created_by, by_worker.on_behalf_of), (sub.id, env.human.user_id));
    let by_person = env.store.add_attachment(&env.human, key, picture("p", 2)).await.unwrap();

    assert!(matches!(env.store.remove_attachment(&other, by_worker.id).await, Err(Error::Forbidden(_))));
    assert!(matches!(env.store.remove_attachment(&worker, by_person.id).await, Err(Error::Forbidden(_))));
    env.store.remove_attachment(&factory, by_worker.id).await.unwrap();

    // Undo goes by the same rule: an agent can't take a person's picture off by undoing its add.
    let ctx = env.human.next_batch();
    let added = env.store.add_attachment(&ctx, key, picture("q", 3)).await.unwrap();
    let events = env.store.list_events(&env.human, EventFilter { batch_id: Some(ctx.batch_id), ..Default::default() });
    let event = events.await.unwrap().remove(0);
    assert!(matches!(env.store.undo_event(&other, event.id).await, Err(Error::Forbidden(_))));
    env.store.remove_attachment(&env.human, added.id).await.unwrap();

    // A token scoped elsewhere can't see the task's pictures at all.
    let web = env
        .store
        .create_project(&env.human, NewProject { key: "WEB".into(), name: "Web".into(), ..Default::default() })
        .await
        .unwrap();
    let scoped = env.human.next_batch().scoped_to(vec![web.id]);
    assert!(matches!(env.store.list_attachments(&scoped, key).await, Err(Error::NotFound(_))));
    assert!(matches!(env.store.attachment_bytes(&scoped, by_person.id).await, Err(Error::NotFound(_))));
    assert!(matches!(env.store.add_attachment(&scoped, key, picture("s", 4)).await, Err(Error::NotFound(_))));
    assert!(matches!(env.store.remove_attachment(&scoped, by_person.id).await, Err(Error::NotFound(_))));

    // A deleted task's pictures are gone with it.
    env.store.delete_task(&env.human, key, None).await.unwrap();
    assert!(matches!(env.store.attachment_bytes(&env.human, by_person.id).await, Err(Error::NotFound(_))));
    assert!(env.store.list_attachments(&env.human, key).await.is_err());
}
