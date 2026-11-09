use super::*;

/// A field of a task the modal edits in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    Title,
    Description,
    Status,
    Priority,
    Grade,
}

impl Field {
    pub(super) fn label(self) -> &'static str {
        match self {
            Field::Title => "Title",
            Field::Description => "Description",
            Field::Status => "Status",
            Field::Priority => "Priority",
            Field::Grade => "Grade",
        }
    }
}

/// What changed in an edit in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Text(String),
    /// The option picked.
    Pick(usize),
}

/// On an inline widget of the modal: which field it edits and, while a save is out, what to go back to.
#[derive(Component, Debug, Clone)]
pub(super) struct Edits {
    pub(super) field: Field,
    /// What a status option means, for [`Field::Status`].
    pub(super) statuses: Vec<Status>,
    pub(super) pending: Option<Pending>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Pending {
    Text { old: String, new: String },
    Pick { old: Option<usize> },
}

/// The PATCH an edit in place sends: only the field that changed, on the version the modal shows. `statuses` says what
/// a status option means.
pub fn edit_request(
    key: &str,
    version: i64,
    field: Field,
    change: &Change,
    statuses: &[Status],
) -> Result<Request, String> {
    let mut update = TaskUpdate::default();
    match (field, change) {
        (Field::Title, Change::Text(t)) if t.trim().is_empty() => return Err("a task needs a title".into()),
        (Field::Title, Change::Text(t)) => update.title = Some(t.trim().to_string()),
        (Field::Description, Change::Text(t)) => update.description = Some(t.clone()),
        (Field::Status, Change::Pick(i)) => update.status = Some(*statuses.get(*i).ok_or("no such status")?),
        (Field::Priority, Change::Pick(i)) => update.priority = Some(*Priority::ALL.get(*i).ok_or("no such priority")?),
        (Field::Grade, Change::Pick(i)) => update.grade = Some(grade_choices().nth(*i).ok_or("no such grade")?),
        _ => return Err("that field isn't edited that way".into()),
    };
    Ok(Request::new(Call::UpdateTask { key: TaskKey(key.into()), update }, Some(version)))
}

/// How many lines the description's input is while it is edited.
pub(super) const DESCRIPTION_LINES: f32 = 6.0;

/// The title or description was edited: send it, or put it back with why not.
pub(super) fn on_inline_edited(
    edited: On<InlineEdited>,
    mut edits: Query<&mut Edits>,
    mut texts: Query<&mut InlineText>,
    state: Res<InspectorState>,
    connection: Res<State<Connection>>,
    mut submit: MessageWriter<Submit>,
) {
    let Ok(mut e) = edits.get_mut(edited.entity) else { return };
    let change = Change::Text(edited.new.clone());
    match edit_for(&state, &connection, e.field, &change, &e.statuses) {
        Ok(request) => {
            e.pending = Some(Pending::Text { old: edited.old.clone(), new: edited.new.clone() });
            submit.write(Submit { source: edited.entity, request });
        }
        Err(why) => {
            if let Ok(mut t) = texts.get_mut(edited.entity) {
                t.fail_keeping(edited.old.clone(), edited.new.clone(), why);
            }
        }
    }
}

/// A sidebar field was picked: send it, or put it back with why not.
pub(super) fn on_inline_chosen(
    chosen: On<InlineChosen>,
    mut edits: Query<&mut Edits>,
    mut fields: Query<&mut InlineField>,
    state: Res<InspectorState>,
    connection: Res<State<Connection>>,
    mut submit: MessageWriter<Submit>,
) {
    let Ok(mut e) = edits.get_mut(chosen.entity) else { return };
    match edit_for(&state, &connection, e.field, &Change::Pick(chosen.new), &e.statuses) {
        Ok(request) => {
            e.pending = Some(Pending::Pick { old: chosen.old });
            submit.write(Submit { source: chosen.entity, request });
        }
        Err(why) => {
            if let Ok(mut f) = fields.get_mut(chosen.entity) {
                f.fail(chosen.old, why);
            }
        }
    }
}

/// The request for an edit of the task shown, if it can be sent now.
pub(super) fn edit_for(
    state: &InspectorState,
    connection: &State<Connection>,
    field: Field,
    change: &Change,
    statuses: &[Status],
) -> Result<Request, String> {
    if *connection.get() != Connection::Live {
        return Err("Not connected to the server, so not saved.".into());
    }
    let view = state.0.as_ref().ok_or("no task is shown")?;
    let facts = view.task.as_ref().ok_or("no task is shown")?;
    edit_request(&key_of(view), facts.version, field, change, statuses)
}

/// The server answered an edit in place: it stays, or goes back with the reason. One that lost to another change
/// keeps what was typed for the next edit and fetches the task again, so that edit goes on the newer version.
pub(super) fn answer_edits(
    mut done: MessageReader<CommandDone>,
    mut edits: Query<&mut Edits>,
    mut texts: Query<&mut InlineText>,
    mut fields: Query<&mut InlineField>,
    mut refetch: MessageWriter<Refetch>,
) {
    for d in done.read() {
        let Ok(mut e) = edits.get_mut(d.source) else { continue };
        let Some(pending) = e.pending.take() else { continue };
        let conflict = matches!(d.outcome, Outcome::Conflict { .. });
        if conflict {
            refetch.write(Refetch);
        }
        let why = describe(&d.outcome);
        match (pending, why) {
            (Pending::Text { .. }, None) => {
                if let Ok(mut t) = texts.get_mut(d.source) {
                    t.confirm();
                }
            }
            (Pending::Pick { .. }, None) => {
                if let Ok(mut f) = fields.get_mut(d.source) {
                    f.confirm();
                }
            }
            (Pending::Text { old, new }, Some(why)) => {
                if let Ok(mut t) = texts.get_mut(d.source) {
                    if conflict {
                        t.fail_keeping(old, new, format!("{why}. Yours is kept: edit to save it again."));
                    } else {
                        t.fail(old, why);
                    }
                }
            }
            (Pending::Pick { old }, Some(why)) => {
                if let Ok(mut f) = fields.get_mut(d.source) {
                    f.fail(old, why);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {

    use super::super::testing::settle;
    use super::super::testing::*;
    use super::*;

    fn editor(app: &mut App, field: Field) -> Entity {
        let world = app.world_mut();
        let mut q = world.query::<(Entity, &Edits)>();
        q.iter(world).find(|(_, e)| e.field == field).map(|(e, _)| e).expect("the field's editor")
    }

    fn answer(app: &mut App, source: Entity, outcome: Outcome) {
        app.world_mut().write_message(CommandDone { source, id: crate::commands::CommandId(0), outcome });
        settle(app);
    }

    #[test]
    fn each_field_sends_only_itself_on_the_version_shown() {
        let st = [Status::InProgress, Status::InReview];
        let r = |field, change| edit_request("TRAIL-7", 4, field, &change, &st).unwrap();
        let title = r(Field::Title, Change::Text("  New title ".into()));
        assert_eq!(
            (title.call.method(), title.call.path().as_str(), title.if_match),
            (Method::PATCH, "/v1/tasks/TRAIL-7", Some(4))
        );
        assert_eq!(title.call.body(), Some(json!({"title": "New title"})));
        assert_eq!(r(Field::Description, Change::Text("why".into())).call.body(), Some(json!({"description": "why"})));
        assert_eq!(r(Field::Status, Change::Pick(1)).call.body(), Some(json!({"status": "in_review"})));
        assert_eq!(r(Field::Priority, Change::Pick(0)).call.body(), Some(json!({"priority": 1})));
        assert_eq!(r(Field::Grade, Change::Pick(2)).call.body(), Some(json!({"grade": 2})));
        assert_eq!(r(Field::Grade, Change::Pick(0)).call.body(), Some(json!({"grade": null})), "none clears it");
        assert!(edit_request("TRAIL-7", 4, Field::Title, &Change::Text(" ".into()), &st).is_err());
    }

    #[test]
    fn an_edited_title_is_sent_and_stays_once_it_went_through() {
        let mut app = app();
        let title = editor(&mut app, Field::Title);
        app.world_mut().trigger(InlineEdited { entity: title, old: "do it".into(), new: "do it well".into() });
        let sent = sent(&mut app);
        assert_eq!(sent.len(), 1);
        assert_eq!((sent[0].source, sent[0].request.if_match), (title, Some(4)));
        assert_eq!(sent[0].request.call.body(), Some(json!({"title": "do it well"})));
        answer(&mut app, title, Outcome::Ok { version: Some(5) });
        assert!(app.world().get::<Edits>(title).unwrap().pending.is_none());
        assert!(app.world().get::<InlineText>(title).unwrap().error().is_empty());
    }

    #[derive(Resource, Default)]
    struct Refetched(usize);

    fn count_refetches(mut asked: MessageReader<Refetch>, mut n: ResMut<Refetched>) {
        n.0 += asked.read().count();
    }

    #[test]
    fn a_conflict_fetches_again_says_so_and_keeps_what_was_typed() {
        let mut app = app();
        let description = editor(&mut app, Field::Description);
        app.world_mut().trigger(InlineEdited { entity: description, old: String::new(), new: "mine".into() });
        sent(&mut app);
        // Counted by a reader rather than drained afterwards: MinimalPlugins drops messages after fixed-timestep
        // ticks, which follow wall-clock time, so on a slow runner settle() could outlive the message.
        app.init_resource::<Refetched>().add_systems(Update, count_refetches);
        answer(&mut app, description, Outcome::Conflict { current_version: Some(5), message: "stale".into() });
        let t = app.world().get::<InlineText>(description).unwrap();
        assert_eq!(t.draft(), Some("mine"));
        assert!(t.error().contains("changed since you looked"), "{}", t.error());
        assert_eq!(app.world().resource::<Refetched>().0, 1, "the task is fetched again");
    }

    #[test]
    fn a_refused_status_move_shows_its_reason_beside_the_field() {
        let mut app = app();
        let status = editor(&mut app, Field::Status);
        let statuses = app.world().get::<Edits>(status).unwrap().statuses.clone();
        assert_eq!(statuses[0], Status::InProgress, "its own status first");
        let to = statuses.iter().position(|s| *s == Status::NeedsInput).unwrap();
        app.world_mut().trigger(InlineChosen { entity: status, old: Some(0), new: to });
        let sent = sent(&mut app);
        assert_eq!(sent[0].request.call.body(), Some(json!({"status": "needs_input"})));
        answer(&mut app, status, Outcome::Failed("say why it's blocked".into()));
        let f = app.world().get::<InlineField>(status).unwrap();
        assert_eq!((f.selected(), f.error()), (Some(0), "say why it's blocked"));
    }

    #[test]
    fn offline_the_fields_are_text_and_add_sub_task_is_off() {
        let mut app = app();
        assert!(app.world_mut().query::<&Edits>().iter(app.world()).count() >= 5);
        app.world_mut().resource_mut::<NextState<Connection>>().set(Connection::Offline);
        settle(&mut app);
        assert_eq!(app.world_mut().query::<&Edits>().iter(app.world()).count(), 0);
        let add = app.world_mut().query_filtered::<Entity, With<Act>>().iter(app.world()).collect::<Vec<_>>();
        let world = app.world();
        assert!(add.iter().any(|e| world.get::<Act>(*e) == Some(&Act::NewSubtask("TRAIL-7".into()))
            && world.get::<InteractionDisabled>(*e).is_some()));
    }
}
