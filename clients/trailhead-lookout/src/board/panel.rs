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

/// Marks a card's tool button, which is disabled while the connection isn't live.
#[derive(Component)]
pub(super) struct CardTool;

/// The parts of a card that are updated in place, marked on the entities that hold them.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub(super) enum CardPart {
    Key,
    Title,
    Under,
    Found,
    Badges,
    Tools,
}

/// Which tool button of a card an entity is, so a changed card keeps the buttons it still has.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToolKind {
    Edit,
    Comment,
    Answer,
    Pull,
}

impl ToolKind {
    fn of(action: &CardAction) -> Self {
        match action {
            CardAction::Edit => ToolKind::Edit,
            CardAction::Comment => ToolKind::Comment,
            CardAction::Answer { .. } => ToolKind::Answer,
        }
    }
}

/// Gives a new card its shell and parts. What the parts show is kept in step by [`update_cards`] and
/// [`sync_card_rows`], never by drawing the card again.
pub(super) fn dress_cards(cards: Query<(Entity, &BoardCard), Added<BoardCard>>, mut commands: Commands) {
    for (shell, BoardCard(card)) in &cards {
        commands.entity(shell).apply_scene(card_shell(card.depth));
        let key = commands.queue_spawn_scene(badge(card.key.clone(), None, None)).id();
        let title = commands.queue_spawn_scene(card_title(card.title.clone())).id();
        let under = commands.queue_spawn_scene(card_under(card.parent.clone().unwrap_or_default())).id();
        let found = commands.queue_spawn_scene(card_found(card.found.clone().unwrap_or_default())).id();
        let badges = commands.queue_spawn_scene(card_row()).id();
        let tools = commands.queue_spawn_scene(card_row()).id();
        for (part, kind) in [
            (key, CardPart::Key),
            (title, CardPart::Title),
            (under, CardPart::Under),
            (found, CardPart::Found),
            (badges, CardPart::Badges),
            (tools, CardPart::Tools),
        ] {
            commands.entity(part).insert((kind, ChildOf(shell)));
        }
    }
}

/// The text entity of a part: the entity itself when it is a text, else its first child that is.
fn text_of(part: Entity, kids: &Query<&Children>, texts: &Query<&mut Text>) -> Option<Entity> {
    if texts.contains(part) {
        return Some(part);
    }
    kids.get(part).ok()?.iter().find(|c| texts.contains(*c))
}

fn set_text(part: Entity, now: &str, kids: &Query<&Children>, texts: &mut Query<&mut Text>) {
    if let Some(mut text) = text_of(part, kids, texts).and_then(|e| texts.get_mut(e).ok())
        && text.0 != now
    {
        text.0 = now.to_string();
    }
}

fn set_shown(part: Entity, shown: bool, nodes: &mut Query<&mut Node>) {
    if let Ok(mut node) = nodes.get_mut(part) {
        let want = if shown { Display::Flex } else { Display::None };
        if node.display != want {
            node.display = want;
        }
    }
}

/// Keeps what each card shows in step with its view model, in place: its texts, the optional lines, the indent, and
/// the label, tone and hint of the badges it already has. It checks every frame rather than on a change, so a change
/// that lands before a part's scene has resolved still arrives.
#[derive(bevy::ecs::system::SystemParam)]
pub(super) struct CardParts<'w, 's> {
    parts: Query<'w, 's, &'static CardPart>,
    kids: Query<'w, 's, &'static Children>,
    tones: Query<'w, 's, &'static TextTone>,
    hints: Query<'w, 's, &'static Hint>,
    texts: Query<'w, 's, &'static mut Text>,
    nodes: Query<'w, 's, &'static mut Node>,
}

pub(super) fn update_cards(cards: Query<(Entity, &BoardCard, &Children)>, mut p: CardParts, mut commands: Commands) {
    for (shell, BoardCard(card), children) in &cards {
        let margin = UiRect::left(px(space::INDENT * card.depth as f32));
        if let Ok(mut node) = p.nodes.get_mut(shell)
            && node.margin != margin
        {
            node.margin = margin;
        }
        for &part in children {
            let Ok(kind) = p.parts.get(part).copied() else { continue };
            match kind {
                CardPart::Key => set_text(part, &card.key, &p.kids, &mut p.texts),
                CardPart::Title => set_text(part, &card.title, &p.kids, &mut p.texts),
                CardPart::Under => {
                    let line = format!("under {}", card.parent.as_deref().unwrap_or_default());
                    set_text(part, &line, &p.kids, &mut p.texts);
                    set_shown(part, card.parent.is_some(), &mut p.nodes);
                }
                CardPart::Found => {
                    set_text(part, card.found.as_deref().unwrap_or_default(), &p.kids, &mut p.texts);
                    set_shown(part, card.found.is_some(), &mut p.nodes);
                }
                CardPart::Badges => update_badges(part, card, &mut p, &mut commands),
                CardPart::Tools => {}
            }
        }
    }
}

fn update_badges(row: Entity, card: &Card, p: &mut CardParts, commands: &mut Commands) {
    let shown: Vec<Entity> = p.kids.get(row).map(|c| c.iter().collect()).unwrap_or_default();
    for (badge, wanted) in shown.into_iter().zip(&card.badges) {
        set_text(badge, &wanted.label, &p.kids, &mut p.texts);
        if let Some(label) = text_of(badge, &p.kids, &p.texts) {
            let tone = wanted.tone.clone().unwrap_or(token::TEXT_DIM);
            if p.tones.get(label).map(|t| &t.0) != Ok(&tone) {
                commands.entity(label).insert(TextTone(tone));
            }
        }
        match (&wanted.hint, p.hints.get(badge)) {
            (Some(h), Ok(Hint(now))) if h == now => {}
            (Some(h), _) => {
                commands.entity(badge).insert(Hint(h.clone()));
            }
            (None, Ok(_)) => {
                commands.entity(badge).remove::<Hint>();
            }
            (None, Err(_)) => {}
        }
    }
}

/// Removes the entities of `row` that `keep` rejects, leaving the rest as they are.
fn drop_unwanted(row: &[Entity], keep: impl Fn(usize, Entity) -> bool, commands: &mut Commands) {
    for (at, &e) in row.iter().enumerate() {
        if !keep(at, e) {
            commands.entity(e).try_despawn();
        }
    }
}

type ToolState<'a> = (&'a ToolKind, Option<&'a Act>, Option<&'a ExternalLink>);

/// Brings the rows of a card whose view model changed to what it now has: badges added or dropped at the end, tool
/// buttons added, dropped, or given their new target. A badge or button that stays is the same entity.
pub(super) fn sync_card_rows(
    cards: Query<(&BoardCard, &Children), Changed<BoardCard>>,
    parts: Query<&CardPart>,
    kids: Query<&Children>,
    tools: Query<ToolState>,
    connection: Res<State<Connection>>,
    mut commands: Commands,
) {
    let live = *connection.get() == Connection::Live;
    for (BoardCard(card), children) in &cards {
        for &row in children {
            let current: Vec<Entity> = kids.get(row).map(|c| c.iter().collect()).unwrap_or_default();
            match parts.get(row) {
                Ok(CardPart::Badges) => {
                    drop_unwanted(&current, |at, _| at < card.badges.len(), &mut commands);
                    for b in card.badges.iter().skip(current.len()) {
                        let mut shown = commands.queue_spawn_scene(badge(b.label.clone(), None, b.tone.clone()));
                        shown.insert(ChildOf(row));
                        if let Some(hint) = &b.hint {
                            shown.insert(Hint(hint.clone()));
                        }
                    }
                }
                Ok(CardPart::Tools) => sync_tools(card, row, &current, &tools, live, &mut commands),
                _ => {}
            }
        }
    }
}

fn sync_tools(
    card: &Card,
    row: Entity,
    current: &[Entity],
    tools: &Query<ToolState>,
    live: bool,
    commands: &mut Commands,
) {
    let mut wanted: Vec<ToolKind> = card.actions.iter().map(ToolKind::of).collect();
    if card.pr.is_some() {
        wanted.push(ToolKind::Pull);
    }
    let kind = |e: Entity| tools.get(e).map(|t| *t.0).ok();
    drop_unwanted(current, |_, e| kind(e).is_some_and(|k| wanted.contains(&k)), commands);
    let present = |want: ToolKind| current.iter().copied().find(|e| kind(*e) == Some(want));
    for action in &card.actions {
        let act = action.act(&card.key);
        match present(ToolKind::of(action)) {
            Some(e) if tools.get(e).is_ok_and(|t| t.1 != Some(&act)) => {
                commands.entity(e).insert(act);
            }
            Some(_) => {}
            None => spawn_tool(commands, row, action, act, live),
        }
    }
    let Some(pr) = &card.pr else { return };
    let hint = format!("Open pull request {}", pr.label);
    match present(ToolKind::Pull) {
        Some(e) if tools.get(e).is_ok_and(|t| t.2.map(|l| &l.url) != Some(&pr.url)) => {
            commands.entity(e).insert((ExternalLink { url: pr.url.clone() }, Hint(hint)));
        }
        Some(_) => {}
        None => {
            // A page in the browser, so it opens offline too.
            let link = link_tool_button(Icon::PullRequest, hint, pr.url.clone(), ButtonKind::Plain);
            commands.queue_spawn_scene(link).insert((ToolKind::Pull, ChildOf(row)));
        }
    }
}

fn spawn_tool(commands: &mut Commands, row: Entity, action: &CardAction, act: Act, live: bool) {
    let (icon, hint) = match action {
        CardAction::Edit => (Icon::Pencil, "Edit"),
        CardAction::Comment => (Icon::Comment, "Comment"),
        CardAction::Answer { .. } => (Icon::Help, "Answer the open question"),
    };
    let mut tool = commands.queue_spawn_scene(tool_button(icon, hint, ButtonKind::Plain));
    tool.insert((act, CardTool, ToolKind::of(action), ChildOf(row)));
    if !live {
        tool.insert(InteractionDisabled);
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

/// Draws the "Nothing here" line and the "Show more" button when they appear.
pub(super) fn dress_slots(
    empty: Query<Entity, Added<ColumnEmpty>>,
    more: Query<(Entity, &ColumnMore), Added<ColumnMore>>,
    mut commands: Commands,
) {
    for entity in &empty {
        commands.entity(entity).queue_apply_scene(widgets::column_empty());
    }
    for (entity, more) in &more {
        let label = format!("Show {} more", more.next);
        commands.entity(entity).queue_apply_scene(button(label, None, ButtonKind::Normal));
    }
}

/// Relabels each "Show more" button by editing its text when its count changes. It checks every frame, so a count
/// that changes before the button's scene has resolved still arrives.
pub(super) fn relabel_more(more: Query<(Entity, &ColumnMore)>, kids: Query<&Children>, mut texts: Query<&mut Text>) {
    for (button, more) in &more {
        let label = format!("Show {} more", more.next);
        let found = kids.iter_descendants(button).find(|e| texts.contains(*e));
        if let Some(mut text) = found.and_then(|e| texts.get_mut(e).ok())
            && text.0 != label
        {
            text.0 = label;
        }
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

#[cfg(test)]
mod in_place_tests {
    use bevy::state::app::StatesPlugin;

    use super::*;

    fn card(title: &str, badges: &[&str], actions: Vec<CardAction>) -> Card {
        Card {
            id: "t1".into(),
            key: "TRAIL-1".into(),
            title: title.into(),
            parent: None,
            depth: 0,
            badges: badges
                .iter()
                .map(|l| Badge { kind: BadgeKind::Priority, label: l.to_string(), tone: None, hint: None })
                .collect(),
            actions,
            found: None,
            pr: None,
        }
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin, AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .init_state::<Connection>()
            .add_systems(Update, (dress_cards, update_cards, sync_card_rows, dress_slots, relabel_more).chain());
        app
    }

    fn settle(app: &mut App) {
        for _ in 0..4 {
            app.update();
        }
    }

    fn subtree(app: &mut App, root: Entity) -> Vec<Entity> {
        let mut found = vec![root];
        let mut at = 0;
        while at < found.len() {
            if let Some(children) = app.world().get::<Children>(found[at]) {
                found.extend(children.iter());
            }
            at += 1;
        }
        found
    }

    fn texts(app: &mut App, root: Entity) -> Vec<String> {
        let all = subtree(app, root);
        all.into_iter().filter_map(|e| app.world().get::<Text>(e).map(|t| t.0.clone())).collect()
    }

    #[test]
    fn a_changed_card_keeps_its_entities_and_shows_the_new_text_and_badges() {
        let mut app = app();
        let shell = app.world_mut().spawn(BoardCard(card("old", &["P1", "grade 2"], vec![CardAction::Edit]))).id();
        settle(&mut app);
        let before = subtree(&mut app, shell);
        assert!(texts(&mut app, shell).contains(&"old".to_string()));
        app.world_mut().get_mut::<BoardCard>(shell).unwrap().0 =
            card("new", &["P3", "grade 2"], vec![CardAction::Edit]);
        settle(&mut app);
        assert_eq!(subtree(&mut app, shell), before, "the same entities, none respawned");
        let shown = texts(&mut app, shell);
        assert!(shown.contains(&"new".to_string()) && !shown.contains(&"old".to_string()));
        assert!(shown.contains(&"P3".to_string()) && !shown.contains(&"P1".to_string()));
        // A badge and a button added later join; the ones there stay.
        let answer = CardAction::Answer { comment_id: "c".into(), question: "q?".into() };
        app.world_mut().get_mut::<BoardCard>(shell).unwrap().0 =
            card("new", &["P3", "grade 2", "holder"], vec![CardAction::Edit, answer]);
        settle(&mut app);
        let after = subtree(&mut app, shell);
        assert!(before.iter().all(|e| after.contains(e)));
        assert!(texts(&mut app, shell).contains(&"holder".to_string()));
        let mut tools = app.world_mut().query::<&ToolKind>();
        assert_eq!(tools.iter(app.world()).count(), 2);
    }

    #[test]
    fn show_more_is_relabelled_by_editing_its_text() {
        let mut app = app();
        let more = app.world_mut().spawn(ColumnMore { status: Status::Open, next: 25 }).id();
        settle(&mut app);
        let before = subtree(&mut app, more);
        assert!(texts(&mut app, more).contains(&"Show 25 more".to_string()));
        app.world_mut().get_mut::<ColumnMore>(more).unwrap().next = 7;
        settle(&mut app);
        assert_eq!(subtree(&mut app, more), before);
        assert!(texts(&mut app, more).contains(&"Show 7 more".to_string()));
    }
}
