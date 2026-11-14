use super::*;

#[derive(Component)]
pub(super) struct BoardPanel;

pub(super) fn spawn_panel(mut commands: Commands, kept: Res<BoardScroll>) {
    let panel = commands
        .spawn((
            Name::new("board"),
            BoardPanel,
            widgets::BesideSidebar,
            Node {
                // A layout node is never smaller than its content unless told so: without these the panel grew past
                // the window to fit every card, so the board and its columns never overflowed (TRAIL-836).
                min_width: px(0),
                min_height: px(0),
                overflow: Overflow::clip(),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(px(space::PANEL_INSET)),
                row_gap: px(space::ROWS),
                ..widgets::beside_sidebar()
            },
        ))
        .id();
    commands.spawn((
        Name::new("header"),
        HeaderBar,
        Node { align_items: AlignItems::End, column_gap: px(space::GROUPS), ..default() },
        ChildOf(panel),
    ));
    // Columns past the window's edge are reached by scrolling sideways (Shift + wheel, or a touchpad swipe), and a
    // long column by scrolling down on its own. This node and the columns in it live as long as the panel.
    let columns = commands
        .spawn((
            Node {
                column_gap: px(space::GROUPS),
                align_items: AlignItems::FlexStart,
                flex_grow: 1.0,
                min_height: px(0),
                overflow: Overflow::scroll_x(),
                ..default()
            },
            BoardColumns,
            ScrollBoth,
            ScrollPosition(kept.board),
            ChildOf(panel),
        ))
        .id();
    commands.queue_spawn_scene(widgets::scrollbar(columns, ControlOrientation::Horizontal)).insert(ChildOf(panel));
}

pub(super) fn despawn_panel(mut commands: Commands, panel: Query<Entity, With<BoardPanel>>) {
    for e in &panel {
        commands.entity(e).despawn();
    }
}

/// What the cards of one column build now: the first `limit` of them, and how many are left out. A page that ends
/// partway through a parent's subtasks runs on to the last of them, so a parent is not split from its children,
/// unless that run is longer than [`PAGE`]: then it is cut there and the rest waits behind the column's "Show more".
pub fn visible(cards: &[Card], limit: usize) -> (&[Card], usize) {
    let mut shown = limit.min(cards.len());
    let most = shown + PAGE;
    while shown > 0 && shown < cards.len().min(most) && cards[shown].depth > 0 {
        shown += 1;
    }
    (&cards[..shown], cards.len() - shown)
}

/// How many cards "Show more" adds to a column showing `limit`: what [`visible`] builds for a page more, less what is shown.
pub fn next_page(cards: &[Card], limit: usize) -> usize {
    visible(cards, limit + PAGE).0.len() - visible(cards, limit).0.len()
}

/// Where the board was scrolled to when the view last left it. The panel's nodes stay as they are while the board is
/// showing, so this only brings the positions back after a trip to another view, which builds the panel anew.
#[derive(Resource, Default)]
pub(super) struct BoardScroll {
    board: Vec2,
    columns: HashMap<Status, f32>,
}

pub(super) fn save_scroll(
    board: Query<&ScrollPosition, With<BoardColumns>>,
    columns: Query<(&ColumnBody, &ScrollPosition)>,
    mut kept: ResMut<BoardScroll>,
) {
    if let Ok(position) = board.single() {
        kept.board = position.0;
    }
    for (column, position) in &columns {
        kept.columns.insert(column.0, position.y);
    }
}

/// Gives a new column its look: the shell, header, scrolling body and scrollbar.
pub(super) fn dress_columns(
    bodies: Query<(Entity, &ColumnBody, &ChildOf), Added<ColumnBody>>,
    columns: Query<&BoardColumn>,
    kept: Res<BoardScroll>,
    mut commands: Commands,
) {
    for (body, ColumnBody(status), parent) in &bodies {
        let shell = parent.parent();
        let count = columns.get(shell).map_or(0, |c| c.count);
        let at = kept.columns.get(status).copied().unwrap_or_default();
        commands.entity(shell).insert(widgets::column_shell(*status));
        commands.entity(body).insert((widgets::column_body(), ScrollPosition(Vec2::new(0.0, at))));
        // The body and its bar share a frame under the header, so the bar's track is the body's.
        let frame = commands.spawn((widgets::column_frame(), ChildOf(shell))).id();
        commands.entity(body).insert(ChildOf(frame));
        widgets::column_scrollbar(&mut commands, frame, body);
        widgets::column_header(&mut commands, shell, *status, count);
    }
}

/// Keeps each header's count in step with its column, in place.
pub(super) fn update_counts(mut counts: Query<(&widgets::ColumnCount, &mut Text)>, columns: Query<&BoardColumn>) {
    for (count, mut text) in &mut counts {
        if let Ok(column) = columns.get(count.0) {
            let now = column.count.to_string();
            if text.0 != now {
                text.0 = now;
            }
        }
    }
}

/// A card's tool button that opens this agent (an id) in the inspector.
#[derive(Component)]
pub(super) struct InspectHolder(String);

/// Marks a card's tool button, which is disabled while the connection isn't live.
#[derive(Component)]
pub(super) struct CardTool;

/// Draws a card when it is new, and again only when its view model changed.
pub(super) fn dress_cards(
    cards: Query<(Entity, &BoardCard, Option<&Children>), Changed<BoardCard>>,
    connection: Res<State<Connection>>,
    mut commands: Commands,
) {
    let live = *connection.get() == Connection::Live;
    for (shell, BoardCard(card), children) in &cards {
        for child in children.into_iter().flatten() {
            commands.entity(*child).despawn();
        }
        commands.entity(shell).apply_scene(card_shell(card.depth));
        commands.queue_spawn_scene(badge(card.key.clone(), None, None)).insert(ChildOf(shell));
        commands.queue_spawn_scene(card_title(card.title.clone())).insert(ChildOf(shell));
        if let Some(parent) = &card.parent {
            commands.queue_spawn_scene(card_under(parent.clone())).insert(ChildOf(shell));
        }
        if let Some(found) = &card.found {
            commands.queue_spawn_scene(card_found(found.clone())).insert(ChildOf(shell));
        }
        let badges = commands.queue_spawn_scene(card_row()).insert(ChildOf(shell)).id();
        for b in &card.badges {
            let mut shown = commands.queue_spawn_scene(badge(b.label.clone(), None, b.tone.clone()));
            shown.insert(ChildOf(badges));
            if let Some(hint) = &b.hint {
                shown.insert(Hint(hint.clone()));
            }
        }
        let tools = commands.queue_spawn_scene(card_row()).insert(ChildOf(shell)).id();
        for action in &card.actions {
            let (icon, hint) = match action {
                CardAction::Edit => (Icon::Pencil, "Edit"),
                CardAction::Comment => (Icon::Comment, "Comment"),
                CardAction::Answer { .. } => (Icon::Help, "Answer the open question"),
            };
            let mut tool = commands.queue_spawn_scene(tool_button(icon, hint, ButtonKind::Plain));
            tool.insert((action.act(&card.key), CardTool, ChildOf(tools)));
            if !live {
                tool.insert(InteractionDisabled);
            }
        }
        if let Some(holder) = &card.holder {
            let hint = format!("Inspect {}", holder.name);
            commands
                .queue_spawn_scene(tool_button(Icon::Inspect, hint, ButtonKind::Plain))
                .insert((InspectHolder(holder.id.clone()), ChildOf(tools)));
        }
        // A page in the browser, so it opens offline too.
        if let Some(pr) = &card.pr {
            let hint = format!("Open pull request {}", pr.label);
            commands
                .queue_spawn_scene(link_tool_button(Icon::PullRequest, hint, pr.url.clone(), ButtonKind::Plain))
                .insert(ChildOf(tools));
        }
    }
}

/// Enables and disables the cards' tool buttons as the connection goes live and away, without redrawing them.
pub(super) fn tools_follow_connection(
    connection: Res<State<Connection>>,
    tools: Query<Entity, With<CardTool>>,
    mut commands: Commands,
) {
    if !connection.is_changed() {
        return;
    }
    let live = *connection.get() == Connection::Live;
    for tool in &tools {
        if live {
            commands.entity(tool).remove::<InteractionDisabled>();
        } else {
            commands.entity(tool).insert(InteractionDisabled);
        }
    }
}

/// Draws the "Nothing here" line and the "Show more" button when they appear, and the button again when its count
/// changes.
pub(super) fn dress_slots(
    empty: Query<Entity, Added<ColumnEmpty>>,
    more: Query<(Entity, &ColumnMore, Option<&Children>), Changed<ColumnMore>>,
    mut commands: Commands,
) {
    for entity in &empty {
        commands.entity(entity).queue_apply_scene(widgets::column_empty());
    }
    for (entity, more, children) in &more {
        for child in children.into_iter().flatten() {
            commands.entity(*child).despawn();
        }
        let label = format!("Show {} more", more.next);
        commands.entity(entity).queue_apply_scene(button(label, None, ButtonKind::Normal));
    }
}

/// Opens a card in the inspector when it is pressed.
pub(super) fn on_card_press(activate: On<Activate>, cards: Query<&BoardCard>, mut selection: ResMut<Selection>) {
    if let Ok(BoardCard(card)) = cards.get(activate.entity) {
        selection.0 = Some(Subject::Task(card.id.clone()));
    }
}

pub(super) fn on_more_press(activate: On<Activate>, more: Query<&ColumnMore>, mut shown: ResMut<Shown>) {
    if let Ok(more) = more.get(activate.entity) {
        shown.more(more.status);
    }
}

/// Opens the agent holding a card's task in the inspector.
pub(super) fn on_inspect_holder(
    activate: On<Activate>,
    tools: Query<&InspectHolder>,
    mut selection: ResMut<Selection>,
) {
    if let Ok(InspectHolder(id)) = tools.get(activate.entity) {
        selection.0 = Some(Subject::Agent(id.clone()));
    }
}

#[cfg(test)]
mod holder_tests {
    use super::*;

    #[test]
    fn pressing_the_holder_tool_selects_the_agent() {
        let mut app = App::new();
        app.init_resource::<Selection>().add_observer(on_inspect_holder);
        let tool = app.world_mut().spawn(InspectHolder("a1".into())).id();
        app.world_mut().trigger(Activate { entity: tool });
        assert_eq!(app.world().resource::<Selection>().0, Some(Subject::Agent("a1".into())));
    }
}
