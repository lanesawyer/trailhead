use super::*;

/// How a [`Block::Text`] is colored; fixed for the item that holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tone {
    Body,
    Dim,
    Warn,
}

/// One thing in a section of the panel. Items are keyed (`status/line/0`, `q/<comment id>`, `act/Release`), so a
/// change in the task changes the entities of what changed and nothing else.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Block {
    /// A line of text; only the words change while the key stays.
    Text { text: String, size: f32, tone: Tone },
    /// The `P1` badge. Its key holds the priority, so a new priority is a new badge.
    Priority(Priority),
    /// The `G2` badge, toned by grade. Its key holds the grade, so a new grade is a new badge.
    GradeBadge(Grade),
    /// The wrapping row of a PR's gates, which are [`Gate`]s keyed under it.
    Gates,
    /// An open question, with its answer and resolve buttons and what became of the last resolve.
    Question { key: String, id: String, body: String, live: bool, note: Option<Control> },
    /// The grade dropdown (live only); the offline grade is a [`Block::Text`].
    Grade { grade: Option<Grade>, note: Option<Control> },
    /// The status dropdown: the task's own status first.
    Move { statuses: Vec<Status>, note: Option<Control> },
    /// A button for `action` (a revert has its window's dropdown, `window` hours).
    Act { action: Action, window: Option<i64>, note: Option<Control> },
    /// A subtask's row, which opens it. Its key holds what it shows, so a change is a new row.
    Subtask(Subtask),
    /// The parent's key and title, which open it. Its key holds what it shows, so a change is a new button.
    Parent(Subtask),
    /// The project, by key, which opens its page.
    Project(String),
    /// A comment in the activity. An open question has answer and resolve buttons; its key holds whether it is open.
    Entry { key: String, entry: Entry, live: bool, note: Option<Control> },
    /// The comment box at the foot of the activity, spawned once per task so what is typed survives redraws.
    Composer { answering: Option<String>, live: bool, note: Option<Control> },
    /// The title or the description, edited where it is read. Its key doesn't hold the words, so an edit in progress
    /// survives a change from elsewhere.
    Inline { field: Field, value: String },
    /// A sidebar field picked from a dropdown where it is read: `options[i]` is shown for what `statuses[i]` (status)
    /// or the field's scale (priority, grade) means.
    Choice { field: Field, options: Vec<String>, selected: Option<usize>, statuses: Vec<Status> },
    /// The button that opens the new-task form under this task.
    AddSubtask { key: String, live: bool },
}

/// One of a PR's gates: its key holds the state, so a gate that changes state is a new badge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Gate {
    pub(super) name: String,
    pub(super) state: GateState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GateState {
    Passed,
    Waiting,
}

/// A block on its entity, which [`dress`] brings in line with it when it changes.
#[derive(Component, Debug, Clone)]
pub(super) struct Shown(pub(super) Block);

/// The parts of a block's entity that are set in place.
#[derive(Component, Debug, Clone, Copy, Default)]
pub(super) struct Parts {
    pub(super) text: Option<Entity>,
    pub(super) answer: Option<Entity>,
    pub(super) resolve: Option<Entity>,
    pub(super) select: Option<Entity>,
    pub(super) note: Option<Entity>,
}

pub(super) type Plan = Vec<(String, Vec<(String, Block)>)>;

pub(super) fn text_block(text: impl Into<String>, size: f32, tone: Tone) -> Block {
    Block::Text { text: text.into(), size, tone }
}

/// The panel's sections and their items, in order. `note` is where a control's command stands, by action.
pub(super) fn layout(
    view: &InspectorView,
    live: bool,
    offline: &str,
    window: i64,
    note: &dyn Fn(&Action) -> Option<Control>,
) -> Plan {
    let mut plan: Plan = Vec::new();
    let mut section = |key: &str, heading: &str, mut items: Vec<(String, Block)>| {
        if !heading.is_empty() {
            items.insert(0, (format!("{key}/heading"), text_block(heading, size::SECONDARY, Tone::Dim)));
        }
        plan.push((key.to_string(), items));
    };

    let (run, mut status): (Vec<String>, Vec<String>) = view.lines.iter().cloned().partition(|l| is_run_line(l));
    // The "Move to" dropdown shows the status when it is drawn; otherwise the section says it in words.
    let has_move = live && view.actions.iter().any(|a| matches!(a, Action::Status(_)));
    if let (Some(s), false) = (view.status, has_move) {
        status.insert(0, format!("status: {}", status_label(s)));
    }
    for (key, heading, lines) in [("status", "Status", status), ("run", "Run", run)] {
        if lines.is_empty() && !(key == "status" && (view.priority.is_some() || view.status.is_some())) {
            continue;
        }
        let mut items = Vec::new();
        if let (true, Some(p)) = (key == "status", view.priority) {
            items.push((format!("{key}/priority/{p}"), Block::Priority(p)));
        }
        items.extend(
            lines
                .into_iter()
                .enumerate()
                .map(|(i, l)| (format!("{key}/line/{i}"), text_block(l, size::BODY, Tone::Body))),
        );
        if key == "status" && view.status.is_some() {
            if live {
                let note = note(&Action::Grade(None));
                items.push(("status/grade".to_string(), Block::Grade { grade: view.grade, note }));
            } else {
                let grade = view.grade.map_or_else(|| "none".to_string(), |g| g.to_string());
                items.push((
                    "status/grade/off".to_string(),
                    text_block(format!("grade: {grade}"), size::BODY, Tone::Body),
                ));
            }
        }
        section(key, heading, items);
    }

    if !view.questions.is_empty() {
        let items = view
            .questions
            .iter()
            .map(|q| {
                let block = Block::Question {
                    key: q.key.clone(),
                    id: q.id.clone(),
                    body: q.body.clone(),
                    live,
                    note: note(&Action::Resolve(q.id.clone())),
                };
                (format!("q/{}", q.id), block)
            })
            .collect();
        section("questions", "Questions", items);
    }

    if view.pr.is_some() || view.merge_note.is_some() {
        let mut items = Vec::new();
        if let Some(pr) = &view.pr {
            items
                .push(("pr/url".to_string(), text_block(format!("#{} {}", pr.number, pr.url), size::BODY, Tone::Body)));
            items.push(("pr/state".to_string(), text_block(&pr.state, size::SECONDARY, Tone::Dim)));
            if let Some(g) = pr.grade {
                items.push((format!("pr/grade/{g}"), Block::GradeBadge(g)));
            }
            if !gates(pr).is_empty() {
                items.push(("pr/gates".to_string(), Block::Gates));
            }
            if let Some(problem) = &pr.problem {
                items.push(("pr/problem".to_string(), text_block(problem, size::BODY, Tone::Warn)));
            }
        }
        if let Some(merge_note) = &view.merge_note {
            items.push(("pr/note".to_string(), text_block(merge_note, size::SECONDARY, Tone::Dim)));
        }
        section("pr", "Pull request", items);
    }

    if !live {
        let items = vec![("offline/text".to_string(), text_block(offline, size::BODY, Tone::Warn))];
        section("offline", "", items);
        return plan;
    }

    let mut items = Vec::new();
    // One dropdown for every status the task can move to, the current one showing.
    let moves: Vec<Status> =
        view.actions.iter().filter_map(|a| if let Action::Status(st) = a { Some(*st) } else { None }).collect();
    if let (Some(current), false) = (view.status, moves.is_empty()) {
        let statuses = std::iter::once(current).chain(moves).collect();
        items.push(("move".to_string(), Block::Move { statuses, note: note(&MOVE_CONTROL) }));
    }
    for action in &view.actions {
        let action = match action {
            Action::Status(_) | Action::Resolve(_) => continue,
            // One control however far back, so the answer finds it when the window changes.
            Action::Revert(_) => Action::Revert(0),
            other => other.clone(),
        };
        let window = matches!(action, Action::Revert(_)).then_some(window);
        let block = Block::Act { note: note(&action), window, action: action.clone() };
        items.push((format!("act/{action:?}"), block));
    }
    section("actions", "Actions", items);
    plan
}

/// The task modal's two columns, each a list of sections: the main column (title, description, subtasks, open
/// questions, the PR) and the sidebar (one fact per section, then the actions). Section keys are unique across both.
pub(super) fn layout_task(
    view: &InspectorView,
    facts: &TaskFacts,
    live: bool,
    offline: &str,
    human: &[Grade],
    answering: Option<&str>,
    note: &dyn Fn(&Action) -> Option<Control>,
) -> (Plan, Plan) {
    fn add(plan: &mut Plan, key: &str, heading: &str, mut items: Vec<(String, Block)>) {
        if !heading.is_empty() {
            items.insert(0, (format!("{key}/heading"), text_block(heading, size::SECONDARY, Tone::Dim)));
        }
        plan.push((key.to_string(), items));
    }
    let lines = |key: &str, lines: &[String]| -> Vec<(String, Block)> {
        lines.iter().enumerate().map(|(i, l)| (format!("{key}/{i}"), text_block(l, size::BODY, Tone::Body))).collect()
    };

    let mut main: Plan = Vec::new();
    let key = key_of(view);
    let (title, description) = if live {
        (
            ("m/title/edit".to_string(), Block::Inline { field: Field::Title, value: facts.title.clone() }),
            (
                "m/description/edit".to_string(),
                Block::Inline { field: Field::Description, value: facts.description.clone() },
            ),
        )
    } else {
        let description = if facts.description.is_empty() {
            text_block("No description", size::BODY, Tone::Dim)
        } else {
            text_block(&facts.description, size::BODY, Tone::Body)
        };
        (
            ("m/title/text".to_string(), text_block(&facts.title, size::DETAIL_TITLE, Tone::Body)),
            ("m/description/text".to_string(), description),
        )
    };
    add(&mut main, "m/title", "", vec![title]);
    add(&mut main, "m/description", "", vec![description]);
    let mut rows: Vec<(String, Block)> = facts
        .subtasks
        .iter()
        .map(|t| (format!("m/sub/{}/{:?}/{}", t.id, t.status, t.title), Block::Subtask(t.clone())))
        .collect();
    rows.push(("m/sub/add".into(), Block::AddSubtask { key: key.clone(), live }));
    add(&mut main, "m/subtasks", "Sub-tasks", rows);
    if view.pr.is_some() || view.merge_note.is_some() {
        let mut items = Vec::new();
        if let Some(pr) = &view.pr {
            items
                .push(("pr/url".to_string(), text_block(format!("#{} {}", pr.number, pr.url), size::BODY, Tone::Body)));
            items.push(("pr/state".to_string(), text_block(&pr.state, size::SECONDARY, Tone::Dim)));
            if let Some(g) = pr.grade {
                items.push((format!("pr/grade/{g}"), Block::GradeBadge(g)));
            }
            if !gates(pr).is_empty() {
                items.push(("pr/gates".to_string(), Block::Gates));
            }
            if let Some(problem) = &pr.problem {
                items.push(("pr/problem".to_string(), text_block(problem, size::BODY, Tone::Warn)));
            }
        }
        if let Some(merge_note) = &view.merge_note {
            items.push(("pr/note".to_string(), text_block(merge_note, size::SECONDARY, Tone::Dim)));
        }
        add(&mut main, "m/pr", "Pull request", items);
    }
    let mut activity: Vec<(String, Block)> = facts
        .activity
        .iter()
        .map(|e| {
            let note = if e.open_question { note(&Action::Resolve(e.id.clone())) } else { None };
            let block = Block::Entry { key: key.clone(), entry: e.clone(), live, note };
            // Who is reading may be learned after the first draw, which moves their entries to the right.
            (format!("a/{}/{}/{}", e.id, e.open_question, e.mine), block)
        })
        .collect();
    if activity.is_empty() {
        activity.push(("a/none".into(), text_block("No comments yet", size::BODY, Tone::Dim)));
    }
    activity.push((
        "a/compose".into(),
        Block::Composer { answering: answering.map(str::to_string), live, note: note(&Action::Comment) },
    ));
    add(&mut main, "m/activity", "Activity", activity);
    if let Some(why) = &facts.fetch_error {
        let words = format!("Couldn't load the whole task: {why}");
        add(&mut main, "m/fetch", "", vec![("m/fetch/text".into(), text_block(words, size::SECONDARY, Tone::Warn))]);
    }
    if !live {
        add(&mut main, "m/offline", "", vec![("m/offline/text".into(), text_block(offline, size::BODY, Tone::Warn))]);
    }

    let mut side: Plan = Vec::new();
    if let Some(key) = &facts.project {
        add(&mut side, "s/project", "Project", vec![(format!("s/project/{key}"), Block::Project(key.clone()))]);
    }
    if let Some(parent) = &facts.parent {
        let row = (format!("s/parent/{}/{}", parent.id, parent.title), Block::Parent(parent.clone()));
        add(&mut side, "s/parent", "Parent", vec![row]);
    }
    let status = view.status.unwrap_or(Status::Open);
    let mut fields = Vec::new();
    if live {
        let moves = view.actions.iter().filter_map(|a| if let Action::Status(st) = a { Some(*st) } else { None });
        let statuses: Vec<Status> = std::iter::once(status).chain(moves).collect();
        let options = status_labels(&statuses);
        fields.push((
            "s/fields/status".to_string(),
            Block::Choice { field: Field::Status, options, selected: Some(0), statuses },
        ));
        let priorities = Priority::ALL.iter().map(|p| format!("P{p}")).collect();
        let selected = view.priority.and_then(|p| Priority::ALL.iter().position(|q| *q == p));
        fields.push((
            "s/fields/priority".to_string(),
            Block::Choice { field: Field::Priority, options: priorities, selected, statuses: Vec::new() },
        ));
        fields.push((
            "s/fields/grade".to_string(),
            Block::Choice {
                field: Field::Grade,
                options: grade_options(human),
                selected: grade_index(view.grade),
                statuses: Vec::new(),
            },
        ));
    } else {
        fields.push((
            "s/fields/status/text".to_string(),
            text_block(format!("Status: {}", status_label(status)), size::BODY, Tone::Body),
        ));
        if let Some(p) = view.priority {
            fields.push((format!("s/fields/priority/{p}"), Block::Priority(p)));
        }
        let words = view.grade.map_or_else(|| "none".to_string(), |g| format!("G{g}"));
        fields.push(("s/fields/grade/text".to_string(), text_block(format!("Grade: {words}"), size::BODY, Tone::Body)));
    }
    if let Some(g) = view.grade {
        let words = if human.contains(&g) {
            "Above Treeline: a person merges it"
        } else {
            "Below Treeline: merges once its gates pass"
        };
        fields.push(("s/fields/treeline".to_string(), text_block(words, size::SECONDARY, Tone::Dim)));
    }
    add(&mut side, "s/fields", "", fields);
    let claim = match &facts.claim {
        Some(line) => vec![("s/claim/text".to_string(), text_block(line, size::BODY, Tone::Body))],
        None => vec![("s/claim/none".to_string(), text_block("Nobody holds it", size::BODY, Tone::Dim))],
    };
    add(&mut side, "s/claim", "Claim", claim);
    for (key, heading, rows) in [
        ("s/dates", "Dates", &facts.dates),
        ("s/links", "Dependencies", &facts.links),
        ("s/run", "Factory run", &facts.run),
        ("s/details", "Details", &facts.details),
    ] {
        if !rows.is_empty() {
            add(&mut side, key, heading, lines(key, rows));
        }
    }
    (main, side)
}

/// The task's key, the last part of the view's breadcrumb.
pub(super) fn key_of(view: &InspectorView) -> String {
    view.task.as_ref().and_then(|f| f.breadcrumb.rsplit(" › ").next()).unwrap_or_default().to_string()
}

/// A PR's gates, in the order the panel shows them.
pub(super) fn gates(pr: &PrPanel) -> Vec<(String, Gate)> {
    [(&pr.passed, GateState::Passed, "passed"), (&pr.waiting, GateState::Waiting, "waiting")]
        .into_iter()
        .flat_map(|(names, state, word)| {
            names.iter().map(move |name| (format!("{word}/{name}"), Gate { name: name.clone(), state }))
        })
        .collect()
}

pub(super) fn window_labels() -> Vec<String> {
    WINDOWS.iter().map(|h| format!("{h}h ago")).collect()
}

/// The words beside a control for where its command stands, and their color; no words for none.
pub(super) fn note_look(note: &Option<Control>) -> (String, Token) {
    match note {
        Some(Control::Busy) => ("sending…".into(), token::TEXT_DIM),
        // The command went through: not the task's status, so not `status_label`'s.
        Some(Control::Done) => (String::from("done"), token::TEXT_DIM),
        Some(Control::Refused(why)) => (why.clone(), token::DANGER),
        _ => (String::new(), token::TEXT_DIM),
    }
}

pub(super) fn spawn_note(commands: &mut Commands, parent: Entity, note: &Option<Control>) -> Entity {
    let (words, tone) = note_look(note);
    let display = if words.is_empty() { Display::None } else { Display::Flex };
    commands
        .spawn_scene(bsn! { @text_toned(words, size::SECONDARY, tone) Node { display: { display } } })
        .insert(ChildOf(parent))
        .id()
}

/// Disables a button while the connection is not live.
pub(super) fn gate_button(commands: &mut Commands, button: Entity, live: bool) {
    if live {
        commands.entity(button).remove::<InteractionDisabled>();
    } else {
        commands.entity(button).insert(InteractionDisabled);
    }
}

/// Makes the entity for a new block, not yet parented.
pub(super) fn spawn_block(
    commands: &mut Commands,
    controls: &mut Controls,
    subject: &Subject,
    block: &Block,
    human: &[Grade],
) -> Entity {
    let item = match block {
        Block::Text { text: words, size, tone } => {
            let scene: Box<dyn Scene> = match tone {
                Tone::Body => Box::new(text(words.clone(), *size)),
                Tone::Dim => Box::new(text_dim(words.clone(), *size)),
                Tone::Warn => Box::new(text_toned(words.clone(), *size, token::WARN)),
            };
            commands.spawn_scene(scene).id()
        }
        Block::Priority(p) => commands.spawn_scene(badge(format!("P{p}"), None, Some(priority_tone(*p)))).id(),
        Block::GradeBadge(g) => commands.spawn_scene(badge(format!("G{g}"), None, Some(grade_tone(*g)))).id(),
        Block::Gates => commands.spawn_scene(bsn! { @row() Node { flex_wrap: FlexWrap::Wrap } }).id(),
        Block::Question { key, id, body, live, note } => {
            let item = commands
                .spawn(Node { flex_direction: FlexDirection::Column, row_gap: px(space::ROWS), ..default() })
                .id();
            let line = commands.spawn_scene(bsn! { @row() }).insert(ChildOf(item)).id();
            let words = commands
                .spawn_scene(
                    bsn! { @text_toned(format!("? {body}"), size::BODY, token::WARN) Node { flex_grow: 1.0, flex_shrink: 1.0, min_width: px(0) } },
                )
                .insert(ChildOf(line))
                .id();
            // The same form the board opens; it saves even after the inspector moves on.
            let act = Act::Answer { key: key.clone(), comment_id: id.clone(), question: body.clone() };
            let answer = commands
                .spawn_scene(bsn! { @tool_button(Icon::Comment, "Answer", ButtonKind::Normal) })
                .insert((act, ChildOf(line)))
                .id();
            let resolve_action = Action::Resolve(id.clone());
            let source = control(controls, commands, subject, &resolve_action);
            let resolve = commands
                .spawn_scene(bsn! {
                    @tool_button(Icon::Check, "Resolve without an answer", ButtonKind::Normal)
                    @sends(source, subject.clone(), resolve_action.clone())
                })
                .insert(ChildOf(line))
                .id();
            gate_button(commands, answer, *live);
            gate_button(commands, resolve, *live);
            let note = spawn_note(commands, item, note);
            commands.entity(item).insert(Parts {
                text: Some(words),
                answer: Some(answer),
                resolve: Some(resolve),
                note: Some(note),
                ..default()
            });
            item
        }
        Block::Grade { grade, note } => {
            let line = commands.spawn_scene(bsn! { @row() }).id();
            commands.spawn_scene(text_dim("Grade", size::SECONDARY)).insert(ChildOf(line));
            let source = control(controls, commands, subject, &Action::Grade(None));
            let options = grade_options(human);
            let max_visible = options.len();
            let picker = commands
                .spawn_scene(select(options, grade_index(*grade), max_visible))
                .insert((GradeChoice { source, subject: subject.clone() }, ChildOf(line)))
                .id();
            let note = spawn_note(commands, line, note);
            commands.entity(line).insert(Parts { select: Some(picker), note: Some(note), ..default() });
            line
        }
        Block::Move { statuses, note } => {
            let line = commands.spawn_scene(bsn! { @row() }).id();
            commands.spawn_scene(text_dim("Move to", size::SECONDARY)).insert(ChildOf(line));
            let source = control(controls, commands, subject, &MOVE_CONTROL);
            let max_visible = statuses.len();
            let picker = commands
                .spawn_scene(select(status_labels(statuses), Some(0), max_visible))
                .insert((StatusMove { source, subject: subject.clone(), statuses: statuses.clone() }, ChildOf(line)))
                .id();
            let note = spawn_note(commands, line, note);
            commands.entity(line).insert(Parts { select: Some(picker), note: Some(note), ..default() });
            line
        }
        Block::Entry { key, entry, live, note } => {
            let side = if entry.mine { Side::Mine } else { Side::Theirs };
            let item = commands.spawn_scene(bubble(side)).id();
            let mut head = vec![kind_label(entry.kind).to_string(), entry.author.clone(), entry.when.clone()];
            head.extend(entry.tags.iter().cloned());
            commands.spawn_scene(text_dim(head.join(" · "), size::SECONDARY)).insert(ChildOf(item));
            let tone = if entry.open_question { token::WARN } else { token::TEXT };
            commands.spawn_scene(text_toned(entry.body.clone(), size::BODY, tone)).insert(ChildOf(item));
            if entry.open_question {
                let line = commands.spawn_scene(bsn! { @row() }).insert(ChildOf(item)).id();
                let act = Act::Answer { key: key.clone(), comment_id: entry.id.clone(), question: entry.body.clone() };
                let answer = commands
                    .spawn_scene(bsn! { @tool_button(Icon::Comment, "Answer", ButtonKind::Normal) })
                    .insert((act, ChildOf(line)))
                    .id();
                let resolve_action = Action::Resolve(entry.id.clone());
                let source = control(controls, commands, subject, &resolve_action);
                let resolve = commands
                    .spawn_scene(bsn! {
                        @tool_button(Icon::Check, "Resolve without an answer", ButtonKind::Normal)
                        @sends(source, subject.clone(), resolve_action.clone())
                    })
                    .insert(ChildOf(line))
                    .id();
                gate_button(commands, answer, *live);
                gate_button(commands, resolve, *live);
                let note = spawn_note(commands, item, note);
                commands.entity(item).insert(Parts {
                    answer: Some(answer),
                    resolve: Some(resolve),
                    note: Some(note),
                    ..default()
                });
            }
            item
        }
        Block::Composer { answering, live, note } => {
            let item = commands
                .spawn(Node { flex_direction: FlexDirection::Column, row_gap: px(space::ROWS), ..default() })
                .id();
            let line = commands.spawn_scene(bsn! { @row() }).insert(ChildOf(item)).id();
            let words = answering.as_ref().map_or_else(String::new, |q| format!("Answering: {q}"));
            let shown = display(answering.is_some());
            let answering_text = commands
                .spawn_scene(bsn! {
                    @text_dim(words, size::SECONDARY)
                    Node { display: { shown }, flex_shrink: 1.0, min_width: px(0) }
                })
                .insert((ComposePart::Answering, ChildOf(line)))
                .id();
            let stop = commands
                .spawn_scene(bsn! { @tool_button(Icon::X, "Comment instead", ButtonKind::Plain) })
                .insert((ComposePart::StopAnswering, ChildOf(line)))
                .id();
            let labels: Vec<String> = COMPOSE_KINDS.iter().map(|k| kind_label(*k).to_string()).collect();
            let kind = commands
                .spawn_scene(select(labels, Some(0), COMPOSE_KINDS.len()))
                .insert((ComposePart::Kind, ChildOf(line)))
                .id();
            commands.spawn_scene(text_input(COMPOSE_LINES, bsn! { ComposePart::Input })).insert(ChildOf(item));
            let foot = commands.spawn_scene(bsn! { @row() }).insert(ChildOf(item)).id();
            let send = commands
                .spawn_scene(bsn! { @button("Send", Some(Icon::Send), ButtonKind::Primary) })
                .insert((ComposePart::Send, ChildOf(foot)))
                .id();
            gate_button(commands, send, *live);
            // The box's answer goes to its own control, which outlives redraws like every other.
            control(controls, commands, subject, &Action::Comment);
            let note = spawn_note(commands, foot, note);
            commands.entity(item).insert(Parts {
                text: Some(answering_text),
                answer: Some(stop),
                resolve: Some(send),
                select: Some(kind),
                note: Some(note),
            });
            // The answering line and the kind's dropdown take turns; `dress` sets which once the parts exist.
            item
        }
        Block::Inline { field, value } => {
            let (hint, lines, font) = match field {
                Field::Title => ("Add a title", 1.0, size::DETAIL_TITLE),
                _ => ("Add a description", DESCRIPTION_LINES, size::BODY),
            };
            commands
                .spawn_scene(inline_text_sized(value.clone(), hint, lines, font))
                .insert(Edits { field: *field, statuses: Vec::new(), pending: None })
                .id()
        }
        Block::Choice { field, options, selected, statuses } => commands
            .spawn_scene(inline_field(field.label(), options.clone(), *selected))
            .insert(Edits { field: *field, statuses: statuses.clone(), pending: None })
            .id(),
        Block::AddSubtask { key, live } => {
            let button = commands
                .spawn_scene(bsn! { @button("Add sub-task", Some(Icon::Plus), ButtonKind::Normal) })
                .insert(Act::NewSubtask(key.clone()))
                .id();
            gate_button(commands, button, *live);
            button
        }
        Block::Parent(parent) => {
            let caption = format!("{}  {}", parent.key, parent.title);
            commands
                .spawn_scene(bsn! { @button(caption, None, ButtonKind::Plain) })
                .insert(OpensTask(parent.id.clone()))
                .id()
        }
        Block::Project(key) => commands
            .spawn_scene(bsn! { @button(key.clone(), None, ButtonKind::Plain) })
            .insert(OpensProject(key.clone()))
            .id(),
        Block::Subtask(sub) => {
            let line = commands.spawn_scene(bsn! { @row() }).id();
            let caption = format!("{}  {}", sub.key, sub.title);
            commands
                .spawn_scene(bsn! { @button(caption, None, ButtonKind::Plain) })
                .insert((OpensTask(sub.id.clone()), ChildOf(line)));
            commands.spawn_scene(text_dim(status_label(sub.status), size::SECONDARY)).insert(ChildOf(line));
            line
        }
        Block::Act { action, window, note } => {
            let line = commands.spawn_scene(bsn! { @row() }).id();
            let source = control(controls, commands, subject, action);
            let press = sends(source, subject.clone(), action.clone());
            match action {
                Action::Release => commands
                    .spawn_scene(bsn! { @button(action.label(), Some(Icon::Release), ButtonKind::Normal) @{press} }),
                Action::Undo => commands
                    .spawn_scene(bsn! { @button(action.label(), Some(Icon::Undo), ButtonKind::Normal) @{press} }),
                Action::Cancel => {
                    commands.spawn_scene(bsn! { @danger_button(action.label(), Some(Icon::Stop)) @{press} })
                }
                Action::Revert(_) => {
                    commands.spawn_scene(bsn! { @danger_button(action.label(), Some(Icon::History)) @{press} })
                }
                // Not drawn as an action block: they have their own controls, or none.
                Action::Status(_) | Action::Resolve(_) | Action::Grade(_) | Action::Comment | Action::Menu => {
                    commands.spawn_empty()
                }
            }
            .insert(ChildOf(line));
            let mut parts = Parts::default();
            if let Some(hours) = window {
                let selected = WINDOWS.iter().position(|h| h == hours);
                parts.select = Some(
                    commands
                        .spawn_scene(select(window_labels(), selected, WINDOWS.len()))
                        .insert((WindowChoice, ChildOf(line)))
                        .id(),
                );
            }
            parts.note = Some(spawn_note(commands, line, note));
            commands.entity(line).insert(parts);
            line
        }
    };
    commands.entity(item).insert(Shown(block.clone()));
    item
}

/// Sets a note's words, color and whether it takes room.
pub(super) fn dress_note(
    commands: &mut Commands,
    texts: &mut Query<&mut Text>,
    nodes: &mut Query<&mut Node>,
    note: Entity,
    state: &Option<Control>,
) {
    let (words, tone) = note_look(state);
    let display = if words.is_empty() { Display::None } else { Display::Flex };
    if let Ok(mut t) = texts.get_mut(note)
        && t.0 != words
    {
        t.0 = words;
    }
    if let Ok(mut node) = nodes.get_mut(note)
        && node.display != display
    {
        node.display = display;
    }
    commands.entity(note).insert(TextTone(tone));
}

/// Brings the parts of changed blocks in line with them: words, notes, disabled buttons, dropdown rows.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(super) fn dress(
    shown: Query<(&Shown, Option<&Parts>)>,
    every: Query<Entity, With<Shown>>,
    factory: Res<FactoryStatus>,
    changed: Query<Entity, Changed<Shown>>,
    mut texts: Query<&mut Text>,
    mut nodes: Query<&mut Node>,
    mut moves: Query<&mut StatusMove>,
    mut edit: SelectEdit,
    (mut inline, mut fields, mut edits): (Query<&mut InlineText>, Query<&mut InlineField>, Query<&mut Edits>),
    mut again: Local<Vec<Entity>>,
    mut commands: Commands,
) {
    let mut todo: Vec<Entity> = std::mem::take(&mut *again);
    todo.extend(changed.iter());
    if factory.is_changed() {
        todo.extend(every.iter());
    }
    todo.dedup();
    for entity in todo {
        let Ok((Shown(block), parts)) = shown.get(entity) else { continue };
        match block {
            // A value from elsewhere; the widget leaves it alone while it is being edited or saved.
            Block::Inline { value, .. } => {
                if let Ok(mut t) = inline.get_mut(entity)
                    && t.value() != value
                {
                    t.set_value(value);
                }
                continue;
            }
            Block::Choice { options, selected, statuses, .. } => {
                if let Ok(mut f) = fields.get_mut(entity) {
                    f.set(options.clone(), *selected);
                }
                if let Ok(mut e) = edits.get_mut(entity)
                    && e.statuses != *statuses
                {
                    e.statuses = statuses.clone();
                }
                continue;
            }
            Block::AddSubtask { live, .. } => {
                gate_button(&mut commands, entity, *live);
                continue;
            }
            _ => {}
        }
        if let Block::Text { text: words, .. } = block {
            if let Ok(mut t) = texts.get_mut(entity)
                && t.0 != *words
            {
                t.0 = words.clone();
            }
            continue;
        }
        let Some(parts) = parts else { continue };
        match block {
            Block::Question { key, id, body, live, note } => {
                let words = format!("? {body}");
                if let Some(Ok(mut t)) = parts.text.map(|e| texts.get_mut(e))
                    && t.0 != words
                {
                    t.0 = words;
                }
                for button in [parts.answer, parts.resolve].into_iter().flatten() {
                    gate_button(&mut commands, button, *live);
                }
                if let Some(answer) = parts.answer {
                    let act = Act::Answer { key: key.clone(), comment_id: id.clone(), question: body.clone() };
                    commands.entity(answer).insert(act);
                }
                if let Some(n) = parts.note {
                    dress_note(&mut commands, &mut texts, &mut nodes, n, note);
                }
            }
            Block::Grade { grade, note } => {
                if let Some(picker) = parts.select
                    && !edit.set(picker, &grade_options(&factory.human_grades()), grade_index(*grade))
                {
                    again.push(entity);
                }
                if let Some(n) = parts.note {
                    dress_note(&mut commands, &mut texts, &mut nodes, n, note);
                }
            }
            Block::Move { statuses, note } => {
                if let Some(picker) = parts.select {
                    if let Ok(mut m) = moves.get_mut(picker)
                        && m.statuses != *statuses
                    {
                        m.statuses = statuses.clone();
                    }
                    if !edit.set(picker, &status_labels(statuses), Some(0)) {
                        again.push(entity);
                    }
                }
                if let Some(n) = parts.note {
                    dress_note(&mut commands, &mut texts, &mut nodes, n, note);
                }
            }
            Block::Act { window, note, .. } => {
                if let (Some(picker), Some(hours)) = (parts.select, window)
                    && !edit.set(picker, &window_labels(), WINDOWS.iter().position(|h| h == hours))
                {
                    again.push(entity);
                }
                if let Some(n) = parts.note {
                    dress_note(&mut commands, &mut texts, &mut nodes, n, note);
                }
            }
            Block::Entry { live, note, .. } => {
                for button in [parts.answer, parts.resolve].into_iter().flatten() {
                    gate_button(&mut commands, button, *live);
                }
                if let Some(n) = parts.note {
                    dress_note(&mut commands, &mut texts, &mut nodes, n, note);
                }
            }
            Block::Composer { answering, live, note } => {
                let words = answering.as_ref().map_or_else(String::new, |q| format!("Answering: {q}"));
                if let Some(Ok(mut t)) = parts.text.map(|e| texts.get_mut(e))
                    && t.0 != words
                {
                    t.0 = words;
                }
                let turns = [
                    (parts.text, answering.is_some()),
                    (parts.answer, answering.is_some()),
                    (parts.select, answering.is_none()),
                ];
                for (part, shown) in turns {
                    if let Some(Ok(mut node)) = part.map(|e| nodes.get_mut(e))
                        && node.display != display(shown)
                    {
                        node.display = display(shown);
                    }
                }
                if let Some(send) = parts.resolve {
                    gate_button(&mut commands, send, *live);
                }
                if let Some(n) = parts.note {
                    dress_note(&mut commands, &mut texts, &mut nodes, n, note);
                }
            }
            Block::Text { .. }
            | Block::Priority(_)
            | Block::GradeBadge(_)
            | Block::Gates
            | Block::Subtask(_)
            | Block::Project(_)
            | Block::Parent(_)
            | Block::Inline { .. }
            | Block::Choice { .. }
            | Block::AddSubtask { .. } => {}
        }
    }
}
