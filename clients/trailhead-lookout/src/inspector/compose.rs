use super::*;

/// How many lines the comment box's input shows.
pub(super) const COMPOSE_LINES: f32 = 4.0;

/// The kinds the comment box can send, in the order of its dropdown.
pub(super) const COMPOSE_KINDS: [CommentKind; 4] =
    [CommentKind::Note, CommentKind::Handoff, CommentKind::Decision, CommentKind::Question];

/// A comment kind in words, as the activity and the comment box show it.
pub(super) fn kind_label(kind: CommentKind) -> &'static str {
    match kind {
        CommentKind::Note => "Note",
        CommentKind::Handoff => "Handoff",
        CommentKind::Decision => "Decision",
        CommentKind::Question => "Question",
        CommentKind::Progress => "Progress",
        CommentKind::Review => "Review",
    }
}

/// The comment box's state: what it sends next, and whether it answers a question.
#[derive(Resource, Debug)]
pub struct Compose {
    /// The task the box belongs to; another task starts over.
    pub subject: Option<Subject>,
    pub kind: CommentKind,
    /// The question (its id and body) the box answers, if it does.
    pub answering: Option<(String, String)>,
    /// The box's input should take the keyboard once it exists.
    pub focus: bool,
}

impl Default for Compose {
    fn default() -> Self {
        Compose { subject: None, kind: CommentKind::Note, answering: None, focus: false }
    }
}

/// Opens a task in the modal with the keyboard in its comment box, answering a question if there is one. The board's
/// comment and answer buttons send it, and so does a question's answer button in the modal.
#[derive(Event, Debug, Clone, PartialEq, Eq)]
pub struct OpenComposer {
    pub key: String,
    /// The question's id and body.
    pub answer: Option<(String, String)>,
}

/// On the comment box's parts.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(super) enum ComposePart {
    #[default]
    Input,
    Kind,
    Send,
    Answering,
    StopAnswering,
}

/// A different task starts the comment box over.
pub(super) fn follow_compose(state: Res<InspectorState>, mut compose: ResMut<Compose>) {
    if !state.is_changed() {
        return;
    }
    let subject = state.0.as_ref().filter(|v| v.task.is_some()).map(|v| v.subject.clone());
    if compose.subject != subject {
        let focus = compose.focus && subject.is_some();
        let answering = if focus { compose.answering.take() } else { None };
        *compose = Compose { subject, answering, focus, ..Compose::default() };
    }
}

/// Opens the task in the modal with its comment box ready, answering the question if there is one.
pub(super) fn on_open_composer(
    open: On<OpenComposer>,
    tasks: Query<&Mirrored<Task>>,
    mut selection: ResMut<Selection>,
    mut compose: ResMut<Compose>,
) {
    let Some(task) = tasks.iter().find(|t| t.0.key == open.key.as_str()) else { return };
    let subject = Subject::Task(task.0.id.to_string());
    if compose.subject.as_ref() != Some(&subject) {
        *compose = Compose::default();
    }
    compose.answering = open.answer.clone();
    compose.focus = true;
    selection.0 = Some(subject);
}

/// Gives the comment box's input the keyboard once it exists, when it was asked for.
pub(super) fn focus_composer(
    mut compose: ResMut<Compose>,
    inputs: Query<Entity, (With<ComposePart>, With<EditableText>)>,
    parts: Query<&ComposePart>,
    mut focus: ResMut<InputFocus>,
) {
    if !compose.focus {
        return;
    }
    if let Some(input) = inputs.iter().find(|e| parts.get(*e) == Ok(&ComposePart::Input)) {
        focus.set(input, FocusCause::Navigated);
        compose.focus = false;
    }
}

/// The comment box's buttons: send, and stop answering.
#[allow(clippy::too_many_arguments)]
pub(super) fn on_compose_press(
    activate: On<Activate>,
    parts: Query<&ComposePart>,
    inputs: Query<(&ComposePart, &EditableText)>,
    state: Res<InspectorState>,
    mut compose: ResMut<Compose>,
    controls: Res<Controls>,
    mut sources: Query<&mut Control>,
    mut submit: MessageWriter<Submit>,
) {
    let Ok(part) = parts.get(activate.entity) else { return };
    match part {
        ComposePart::StopAnswering => compose.answering = None,
        ComposePart::Send => {
            let Some(view) = &state.0 else { return };
            let Some(source) = controls.0.get(&(view.subject.clone(), Action::Comment)).copied() else { return };
            let body = inputs
                .iter()
                .find(|(p, _)| **p == ComposePart::Input)
                .map(|(_, t)| t.value().to_string())
                .unwrap_or_default();
            if body.trim().is_empty() {
                return;
            }
            let request = compose_request(&key_of(view), compose.kind, compose.answering.as_ref(), &body);
            submit.write(Submit { source, request });
            if let Ok(mut c) = sources.get_mut(source) {
                *c = Control::Busy;
            }
        }
        _ => {}
    }
}

/// The request the comment box sends: a comment of `kind` on the task, or the answer to a question.
pub fn compose_request(key: &str, kind: CommentKind, answering: Option<&(String, String)>, body: &str) -> Request {
    let body = body.trim();
    match answering {
        Some((id, _)) => {
            Request::new(Method::POST, format!("/v1/comments/{id}/resolve"), Some(json!({"answer": body})), None)
        }
        None => Request::new(
            Method::POST,
            format!("/v1/tasks/{key}/comments"),
            Some(json!({"kind": kind, "body": body})),
            None,
        ),
    }
}

/// Send is pressable only with something in the box and the connection live.
pub(super) fn gate_send(
    composers: Query<(Entity, &Shown, &Parts)>,
    inputs: Query<(Entity, &ComposePart, &EditableText)>,
    parents: Query<&ChildOf>,
    disabled: Query<Has<InteractionDisabled>>,
    mut commands: Commands,
) {
    for (composer, Shown(block), parts) in &composers {
        let (Block::Composer { live, .. }, Some(send)) = (block, parts.resolve) else { continue };
        let written = inputs
            .iter()
            .filter(|(_, part, _)| **part == ComposePart::Input)
            .find(|(input, ..)| parents.iter_ancestors(*input).any(|a| a == composer))
            .is_some_and(|(_, _, text)| !text.value().to_string().trim().is_empty());
        let off = !(*live && written);
        if disabled.get(send) != Ok(off) {
            gate_button(&mut commands, send, !off);
        }
    }
}

/// A comment the box sent went through: empty the box and stop answering.
pub(super) fn clear_sent(
    controls: Res<Controls>,
    state: Res<InspectorState>,
    changed: Query<&Control, Changed<Control>>,
    mut inputs: Query<(&ComposePart, &mut EditableText)>,
    mut compose: ResMut<Compose>,
) {
    let Some(view) = &state.0 else { return };
    let Some(source) = controls.0.get(&(view.subject.clone(), Action::Comment)) else { return };
    if changed.get(*source) != Ok(&Control::Done) {
        return;
    }
    for (part, mut text) in &mut inputs {
        if *part == ComposePart::Input {
            text.editor_mut().set_text("");
        }
    }
    if compose.answering.is_some() {
        compose.answering = None;
    }
}

#[cfg(test)]
mod tests {

    use super::super::testing::settle;
    use super::super::testing::*;
    use super::*;
    use crate::fixture::sid;

    fn compose_button(app: &mut App, part: ComposePart) -> Entity {
        let world = app.world_mut();
        let mut q = world.query::<(Entity, &ComposePart)>();
        q.iter(world).find(|(_, p)| **p == part).map(|(e, _)| e).unwrap()
    }

    #[test]
    fn the_comment_box_sends_a_comment_and_empties_once_it_went_through() {
        let mut app = app();
        let input = composer_input(&mut app);
        app.world_mut().get_mut::<EditableText>(input).unwrap().editor_mut().set_text("looks good");
        app.world_mut().resource_mut::<Compose>().kind = CommentKind::Decision;
        let send = compose_button(&mut app, ComposePart::Send);
        app.world_mut().trigger(Activate { entity: send });
        let sent = sent(&mut app);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].request.path, "/v1/tasks/TRAIL-7/comments");
        assert_eq!(sent[0].request.body, Some(json!({"kind": "decision", "body": "looks good"})));
        assert_eq!(app.world().get::<Control>(sent[0].source), Some(&Control::Busy));
        // The server took it.
        *app.world_mut().get_mut::<Control>(sent[0].source).unwrap() = Control::Done;
        settle(&mut app);
        assert_eq!(app.world().get::<EditableText>(input).unwrap().value().to_string(), "");
    }

    #[test]
    fn send_is_disabled_until_the_comment_box_has_words() {
        let mut app = app();
        let send = compose_button(&mut app, ComposePart::Send);
        let disabled = |app: &App| app.world().get::<InteractionDisabled>(send).is_some();
        assert!(disabled(&app), "an empty box can't send");
        let input = composer_input(&mut app);
        app.world_mut().get_mut::<EditableText>(input).unwrap().editor_mut().set_text("  ");
        settle(&mut app);
        assert!(disabled(&app), "blank isn't words");
        app.world_mut().get_mut::<EditableText>(input).unwrap().editor_mut().set_text("looks good");
        settle(&mut app);
        assert!(!disabled(&app));
        app.world_mut().get_mut::<EditableText>(input).unwrap().editor_mut().set_text("");
        settle(&mut app);
        assert!(disabled(&app), "emptied again");
    }

    #[test]
    fn answering_opens_the_task_with_the_box_answering_and_sends_the_answer() {
        let mut app = app();
        app.world_mut().resource_mut::<Selection>().0 = None;
        settle(&mut app);
        let c1 = sid("c1");
        app.world_mut().trigger(OpenComposer { key: "TRAIL-7".into(), answer: Some((c1.clone(), "which db?".into())) });
        settle(&mut app);
        assert_eq!(app.world().resource::<Selection>().0, Some(Subject::Task(sid("t7"))));
        assert!(texts(&mut app).iter().any(|t| t == "Answering: which db?"));
        let input = composer_input(&mut app);
        assert_eq!(app.world().resource::<InputFocus>().get(), Some(input), "the keyboard is in the box");
        app.world_mut().get_mut::<EditableText>(input).unwrap().editor_mut().set_text("sqlite");
        let send = compose_button(&mut app, ComposePart::Send);
        app.world_mut().trigger(Activate { entity: send });
        let sent = sent(&mut app);
        assert_eq!(sent[0].request.path, format!("/v1/comments/{c1}/resolve"));
        assert_eq!(sent[0].request.body, Some(json!({"answer": "sqlite"})));
        // Stopping answering goes back to a comment.
        let stop = compose_button(&mut app, ComposePart::StopAnswering);
        app.world_mut().trigger(Activate { entity: stop });
        assert_eq!(app.world().resource::<Compose>().answering, None);
    }
}
