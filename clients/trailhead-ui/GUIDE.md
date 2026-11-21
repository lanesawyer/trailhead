# The trailhead-ui kit: a guide for building views

This is the source of truth for how views are built from the kit: what it has, how each piece is used and updated,
and the rules that keep apps built on it consistent. It lives in the crate so that it matches the revision you build
against: an app that pins `trailhead` at a git revision reads the copy cargo checked out at that revision
(`~/.cargo/git/checkouts/trailhead-*/<rev>/clients/trailhead-ui/GUIDE.md`). Everything here was checked against the
code at the revision it sits in; if the two disagree, the code is right and this file has a bug.

An app written for the Trailhead shell also reads `clients/trailhead-shell/GUIDE.md`, which covers the contract with
the shell, the connection to the server and testing an app. This file is only about views.

## 1. What the kit is

The kit is the `trailhead-ui` crate (`clients/trailhead-ui/src/`, MIT OR Apache-2.0): every control and surface an
app's views are built from, one widget to a file, all re-exported from `lib.rs`. It is built on Bevy 0.20's
`bevy_feathers` and `bevy_ui_widgets`, and it knows nothing of Trailhead.

### The one rule: use a widget, or grow one

Before you write a `Node` with a background, a button, a dropdown or a text style in a view, look in the kit.

- **The kit has it:** call it. Do not copy its insides into the view and tweak them. Two similar buttons, cards or
  fields that drift apart are a bug, however small the difference.
- **It nearly fits:** the widget grows an option (a variant, a prop), and the view uses that.
- **It doesn't exist:** a new widget file goes in the kit, built on feathers, with a doc comment saying what it is
  for. A widget never imports a view module. A widget about one app's domain (Lookout's task card and board column)
  lives in that app, not the kit.
- **Layouts that repeat are widgets too.** A panel header, a section with a heading, a label-and-value row, a form's
  button bar: if more than one surface has or will have it, it is a widget in the kit, even while only one uses it.
  Composing kit pieces inline the same way in two views drifts just as two hand-made buttons do (TRAIL-861). Before
  building one, look in the other views for the same shape.

**Who changes the kit.** An app's author does not edit the kit to unblock an app. When an app needs something the kit
lacks, that is a task in the kit's queue (TRAIL-888) in the `trailhead` repo; a maintainer of the kit builds it, a
person reviews it, and the app's task waits for it and for the apps repo's pin to move. In the `trailhead` repo itself,
the agents and people working on the kit change it under the rules in this file. A lint in the apps repo refuses any
UI code that is not the kit's, so an app cannot work around a missing widget (see section 6).

Much of the kit is ported from Lane's `bevy-data-explorer` (`src/widgets/`, on GitHub at
lanesawyer/bevy-data-explorer). Porting a widget from there is better than writing a new one.

### The three shapes a widget comes in

Pick a widget's shape by what the caller does with it, and name a choice instead of passing a bare `bool` (`Fold`,
`ButtonKind::Danger`):

- **A scene**, `fn x(..) -> impl Scene`, spawned with `commands.spawn_scene(x(..))` or
  `commands.queue_spawn_scene(x(..))`, or placed inside another scene with `@x(..)`. Most widgets are this. Components
  written after it in a `bsn!` (a marker, a `Node { .. }` patch) land on its root.
- **`fn spawn_x(commands, ..) -> XParts`**, when the caller fills or keeps hold of what is inside (an accordion's body,
  a menu's popup, a search field's text entity). It spawns unparented unless it takes a parent; put its root where it
  goes with `ChildOf`.
- **A `Node` or a bundle**, when the caller extends it: a node to finish with `..beside_sidebar()`, or components to
  spawn beside the caller's own marker (`scroll_body()`).

Updating a widget after it is spawned goes through a component you set or an `*Edit` system parameter; the catalogue
says which for each.

### Setting up

In the Trailhead shell, `trailhead_shell::Shell` does all of this for every app; an app adds nothing. A binary that
uses the kit without the shell adds `DefaultPlugins`, the kit's `ThemePlugin` with its `Theme` (which brings
feathers' plugins), and the kit's `UiPlugin`. Without a theme every color is missing and feathers draws it fuchsia.

The kit's per-frame systems (opening accordions, cutting text to fit, painting tabs and hovers, styling selects) run
in the `UiPlace` system set in `Update`. An app's systems that spawn or change widgets go before it:
`.before(UiPlace)`.

Presses, choices and values come from `bevy::ui_widgets` events, which the kit does not re-export because they are
Bevy's, not feathers': `Activate` (a press), `ValueChange<T>` (a toggle, slider or tab strip), `SelectedTab`,
`ControlOrientation`. A disabled control is `bevy::ui::InteractionDisabled` on it. Text typed into a field is
`bevy::text::EditableText` on the field's text entity.

## 2. Catalogue

Every public item the kit exports, grouped by what you are trying to do. Signatures are as in the code; `impl Into<String>`
is written `Into<String>` to save room.

### Show text

| Item | Signature | What it is for | Updated by |
| --- | --- | --- | --- |
| `text` | `text(content: Into<String>, size: Size) -> impl Scene` | A label at a size from `size`. | `set_text` on its `Text`. |
| `text_dim` | `text_dim(content: Into<String>, size: Size) -> impl Scene` | A label secondary to what is beside it. | `set_text`. |
| `text_toned` | `text_toned(content: Into<String>, size: Size, tone: Token) -> impl Scene` | A label in a theme color (a warning line). | `set_text`; insert a new `TextTone` to recolor. |
| `text_mono` | `text_mono(content: Into<String>, size: Size, tone: Token) -> impl Bundle` | Monospace (Bevy's built-in Fira Mono): log lines, ids, code. A bundle, not a scene: `commands.spawn(text_mono(..))`. | `set_text`; a new `TextTone`. |
| `button_text` | `button_text(text: Into<String>) -> impl Scene` | A caption inside a hand-built button, in the button's own text color. Feathers' `label` keeps the window's text color on a primary or disabled button; this one follows the button. | `set_text`. |
| `Size`, `size` | `size::SMALL` .. `size::DETAIL_TITLE`; `Size::px(self) -> f32` | The text scale (section 3). | |
| `text_truncated` | `text_truncated(content: Into<String>, size: Size) -> impl Scene` | One line cut with an ellipsis to the room its row leaves it, growing to take that room: a list row's title beside its buttons. The row's width must come from outside it, as for `Truncated`. | Set its `Truncated`'s `full`. |
| `Truncated` | `Truncated { full: String, size: Size }` (component) | A text cut with an ellipsis to the room its parent leaves beside its siblings. The parent's width must come from outside it (`flex_grow`, `min_width: px(0)`), or each cut narrows it again. | Set `full`, never the `Text`, which the kit rewrites. |
| `truncate_to_width` | `truncate_to_width(text: &str, width: f32, font_size: Size) -> String` | Cut a string to a width by the kit's fixed glyph estimate. | |
| `width_of` | `width_of(chars: usize, font_size: Size) -> f32` | How wide that many characters come out, by the same estimate, so a box sized by one and filled by the other agree. | |

Long text on cards and in lists is clamped or truncated; a full description, handoff or question belongs in a detail
panel.

### A button

| Item | Signature | What it is for | Updated by |
| --- | --- | --- | --- |
| `button` | `button(caption: Into<String>, icon: Option<Icon>, kind: ButtonKind) -> impl Scene` | A button with words, after an icon when given. | The caption is the button's descendant `Text`: walk `Children` and `set_text` it (the kit has no `ButtonEdit` yet). Disable with `InteractionDisabled`; the caption dims itself. |
| `tool_button` | `tool_button(icon: Icon, hint: Into<String>, kind: ButtonKind) -> impl Scene` | An icon alone, with a `Hint` saying what it does. `Plain` on cards and lists, `Normal` in a toolbar. | Its `Hint`'s `.0`. |
| `ButtonKind` | `Normal` (default), `Primary`, `Plain`, `Danger` | How much a button stands out. One `Primary` per surface (New task, Save); `Danger` for something destructive (cancel, revert): a normal button with its caption in the danger color; `Plain` has no background until hovered. | Fixed at spawn. |
| `button_group` | `button_group(options: Vec<(String, Option<Icon>)>, chosen: Option<usize>) -> impl Scene` | Two to four short options side by side as one control, the chosen one primary (light, dark, desktop), or a short ladder of steps a person should see all of (a text size's eight). Pressing one triggers `Picked` on the group. | Set `ButtonGroup { chosen }` on the group's root. |
| `ButtonGroup` | `ButtonGroup { chosen: Option<usize> }` (component) | A button group's state. | |
| `link_button` | `link_button(icon: Icon, text: Into<String>, url: Into<String>, kind: ButtonKind) -> impl Scene` | A button that opens a URL in the system's browser. `Plain` reads as a link in running text. | Its `ExternalLink { url }`. |
| `link_tool_button` | `link_tool_button(icon: Icon, hint: Into<String>, url: Into<String>, kind: ButtonKind) -> impl Scene` | The same as an icon-only tool button. | Its `ExternalLink`. |
| `ExternalLink` | `ExternalLink { url: String }` (component) | The URL a link button opens; the kit's observer opens it. | |
| `check_circle` | `check_circle(checked: bool, hint: Into<String>) -> impl Scene` | A round check to mark something done (a todo's "complete"): an empty ring that draws its check in the accent under the pointer, and is filled with the accent and the check while checked or held down. No square background. A press, Enter or Space sends `Activate` from it; it does not check itself, so the caller sets the state from its data. Focusable, with `hint` as its `Hint`; `InteractionDisabled` dims it and stops presses. | Set `CheckCircle { checked }` with `set_if_neq`; the kit repaints it. |
| `CheckCircle`, `CheckRing`, `CheckMark` | `CheckCircle { checked: bool }` (component on the root); markers | The check's state, and its ring and mark, which the kit paints. | |
| `HoverTone` | `HoverTone { rest: Token, hover: Token }` (component) | Something pressable that is not a button (a card, a hand-built row, a tab) answering the pointer as a button does. The hover token must show against the surface the node sits on. | Insert a new one. |

What a press does is the caller's: put a marker on the button and observe `Activate` for it (recipe 2). A button that
opens a menu is `spawn_icon_menu`, with its chevron; a button that acts never has a chevron.

A todo row's check, spawned with the row and kept in step with it in place (the press is the caller's, as for a
button; the check shows what the data says, not what was pressed):

```rust
commands.spawn_scene(check_circle(row.done, "Complete")).insert((CompleteCheck(row.key.clone()), ChildOf(line)));
// In the row's `Keyed` update, with `checks: Query<&mut CheckCircle>`:
if let Ok(mut check) = checks.get_mut(check_entity) {
    check.set_if_neq(CheckCircle { checked: row.done });
}
```

### A choice

| Item | Signature | What it is for | Updated by |
| --- | --- | --- | --- |
| `select` | `select(options: Vec<String>, selected: Option<usize>, max_visible: usize) -> impl Scene` | A dropdown for a fixed set of choices (a filter, a priority, a status). At most `max_visible` rows before the list scrolls. The caption stays on one line, cut with an ellipsis. Tag it with your own marker and observe `Picked`. | `SelectEdit` (below). Never respawn it. |
| `select_toned` | `select_toned(options: Vec<String>, tones: Vec<Option<Token>>, selected: Option<usize>, max_visible: usize) -> impl Scene` | A select whose options are shown in colors (a status, a priority); the caption takes the chosen one's. | `SelectEdit::set_toned`. |
| `select_iconed` | `select_iconed(options: Vec<String>, icons: Vec<Icon>, selected: Option<usize>, max_visible: usize) -> impl Scene` | A select with an icon before each option, and the chosen one's before the caption. | `SelectEdit::set`. |
| `Picked` | `Picked { entity: Entity, index: usize }` (entity event) | Which option of a select or a button group was picked, triggered on the control itself. | |
| `SelectEdit` | system parameter: `is_open(select) -> bool`, `set_enabled(select, enabled: bool)`, `is_enabled(select) -> bool`, `selected_rows(select) -> Vec<usize>`, `set(select, options: &[String], selected: Option<usize>) -> bool`, `set_toned(select, options, tones: &[Option<Token>], selected) -> bool` | Changes a spawned select in place: the chosen row, and its rows only when the options changed. It touches nothing while the popup is open and returns `false`; try again next frame. | |
| `SelectOptions`, `SelectTones`, `SelectIcons` | components holding `Vec<String>`, `Vec<Option<Token>>`, `Vec<Icon>` | The state a select was last given, which `SelectEdit` compares against. Read them; do not write them. | |
| `toggle_switch` | `toggle_switch(label: Into<String>, checked: bool, own: impl Scene) -> impl Scene` | A setting that is on or off, with its label after it. `own` goes on the switch itself (your marker). A press flips it and sends `ValueChange<bool>` from the switch. | The switch keeps its own state (`bevy::ui::Checked`). |
| `slider` | `slider(value: f32, range: (f32, f32), decimals: i32) -> impl Scene` | A number picked on a track; a click sets the value where it lands. The kit writes the value back. | Read `SliderValue`, or observe `ValueChange<f32>`. |
| `spawn_choice_tile` | `spawn_choice_tile(commands: &mut Commands, caption: Into<String>, chosen: bool) -> ChoiceTileParts` | One of a row of choices a person should see rather than read: a small picture (`extent::TILE_PICTURE_WIDTH` by `TILE_PICTURE_HEIGHT`, clipped to its corners) with its caption under it, pressable as a whole (it sends `Activate` and answers the pointer as a plain button does), the one chosen ringed in the accent. Spawned unparented; lay the tiles out in a row that wraps, `space::ROWS` apart. It does not choose itself. The Trailhead shell's ways of switching apps are these, each picture a preview that plays. | Set `ChoiceTile { chosen }` on every tile with `set_if_neq`; the kit repaints the ring and the caption. |
| `ChoiceTileParts` | `ChoiceTileParts { root: Entity, picture: Entity }` | Put `root` in the row and fill `picture`. | |
| `ChoiceTile`, `ChoicePicture`, `ChoiceCaption` | `ChoiceTile { chosen: bool }` (component on the root); markers | The tile's state, and its picture and caption. | |

A fixed set of choices is a dropdown, never a button that cycles through values. Two to four short options that
should all show at once are a `button_group`, and so is a ladder of a few short steps where you are on it
matters (the text sizes, 80% to 200%). Choices that are best shown (a few kinds of motion, each a preview) are a row of
`spawn_choice_tile`s. A list floating under a text field (a search's suggestions) is a
`dropdown_list`, not a `select`.

### A text field

| Item | Signature | What it is for | Updated by |
| --- | --- | --- | --- |
| `text_input` | `text_input(lines: f32, own: impl Scene) -> impl Scene` | A text input `lines` high (more than one allows newlines and wraps at words), filling the width of what holds it. One line is `extent::CONTROL_HEIGHT` tall, as a button and a select are, so it lines up beside them; more lines grow past it. `own` lands on the text entity, where your marker goes. Enter in a one-line input triggers `TextEntered` on the text entity. | Read and write `EditableText` on the text entity. |
| `text_input_sized` | `text_input_sized(lines: f32, font_size: Size, own: impl Scene) -> impl Scene` | The same, typing at another size, so an input can stand in for a text of that size. | |
| `TextEntered` | `TextEntered { entity: Entity, value: String }` (entity event) | Enter was pressed in a one-line input (not held, not mid-composition), with what it holds: how a field submits from the keyboard (recipe 5). Several lines take Enter as a new line and never trigger it. The kit's `inline_text` saves on it too. | |
| `InitialText` | `InitialText(pub String)` (component) | Text an input holds when it appears, with the cursor at its end: a form opened on something that exists. Put it in `own`. | |
| `field` | `field(label: Into<String>, control: impl SceneList) -> impl Scene` | A label above a control (an input, a select). Pass the control into the scene rather than adding it as a child afterwards: the scene's own `Children` replace any added before it spawns, which once left a select orphaned at the window's top left (TRAIL-850). | |
| `field_well` | `field_well() -> impl Scene` | A raised box to set text fields in. Feathers fills a field with the same gray as a pane's body and a menu, so a field placed straight in either cannot be seen. | |
| `spawn_search_field` | `spawn_search_field(commands: &mut Commands, hint: Into<String>) -> SearchField` | A search box, `extent::SEARCH_WIDTH` wide, with a hint while empty and a clear button while not; Escape clears it. | Read `EditableText` on `SearchField::field`. |
| `spawn_search_field_filling` | `spawn_search_field_filling(commands: &mut Commands, hint: Into<String>) -> SearchField` | The same, as wide as what it sits in (a sidebar, a column). | |
| `SearchField` | `SearchField { entry: Entity, field: Entity }` | `entry` is the box to place; `field` is the text entity to mark and read. | |
| `SearchHint`, `ClearSearch` | components | The hint and the clear button inside a search field; the kit keeps them. | |
| `matches_search` | `matches_search(query: &str, fields: &[&str]) -> bool` | Whether every word of the query appears somewhere in the fields, ignoring case: the matching every searched list shares. | |
| `inline_text` | `inline_text(value: Into<String>, hint: Into<String>, lines: f32) -> impl Scene` | A text edited where it is read (a title, a description): click to edit, Save and Cancel under it; one line also saves on Enter; Escape cancels. It triggers `InlineEdited` and shows "Saving…" until you answer. | Its `InlineText` component (below). |
| `inline_text_sized` | `inline_text_sized(value, hint, lines: f32, font_size: Size) -> impl Scene` | The same at another size (a detail view's title). | |
| `inline_field` | `inline_field(label: Into<String>, options: Vec<String>, tones: Vec<Option<Token>>, selected: Option<usize>) -> impl Scene` | A sidebar row: a label and a `select_toned`, always shown; picking another option triggers `InlineChosen`. | Its `InlineField` component. |
| `InlineEdited` | `InlineEdited { entity, old: String, new: String }` (entity event) | An inline text's change, for you to save. | |
| `InlineChosen` | `InlineChosen { entity, old: Option<usize>, new: usize }` (entity event) | An inline field's change. | |
| `InlineText` | component: `value()`, `mode()`, `error()`, `draft()`, `set_value(&str)`, `confirm()`, `fail(old, message)`, `fail_keeping(old, draft, message)` | An inline text's state. Answer a change with `confirm` or `fail`; `fail_keeping` keeps what was typed for the next edit (a save that lost to someone else's change). `set_value` shows a value from elsewhere, ignored mid-edit. | |
| `InlineField` | component: `selected()`, `mode()`, `error()`, `set(options, tones, selected)`, `confirm()`, `fail(old, message)` | An inline field's state, answered the same way. | |
| `InlineMode` | `Reading`, `Editing`, `Saving` | Where an inline widget is in its edit. | |

Neither inline widget saves anything itself: the caller decides what saving means and may refuse. An Escape that
cancels an inline edit is not passed on, so it does not also close the modal the text sits in.

### A list that changes

| Item | Signature | What it is for | Updated by |
| --- | --- | --- | --- |
| `Keyed<M>` | `Keyed::default()`; `sync(&mut self, commands, wanted: &[(Entity, Vec<(String, M)>)], spawn: FnMut(&mut Commands, &str, &M) -> Entity, update: FnMut(&mut Commands, Entity, &M))`; `entity(&str)`, `len()`, `is_empty()`, `forget()`, `clear(commands)` | Keeps the children of one or more parents in step with keyed view models (`M: Clone + PartialEq`): spawns an entity for a new key, calls `update` when a key's model changed, reorders and moves without respawning, despawns a key that is gone. A key that moves to another parent keeps its entity. The parents must hold nothing but these entities. `spawn` must not parent what it spawns. | Call `sync` again with the new models (recipe 1). |
| `table_head` | `table_head(columns: &[TableColumn]) -> impl Scene` | Dim headings over a table's columns, with a rule under them. | |
| `table_row` | `table_row(columns: &[TableColumn], cells: Vec<Box<dyn SceneList>>) -> impl Scene` | A row, one cell per column in their order, pressable as a whole (it sends `Activate` and lightens under the pointer). Keep rows with `Keyed` and update their cells in place. | Mark each cell's text and `set_text` it. |
| `TableColumn` | `TableColumn::new(heading: Into<String>, width: ColumnWidth, align: CellAlign)` | A column: heading, width, which side its cells sit on. | |
| `ColumnWidth` | `Fixed(f32)`, `Fill` | Pixels for a number, a key or a bar; what the fixed columns leave for a name. | |
| `CellAlign` | `Start` (default), `End` | `End` for numbers. | |

The table is for a list short enough to build whole. A list of hundreds builds only what is on screen (Lookout's board
columns do this with `PAGE` and "show more").

### A list a person can reorder

| Item | Signature | What it is for | Updated by |
| --- | --- | --- | --- |
| `ReorderList` | `ReorderList { orientation: ControlOrientation }`; `ReorderList::vertical()`, `horizontal()`, `keys(self) -> &'static str` | On a container whose items a person puts in their own order (the apps on the rail). `keys()` names the keys that move an item ("Alt+Up or Down"), for its hint. | |
| `ReorderItem` | `ReorderItem(pub String)` (component) | On each child that can move, with a key that stays with it (an id, never a place). A child without one (a list's foot) stays put and nothing lands among it. | |
| `Reordered` | `Reordered { list: Entity, key: String, from: usize, to: usize }` (entity event, on the list) | An item was moved: the one keyed `key`, from place `from` to `to` among the list's items (children without a `ReorderItem` are not counted). Apply it to your own order, in the frame you hear it. | |
| `Lifted` | component | On an item while it is dragged, for a test or a shot to find. | |
| `DRAG_THRESHOLD`, `RELEASE_SLACK` | `f32`, UI pixels | How far the pointer moves before a press becomes a drag (5), and how far outside its list an item can be let go and still land (32). | |
| `ReorderPlugin` | plugin | The drag and the keys. Part of `UiPlugin`. | |

A press on an item that moves the pointer further than `DRAG_THRESHOLD` lifts it: it is drawn over the others with an
accent border (a button on a surface of its own), follows the pointer along the list, no further than its first and
last items, and the others slide aside to show where it will land. Letting go triggers one `Reordered`; Escape, or
letting go more than `RELEASE_SLACK` outside the list, puts everything back with no event. With an item focused, Alt
and Up or Down (Left or Right across) moves it one place, with the same event. Hints stay hidden while an item is
lifted, and the cursor is a closed hand.

The kit only moves the items to show the drag (their `UiTransform`) and puts them back when it ends: it never
reorders the children. Your own data stays the one source of the order, and your in-place update (`Keyed`, or
`replace_children` on what you keep) moves the nodes (recipe 7). This is reordering within one list: dragging an item
to another list, or dropping it onto a target, is not in the kit yet. The list does not scroll itself while an item is
dragged near its edge.

### A list of values a person adds to and takes from

| Item | Signature | What it is for | Updated by |
| --- | --- | --- | --- |
| `value_list` | `value_list(values: Vec<String>, empty: Into<String>) -> impl Scene` | Short values a person adds to and takes from (directories, names), as a setting's control: each on a line of its own, cut to its room, with an X that removes it, and under them a field and Add (Enter in the field adds too). `empty` is said, dim, while there are none. It fills the width it is given. Put your marker on it. | Set its `ValueList` with `set_if_neq`; the kit keeps the lines in step in place. |
| `ValueList` | `ValueList { values: Vec<String> }` (component, on the root) | The values the list shows. | |
| `ValuesEdited` | `ValuesEdited { entity: Entity, values: Vec<String> }` (entity event, on the root) | An add or a remove asked for, with the values as they would be after it. The list doesn't change itself: set `ValueList` to what you keep. | |
| `ValueLines`, `ValueLine`, `ValueListEmpty`, `ValueField`, `RemoveValue`, `AddValue` | components | The box of lines, each line (`ValueLine { index, value }`), the empty line, the field's text entity, and the buttons, which the kit keeps; for a test to find. | |

An add trims what is typed and clears the field; an empty value, or one already listed, adds nothing and stays in the
field. The Trailhead shell's Server settings list the factory's repos with it.

### A panel, a page or a column

| Item | Signature | What it is for |
| --- | --- | --- |
| `panel` | `panel() -> impl Scene` | A panel over the content (an inspector, a form, the connect panel): a padded, rounded column on `Surface::Panel`. Position it with a `Node` of your own and give it a `GlobalZIndex` from `layer`. |
| `section` | `section(heading: Into<String>) -> impl Scene` | A group of rows within a panel, under a dim heading. |
| `section_frame` | `section_frame() -> impl Scene` | A section's box without its heading, for a caller that keeps the heading among the section's items and updates them in place. |
| `row` | `row() -> impl Scene` | Controls side by side, centered on each other, `space::CONTROLS` apart. |
| `panel_header` | `panel_header(title: Into<String>) -> impl Scene` | A panel's or modal's title row, wrapping when long, with a close X pinned top right. The X sends `RequestClose` from itself, which climbs to whatever owns the panel and is answered there. |
| `PanelTitle` | component | Marks the header's title text, so a changing title is set in place with `set_text`. |
| `PanelTools` | component | The row at the header's right, ending with the X: insert a panel's own controls at its start. |
| `sidebar_card` | `sidebar_card() -> impl Scene` | A raised box in a sidebar for something to keep an eye on (a usage meter), spawned just before the `sidebar_footer`. |
| `spawn_detail_panes` | `spawn_detail_panes(commands: &mut Commands, parent: Entity) -> DetailPanes` | A detail modal's body: a wide main column beside a narrower sidebar of facts, spawned under `parent` (a modal's body). Use it in a modal `extent::DETAIL_WIDTH` wide. |
| `DetailPanes` | `DetailPanes { main: Entity, side: Entity }` | The two columns to fill. `DetailMain` and `DetailSide` mark them. |
| `spawn_accordion` | `spawn_accordion(commands: &mut Commands, title: &str, fold: Fold, level: SectionLevel) -> AccordionParts` | A section that opens and closes, drawn as a feathers pane (`SectionLevel::Pane`, a division of a panel) or group (`SectionLevel::Group`, a section inside one). A press on the header toggles it. Do not nest a pane in a pane. |
| `AccordionParts` | `AccordionParts { section, body, header }` | Fill `body`; add controls beside the title in `header`. |
| `spawn_header_button` | `spawn_header_button(commands: &mut Commands, header: Entity, icon: Icon) -> Entity` | A tool button at the right of an accordion's header; mark the returned entity and observe `Activate`. |
| `Accordion` | `Accordion { open: bool }` (component, on `section`) | Set `open` to open or close it. |
| `Fold` | `Open`, `Closed` | How an accordion starts. |
| `AccordionTitle`, `AccordionCaret` | components | The full title (`AccordionTitle { full }`, set it to retitle) and the caret, which the kit keeps. |
| `ACCORDION_INDENT` | `f32` | The space between an accordion body's sides and what is in it. |

Pages that fill the window are under "A sidebar, the rail and navigation".

### Settings

| Item | Signature | What it is for | Updated by |
| --- | --- | --- | --- |
| `settings_page` | `settings_page() -> impl Scene` | A page of settings (a settings modal's body, or one tab's page of it): a column, its settings `space::ROWS` apart and its sections a group apart. Spawn them as its children, in order. | |
| `spawn_settings_section` | `spawn_settings_section(commands: &mut Commands, heading: Into<String>) -> SettingsSection` | A group of settings under a dim heading, spawned unparented. | Fill or `Keyed`-sync its `rows`. |
| `SettingsSection` | `SettingsSection { section: Entity, rows: Entity }` | Put `section` in the page with `ChildOf`; put the settings in `rows`, which holds nothing else. | |
| `setting` | `setting(label: Into<String>, note: Into<String>, control: impl SceneList) -> impl Scene` | One setting: its label over its control (a `select`, a `button_group`, a value's `text`), with a dim note under it saying what it does; an empty note is hidden. Pass the control into the scene with your marker on it, as for `field`. | The control's own way; the note is `SettingNote`'s `Text` (`set_text`). |
| `setting_toggle` | `setting_toggle(label: Into<String>, note: Into<String>, checked: bool, own: impl Scene) -> impl Scene` | A setting that is on or off: a `toggle_switch` with its label after it, and the note under it. `own` goes on the switch, which sends `ValueChange<bool>`. | The switch keeps its own state. |
| `SettingNote` | component | A setting's note. | |
| `spawn_settings_tabs` | `spawn_settings_tabs(commands: &mut Commands, parent: Entity) -> SettingsTabs` | A settings modal with pages: a vertical `tab_strip` `extent::SETTINGS_TABS_WIDTH` wide down the side, and a pane beside it for the page shown, spawned under `parent` (a modal's body). It grows into a tall modal's height, so a page can fill what is left. | |
| `SettingsTabs` | `SettingsTabs { root: Entity, strip: Entity, pane: Entity }` | Fill `strip` with `tab`s (a `tab_heading` over each group); spawn the page shown as a `settings_page()` in `pane` when its tab is picked, and despawn it when another is. | The strip's `SelectedTab`. |

Every settings surface is built from these, Lookout's settings modal included, so settings look alike in every app.
Never stack `toggle_switch`es or `field`s in a modal body by hand: bare, with no gap, a list of toggles reads as one
block of text (TRAIL-1610). Recipe 6 is a settings modal. In the Trailhead shell an app does not open a settings modal
of its own: it adds pages to the shell's (the shell's guide, "Settings pages").

### A sidebar, the rail and navigation

| Item | Signature | What it is for | Updated by |
| --- | --- | --- | --- |
| `sidebar` | `sidebar() -> impl Scene` | An app's sidebar down the left of the window, `SidebarWidth` wide, dragged wider or narrower by its right edge. A root of its own; spawn its contents as its children. | |
| `sidebar_footer` | `sidebar_footer() -> impl Scene` | The bottom of a sidebar, pushed to its foot, its children in a row (a connection chip, settings). Spawn it last. | |
| `nav_heading` | `nav_heading(words: Into<String>) -> impl Scene` | The dim words over a group of nav items. | |
| `nav_heading_action` | `nav_heading_action(words: Into<String>, heading: impl Scene, icon: Icon, hint: Into<String>, action: impl Scene) -> impl Scene` | A heading whose words are themselves a place to go, with a plain tool button at its end for something done to the group (adding to it). `heading` lands on the words' button and `action` on the tool button: your markers. | The words' `NavItem`. |
| `nav_item` | `nav_item(caption: Into<String>, icon: Option<Icon>) -> impl Scene` | A place to go in a sidebar, the sidebar's width, plain until it is the page shown. | `NavItem { current }`, with `set_if_neq`. |
| `nav_item_noted` | `nav_item_noted(caption: Into<String>, note: Into<String>, end: Into<String>) -> impl Scene` | A nav item with a dim note after its caption and a dim value at its end (a project's name, its key, its open count). The caption is cut to fit. | `NavEdit::set_noted(item, caption: &str, note: &str, end: &str)`, which writes only what differs. |
| `NavEdit`, `NavPart` | system parameter; component (`Caption`, `Note`, `End`) | The noted nav item's editor and the parts it finds. | |
| `NavItem` | `NavItem { current: bool }` (component) | Whether a nav item or rail item is the page or app shown, which gives it a background. | |
| `beside_sidebar` | `beside_sidebar() -> Node` | The area right of a sidebar, for a page to fill: finish it with `..beside_sidebar()` and put `BesideSidebar` on the page too, so it follows the sidebar's width. | The kit moves it. |
| `BesideSidebar` | component | On a page beside the sidebar. | |
| `rail` | `rail() -> impl Scene` | The strip of apps down the left edge of a window that holds several. The Trailhead shell draws it; an app never does. | |
| `rail_item` | `rail_item(icon: Icon, name: Into<String>) -> impl Scene` | An app's button on the rail. | `NavItem`. |
| `rail_foot` | `rail_foot() -> impl Scene` | The bottom of the rail, for what the window says about itself rather than about an app (the shell's Ask button, its update ready to restart onto): `tool_button`s, spawned after the rail items. The shell draws it; an app never does. | Hide or show the foot, or a button in it, with `set_display`. |
| `beside_rail` | `beside_rail() -> Node` | The whole window right of the rail, for an app with no sidebar. Put `BesideRail` on the page too. With no rail it starts at the window's left edge. | The kit moves it. |
| `BesideRail` | component | On a page beside the rail. | |
| `SidebarWidth` | resource: `SidebarWidth::new(width: f32)`, `get(self) -> f32` | How wide the sidebar is, held between `extent::SIDEBAR_MIN_WIDTH` and `SIDEBAR_MAX_WIDTH`. Set it to restore a saved width. | |
| `SidebarEdge` | component | The strip along the sidebar's edge that drags it, for a test to find. | |
| `tab_strip` | `tab_strip(orientation: ControlOrientation) -> impl Scene` | A strip of tabs, ruled along the edge it shares with what it shows. A click or an arrow key raises `ValueChange<Option<Entity>>` on the strip, naming the tab. | Insert `SelectedTab(Some(tab))` on the strip from what you keep as the page shown (or add `bevy::ui_widgets::tablist_self_update` when the strip is all there is). |
| `tab` | `tab(caption: Into<String>, orientation: ControlOrientation) -> impl Scene` | One tab, a child of a strip of the same orientation. Captions are cut to the strip's room. | |
| `TabCaption` | component | A tab's caption, holding the whole of it; the kit cuts the `Text`. | |
| `tab_heading` | `tab_heading(words: Into<String>) -> impl Scene` | Dim words over a group of tabs in a strip down the side (one app's settings pages), lined up with the captions. Not a tab: clicks and arrow keys pass over it. | |

When the kit moves the sidebar, `beside_sidebar` and `beside_rail` pages, it compares every frame, because a rail or a
sidebar coming or going moves the others; a node already in place is left untouched.

### A modal, a menu or a hint

| Item | Signature | What it is for |
| --- | --- | --- |
| `Modal` | trait on a marker component: `title(&self) -> String`; `width(&self) -> f32` (default `extent::MODAL_WIDTH`); `tall(&self) -> bool`; `wide(&self) -> bool` (both default `false`) | A kind of modal. Every form and dialog is one. `tall` takes all the height it may, for a detail view that reads better at a steady size; `wide` takes all the width it may, with `tall`, for a modal that holds a whole area of the app (settings). |
| `open_modal` | `open_modal<M: Modal>(commands: &mut Commands, modal: M) -> Entity` | Opens the modal over everything and returns its root (a `ModalRoot`). Close it by despawning the root. |
| `ModalSlots` | `ModalSlots { body: Entity, footer: Entity, tools: Entity }` (component on the root) | Inserted a frame or so after `open_modal`, once the modal has spawned: fill `body` (it scrolls), put the buttons in `footer` (it never scrolls away), and header controls at the start of `tools`. Fill it from a system on `Added<ModalSlots>`. |
| `ModalBody`, `ModalFooter`, `ModalRoot` | components | The body, the footer and the backdrop root, for queries. |
| `ModalFocus` | component | On a control inside a modal (a form's first field) to give it the keyboard when it appears. Without one the modal's title takes it. The rule: a modal focuses its first thing to act on; one with nothing to type into focuses its title. |
| `ModalLocked` | component | On a modal's root while it cannot close (a save in flight): requests to close it are ignored. |
| `ModalClosed` | `ModalClosed { modal: Entity }` (event) | Sent just before a modal asked to close is despawned, for its owner to drop what it holds. |
| `MAX_HEIGHT_VH` | `f32` | How much of the window's height a modal may take. |
| `spawn_icon_menu` | `spawn_icon_menu(commands: &mut Commands, icon: Icon, hint: Into<String>) -> Menu` | A button showing `icon` and a chevron, with a popup under it. Feathers places the popup and closes it on Escape or a click outside. |
| `Menu` | `Menu { root, button, popup }` | Put `root` where it goes; fill `popup` with `menu_item`s. |
| `menu_item` | `menu_item(caption: Into<String>, icon: Option<Icon>, kind: ButtonKind) -> impl Scene` | A row of a menu, in the danger color for `ButtonKind::Danger`. Pressing it sends `Activate` from it and closes the menu. |
| `close_menu_holding` | `close_menu_holding(commands: &mut Commands, inside: Entity)` | Closes the menu holding `inside`, once something else in it (not a `menu_item`) is done. |
| `MENU_WIDTH` | `f32` | A menu popup's width. |
| `dropdown_list` | `dropdown_list() -> impl Scene` | A list hanging under a field (a search's suggestions), floating over what comes after so showing it moves nothing. Put it last in a node holding just the field, which it takes its width from. |
| `dropdown_item` | `dropdown_item(caption: Into<String>) -> impl Scene` | A row of a dropdown list: a plain button the list's width, cut with an ellipsis. |
| `Hint` | `Hint(pub String)` (component) | A short hint shown beside whatever the pointer is over: icon-only buttons, truncated text. Set `.0` to change it; empty shows nothing. |
| `HintBounds` | component | On a surface hints stay inside when they fit (a modal). Elsewhere they only stay inside the window. |

Closing a modal: the X, Escape and a click outside all end as `RequestClose` (from `bevy::ui_widgets`), which the kit
answers by despawning the modal, by default for every modal, so no form can opt out by accident. A modal is never
taller than `MAX_HEIGHT_VH` of the window: only its body scrolls, with the kit's scrollbar, so the footer stays in
view.

### Progress, badges, chips, notices and messages

| Item | Signature | What it is for | Updated by |
| --- | --- | --- | --- |
| `progress_bar` | `progress_bar(fraction: f32) -> impl Scene` | A track filled from the left in the accent, `fraction` (0 to 1) full, filling the width it is given. | Set `Progress(f32)`; insert a new `ProgressTone(Token)` to recolor (`STATUS_WARN` for one running out of room). |
| `meter` | `meter(label: Into<String>, value: Into<String>, note: Into<String>, fraction: f32, tone: Token) -> impl Scene` | A label and a value on a line, a bar under them and a dim note under that (a usage window, its percent, when it resets). | Each text carries a `MeterPart` (`Label`, `Value`, `Note`); the bar carries `Progress` and `ProgressTone`. |
| `Progress`, `ProgressTone`, `MeterPart` | components | As above. | |
| `badge` | `badge(label: Into<String>, icon: Option<Icon>, tone: Option<Token>) -> impl Scene` | A short label in a small rounded chip, after an icon when given: what a card or row says about its item. Without a tone the text is dim, so a row of badges stays quiet beside the title. | `BadgeEdit::set(badge, label: &str, icon: Option<Icon>, tone: Option<Token>)` and `BadgeEdit::set_shown(badge, shown: bool)`, which write only what differs. A badge that comes and goes keeps its entity and is hidden. |
| `BadgeEdit`, `BadgeIcon`, `BadgeLabel` | system parameter; components | The badge's editor and the parts it finds. | |
| `chip` | `chip(tone: Token, words: Into<String>, hint: Into<String>) -> impl Scene` | A colored dot and a few words, with the full explanation in a hint: state that is always there and rarely needs reading (a connection). | `ChipEdit::set(chip, tone, words: &str, hint: &str)`, which writes only what differs. |
| `dot` | `dot(tone: Token) -> impl Scene` | The chip's dot on its own. | A new `BackgroundTone`. |
| `ChipEdit`, `ChipDot`, `ChipWords` | system parameter; components | The chip's editor and the parts it finds. | |
| `notice` | `notice() -> impl Scene` | A boxed line saying how something went, with an icon for its kind. Hidden until written to. Every status line is one of these, so they look alike wherever they sit. | Insert or set `Notice { tone, message }` (or `Notice::new(tone, message)`); an empty message hides it. |
| `notice_with` | `notice_with(says: Notice) -> impl Scene` | A notice that says something from the moment it is drawn (a row drawn again for a command that failed, a page opened while offline). A `Notice` inserted right after spawning a `notice()` is overwritten by the scene's own empty one. | As `notice`. |
| `Notice`, `Tone` | `Notice { tone: Tone, message: String }`; `Tone::{Info, Success, Warning, Error}`, `token()`, `subtle()` | What a notice says and what kind of news it is. | |
| `skeleton` | `skeleton(rows: usize, row_height: f32) -> impl Scene` | Pulsing placeholder bars where content is on its way. `SkeletonBar` marks each. | Despawn it when the content arrives. |
| `bubble` | `bubble(side: Side) -> impl Scene` | A message in a conversation: the reader's (`Side::Mine`) against the end edge in the accent, others' (`Side::Theirs`) against the start edge. Put it in a column and spawn its lines as children. | |
| `bubble_head` | `bubble_head(icon: Icon, words: Into<String>) -> impl Scene` | A bubble's first line: an icon for the kind of message and dim words about it (who, when). | |

### A picture

| Item | Signature | What it is for | Updated by |
| --- | --- | --- | --- |
| `spawn_picture` | `spawn_picture(commands: &mut Commands, caption: Into<String>, picture: Picture) -> PictureParts` | A picture (a screenshot) in a box `extent::PICTURE_WIDTH` by `PICTURE_HEIGHT`, bordered, fitted inside it keeping its aspect ratio, with its caption under it cut to the box's width. Pressable as a whole: a press, Enter or Space sends `Activate` from the root, for the caller to open the large view. Spawned unparented. | Set the `Picture` on `PictureParts::frame` with `set_if_neq`; the kit shows it in place. |
| `PictureParts` | `PictureParts { root: Entity, frame: Entity }` | Put `root` where it goes; `frame` holds the `Picture`. | |
| `Picture` | `Loading` (default), `Ready { image: Handle<Image>, width: u32, height: u32 }`, `Failed(String)` (component on a frame) | What a frame shows: "Loading…" dim, the image, or why it failed in the danger color. `width` and `height` are the image's pixels, which the frame fits it by. | |
| `png_image` | `png_image(bytes: &[u8]) -> Result<Image, String>` | A PNG's pixels as an `Image` to add to `Assets<Image>` (kept where it is drawn only, not in the app's memory too), or why it could not be read. Bevy is built without its image formats; this uses the `png` crate. Cache the handle by the bytes' hash rather than decoding the same picture twice. | |
| `open_picture_view` | `open_picture_view(commands: &mut Commands, title: Into<String>, pictures: Vec<ViewedPicture>) -> Entity` | One picture, or a pair (before and after), as large as a modal may be (`wide` and `tall`), each fitted to its room under its caption. A pair stands side by side, and a `button_group` in the header shows either alone in the same place, so flipping between them shows a small difference. A second group sizes them to Fit or at Actual size (pixel for pixel, scrolling both ways). Escape, the X or a click outside closes it. Returns the modal's root. | Set the `Picture` on the frame whose `PictureViewSlot` is the picture's index, as for a thumbnail. |
| `ViewedPicture` | `ViewedPicture { caption: String, picture: Picture }` | One picture of the large view. | |
| `PictureViewSlot` | `PictureViewSlot(pub usize)` (component on a large view's frame) | Which of the pictures given a frame holds, from 0. | |
| `PictureFit` | `Fit` (default), `Actual` (component on a frame) | Whether the frame fits its image to its room or shows it pixel for pixel. The large view's group sets it. | |
| `PictureView`, `PictureImage`, `PictureStatus`, `PictureCaption` | components | The large view's modal, and a frame's image, its status words and a thumbnail's caption, which the kit keeps; for a test to find. | |

The picture does not fetch or decode anything itself: the caller reads the bytes (over the API, from a file),
decodes them with `png_image` once per picture, and sets `Picture::Ready`. Lookout's inspector shows a task's
screenshots this way, before and after, and opens a pair in the large view on a press.

### Scrolling

| Item | Signature | What it is for |
| --- | --- | --- |
| `ScrollBoth` | component | An area that scrolls both ways (Shift + wheel goes sideways). Give it `overflow: Overflow::scroll()` or `scroll_x()`. |
| `ScrollDown` | component | An area that scrolls down only, inside a `ScrollBoth` that takes the sideways part. Give it `overflow: Overflow::scroll_y()`. |
| `scroll_frame` | `scroll_frame() -> impl Bundle` | The node that holds a panel's scrolling body and its scrollbar, so the bar runs along the body only. It takes the height the panel has left and may shrink below its content. |
| `scroll_frame_node` | `scroll_frame_node(grow: f32) -> Node` | A frame's node growing by `grow`, for a frame that sets its own growth or is spawned in a scene. |
| `scroll_body` | `scroll_body() -> impl Bundle` | The body that scrolls down, a column of rows `space::ROWS` apart, carrying `ScrollDown`. Put it in a `scroll_frame`. |
| `scroll_body_node` | `scroll_body_node(gap: f32, grow: f32) -> Node` | A body's node with another gap or growth. Add `ScrollDown` yourself. |
| `scrollbar` | `scrollbar(target: Entity, orientation: ControlOrientation) -> impl Scene` | A scrollbar for the scrolling node `target`, spawned into the target's parent after it. It shows only while the area overflows and the pointer is over it, and fades. A vertical bar lies in the holder's right padding, which must be `space::PANEL_INSET`. |
| `has_scrollbar_room` | `has_scrollbar_room(holder: &Node) -> bool` | Whether a holder has the right padding for its vertical bar; a test pins it for each holder. |
| `SCROLLBAR_WIDTH`, `SCROLLBAR_GAP`, `SCROLLBAR_INSET` | `f32` | How thick a bar is, its gap from its area, and a horizontal bar's inset from the bottom. |

The pattern, from Lookout's sidebar:

```rust
let frame = commands.spawn(scroll_frame()).insert(ChildOf(root)).id();
let places = commands.spawn((scroll_body(), Places)).insert(ChildOf(frame)).id();
commands.queue_spawn_scene(scrollbar(places, ControlOrientation::Vertical)).insert(ChildOf(frame));
```

An area scrolls only while its content overflows it by more than a physical pixel: layout rounds each edge to whole
physical pixels, so a list that fits can come out a pixel past its area, and the wheel once moved it by that pixel
(TRAIL-1610). The wheel, the scrollbar and the kit's clamp all measure the same range, so an area with none shows no
bar and does not move, and a position left over from taller content is brought back when the content shrinks.

### Icons

| Item | Signature | What it is for |
| --- | --- | --- |
| `Icon` | enum; `glyph(self) -> &'static str` | A Lucide glyph from the embedded icon font. The variants are listed in section 3. |
| `icon_text` | `icon_text(icon: Icon) -> impl Scene` | An icon in text, at `size::HEADING`, with no color of its own: give it a `TextTone`. |
| `button_icon` | `button_icon(icon: Icon) -> impl Scene` | An icon as a hand-built button's caption, in the button's text color. `button` and `tool_button` already use it. |

### Writing in place

| Item | Signature | What it is for |
| --- | --- | --- |
| `set_text` | `set_text(text: Mut<Text>, wanted: &str)` | Set a text's string, leaving it untouched if it already reads that. |
| `patch_node` | `patch_node(node: Mut<Node>, edit: impl FnOnce(&mut Node))` | Apply an edit to a copy of a node and store it only if it came out different. |
| `set_display` | `set_display(nodes: &mut Query<&mut Node>, entity: Entity, shown: bool)` | Show or hide one entity's node. |
| `display` | `display(shown: bool) -> Display` | `Flex` or `None`. |

A plain write through `Mut` marks a component changed even when it lands the same value; a changed `Node` sends its
whole tree back through layout and a changed `Text` back through shaping. A sync that runs every frame writes through
these, or compares first, so the UI does not lay itself out when nothing on screen moved.

### Motion

| Item | Signature | What it is for |
| --- | --- | --- |
| `ease_in` | `ease_in(t: f32) -> f32` | Starts slow, ends at full speed (`t` 0 to 1, clamped): something arriving to cover what is there. |
| `ease_out` | `ease_out(t: f32) -> f32` | Starts at full speed and settles: something leaving to show what is under it. |
| `BackgroundFade` | `BackgroundFade(pub f32)` (component; default 1) | A node's background drawn at this share of its tone's opacity, beside a `BackgroundTone` or `Surface`: something fading in or out. The kit repaints it after the theme does, so it holds through a theme change. Set it with `set_if_neq`. |

Something that moves is driven by real time (`Res<Time<Real>>`), so a slow frame carries it further instead of
stretching it, and is worked out from where it started rather than added to frame by frame. While it moves it writes
`bevy::window::RequestRedraw` every frame, since the Trailhead shell's window updates only on input otherwise, and it
stops writing when it stops moving. What moves is set in place each frame (`patch_node`, `set_if_neq` on a
`BackgroundFade`), which is an in-place update, not a redraw. The kit's own: the scrollbar's fade, the skeleton's
pulse; the shell's: the cover drawn while it switches apps.

### The theme and the plugins

| Item | Signature | What it is for |
| --- | --- | --- |
| `Token` | `Token::new(name: &'static str)` (const) | A named color. An app's own is prefixed with its name (`lookout.priority.1`) so it cannot collide with the kit's. |
| `token` | module of `Token` constants; `token::ALL` | The kit's semantic tokens (section 3). |
| `BackgroundTone`, `BorderTone`, `TextTone` | `BackgroundTone(pub Token)` and so on (immutable components) | A node's background, border (all four sides) or text in a token's color, repainted when the theme changes. Insert a new one to change the token. |
| `Surface` | `Window` (default), `Panel`, `Raised`, `Floating` (immutable component); `token(self) -> Token` | A surface: its background in its level's token, and the level passed down so a control on it takes the color that shows there. |
| `Theme` | `new()`, `set(&Token, Color)`, `set_accent(&Token, Color)`, `alias(&Token, &Token)`, `paint(&Token, Color)`, `paint_both(&Token, dark: Color, light: Color)`, `color(&Token) -> Color`, `color_on(&Token, Surface) -> Color`, `has(&Token) -> bool`, `repeated_colors() -> Vec<String>`, `light() -> Theme`, `with_accent(Color) -> Theme` | Every token and its color (section 3). |
| `palette` | `GRAY_0` .. `GRAY_3`, `WHITE`, `ACCENT` | The raw colors the theme is painted from, named only where a theme is built. |
| `ThemePlugin` | `ThemePlugin { theme: Theme, choice: ThemeChoice, follow_desktop: bool }`, `ThemePlugin::new(theme)` (follows the desktop), `ThemePlugin::fixed(theme, choice)` (never asks it) | Feathers' controls and the theme they are drawn in. The shell adds it; its screenshots add it `fixed`, so nothing asynchronous changes the theme under a shot. |
| `Appearance` | resource: `choice: ThemeChoice`, `new(choice)`, `is_dark()`, `reduce_motion()`, `wear(&Theme) -> Theme` | The theme being worn: the person's choice and what the desktop says. Set `choice` to change it. `reduce_motion()` says the desktop asks for less motion (on Linux the portal's `reduced-motion`, or GNOME's animations turned off): what would move to show a change should make it at once. |
| `ThemeColors` | system param: `color(&Token) -> Color`, `is_changed() -> bool` | The worn theme's colors for what no tone component reaches, such as a 3D scene's materials and lights (Lookout's trail map), so they follow the light theme and the accent too. A node's colors stay `BackgroundTone`, `TextTone` and `BorderTone`. |
| `ThemeChoice` | `Desktop` (default), `Light`, `Dark`; `ALL`, `label()`, `icon()` | Where light or dark comes from. Once a person picks light or dark the desktop is no longer followed; the accent is followed either way. |
| `UiPlugin` | plugin | The icon font, hints, modals, inline widgets, scrolling, the sidebar's drag, reordering lists, the `TextSize` and its keys, and the systems in `UiPlace`. The shell adds it. |
| `UiPlace` | system set | Where the kit's per-frame systems run in `Update`. |
| `ModalPlugin`, `InlinePlugin`, `ReorderPlugin` | plugins | Part of `UiPlugin`. Add one on its own only in a headless test of logic that opens modals, answers inline edits or reorders a list without `UiPlugin`; adding it beside `UiPlugin` adds it twice. |
| `layer` | module of `i32` constants | Which surface is over which (section 3). |
| `lint` | module | The design-system lint and the scan for wholesale redraws, `lint::redraws` (section 6). |

### How big the UI is drawn

| Item | Signature | What it is for | Updated by |
| --- | --- | --- | --- |
| `TextSize` | resource: `TextSize::STEPS` (`[80, 90, 100, 110, 125, 150, 175, 200]`, percent), `DEFAULT` (100%), `all()`, `at(step) -> Option<TextSize>`, `nearest(percent: u32)`, `step()`, `percent() -> u32`, `scale() -> f32`, `label() -> String` ("125%"), `bigger()`, `smaller()`, `reset()`, `is_default()` | How big the whole window's UI is drawn: text, icons, spacing, controls and panels together, through Bevy's `UiScale`, which it owns. A person moves it with Ctrl (Cmd on macOS) and `=` or `+` (bigger), `-` (smaller) and `0` (100%), from anywhere in the window, a focused text field included. `all()` and `label()` are what a settings menu lists. Set it as the app is built to start at a kept size: it is drawn at from the first frame. | Trigger `ChangeTextSize`, or set the resource. Never write `UiScale`. |
| `ChangeTextSize` | event: `Bigger`, `Smaller`, `Reset`, `To(TextSize)` | A change of size from an app, a menu or a test, as the keys make. | |
| `TextSizePlugin` | plugin | The resource, its keys and `ChangeTextSize`. Part of `UiPlugin`. | |

It scales everything rather than text alone because the kit lays out in named sizes and spaces: text grown on its
own would overflow the buttons, rows and badges sized to hold it. In the Trailhead shell the size is kept for the next
launch and applies to every app; an app adds nothing. The kit shows nothing when it changes (it has no toast); the
window simply redraws larger or smaller.

### Test helpers

`trailhead_ui::test_support`, behind the kit's `test-support` feature (turn it on in `[dev-dependencies]`):
`press_key(world: &mut World, target: Entity, key_code: KeyCode, logical_key: Key)` triggers a key press on `target`
as if it had focus, without running a frame; `press_escape(world, target)` presses Escape.

### Near-synonyms: which to use when

- **`select` / `button_group` / `spawn_choice_tile` / `toggle_switch` / `dropdown_list` / `spawn_icon_menu`.** A choice
  from a fixed set is a `select`. Two to four short options that should all show at once, or a short ladder of steps,
  are a `button_group`. Choices best seen, each a small picture, are choice tiles. On or off is a `toggle_switch`. Suggestions under a text field are a `dropdown_list`. Actions behind a button are a menu.
- **`select` / `select_toned` / `select_iconed` / `inline_field`.** Plain options, options in colors that encode data,
  options with icons, and a labelled select in a sidebar whose change the caller confirms or refuses.
- **`text_input` / `inline_text` / `spawn_search_field`.** A field in a form; a value edited where it is read, with
  Save and Cancel; a search box with a hint and a clear button.
- **`button` / `tool_button` / `check_circle` / `link_button` / `nav_item` / `menu_item` / `dropdown_item` / `table_row` /
  `HoverTone`.** A button with words; one with an icon only; a round check that marks an item done; one that opens a
  URL; a place to go in a sidebar; a row of a menu; a row of a dropdown list; a table row; anything else pressable.
  A setting that is on or off is a `toggle_switch`, not a check.
- **`badge` / `chip` / `notice`.** Something an item says about itself (a key, a priority); state that is always
  there, with a dot and a hint; a message about how something went. All three are updated in place.
- **`field` / `setting` / `setting_toggle`.** A label over a control in a form; the same in a settings page, with a
  note under it; a setting that is on or off.
- **`panel` / `section` / `field_well` / `sidebar_card` / `spawn_accordion`.** A surface over the content; a group of
  rows inside one; a box to set text fields in; a raised box in a sidebar; a section that opens and closes.
- **`text` / `text_dim` / `text_toned` / `text_truncated` / `button_text`.** Body text; secondary text; text in a
  status color; one line cut to fit its row; a caption inside a hand-built button.
- **`spawn_choice_tile` / `spawn_picture`.** A small picture that is one of a row of choices; a picture shown for
  itself (a screenshot), which opens large when pressed.
- **`Keyed` / `ReorderList`.** A list that follows your data's order; a list whose order a person sets, which reports
  each move for you to apply to that data (and then `Keyed` moves the nodes).
- **`scroll_frame` + `scroll_body` / `ScrollBoth`.** A panel's body that scrolls down; an area (a board of columns) that
  scrolls both ways, with `ScrollDown` areas inside it.

## 3. Colors, sizes, spacing, icons and layers

### Colors

Colors come in three layers. The `palette` is raw colors, named only where a theme is built. The `token`s are
meanings, which views and widgets name. A widget's own token, or an app's colors that encode data, sit on top.

The kit's tokens (`token::*`):

| Group | Tokens |
| --- | --- |
| Surfaces, by elevation | `SURFACE_WINDOW` (behind everything), `SURFACE_PANEL` (an inspector, a sidebar, a modal's body), `SURFACE_RAISED` (a card, someone else's message, a field well), `SURFACE_FLOATING` (a hint, a menu, a popup) |
| Text | `TEXT`, `TEXT_DIM` (secondary), `TEXT_DISABLED` |
| Border | `BORDER` (a border, the rule between sections or beside a sidebar) |
| Fills, each with `_HOVER` and `_PRESSED` | `FILL_SOLID` (a badge, a placeholder bar, a normal button), `FILL_ITEM` (no fill until hovered: a plain button, a tab, a row), `FILL_ACCENT` (the one call to action, the tab picked, the reader's own message), `FILL_FIELD` (where text is typed) |
| Statuses, each with `_SUBTLE` | `STATUS_OK` (went well), `STATUS_WARN` (wants a person), `STATUS_DANGER` (destructive or failed), `STATUS_INFO` (under way, or worth knowing) |

The rules, with their reasons:

- **A color is a token on a node, never a literal.** Use `BackgroundTone`, `TextTone` or `BorderTone`, never
  `Color::srgb(..)` or `BackgroundColor`. A literal does not follow a theme switch, the light theme or the desktop's
  accent, so it stays dark in a light theme. The tone components are immutable: insert a new one to change the token.
- **Name a meaning.** Views and widgets name the semantic tokens. Each one sits on one of feathers' semantic slots
  where feathers has one, so feathers' own controls wear it too. Do not add a token for a widget unless it has to be
  restyled on its own; then `Theme::alias` it to a meaning. Feathers' own `tokens::` appear only inside the kit widget
  wrapping that control.
- **Fuchsia means a missing token** (TRAIL-777). If you see magenta, a token is not in the theme. `Theme::has` and
  `token::ALL` let a test check that every token resolves.
- **A surface is a `Surface`, not a `BackgroundTone` of a surface token.** `Surface` also tells feathers what the
  controls inside sit on, so a field or solid fill on a card takes the color that shows there. Check a color against a
  surface in a test with `Theme::color_on`.
- **The theme API.** An app builds on `Theme::new()` (in the shell, in `ShellApp::paint`). `Theme::set` repaints a
  meaning everywhere, and every alias follows; `set_accent` sets a shade of `palette::ACCENT`, which then follows the
  desktop's accent. `Theme::alias` points a token at a meaning (Lookout's `PRIORITY_1` is `STATUS_DANGER`).
  `Theme::paint` is only for an app's categorical colors, which encode data (Lookout's statuses, priorities and
  grades), each on a slot of its own so repainting it changes nothing else. `Theme::paint_both` paints one with its
  own light color too, where the dark one turned over would read wrong (the trail map's night sky is not a day sky). `palette` and color literals appear only
  where the theme is built.
- **Paint once; check for repeats.** `Theme::repeated_colors` lists a painted color that another painted token or a
  meaning already has, which should be an alias instead. An app's test asserts it is empty.
- **Paint dark; the kit derives light and the accent.** `Theme::light` turns near-neutrals over and darkens colors that
  mean something; `Theme::with_accent` moves the accent's shades onto the desktop's. Which one is worn is
  `Appearance`'s. The light theme is a first cut, not a designed palette.

### Text sizes

`Size` is made only in the kit, and the text functions take one, so a size typed as a number does not compile. Sizes
are named for where text sits, so the scale is one edit. `Size::px` is for measuring (a line's height).

| `size::` | px | For |
| --- | --- | --- |
| `SMALL` | 11 | Badges, a card's key, counts beside a column's name |
| `SECONDARY` | 12 | A card's secondary line, a field's label |
| `BODY` | 13 | Body text, button captions (feathers' own size) |
| `CARD_TITLE` | 14 | A card's title |
| `HEADING` | 15 | A column's name, a panel's heading, an icon |
| `APP_TITLE` | 18 | The app's name at the top of its sidebar or page |
| `DETAIL_TITLE` | 18 | The title of the one thing a detail modal shows |

### Spacing, corners, strokes and extents

Gaps, padding, margins and offsets come from `space`, corners from `radius`, border widths from `stroke`, and panel
widths and offsets from `extent`, each named for what it is for. These go into a `Node`'s `Val`s, which take any
number, so the lint is what finds a literal there (section 6). If no name fits, add one to the kit with a doc
comment. The scale underneath (`step`: 2, 4, 8, 12) is private to the kit: name the purpose, not the step.

| `space::` | px | For |
| --- | --- | --- |
| `SEAM` | 1 | The join between controls drawn as one (a button group's options) |
| `STACKED` | 2 | Lines within one item: a key over a title, a label over its input |
| `ICON_LABEL` | 4 | An icon and the text beside it, inside one control |
| `CONTROLS` | 4 | Controls side by side in a row |
| `LIST_ITEMS` | 4 | Rows of a list that read as one block |
| `CARDS` | 8 | Cards stacked in one column |
| `ROWS` | 8 | Rows of controls in a section or panel |
| `GROUPS` | 12 | Groups of rows within one panel, and columns side by side |
| `INDENT` | 12 | How far a nested item sits in from its parent |
| `HINT_MARGIN` | 4 | The least room a hint keeps from the window's or its surface's edge |
| `DOT` | 8 | A status chip's dot, across |
| `BADGE_INSET` | 2 | Inside a badge |
| `LABEL_INSET` | 4 | Inside a small labelled box |
| `INPUT_INSET` | 4 | Inside an inline text's input, before and after the text |
| `CONTROL_INSET` | 8 | Inside a card, a field, a hint |
| `PANEL_INSET` | 12 | Inside a panel, a column, a header bar |

| `radius::` | px | For |
| --- | --- | --- |
| `SMALL` | 3 | A badge, a hint |
| `CONTROL` | 4 | A card, a field, a button |
| `PANEL` | 6 | A column, a panel |

| `stroke::` | px | For |
| --- | --- | --- |
| `HAIRLINE` | 1 | A border, a divider |
| `GRIP` | 2 | The line a drag edge shows while hovered or dragged |
| `RING` | 1.5 | A round check's ring |

| `extent::` | px | For |
| --- | --- | --- |
| `RAIL_WIDTH` | 48 | The rail of apps |
| `SIDEBAR_WIDTH` | 220 | An app's sidebar until its edge is dragged |
| `SIDEBAR_MIN_WIDTH`, `SIDEBAR_MAX_WIDTH` | 200, 500 | How far the sidebar is dragged |
| `DRAG_EDGE` | 6 | The strip along a sidebar's edge that drags it |
| `CONTROL_HEIGHT` | 24 | A control one line high: a button, a select, a one-line text input (feathers' row height) |
| `CHECK_SIZE` | 16 | A round check's ring, across |
| `PROGRESS_HEIGHT` | 6 | A progress bar |
| `SIDE_PANEL_WIDTH` | 360 | A panel down the right of the window (an inspector) |
| `MODAL_WIDTH` | 640 | A modal, unless its kind says otherwise |
| `SEARCH_WIDTH` | 220 | A search box in a row of controls |
| `FIELD_LABEL_WIDTH` | 90 | The label column in a sidebar field |
| `DETAIL_WIDTH`, `DETAIL_SIDEBAR_WIDTH` | 960, 280 | A detail modal and its sidebar of facts |
| `SETTINGS_TABS_WIDTH` | 180 | The tabs down the side of a settings modal with pages |
| `TILE_PICTURE_WIDTH`, `TILE_PICTURE_HEIGHT` | 72, 44 | A choice tile's picture |
| `PICTURE_WIDTH`, `PICTURE_HEIGHT` | 160, 100 | A picture's thumbnail box (`spawn_picture`): two side by side fit a side panel |

The lint reads lines, not meaning: a number hidden in a constant named for nothing gets past it. Name constants for
what they are for, and prefer the kit's names over constants of your own.

### Icons

Icons are `Icon` variants, glyphs from the embedded Lucide font (`src/assets/lucide.ttf`, licensed in
`src/assets/LUCIDE-LICENSE`). A font rather than images: glyphs are drawn at the physical size, so they stay sharp,
and they take the text color a button gives its caption, so hover and disabled repaint them. The text font has no icon
glyphs, so anything that is not an `Icon` shows as a box or a question mark.

The variants (the Lucide name in brackets where it differs): `Archive`, `ArchiveRestore`, `Board` (kanban), `Bot`,
`Calendar`, `Check`, `ChevronDown`, `ChevronRight`, `ChevronUp`, `Circle`, `CircleAlert`, `CircleCheck`, `CircleX`, `Comment`
(message-square), `Copy`, `Ellipsis`, `Eye`, `ExternalLink`, `Filter`, `Flag`, `Footprints`, `Help` (circle-help),
`Handshake`, `History`, `Info`, `Inspect` (panel-right-open), `LogIn`, `Map`, `Monitor`, `Moon`, `Pencil`, `Plus`,
`PullRequest` (git-pull-request), `Refresh` (refresh-cw), `Release` (lock-open), `Search`, `Send`, `Settings`,
`Shield`, `Signpost`, `Stop` (ban), `Sun`, `TriangleAlert`, `Undo` (undo-2), `User`, `Wifi`, `WifiOff`, `X`.
The list in `src/icons.rs` is the one to trust.

Adding an icon is one variant and its codepoint in `Icon::glyph`, taken from the same font file's glyph names (its
`cmap` and `post` tables), which is the same `lucide-static` release the font came from. Codepoints are not stable
across Lucide releases, so never take one from a different release.

### Layers

A surface that fills the window is spawned after the overlays, so an overlay without a `GlobalZIndex` loses every
click to it, even though it draws on top (TRAIL-776). Every overlay takes its layer from `layer`:

| `layer::` | Value | What sits there |
| --- | --- | --- |
| `BOARD` | 0 | An app's main surface and its header (named for Lookout's board) |
| `CONNECT` | 1 | The shell's connect panel along the bottom while not live |
| `SIDEBAR_EDGE` | 1 | The sidebar's drag edge, half over the page beside it |
| `INSPECTOR` | 2 | A side panel down the right |
| `SWITCH` | 5 | The cover a window holding several apps draws over the app area while it switches apps (the Trailhead shell's) |
| `MODAL` | 10 | Every form and dialog, over a backdrop covering the window |
| `MENU` | 20 | Menu and dropdown popups, over whatever opened them |
| `HINT` | 30 | The hint over a hovered control, over everything |

The order is: the app's main surface at the bottom, panels above it, the shell's cover while it switches apps, then
modals, menus and hints on top. The kit's
own modal, menu, dropdown and hint widgets already set theirs.

## 4. Rules that came from real bugs

Most of these built cleanly and failed silently. The reason is kept with each rule, and the task with it where there
is one, so the history can be found.

### Feathers stays in the kit, and upstream comes first

Only `clients/trailhead-ui` imports `bevy::feathers`; an app names only what the kit exports. When a view needs
something feathers has and the kit does not expose (a token, a control, a component to query in a test), the kit grows:
it wraps it in a kit type or function and exports that. A feathers type is never re-exported as it is, and an app never
reaches past the kit, even in a test. The kit's API is the seam that keeps a feathers upgrade to one crate (TRAIL-1102).

`bevy_feathers` 0.20 has buttons (`ButtonVariant`: normal, primary, plain; the kit's `ButtonKind`), tool buttons, menu
buttons and popups, `FeathersSelect` for dropdowns, text inputs, checkboxes, toggles, sliders, scrollbars, list views
and panes. The kit wraps these and does not draw its own where feathers has one. Where feathers lacks something or has
a bug, the kit works around it in one widget with a comment saying what upstream is missing, and upstreaming the fix is
preferred over growing a private copy. The kit's systems `UiPlugin` runs stay `pub(crate)`.

Feathers' look, as the kit uses it:

- **A fixed set of choices is a dropdown**, never a button that cycles through values.
- **A button that opens a menu shows a chevron; one that acts does not.** Any icon but the ellipsis reads as a button
  that acts when pressed, so the chevron says this one opens something.
- **Every button changes background on hover**, `Plain` ones included. The hover colors are theme tokens painted in
  `theme.rs`: feathers' own were a shade of the surface, too faint to read as a response. Something pressable that is
  not a button carries a `HoverTone`, with a hover token visible against the surface it sits on.
- **One primary button per surface.** Everything else is normal, or an icon-only tool button with a hint.

### A surface is spawned once and updated in place (TRAIL-880)

Despawning a parent's children and spawning them again on a change is a bug, however small the surface: the dropdown
you just used is despawned under you, the header flashes, scroll positions go. The only despawns are opening and
closing the surface itself (a page on entering and leaving its state, a modal, a panel). On a change, set what
changed:

- a text with `set_text`, a node with `patch_node` or `set_display`;
- a select's chosen row and, only when its options changed, its rows, with `SelectEdit` (never while its popup is
  open: it returns `false` and you try again);
- a chip's tone, words and hint with `ChipEdit`; a badge's words, icon, tone and whether it shows with `BadgeEdit`; a
  noted nav item's caption, note and end with `NavEdit`;
- a nav item's `NavItem { current }`, a check's `CheckCircle { checked }`, a button group's `ButtonGroup { chosen }`, a bar's `Progress`, a notice's
  `Notice`, an accordion's `Accordion { open }`, a truncated text's `Truncated { full }`;
- a disabled state with `InteractionDisabled`;
- a list that gains, loses and reorders items with `Keyed`.

Each app guards this with two tests: one that updates a view and checks every other entity is the same `Entity` (the
example app's `pressing_the_button_changes_the_count_line_and_nothing_else_is_respawned`), and the kit's scan of its
sources for the shape of a wholesale redraw, `lint::redraws` (section 6).

### A marker beside a queued scene is added before the scene (TRAIL-1596)

`commands.queue_spawn_scene(x(..)).insert(Marker)` inserts `Marker` at once, but the scene's own components (a text's
`Text`, a notice's `Notice`, a nav item's `NavItem`) land a command later, over anything of the same type inserted
beside them. So a system that waits for `Ref<Marker>::is_added()` to write into the widget finds nothing to write on
that frame, and on the frame the widget arrives the marker is no longer new; a `Notice` inserted beside a queued
`notice()` is overwritten by the scene's empty one. Either spawn it with `commands.spawn_scene(x(..))`, which
resolves the scene when the command runs, so what is inserted after it lands after the scene's components (a kit
scene always resolves at once); or spawn the widget saying what it should (`notice_with`, a text's own words); or
watch what the scene gives (`Ref<Text>::is_added()`, `Added<Notice>`), never the marker.

### A node is never smaller than its content unless you say so (TRAIL-836)

`width` or `height: percent(100)` loses to the content's size in Taffy. A full-window panel full of cards grew past
the window, so nothing inside it could overflow and nothing scrolled. A panel or column that should hold its children
to a size and let them scroll needs `min_width: px(0)` / `min_height: px(0)`, and its scrolling child the matching
`overflow: Overflow::scroll_*()`. `Overflow::clip()` alone does not do it. `scroll_frame` and `scroll_body` carry the
minimums for you.

### Full-window surfaces and layers (TRAIL-776)

A full-window surface is spawned after the overlays, so an overlay without a `GlobalZIndex` loses every click to it,
though it draws on top. Give every overlay its layer from `layer` (section 3).

### Scrolling goes through the kit (TRAIL-836)

Mark an area `ScrollBoth` or `ScrollDown`. The kit's one observer walks up from what the pointer is over to the
nearest of each and splits the wheel turn between them. Do not add a wheel observer of your own per area: a bubbling
`PointerScroll` keeps naming the entity it was fired on, so an ancestor's handler never sees its own entity. A view
that did rebuild its nodes would have to carry `ScrollPosition` over itself; keeping the node is the fix.

### Presses arrive as `Activate`

A press is `bevy::ui_widgets::Activate` on the pressed entity, not `Interaction`. Observe it and check the entity is
yours: an observer hears every `Activate` in the app.

### Logic and drawing are separate plugins

`XPlugin` holds the state, resources and observers and runs under `MinimalPlugins` in tests; `XUiPlugin` spawns and
draws, and only a windowed app adds it. In the Trailhead shell, the first goes in `ShellApp::build` and the second in
`ShellApp::build_ui`. The binary wires the app-level dependencies (`DefaultPlugins`, `ThemePlugin`, `UiPlugin`, winit
settings); in the shell, `trailhead_shell::Shell` does it for every app. A plugin never adds `StatesPlugin` or another
app-level plugin for its tests' sake; the test app adds what `MinimalPlugins` lacks (TRAIL-774).

### Test the logic headless

Pure functions that turn state into a view model carry most of the tests (Lookout's `board::build`,
`inspector::build`, `project_rows`). A view model that derives `PartialEq` is also what `Keyed` compares.

### Keep the clipboard feature

`system_clipboard` must stay in the binary's Bevy features, or no text field can paste. The shell's `Cargo.toml` has
it.

### Sizes are the kit's names, and measurements are in UI pixels (TRAIL-1622)

A person can draw the whole UI at 80% to 200% (`TextSize`), which sets Bevy's `UiScale`. Under it, the units `Val::Px`
is in, UI pixels, are that multiple of the window's logical pixels. Two rules keep a view right at every size:

- **Sizes come from the kit's names, never from the window.** Widths, gaps and offsets are `extent`, `space`, `radius`
  and `stroke` values (or shares of what holds them: `percent`, `flex_grow`), so they grow with the text. A size worked
  out from the window's pixels (`Window::width()`, `resolution`) is in logical pixels and comes out wrong by the text
  size. To cap something at the window, use `vw` and `vh`, which are shares of the window whatever the text size (the
  modal's frame does).
- **Anything that reads layout or the pointer converts to UI pixels.** `ComputedNode`'s fields (`size`,
  `content_size`, `scroll_position`, `padding`, `border`) and `UiGlobalTransform`'s translation are physical: multiply
  by `ComputedNode::inverse_scale_factor()` for UI pixels, which `Val::Px`, `ScrollPosition`, the kit's sizes and
  `ComputedUiRenderTargetInfo::logical_size()` are all in (Bevy calls these "logical"; under a text size they are
  not the window's). `TextLayoutInfo::size` divided by its `scale_factor` is in UI pixels too. The window's size,
  `Window::cursor_position()`, a pointer event's `pointer.position`, a drag's `distance` and `delta`, and a wheel's
  pixels are logical: divide them by `UiScale` (or `TextSize::scale()`) before measuring them against layout. Bevy's
  own sliders, scrollbars, text inputs and popovers already do. In the kit the sidebar's drag and the wheel were the
  two places that mixed them; bevy-data-explorer, where this came from, had to fix its docks, drags, tooltips,
  viewports and selection rectangles.

Test what measures at a text size other than 100%: the kit's tests lay nodes out headless at `UiScale` 1.5
(`layout_test::layout_app`), and a shots test takes its busiest views at 150% too.

### A drag is not a press (TRAIL-1621)

Bevy's picking reports a drag from the first pixel the pointer moves while pressed, and a release over the pressed
button still clicks it, so an item that is both pressable and draggable would open on every drag that ends over it. A
press becomes a drag only once the pointer has gone `DRAG_THRESHOLD` UI pixels (a drag's `distance` divided by
`UiScale`, as everywhere); until then it is a press, and a hand that shakes still opens the item. The kit then takes
`bevy::ui::Pressed` off the button, which is what feathers' click checks before it sends `Activate`, so the release
that ends a drag never activates. A view never undoes this by sending `Activate` itself from a drag's end.

### Look before you fix

When layout or input misbehaves, measure rather than guess: log `ComputedNode` `size` and `content_size` (times
`inverse_scale_factor` for UI pixels) of the nodes involved, and what an observer receives, with a temporary
`info!(target: "probe", ..)` removed before committing. Never drive a person's mouse to test it.

## 5. Recipes

Each one is lifted from or closely modelled on real code; the file is named. They import from `trailhead_ui` and
`bevy::prelude::*` (which brings `bsn!`, `bsn_list!`, `Scene` and `SceneList`), plus the `bevy::ui_widgets` names
shown.

### Recipe 1: a page with a title and a list kept in step with data

Modelled on Lookout's `clients/trailhead-lookout/src/projects_page.rs` (`spawn_page`, `cells`, `sync_rows`), with the
table replaced by plain rows. The page is spawned once when its state is entered; the rows follow the view models
through `Keyed`; a changed row has its texts set in place.

```rust
use bevy::prelude::*;
use trailhead_ui::{BesideRail, Keyed, Surface, beside_rail, row, set_text, size, space, text, text_dim};

/// One line's view model: what it shows, compared by `Keyed` to decide whether to update it.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub name: String,
    pub count: usize,
}

/// The lines the page should show, kept by the app's logic.
#[derive(Resource, Default)]
pub struct Wanted(pub Vec<Line>);

#[derive(Component)]
struct LinesPage;

/// The node the lines are kept under: it holds nothing else.
#[derive(Component)]
struct LinesBody;

/// Which text of a line this is, for updating it in place.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
enum LineText {
    #[default]
    Name,
    Count,
}

/// The lines' entities, by key.
#[derive(Resource, Default)]
struct Lines(Keyed<Line>);

fn spawn_page(mut commands: Commands, mut lines: ResMut<Lines>) {
    // The lines of a page drawn before went with it.
    lines.0 = Keyed::default();
    let page = commands
        .spawn((
            LinesPage,
            BesideRail,
            Surface::Window,
            Node {
                flex_direction: FlexDirection::Column,
                min_width: px(0),
                min_height: px(0),
                padding: UiRect::all(px(space::PANEL_INSET)),
                row_gap: px(space::GROUPS),
                ..beside_rail()
            },
        ))
        .id();
    commands.queue_spawn_scene(text("Lines", size::APP_TITLE)).insert(ChildOf(page));
    commands.spawn((
        Node { flex_direction: FlexDirection::Column, row_gap: px(space::ROWS), ..default() },
        LinesBody,
        ChildOf(page),
    ));
}

fn despawn_page(mut commands: Commands, pages: Query<Entity, With<LinesPage>>) {
    for page in &pages {
        commands.entity(page).despawn();
    }
}

/// A new line's scene.
fn line_scene(line: &Line) -> impl Scene {
    let (name, count) = (line.name.clone(), line.count.to_string());
    bsn! {
        @row()
        Children [
            @text(name, size::BODY) LineText::Name
            --
            @text_dim(count, size::SECONDARY) LineText::Count
        ]
    }
}

/// One line per wanted item, spawned, moved, updated or despawned by key; nothing else is touched.
fn sync_lines(
    wanted: Res<Wanted>,
    body: Query<Entity, With<LinesBody>>,
    children: Query<&Children>,
    parts: Query<&LineText>,
    mut texts: Query<&mut Text>,
    mut lines: ResMut<Lines>,
    mut commands: Commands,
) {
    let Ok(body) = body.single() else { return };
    let wanted = [(body, wanted.0.iter().map(|l| (l.name.clone(), l.clone())).collect::<Vec<_>>())];
    lines.0.sync(
        &mut commands,
        &wanted,
        |commands, _, line| commands.spawn_scene(line_scene(line)).id(),
        |_, entity, line| {
            for e in children.iter_descendants(entity) {
                let Ok(part) = parts.get(e) else { continue };
                let words = match part {
                    LineText::Name => line.name.clone(),
                    LineText::Count => line.count.to_string(),
                };
                if let Ok(text) = texts.get_mut(e) {
                    set_text(text, &words);
                }
            }
        },
    );
}
```

Registered under the page's state (in the shell, an app's own state or `ActiveApp`, as the shell guide explains):

```rust
app.init_resource::<Wanted>()
    .init_resource::<Lines>()
    .add_systems(OnEnter(MyView::Lines), spawn_page)
    .add_systems(OnExit(MyView::Lines), despawn_page)
    .add_systems(Update, sync_lines.run_if(in_state(MyView::Lines)));
```

`sync` with models that have not changed issues no commands, so running it every frame costs a comparison per line.
For a long list, return early when nothing it reads has changed, as `sync_rows` does with its `changed` query and a
`Local` holding the body it last drew.

### Recipe 2: a button that does something

Copied from the example app, `clients/trailhead-app-example/src/lib.rs`. The logic is an event any caller (a test, a
key binding) can trigger; the button is one such caller. The observer checks the pressed entity is its own, because an
observer hears every `Activate`.

```rust
use bevy::prelude::*;
use bevy::ui_widgets::Activate;
use trailhead_ui::{ButtonKind, button};

/// A person pressed the button. The page triggers it; a test can trigger it with no page.
#[derive(Event, Debug, Clone, Copy)]
pub struct Press;

/// The page's button.
#[derive(Component)]
struct PressButton;

fn on_button(activate: On<Activate>, buttons: Query<(), With<PressButton>>, mut commands: Commands) {
    if buttons.contains(activate.entity) {
        commands.trigger(Press);
    }
}

// In the page's spawn system:
// commands.queue_spawn_scene(button("Press", None, ButtonKind::Normal)).insert((PressButton, ChildOf(page)));
```

`on_button` goes in the drawing plugin (`app.add_observer(on_button)`), the observer of `Press` that changes state in
the logic plugin.

### Recipe 3: a dropdown whose choice changes state

Modelled on Lookout's board header, `clients/trailhead-lookout/src/board/header.rs` (`on_pick`, `dress_header`,
`update_header`). The select is tagged with a marker, a pick sets the state, and a change to the state from anywhere
else is shown in place with `SelectEdit`.

```rust
use bevy::prelude::*;
use trailhead_ui::{Picked, SelectEdit, field, select};

/// Which items the page shows.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    #[default]
    Open,
    Done,
    All,
}

impl Shown {
    const ALL: [Shown; 3] = [Shown::Open, Shown::Done, Shown::All];

    fn label(self) -> &'static str {
        match self {
            Shown::Open => "Open",
            Shown::Done => "Done",
            Shown::All => "All",
        }
    }
}

/// Tags the dropdown that sets [`Shown`].
#[derive(Component, Debug, Clone, Copy, Default)]
struct ShownSelect;

fn options() -> Vec<String> {
    Shown::ALL.iter().map(|s| s.label().to_string()).collect()
}

/// Spawns the dropdown once, in a labelled field, under `parent` (call it from the page's spawn system).
fn spawn_shown_select(commands: &mut Commands, parent: Entity, shown: Shown) {
    let (names, at) = (options(), Shown::ALL.iter().position(|s| *s == shown));
    let control = bsn_list! { @select(names, at, 8) ShownSelect };
    commands.queue_spawn_scene(field("Show", control)).insert(ChildOf(parent));
}

/// A pick sets the state. Logic: it goes in the plugin that runs headless.
fn on_pick(pick: On<Picked>, selects: Query<(), With<ShownSelect>>, mut shown: ResMut<Shown>) {
    if !selects.contains(pick.entity) {
        return;
    }
    if let Some(&choice) = Shown::ALL.get(pick.index) {
        shown.set_if_neq(choice);
    }
}

/// Shows a change made elsewhere, in place. A select whose popup is open is left alone and tried again next frame.
fn show_shown(
    shown: Res<Shown>,
    selects: Query<Entity, With<ShownSelect>>,
    mut edit: SelectEdit,
    mut pending: Local<bool>,
) {
    if !shown.is_changed() && !*pending {
        return;
    }
    let (names, at) = (options(), Shown::ALL.iter().position(|s| *s == *shown));
    *pending = false;
    for select in &selects {
        if !edit.set(select, &names, at) {
            *pending = true;
        }
    }
}
```

The select is passed into `field` as part of the scene, not added as a child afterwards (TRAIL-850). The options stay
the same here, so `edit.set` only moves the chosen row; with options that change, it rebuilds only that select's rows.

### Recipe 4: a text updated in place

Copied from the example app, `clients/trailhead-app-example/src/lib.rs` (`show_count`): the count line is spawned once
with a marker and its `Text` is set when the count changes.

```rust
/// The line that says how many times the button was pressed.
#[derive(Component)]
struct CountLine;

// Spawned once with the page:
// commands.queue_spawn_scene(text(count_words(presses.0), size::BODY)).insert((CountLine, ChildOf(page)));

/// Keeps the count line in step with the count, in place.
fn show_count(presses: Res<Presses>, mut lines: Query<&mut Text, With<CountLine>>) {
    if presses.is_changed() {
        for line in &mut lines {
            set_text(line, &count_words(presses.0));
        }
    }
}
```

A chip is updated the same way through its editor, as Lookout's sidebar does (`update_chip` in
`clients/trailhead-lookout/src/sidebar.rs`): `edit.set(chip, tone, words, &hint)` with `mut edit: ChipEdit`.

### Recipe 5: a field that adds on Enter, as its button does

Modelled on Ticklist's add row (in the apps repo). A one-line `text_input` triggers `TextEntered` on its text entity
when Enter is pressed; the button and the key end in the same event of the app's own, so neither path is special.

```rust
use bevy::prelude::*;
use bevy::text::EditableText;
use bevy::ui_widgets::Activate;
use trailhead_ui::{ButtonKind, Icon, TextEntered, button, text_input};

/// Add an item with this title. The field and the button trigger it; a test can trigger it with no page.
#[derive(Event, Debug, Clone)]
pub struct AddItem(pub String);

/// The add field's text entity.
#[derive(Component, Clone, Default)]
struct AddField;

#[derive(Component)]
struct AddButton;

// In the page's spawn system, in a row:
// let holder = commands.spawn((Node { flex_grow: 1.0, min_width: px(0), ..default() }, ChildOf(bar))).id();
// commands.queue_spawn_scene(text_input(1.0, bsn! { AddField })).insert(ChildOf(holder));
// commands.queue_spawn_scene(button("Add", Some(Icon::Plus), ButtonKind::Primary)).insert((AddButton, ChildOf(bar)));

fn on_enter(entered: On<TextEntered>, fields: Query<(), With<AddField>>, mut commands: Commands) {
    if fields.contains(entered.entity) {
        commands.trigger(AddItem(entered.value.clone()));
    }
}

fn on_add(
    activate: On<Activate>,
    buttons: Query<(), With<AddButton>>,
    fields: Query<&EditableText, With<AddField>>,
    mut commands: Commands,
) {
    if buttons.contains(activate.entity)
        && let Some(field) = fields.iter().next()
    {
        commands.trigger(AddItem(field.value().to_string()));
    }
}
```

In a test, `trailhead_ui::test_support::press_key(world, field, KeyCode::Enter, Key::Enter)` presses Enter on the field;
`TextEntered` is triggered from an observer, so call `world.flush()` before looking for what it caused (section 6).

### Recipe 6: a settings modal for your app

Modelled on Ticklist's settings (in the apps repo) and Lookout's, `clients/trailhead-lookout/src/settings.rs`. A gear
opens a modal; its body is filled once, when `ModalSlots` arrives, with a section per group of settings. Settings
fixed at spawn are spawned into the section's `rows`; a list that follows data (one toggle per project) is kept there by
`Keyed`, so the modal is never redrawn. What a switch or a pick does is an observer of the app's, as for any control.

```rust
use bevy::prelude::*;
use bevy::ui_widgets::{Activate, ValueChange};
use trailhead_ui::{Keyed, Modal, ModalSlots, open_modal, select, setting, setting_toggle, spawn_settings_section};

/// The settings modal.
#[derive(Component)]
pub struct SettingsModal;

impl Modal for SettingsModal {
    fn title(&self) -> String {
        "My app settings".into()
    }
}

/// The gear that opens it (a `tool_button(Icon::Settings, "Settings", ButtonKind::Plain)` in the sidebar's footer).
#[derive(Component)]
pub struct SettingsButton;

/// The dropdown for how the list is sorted.
#[derive(Component, Clone, Default)]
struct SortSelect;

/// The box the project toggles are kept in.
#[derive(Component)]
struct ProjectToggles;

/// On a toggle: the key of the project it shows or hides.
#[derive(Component, Clone, Default)]
struct ProjectToggle(String);

/// One project's toggle, by key: its label and whether it is shown.
#[derive(Debug, Clone, PartialEq)]
pub struct Shown {
    pub key: String,
    pub label: String,
    pub shown: bool,
}

#[derive(Resource, Default)]
struct Toggles(Keyed<Shown>);

fn on_gear(
    activate: On<Activate>,
    gear: Query<(), With<SettingsButton>>,
    open: Query<(), With<SettingsModal>>,
    mut commands: Commands,
) {
    if gear.contains(activate.entity) && open.is_empty() {
        open_modal(&mut commands, SettingsModal);
    }
}

/// Fills a new modal: a section of fixed settings, and one whose toggles follow the projects.
fn fill(new: Query<&ModalSlots, (With<SettingsModal>, Added<ModalSlots>)>, mut commands: Commands) {
    for slots in &new {
        let list = spawn_settings_section(&mut commands, "List");
        commands.entity(list.section).insert(ChildOf(slots.body));
        let sorts = vec!["Due date".to_string(), "Priority".to_string()];
        let control = bsn_list! { @select(sorts, Some(0), 8) SortSelect };
        let sort = setting("Sort by", "How the list orders what is due.", control);
        commands.spawn_scene(sort).insert(ChildOf(list.rows));

        let projects = spawn_settings_section(&mut commands, "Projects shown");
        commands.entity(projects.section).insert(ChildOf(slots.body));
        commands.entity(projects.rows).insert(ProjectToggles);
    }
}

/// One toggle per project, kept by key while the modal is open (forget `Toggles` on `ModalClosed`).
fn sync_toggles(
    wanted: Res<ProjectsShown>,
    rows: Query<Entity, With<ProjectToggles>>,
    mut kept: ResMut<Toggles>,
    mut commands: Commands,
) {
    let Ok(rows) = rows.single() else { return };
    let wanted = [(rows, wanted.0.iter().map(|p| (p.key.clone(), p.clone())).collect::<Vec<_>>())];
    kept.0.sync(
        &mut commands,
        &wanted,
        |commands, _, p| {
            let own = bsn! { ProjectToggle({ p.key.clone() }) };
            commands.spawn_scene(setting_toggle(p.label.clone(), "", p.shown, own)).id()
        },
        // A switch keeps its own state, which is the one that was just written.
        |_, _, _| {},
    );
}

fn on_toggle(change: On<ValueChange<bool>>, toggles: Query<&ProjectToggle>, mut commands: Commands) {
    if let Ok(ProjectToggle(key)) = toggles.get(change.source) {
        commands.trigger(SetShown { key: key.clone(), shown: change.value });
    }
}
```

`ProjectsShown` (a resource holding `Vec<Shown>`), `SetShown` and the observer of `Picked` for `SortSelect` are the
app's own. A modal with pages spawns `spawn_settings_tabs` into the body, a `tab` per page in its strip, and the page
shown as a `settings_page()` in its pane: spawned when its tab is picked and despawned when another is, its settings and
sections in it. The Trailhead shell's settings modal (`clients/trailhead-shell/src/settings_modal.rs`) is built this way,
and an app in the shell adds pages to it rather than opening a modal of its own.

### Recipe 7: a list a person can reorder

Modelled on the Trailhead shell's rail (`clients/trailhead-shell/src/rail.rs`, `on_reordered` and `order_rail`, with
`Apps::move_app` in `lib.rs`). The order is the app's data, kept where the app keeps things; the list is marked once,
each item keyed by its id; a `Reordered` changes the data, and the nodes follow it in place.

```rust
use bevy::prelude::*;
use trailhead_ui::{ReorderItem, ReorderList, Reordered};

/// The person's order of the things listed, by id. The app keeps it (in the shell, in its `LocalStore`).
#[derive(Resource, Default)]
pub struct Order(pub Vec<String>);

/// The list's node.
#[derive(Component)]
struct Things;

/// Each row, with the id it shows.
#[derive(Component)]
struct ThingRow(String);

// In the page's spawn system, once:
// let list = commands.spawn((Things, ReorderList::vertical(), Node { flex_direction: FlexDirection::Column, ..default() })).id();
// for id in &order.0 { commands.spawn_scene(row_scene(id)).insert((ThingRow(id.clone()), ReorderItem(id.clone()), ChildOf(list))); }

/// A move changes the order, and only the order. Logic: a test triggers `Reordered` with no page.
fn on_reordered(moved: On<Reordered>, lists: Query<(), With<Things>>, mut order: ResMut<Order>) {
    if !lists.contains(moved.list) {
        return;
    }
    if let Some(at) = order.0.iter().position(|id| *id == moved.key) {
        let id = order.0.remove(at);
        order.0.insert(moved.to.min(order.0.len()), id);
    }
}

/// Puts the rows in the order, in place, when it changes.
fn order_rows(order: Res<Order>, lists: Query<(Entity, &Children), With<Things>>, rows: Query<&ThingRow>, mut commands: Commands) {
    if !order.is_changed() {
        return;
    }
    for (list, children) in &lists {
        let mut kept: Vec<Entity> = children.iter().collect();
        let place = |e: &Entity| rows.get(*e).ok().and_then(|r| order.0.iter().position(|id| *id == r.0));
        kept.sort_by_key(|e| place(e).unwrap_or(usize::MAX));
        commands.entity(list).replace_children(&kept);
    }
}
```

Run `order_rows` in `Update` before `UiPlace`, so the rows are in their new places in the frame the drag ends; a list
kept by `Keyed` gets the same from a `sync` with the new order. A row's hint can say how to move it from the keys:
`format!("{name} ({} moves it)", ReorderList::vertical().keys())`. A list with something that must stay put at its
end (the rail's foot) leaves `ReorderItem` off it, and keeps it last in `replace_children`.

## 6. Checking your work

### The lint

`trailhead_ui::lint` checks a crate's sources against the design system from one test. Each rule looks at one line at a
time (comments stripped) and says whether it steps off: `no-feathers` (any `feathers::` or `bevy_feathers`, in tests
and `Cargo.toml` too; apps only), `color-literal`, `palette`, `raw-color` (`BackgroundColor`, `BorderColor`,
`TextColor`), `feathers-color` (feathers' `tokens::` or theme components), `font-size`, `spacing`, `radius`, `stroke`
and `position` (a typed `px(..)` offset). `lint::APP` is the list every app runs; `lint::KIT` is the same without
`no-feathers`, which the kit runs over itself in `src/lint.rs`'s tests.

`lint::check(crate_root, dirs, rules, allowed) -> Vec<String>` walks the `.rs` and `.toml` files under `dirs` and
returns each finding as `path:line: [rule] code (fix)`. A file that has to break a rule goes on the test's allowlist
as an `Allowed { path, rule, why }` (`rule` may be `"*"`), with the reason and the task that will take it off, if one
will. An entry that finds nothing fails too, so take it off when the file is fixed. Never widen a rule's exceptions
inside the rule to get a line through. An app adds rules about its own domain as more `Rule`s (Lookout's
`status-label` in `clients/trailhead-lookout/tests/design_system.rs`).

`lint::redraws(crate_root, dirs, allowed: &[AllowedRedraw]) -> Vec<String>` is the guard for in-place updates
(section 4): it finds each function under `dirs` that despawns what it drew to draw it again (a `despawn_children` or
`despawn_related`, a `despawn` of a loop's `child`, any `despawn` in a function named `redraw*`, a despawn in a loop
followed by a spawn), skipping test modules. A function that opens and closes a surface (a modal's `redraw_form`) goes
on the test's list as an `AllowedRedraw { path, function, why }`; an entry that finds nothing fails, as above. The kit
reads the files, so an app's test needs no file access of its own:

```rust
const REDRAWS: &[AllowedRedraw] = &[];

#[test]
fn the_app_updates_in_place() {
    let found = lint::redraws(env!("CARGO_MANIFEST_DIR"), &["src"], REDRAWS);
    assert!(found.is_empty(), "\n{}", found.join("\n"));
}
```

An app for the shell runs the shell's lint (`trailhead_shell::lint`), which adds to these rules the ones that keep an
app inside its limits; `clients/trailhead-app-example/tests/design_system.rs` shows how it is run, and
`clients/trailhead-shell/GUIDE.md` says what it holds an app to.

The rules read lines, not meaning, so they can be fooled; they catch the slips that build cleanly and look fine in a
dark theme on one screen.

### The tests an app's views carry

- **Logic, headless.** The view model as a pure function of the state, and the observers under `MinimalPlugins`
  (recipe 2's `Press` is triggered straight from a test). An event an observer triggers (a press that becomes the
  app's own event, the kit's `TextEntered` from a key) waits in the command queue: after `world.trigger(..)`, call
  `world.flush()` before checking what it caused.
- **In place.** A test that changes something and checks the entity of the changed text is the same and no other
  entity was spawned or despawned (the example app's third test), and `lint::redraws` over `src`.
- **Theme.** If the app paints colors: that each of its tokens resolves (`Theme::has`) and that
  `Theme::repeated_colors` is empty.
- **The lint**, as above.
- **Scrollbar room.** For each holder of a vertical scroll area, `has_scrollbar_room(&node)`.

### What cannot be checked without a window

No headless test builds what the binary builds: layout, fonts, colors on screen, hover, focus and how a view looks are
seen only in a window. A factory sandbox cannot open one, so a change there says in its summary which parts nobody has
seen yet, and every `clients/**` change gets a person's look before it merges. With a display, run the shell and look
for panics and `bevy_feathers::theme: Theme color ... not found` in the log.
