# Writing an app for the Trailhead shell

This is the guide for writing an app that runs in the Trailhead shell: what an app may be, the contract it is written
against, how it reaches the server, and how it is tested. It lives in the `trailhead-shell` crate so that it matches the
revision you build against: an apps repo that pins `trailhead` at a git revision reads the copy cargo checked out at
that revision (`~/.cargo/git/checkouts/trailhead-*/<rev>/clients/trailhead-shell/GUIDE.md`). If this file and the code
disagree, the code is right and this file has a bug.

Everything an app draws comes from the `trailhead-ui` kit, and how views are built is in the kit's own guide,
`clients/trailhead-ui/GUIDE.md`. Read it before writing a view. The design behind the shell is
`docs/app-platform.md` in the `trailhead` repo.

## 1. What an app is, and what it may not be

An app is:

- **A Rust crate** that exposes one type implementing `trailhead_shell::ShellApp`, which names the app in a `Manifest`
  and adds its plugins to the Bevy app. It is compiled into the shell's binary: a change to an app is a rebuild and a
  relaunch.
- **Built only from the kit.** Its views use `trailhead-ui` and nothing else for UI: no `bevy::feathers`, no hand-made
  controls, no color literals. When it needs something the kit lacks, the kit grows; the app does not work around it
  (the kit's guide, section 1).
- **A client of the Trailhead API only.** It reads the tasks and projects the shell mirrors, makes reads of its own
  through the shell's connection, and writes through the shell's commands. It never touches the server's database or
  `trailhead-core`.
- **Without file, network or process access of its own.** No `std::fs`, `std::net` or `std::process`, and no `unsafe`.
  What it keeps on this machine goes in the `LocalStore`, which the shell writes; what it reads and writes on the
  server goes through the shell's connection. An app holds no credentials.

These limits are enforced by the shell's lint (`trailhead_shell::lint`), which an app runs over its sources from one
test, beside the kit's design-system rules. `clients/trailhead-app-example/tests/design_system.rs` shows how it is run;
copy it. A finding is fixed, not allowed: an `Allowed` entry is for a real exception, with its reason, and one that no
longer finds anything fails the test.

Lookout, the factory's app, is the other app in the `trailhead` repo, and the worked patterns below point into it.
The example app (section 8) is the one to copy.

## 2. The contract: `ShellApp`

```rust
pub trait ShellApp: Send + Sync + 'static {
    fn manifest(&self) -> Manifest;
    fn build(&self, app: &mut App);
    fn build_ui(&self, _app: &mut App) {}
    fn paint(&self, _theme: &mut Theme) {}
    fn link_jobs(&self) -> Vec<Box<dyn LinkJob>> { Vec::new() }
    fn settings_pages(&self) -> Vec<SettingsPage> { Vec::new() }
}
```

`Shell::run` calls `paint` first, when it builds the theme; a headless test, which builds no theme, never calls it.
`ShellPlugin` (which `Shell` wraps) calls the rest in the order below when the Bevy app is built:

| Method | When the shell calls it | What belongs in it |
| --- | --- | --- |
| `manifest` | First, for every app, before any app's `build`. | `Manifest { id: AppId, name: &'static str, icon: Icon }`. `id` is the app's name in code (lowercase, never changed once the app has stored anything): its `ActiveApp` value and its `LocalStore` file are keyed by it. `shell` is taken, and two apps may not share an id. `name` is what a person sees on the rail; `icon` is its button there. |
| `paint` | Before the others, from `Shell::run` (through `ShellPlugin::theme`), once for each app, onto one `Theme::new()` all apps share. | The app's own tokens: `Theme::paint` for colors that encode its data, `Theme::alias` for a token of its own that wears one of the kit's meanings. Nothing else. |
| `link_jobs` | Once, before `build`, and only when the shell is connected. The jobs are handed to the net thread. | The app's own work on the net thread while a control plane is held (section 6). Most apps have none and leave the default. |
| `build` | For every app, after the shell has inserted `Apps`, the `LocalStore` and the `ActiveApp` state, and after the connection's plugin when there is one. Headless tests call it too. | The app's state, resources, messages and logic: the plugins that run under `MinimalPlugins`. Read what the `LocalStore` kept here (`app.world().resource::<LocalStore>()`). Observers of the app's own events go here. |
| `build_ui` | Only in a windowed shell, after every app's `build` and after the shell's rail. | What spawns and draws: `OnEnter`/`OnExit` systems that spawn and despawn the app's pages, the systems that keep them in step, the observers of `Activate`, `Picked` and the like. |
| `settings_pages` | Once, after every app's `build`, headless too; before `build_ui`. | The app's pages in the shell's settings modal, listed under the app's name (below, "Settings pages"). Most apps have none and leave the default. |

The split between `build` and `build_ui` is the kit's rule that logic and drawing are separate plugins: everything an
app decides is testable without a window, and only what draws needs one.

### Settings pages

The window has one settings modal, the shell's: a gear at the rail's foot opens it, as Ctrl and `,` (Cmd on macOS) do
from anywhere. Its pages are listed down its side: the shell's own first, under Trailhead (Appearance: the theme, the
text size and how the window switches apps; Notifications, how the person wants to be told; Connection; Server, the
server's user config; About), then each app's under the app's name, in the order of the rail, whichever app is shown.
An app does not open a settings modal of its own; it adds pages to this one.

```rust
use trailhead_shell::settings::{PageId, SettingsPage};
use trailhead_ui::{ButtonKind, button, setting};

/// The page, so the app's own code can open it: `commands.trigger(OpenSettings { page: Some(COUNT_SETTINGS) })`.
pub const COUNT_SETTINGS: PageId = PageId::new(EXAMPLE, "count");

#[derive(Component, Clone, Default)]
struct ResetButton;

// In `impl ShellApp`:
fn settings_pages(&self) -> Vec<SettingsPage> {
    vec![SettingsPage::new(COUNT_SETTINGS.key, "Count", count_settings)]
}

/// Given the page's box (a `settings_page()`) each time the page is shown.
fn count_settings(In(page): In<Entity>, mut commands: Commands) {
    let control = bsn_list! {
        @button("Reset the count", None, ButtonKind::Normal)
        ResetButton
        Node { align_self: AlignSelf::FlexStart }
    };
    commands.spawn_scene(setting("Count", "Sets the count back to nothing.", control)).insert(ChildOf(page));
}
```

- `SettingsPage::new(key, title, fill)`: `key` names the page among the app's (`PageId::new(app_id, key)`), `title` is
  its tab, and `fill` is any system taking `In<Entity>`, so it reads what it needs (`Res<..>`, the `LocalStore`) to
  spawn the page as it is now. Fill the page with the kit's settings widgets only (`setting`, `setting_toggle`,
  `spawn_settings_section`), never bare toggles or fields.
- The page is spawned when it is shown and despawned when another is picked or the modal closes, so keep what it shows
  current from a system of the app's (in `build_ui`) that finds the page's controls by the app's markers and updates
  them in place; it does nothing while the page isn't there. What a control does is an observer of the app's, checking
  the entity is its own, as anywhere.
- `trailhead_shell::settings::OpenSettings { page: Some(id) }` opens the settings at a page (Lookout's inspector opens
  its Logs page this way), or turns to it while they are open; `page: None` opens them at the page last shown.
  `CloseSettings` closes them. `SettingsShown::page()` says what is open.
- A shell with one app has no rail, and so no gear: an app with a place for one (Lookout's sidebar footer) puts a
  `tool_button(Icon::Settings, "Settings", ButtonKind::Plain)` there with `SettingsGear` on it while
  `Apps::has_rail()` is false. The shell opens the settings on a press of anything marked `SettingsGear`.
- To test a page, use `testing::windowed` with the kit's `ModalPlugin` added (`app.add_plugins(ModalPlugin)`), trigger
  `OpenSettings`, settle, and find the page's controls under the `ShownPage`; the example app's
  `the_settings_page_is_listed_under_the_app_and_its_button_resets_the_count` does this.

## 3. The `ActiveApp` state

The shell decides which app is shown, as the `ActiveApp(pub AppId)` state. It starts on the first app on the rail,
unless the `LocalStore` remembers another one from last time, and the rail sets it when a person presses an app's
button. An
app hangs everything it draws on its own value:

- **A switch is covered, and an app does nothing for it.** A press on the rail marks that app's button at once; the
  shell closes a cover over the app area (everything beside the rail, under the shell's modals, taking the pointer
  while it is up), sets `ActiveApp` once the app shown is fully covered, and opens the cover on the new app's page. How
  it covers is the person's choice in the settings (`trailhead_shell::transition`: `None`, `Fade`, `Wipe`, `Curtain`,
  `Iris`, `Switchback`, `Random`), each way about 160 ms to close and as long to open. What an app can count on: its
  `OnExit` and the next app's `OnEnter` run in the same frame, as before, once the old app's page is hidden, and the
  new page has the frame it is entered in to be laid out under the closed cover before it starts to open. What it can't: that `OnEnter` runs on the frame of the
  press, or that `ActiveApp` is the app whose button is marked during those few frames (`Switching::target()` says
  where a switch is going). Nothing an app keeps should hang on the timing; `trailhead_shell::transition::SwitchApp`
  asks for a switch as the rail does. The first app shown at startup, a desktop that asks for less motion, and `None`
  switch at once.

- **Spawn on entering, despawn on leaving.** What the app draws is spawned in `OnEnter(ActiveApp(MY_ID))` and despawned
  in `OnExit(ActiveApp(MY_ID))`. A switched-away app has no entities on screen; it is drawn again when it is switched
  back to.
- **Gate the systems.** Systems that keep the app's pages in step run with `.run_if(in_state(ActiveApp(MY_ID)))`.
- **Logic keeps running.** Teardown is of what is drawn, not of what is known: the app's resources stay, and its
  logic (in `build`) is not gated, so its state stays current while another app is shown and coming back needs no
  reload.
- **Views are a sub-state.** An app with more than one view makes them a sub-state of `ActiveApp`, so leaving the app
  leaves the view, which tears its page down with no code of the app's own. Added with `add_remembered_view`, the app
  comes back to the view it was on, after a switch to another app and after a restart:

  ```rust
  use serde::{Deserialize, Serialize};
  use trailhead_shell::RememberView;

  #[derive(SubStates, Serialize, Deserialize, Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
  #[source(ActiveApp = ActiveApp(MY_APP))]
  pub enum View {
      #[default]
      List,
      Detail,
  }

  // In build, so it holds headless too:
  app.add_remembered_view::<View>(MY_APP);
  // In build_ui:
  app.add_systems(OnEnter(View::List), spawn_list).add_systems(OnExit(View::List), despawn_list);
  ```

  `add_remembered_view::<V>(app_id)` adds the sub-state (do not also call `add_sub_state`), keeps the view in the
  `LocalStore` under `trailhead_shell::view::VIEW` whenever it changes, and sets the kept one just before the
  transition that shows the app is applied, so it is the first view whose `OnEnter` runs: the default is never drawn
  on the way. One such sub-state an app. A view that needs more than its variant to draw (which project a project page
  shows) keeps that part in the `LocalStore` itself and reads it back in `build`.

  Do not restore a view by hand. Set from `OnEnter(ActiveApp(..))`, the default view has already been entered (Bevy
  creates the sub-state in the same transition, before that schedule runs) and is drawn for a frame; a `NextState`
  set before the sub-state exists is taken and dropped. Lookout's `View` (`clients/trailhead-lookout/src/board/mod.rs`)
  is a plain sub-state that starts on the board each time.
- **Observers are not gated.** A system takes `run_if`; an observer does not, and one added in `build` or `build_ui`
  hears every event of its type while another app is shown. Each observer checks that the entity is its own, by a
  marker only the app spawns:

  ```rust
  fn on_button(activate: On<Activate>, buttons: Query<(), With<PressButton>>, mut commands: Commands) {
      if buttons.contains(activate.entity) {
          commands.trigger(Press);
      }
  }
  ```

  The shell does not yet offer a way to say this once.

## 4. Where to draw

The shell hands an app no parent entity. An app's pages are roots of their own, placed beside the rail:

- **With no sidebar**, a page fills `trailhead_ui::beside_rail()` and carries `BesideRail`:
  `Node { .., ..beside_rail() }`. The example app does this.
- **With a sidebar**, the app spawns a kit `sidebar()` on entering and its pages fill `beside_sidebar()` with
  `BesideSidebar`. Lookout does this.

The kit moves the sidebar and both kinds of page over when a rail exists, and back when it does not. A shell with one
app has no rail, and the app has the whole window (`Apps::has_rail()` says which). Use `Surface::Window` for a page's own background, and give overlays
their layer from `trailhead_ui::layer` (the kit's guide, section 3). While the connection is not live the shell draws
its connect panel along the bottom, over whatever app is shown.

## 5. The `LocalStore`

`LocalStore` is a resource holding each app's own keys and values, kept in one JSON file an app
(`<shell dir>/apps/<app id>.json`) and written back by the shell after a frame in which one changed.

```rust
let kept: u32 = app.world().resource::<LocalStore>().get(MY_APP, "presses").unwrap_or_default(); // in build
store.set(MY_APP, "presses", presses.0);                                                     // when it changes
store.remove(MY_APP, "draft");
```

`get<T: DeserializeOwned>(app, key) -> Option<T>` returns `None` for a key that is missing or no longer reads as a `T`.
`set<T: Serialize>(app, key, value)` writes nothing when the value is already there.

- **What it is for:** what must outlive a switch to another app or a restart of the shell: the view the app was on, a
  filter, an unsent draft, a count of the reader's own. The shell keeps the app last shown there too.
- **When it is saved:** after the frame a value changed in, and once more when the shell restarts onto a newer
  build (the update notice on the rail), so nothing kept is lost to a restart.
- **What it is not for:** the app's data. That lives on the server, as tasks or (later) in the app data store. Losing
  this file loses nothing a person cannot put back in a moment, and a write that fails is logged and tried again on
  the next change, never worth stopping the shell for.
- **When to read and write it:** read it in `build`, into the app's resources; write it in the system or observer that
  changes the thing. Do not write it every frame: `set` compares, but there is nothing to gain.

A shell built without a directory (`ShellPlugin` without `store_in`, as in most tests) holds the values only while it
runs.

The shell keeps its own entry beside the apps' (`shell.json`): the app last shown, the text size a person picked
with Ctrl and `=`, `-` or `0` (`text_size`, in percent), which applies to every app, how the window switches apps
(`transition`, the kind's word: `fade` until another is picked), and the person's order of the apps on the rail
(`rail_order`, their ids, once one has been moved). It is read as the shell is built,
so the first frame is drawn at it. An app neither reads nor writes it: to change the size, trigger the kit's
`ChangeTextSize`, and to lay out at every size, follow the kit guide's rule on UI pixels (section 4).

## 6. Reaching the server

The connection belongs to the shell. The binary tells the shell how to connect (`Shell::connecting`,
`signing_in_with`); the shell runs the net thread, the sync and the mirror, the outbox of commands and the connect
panel, and every app shares them. Without `connecting` a shell has no connection, and the resources and state below do
not exist: an app that reads them cannot be built in such a shell. An app's crate depends on `trailhead-model` (the
records' types) and, to write, `trailhead-client` with its `http` feature (the API's calls), both from the same pinned
`trailhead` repo.

### The `Connection` state

`trailhead_shell::connection::Connection` is a Bevy state: `Offline` (the default: nothing dialed, refused,
unreachable, or no usable login), `Syncing` (dialed, signing in or catching up) and `Live` (signed in, the event stream
open; commands may run). `ConnectionStatus { reason: Option<Reason> }` says why it is not `Live`, in words a person can
act on (`Reason` is `Display`; `Reason::cause()` drops the wait before the next try).

Every control that sends something is disabled while not `Live`, with the reason shown: insert
`bevy::ui::InteractionDisabled` on it and show `status.reason` in a hint or a line beside it. Lookout's sidebar does
this for its New task button (`gate_new_task` in `clients/trailhead-lookout/src/sidebar.rs`) and its connection chip
(`update_chip`). Do not let a press queue work behind a greyed-out control: a `Submit` sent outside `Live` is refused
at once.

The other resources: `Api(ApiAccess { base, token, user, user_id })` once signed in (it stays after a drop; check
`Connection` before using it), `LocalEndpoint` (the shell's own endpoint id) and `Dialed` (the control plane held).

### Reading the mirrored tasks and projects

The shell keeps a mirror of the task engine's records as entities (`trailhead_shell::mirror`). Each holds its record
as its `trailhead-model` type in a `Mirrored<T>` component (`Task`, `Project`, `Comment`, `Claim`, `Agent`, `User`,
`Dependency`, `Attachment`, a picture's metadata, and `Notification`), with its `TrailheadId` and its `Rev`. The sync is the only thing that changes them: a snapshot replaces
the lot, and an event replaces one entity only if it is newer. An app never writes to them.

```rust
use trailhead_model::{Project, Task};
use trailhead_shell::mirror::{Mirrored, rows};

fn rebuild(tasks: Query<&Mirrored<Task>>, projects: Query<&Mirrored<Project>>) {
    let tasks: Vec<Task> = rows(&tasks); // a clone of every task in the mirror
    for project in &projects {
        let _name = &project.0.name;
    }
}
```

`rows(&query) -> Vec<T>` clones every record of a kind. To redraw only when something changed, watch
`Changed<Mirrored<T>>` and `RemovedComponents<Mirrored<T>>`, as Lookout's `sync_rows` does
(`clients/trailhead-lookout/src/projects_page.rs`). The `Mirror` resource finds one entity by kind and id
(`mirror.entity(Kind::Task, id)`), and each change arrives first as a `trailhead_shell::sync::SyncItem` message, which
an app may read to notice an event about something it shows (`task_of(&event)` names the task an event is about).

The mirror is filled only while connected; build the view from it whatever the connection, and show the reason beside
it when not `Live`.

### A read of your own: `Fetch`

What the mirror does not hold (a task's every comment, a search) an app reads with a `trailhead_shell::net::Fetch`,
which runs on the net thread on its own task. `Fetch::new(read)` takes a closure that is given the API client
(`trailhead_client::http::Client`) while the link is `Live` and `None` otherwise, and returns a future of the app's own
`Message`. The answer is written into the world as that message. A read has no retry or idempotency key of its own, so
tag the message (with a sequence number) to tell a current answer from a stale one. Modelled on
`clients/trailhead-lookout/src/detail.rs`:

```rust
use trailhead_shell::connection::{Connection, SendNet};
use trailhead_shell::net::{Fetch, NetCommand};

#[derive(Message, Debug, Clone)]
pub struct DetailReply {
    pub seq: u64,
    pub result: Result<Box<TaskDetail>, String>,
}

fn read(seq: u64, key: String) -> Fetch {
    Fetch::new(move |api| {
        let key = key.clone();
        async move {
            let result = match api {
                Some(api) => api.show_task(&TaskKey(key)).await.map(Box::new).map_err(|e| e.chain()),
                None => Err("not connected".into()),
            };
            DetailReply { seq, result }
        }
    })
}

// To ask, while Live:   net.write(SendNet(NetCommand::Fetch(read(seq, key))));   with `mut net: MessageWriter<SendNet>`
// In build:             app.add_message::<DetailReply>();   and a system reading `MessageReader<DetailReply>`
```

Ask only while `Live`, and ask again on `OnEnter(Connection::Live)` for what was in flight when the link dropped,
since it will not be answered (detail.rs's `DetailPlugin` does both).

The client reads bytes as well as JSON: `api.attachment_bytes(id)` returns a picture's PNG, which comes through the
tunnel intact (`tests/connection.rs` reads a 2 MiB one). Read only what is shown, a few at a time, and keep what was
read by its hash: Lookout's `clients/trailhead-lookout/src/pictures.rs` does, and decodes with the kit's `png_image`.

### A write: the commands

Every write is a command: one REST request, with `If-Match` and an `Idempotency-Key`, through the shell's outbox
(`trailhead_shell::commands`).

- **Send** a `Submit { source: Entity, request: Request }` message. `source` is the control the answer goes back to
  (the button, the inline field). `Request::new(call, if_match)` takes a `trailhead_client::http::Call` (the endpoint
  and its typed body, such as `Call::UpdateTask { key, update }`) and the version the control saw, and makes a fresh
  idempotency key that every retry of this command carries.
- **Check** what a test sent with `request.method()`, the shell's `trailhead_shell::commands::Method` (`Get`, `Post`,
  `Put`, `Patch`, `Delete`; `Method::of(&call)` for a bare `Call`), and `request.call.path()`: an app may not name the
  HTTP client, whose method type `Call::method` returns.
- **Read** `CommandDone { source, id, outcome }` messages and take those whose `source` is yours. `Outcome` is
  `Ok { version }`, `Conflict { current_version, message }` (the version the control saw is stale), `Forbidden(..)`,
  `RateLimited { retry_after, message }` or `Failed(..)` (anything else, with the server's words: a `409` such as
  `invalid_transition` is a refusal, not a stale version, so it is `Failed`).
- **Do not apply a write locally.** A successful write comes back through the event stream into the mirror, and the
  view follows the mirror. On `Ok`, close the form or confirm the inline edit; on anything else, show why beside the
  control and keep what the person typed.

The outbox sends only while `Live`. A command in flight when the link drops is sent again with the same key once
`Live` returns, so a retry the server already applied gets that answer back instead of running twice; a rate-limited
one is retried after its `Retry-After`; after `MAX_ATTEMPTS` failures it is reported `Failed`. It never silently
disappears.

Modelled on `clients/trailhead-lookout/src/inspector/edit.rs` and `author.rs`:

```rust
use trailhead_client::http::{Call, TaskUpdate};
use trailhead_model::TaskKey;
use trailhead_shell::commands::{CommandDone, Outcome, Request, Submit};
use trailhead_ui::{InlineEdited, InlineText};

/// On an `inline_text` showing a task's title: the task, the version shown, and the title before the edit in flight.
#[derive(Component)]
struct TitleOf {
    key: String,
    version: i64,
    old: String,
}

fn on_inline_edited(edited: On<InlineEdited>, mut mine: Query<&mut TitleOf>, mut submit: MessageWriter<Submit>) {
    let Ok(mut title) = mine.get_mut(edited.entity) else { return };
    title.old = edited.old.clone();
    let update = TaskUpdate { title: Some(edited.new.trim().to_string()), ..Default::default() };
    let request = Request::new(Call::UpdateTask { key: TaskKey(title.key.clone()), update }, Some(title.version));
    submit.write(Submit { source: edited.entity, request });
}

fn finish(mut done: MessageReader<CommandDone>, mut texts: Query<(&mut InlineText, &TitleOf)>) {
    for d in done.read() {
        let Ok((mut text, title)) = texts.get_mut(d.source) else { continue };
        match &d.outcome {
            Outcome::Ok { .. } => text.confirm(),
            other => text.fail(title.old.clone(), describe(other)),
        }
    }
}

/// An outcome in words a person can act on.
fn describe(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Ok { .. } => String::new(),
        Outcome::Conflict { message, .. } => format!("It changed since you opened it ({message})."),
        Outcome::Forbidden(message) => format!("Not allowed: {message}"),
        Outcome::RateLimited { message, .. } => format!("Slow down ({message})."),
        Outcome::Failed(message) => message.clone(),
    }
}
```

`TitleOf` is the app's own component; the observer goes in `build_ui` beside the inline text, and `finish` in `build`.
Lookout's fuller `describe` is in `author.rs`. Lookout keeps the edit in flight in a `Pending` on the control, which
also lets it refuse a second edit while the first is unanswered.

### Notifications

The server keeps what a person is told (`docs/app-platform.md`, "Notifications"); the shell mirrors them as
`Mirrored<Notification>` and shows them: the bell at the rail's foot with the count of unread ones, its panel, and on
each app's button the count of the unread ones whose `source` is that app's id. An app draws none of it. What it can do
(`trailhead_shell::notifications`):

- **Hear one arrive.** `NotificationArrived(Notification)` is a message written when a notification arrives raised and
  unread off the event stream while the shell runs, and wasn't held before: never for one loaded with a snapshot, at
  launch or after a long disconnect, and never for a change to one (read, unread, resolved). Read it with a
  `MessageReader` to do something of your own. It is written whatever the person's settings say; what the shell does
  about it is theirs (below).
- **Mark the ones about a task read** when the person opens the task: `commands.trigger(ReadNotificationsAbout {
  task_id })` sends a read for each unread one about it. Lookout does this when its task modal opens a task
  (`clients/trailhead-lookout/src/detail.rs`).
- **Mark one read or unread yourself** with a command, `Call::ReadNotification(id)`, `Call::UnreadNotification(id)` or
  `Call::ReadAllNotifications`; the stream brings the change back to the mirror, as for any write.
- **Open the panel**: `commands.trigger(OpenNotifications)`.
- **Hold a bell of your own** in a shell with one app, which has no rail: put a
  `tool_button(Icon::Bell, BELL_HINT, ButtonKind::Plain)` with `NotificationBell` on it where the app keeps such
  things (Lookout's sidebar footer, beside the gear) while `Apps::has_rail()` is false. The shell puts the count on it,
  rings it and opens the panel when it is pressed.
- `NotificationCounts { shown }` is whether the counts are shown, the bell's and the apps', and follows the person's
  setting; `source_label` names a source as the panel does (an app's icon and name, Trailhead for `server`, Factory
  for `factory`).

**The person's settings** (`trailhead_shell::notification_settings`) decide what the shell does with each one. They are
kept on the server and held in `NotificationPrefs { settings, answered, error }`, read when the connection goes live,
every minute while it is, after a save and when the Notifications page opens; until the server has said them, the
defaults hold (every source interrupts, the sound and the counts on). An arrival does what
`settings.alert(source, now, minute of the day)` says, with the time and zone from the `Clock` resource (the
machine's own; a test inserts `Clock::new(..)` to set it):

| Alert | What the shell does |
| --- | --- |
| `None` (a silent source) | Listed in the panel only, and left out of every count, the bell's and the rail's. |
| `Count` (quiet hours or focus) | Counted; no ring, no sound, no desktop notification. Held for the notice at their end. |
| `Ring` (a bell source) | The bell rings (`RingBells`), with the sound if `settings.sound`; never the desktop. |
| `Interrupt` | With window focus, the bell rings with the sound; without it, the desktop shows it, with its theme's sound if `settings.sound`. |

When quiet hours or focus end, what arrived meanwhile and is still unread is said once: one ring with focus, one
desktop notification ("3 things need you") for those from interrupting sources without. The sound inside the window
is the desktop theme's own message sound, played by the desktop (`canberra-gtk-play`, through the `Sound` resource
`Shell::run` inserts); the shell ships none. The person sets all of it on the Notifications page among the shell's own
settings, and focus from the bell's panel; each saves the settings last read with only that part changed
(`SaveNotificationSettings`, a `PUT` command). An app reads `NotificationPrefs` if it wants to, and changes nothing
of it.

The desktop's own notifications are the shell's too (`trailhead_shell::desktop`): one that arrives while the window
has no focus is shown there, with its link buttons, and taken down once it is read or resolved; several arriving
together are one. An app does nothing for it. A test that wants to see what would be shown inserts a `Desktop` holding
its own `DesktopLink`, as `src/desktop.rs`'s tests do; without one a shell shows nothing there. The same goes for
the sound: a `Sound` holding a `Speaker` of the test's own, as `src/notification_settings.rs`'s tests do.

**Raising one of your own.** Trigger `Notify` from the app's code, and the shell sends it to the server as a command:

```rust
use trailhead_shell::notifications::{Notify, ResolveNotifications};

commands.trigger(
    Notify::new(MY_APP, "ticklist.due", "Three are due today")
        .body("Pay the rent, call the vet, water the plants.")
        .subject("due-today")
        .link("Open the list", "https://example.com/list"),
);
// Later, when the thing no longer needs the person, wherever they dealt with it:
commands.trigger(ResolveNotifications { source: MY_APP, subject: "due-today".into() });
```

- `source` is the app's own id: the panel names the app on it and the rail counts it on the app's button, and the
  person's level for the app decides whether it rings or interrupts. `kind` is a short name of the sort of thing it is
  (`<app>.<what>`). `.task(key)` ties it to a task, which resolves it when the task is finished.
- It comes back on the event stream like any other, so do not show it yourself, and nothing is answered to the app.
- Only while the connection is `Live`: raised otherwise it is dropped (the log says so). An app cannot raise one while
  the window is closed.
- The example app raises one every tenth press and resolves it when the count is reset (`count_press`,
  `reset_count`); its test `every_tenth_press_raises_a_notification_and_a_reset_resolves_it` checks what is sent.

To test them, give a snapshot notifications (`fixture::notification_json`) or feed an event with `"entity":
"notification"` and a kind of `notification.raised`, as `src/notifications.rs`'s tests do.

### Work of your own on the net thread: `LinkJob`

A `LinkJob` is work an app runs on the net thread for as long as a control plane is held, such as a stream of its own
over `trailhead/ui/0`. `start(&mut self, link: HeldLink)` is called on the net thread's runtime each time a control
plane is dialed and returns the future to spawn; that task is aborted when the control plane is replaced or let go
(`released(&mut self)` is called then) and when the net thread ends. A job reports over the app's own channel, not
through the shell: the app makes the channel, gives the sending end to the job from `ShellApp::link_jobs`, and adds
the receiving end as a resource in `build` (Lookout's `factory_link()` in
`clients/trailhead-lookout/src/factory_link.rs` returns both halves). Lookout's factory status and log streams are the
only jobs today. An app that only reads the API uses `Fetch`, not a job.

## 7. Testing an app

An app carries four kinds of test, and the example app has one of each. The helpers are the shell's, in
`trailhead_shell::testing`, behind its `test-support` feature: turn it on for `trailhead-shell` in
`[dev-dependencies]`. It pulls in nothing the shell does not already.

- **Logic, headless.** `testing::headless(ShellPlugin::new().with(MyApp).with(Other))` builds a Bevy app with
  `MinimalPlugins`, `StatesPlugin` and the shell (add `.store_in(dir)` with a temporary directory to test what
  outlives a restart), and settles it. It runs `build` only: no window, no kit plugins. Drive it by triggering the
  app's events and with `show(&mut app, id)`, which switches apps at once, without the cover a press on the rail
  draws (it ends any switch under way first), so the app is shown when it returns; `Other` (id `OTHER`) is an app that
  does nothing, standing in for "another app is shown". A press on the rail in a test (`Activate` on its `OpenApp`
  button) goes through the cover, which runs on real time: set `AppTransition(TransitionKind::None)` first, or step
  the frames with `bevy::time::TimeUpdateStrategy::ManualDuration`.
- **Drawing, in a windowed test app.** `testing::windowed(shell)` is the same with the shell windowed, which also runs
  `build_ui` and the rail, with the assets scenes need. The kit's `UiPlugin` and `ThemePlugin` are not added, so this
  checks what is spawned and kept, not how it looks. `settle(&mut app)` runs the few frames a change takes to land.
- **The lint**, over `src`, `tests` and `Cargo.toml` (section 1), and the kit's `redraws` scan over `src`.
- **Shots**, which draw the app's views to PNGs: below, under "Showing how it looks".

Both test apps carry the shell's connection without its net thread (`TestConnection`): the `Connection` state
(`Offline` until a test sets it with `set_connection(&mut app, Connection::Live)`), `ConnectionStatus`, the mirror and
the outbox. So a test of an app that uses the connection needs no server:

- `trailhead_shell::fixture` makes whole model values and their JSON from the few fields a test cares about (`task`,
  `task_json`, `project`, `comment`, `claim`, `agent`, `user`, `sync_event`, `at`). Short names stand in for ids
  (`"t7"`), each turned into a stable UUID by `id`/`sid`.
- `fill(&mut app, Snapshot { at: 1, projects, tasks, ..default() })` replaces the mirror with those records, as a sync
  would; `feed(&mut app, sync_event(json!({..})))` hands it one event off the stream.
- `sent(&mut app) -> Vec<Submit>` takes what the app sent since the last look, and
  `answer(&mut app, source, Outcome::..)` is the server's answer to the command sent from `source`.

```rust
use trailhead_shell::commands::{Method, Outcome};
use trailhead_shell::connection::Connection;
use trailhead_shell::fixture::{project_json, task_json};
use trailhead_shell::sync::Snapshot;
use trailhead_shell::testing::{Other, answer, fill, sent, set_connection, windowed};

let mut app = windowed(ShellPlugin::new().with(MyApp).with(Other));
set_connection(&mut app, Connection::Live);
let projects = vec![project_json(json!({"id": "p1", "key": "HOME"}))];
fill(&mut app, Snapshot { at: 1, projects, tasks: vec![task_json(json!({"id": "t1", "key": "HOME-1"}))], ..default() });
// ... press something that sends a command ...
let out = sent(&mut app);
assert_eq!((out[0].request.method(), out[0].request.call.path().as_str()), (Method::Patch, "/v1/tasks/HOME-1"));
answer(&mut app, out[0].source, Outcome::Failed("try later".into()));
```

The shell's `test-server` feature (which turns on `test-support`) adds `trailhead_shell::testing::server`: an
in-process Trailhead server behind a local iroh endpoint (`Server::new().await`, `Server::config` for the shell's
`NetConfig`, `endpoint`, `allowed_and_live`), to go live against with a shell told to connect, driven with
`testing::until`, `send`, `state` and `reason`. It brings the task engine, the server and axum into the lockfile, so
only a test of work over the real connection wants it: Lookout's `tests/factory_link.rs` and the shell's own
`tests/connection.rs`.

Two traps in these tests:

- **A marker inserted after `queue_spawn_scene` is added before the scene's components.** The scene is spawned
  later, so on the frame a system first sees `Ref<Marker>::is_added()`, the widget's own components (its `Text`,
  `Notice`, `NavItem`) are not there yet, and the frame they arrive `Marker` is no longer added; a component of the
  scene's own type inserted beside it is overwritten. Spawn with `commands.spawn_scene` instead, which puts the scene
  down when the command runs, so what is inserted after lands after it; or give the widget what it should show when
  spawning it (`notice_with`, the text's own words); or watch what the scene gives (`Ref<Text>::is_added()`), never
  the marker. The kit's guide, section 4, has the rule.
- **An event triggered from an observer lands on the next flush.** An observer that triggers an event of the app's own
  (`commands.trigger(AddItem(..))` from an `Activate` or the kit's `TextEntered`) queues it; a test that triggers the
  press with `world.trigger(..)` and reads what it caused at once must call `app.world_mut().flush()` first (or
  `settle`).

What the example app's tests cover, and why each matters:

- `pressing_adds_to_the_count_and_it_is_kept_for_the_next_shell`: the logic runs with no window, and what is kept in
  the `LocalStore` is there for a second shell built on the same directory.
- `switching_away_despawns_the_page_and_switching_back_shows_the_kept_count`: the page exists only while the app is
  shown, and the app's logic keeps running while it is not (a press while another app is shown still counts).
- `every_tenth_press_raises_a_notification_and_a_reset_resolves_it`: what the app raises and resolves is sent as
  commands, checked with `sent`, without a server.
- `the_settings_page_is_listed_under_the_app_and_its_button_resets_the_count`: the app's page is in the shell's
  settings under its name, filled when shown, and what its control does is kept.
- `pressing_the_button_changes_the_count_line_and_nothing_else_is_respawned`: a change is made in place. The count
  line keeps its entity and the set of entities is the same before and after, which is the kit's rule that a surface
  is spawned once and updated in place. `tests/design_system.rs` runs the kit's `redraws` scan for the same rule.

### Showing how it looks: the shots test

The other tests check what is spawned, not how it looks. `tests/shots.rs` draws the app's views to PNGs, with no
window, display or GPU, so a person or a reviewing agent can see a change. The helper is
`trailhead_shell::testing::shot`, behind the shell's `shots` feature (which turns on `test-support` and pulls in nothing
else); the example app's `tests/shots.rs` is the one to copy.

1. In `[dev-dependencies]`, turn on `shots`: `trailhead-shell = { workspace = true, features = ["test-support", "shots"] }`.
2. Write one ``#[test] #[ignore = "needs a render adapter: run with `mise run shots`"] fn shots()``. Build the shell with
   `shot(ShellPlugin::new().with(MyApp).with(Other), ShotOptions { size: (w, h), ..ShotOptions::default() })` (the
   default is 1280×800 at scale 1.0, dark). It is the shell as `Shell` runs it, with Bevy's renderer drawing into an
   image: the kit's theme worn `ThemePlugin::fixed` (the desktop is never asked), `UiPlugin`, the apps windowed and the
   test connection. `shot_with(shell, options, plugins)` adds plugins of the test's own before it starts.
3. Set each view up from fixtures, as in the other tests: the `ShotApp` derefs to the `App`, so `fill`, `feed`,
   `show`, `set_connection`, `settle`, `world_mut().trigger(Activate { entity })` and setting a resource all work on
   it. Nothing comes from a server or the machine.
4. Take `shot.picture()`, assert it is the size asked for (`picture.size() == shot.options().pixels()`) and holds more
   than a handful of colors (`picture.colors() > 16`, so a blank frame fails), and `picture.save(name)`. The name is
   `<app>-<view>-<theme>` (`example-page-dark`, `lookout-board-light`), with the text size after the view when it isn't
   100% (`lookout-board-150-dark`): `shot.options().name("lookout", "board")` makes it. It is written to `SHOTS_DIR`,
   or `shots` in the target directory.
5. Change the view, or `set_theme(ShotTheme::Light)`, `set_size((w, h))`, `set_text_size(TextSize::nearest(150))` or
   `set_options(..)`, and take the next. Pick a size per view that shows all of it: a board wide enough for its
   columns, a page tall enough for its form. `ShotOptions::text_size` is how big the UI is drawn, as a person sets it
   with Ctrl and `=`; `scale` is the screen's own, which makes the picture larger instead. Take an app's busiest views
   at 150% too, and look at each beside its 100% twin: everything larger, nothing cut off, overlapping or left at the
   old size.

Each view is named, and set up from fixtures, so the same name shows the same thing before and after a change. A
picture is the same byte for byte from run to run on one renderer (time steps by a fixed amount a frame, and the
theme, size and scale are fixed), but a GPU and software rendering differ by a level or two a channel, so a before and
an after to compare must come from the same one. Text an app works out from the clock (`3d ago`) or the machine's time
zone changes with them; `mise run shots` sets `TZ=UTC`.

`mise run shots` runs every crate's shots, one cargo invocation, and says where the PNGs went. They need a render
adapter: a GPU, or Mesa's lavapipe in software with `VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.json` (slower,
and with no display, no Wayland, X11 or D-Bus, needed). CI does not run them yet: it has no adapter.

## 8. The example app, file by file

`clients/trailhead-app-example` is the reference a new app is copied from. Copy the crate, rename it, and change the
parts below.

- **`Cargo.toml`.** Depends on `bevy` 0.20 with only the features it uses (`std`, `bevy_state`, `bevy_ui_widgets`,
  `bevy_scene`), on `trailhead-shell` and on `trailhead-ui`; for tests, `trailhead-shell` with `test-support` and
  `tempfile`. An app that reads the mirror adds `trailhead-model`; one that writes adds `trailhead-client` with
  `http`. No feathers, and nothing that reaches the file system, the network or other processes.
- **`src/lib.rs`**, in order:
  - The id, `EXAMPLE: AppId = AppId("example")`, and the `LocalStore` key `PRESSES`.
  - `Example`, the type given to the shell.
  - `COUNT_SETTINGS`, the `PageId` of its page in the shell's settings.
  - The logic's state and events: `Presses` (a resource), `Press` and `ResetCount` (events any caller can trigger).
    `count_press` raises a notification every `MILESTONE` presses and `reset_count` resolves it (section 6,
    "Notifications").
  - The markers: `ExamplePage` (the page's root, present only while the app is shown), `CountLine`, `PressButton`,
    `ResetButton` (on its settings page).
  - `impl ShellApp for Example`: `manifest` names it with `Icon::Flag`; `build` reads the kept count from the
    `LocalStore` and adds the observers that count a press and reset the count, keeping it; `build_ui` adds the
    observers of the buttons, `spawn_page` on `OnEnter(ActiveApp(EXAMPLE))`, `despawn_page` on `OnExit`, and
    `show_count` gated on the app being shown; `settings_pages` gives its one page, Count, which `count_settings`
    fills with a setting holding the Reset button. It has no `paint` and no `link_jobs`.
  - `spawn_page` builds one page: a root filling `beside_rail()` with `BesideRail` and `Surface::Window`, a title, a
    dim line, the count line and a button, each a kit scene spawned under it with `ChildOf`.
  - `show_count` sets the count line's text in place with `set_text` when `Presses` changes.
  - The tests (section 7), on the shell's `testing` helpers.
- **`tests/design_system.rs`**: the lint over the app's sources, with an empty allowlist, and the kit's `redraws`
  scan with an empty one.

## 9. Adding an app to a shell

A shell is assembled by a binary, in about ten lines. `clients/trailhead-desktop/src/main.rs` is the one in this repo:

```rust
fn main() {
    let Some(config) = NetConfig::from_env() else {
        eprintln!("trailhead-shell: no home directory to keep its endpoint key in");
        std::process::exit(1);
    };
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let kept = config.config_dir.clone();
    Shell::new(kept)
        .connecting(config, std::env::args().nth(1))
        .signing_in_with(var("TRAILHEAD_SHELL_TOKEN"))
        .with(Lookout::new().queueing_under(var("LOOKOUT_QUEUE")))
        .run();
}
```

Adding an app is one more `.with(MyApp)`, after the ones already given: the rail shows the apps in that order until a
person drags one to another place (or moves it with Alt and Up or Down), and the first is shown at the first start.
The person's order is kept (`rail_order` in `shell.json`) and applied over the binary's at the next start: an app no
longer given drops out of it, and a new one goes at the end, in the order given. Everything that follows the rail
follows it too: which way a switch's cover travels, the apps' settings pages, the About page's list.
`trailhead_shell::MoveApp { app, to }` moves an app as a drag does, and `Apps` is always in the rail's order. The
rail's foot (Ask, the update notice, the bell, the gear) is not part of the list. `Shell::new(dir)` keeps what the window remembers on this machine under `dir` (the
theme picked, in `theme`, and each app's `LocalStore`, under `apps/`); `NetConfig::from_env` puts that under `shell` in
Trailhead's config directory, or `TRAILHEAD_SHELL_DIR`. `Shell::run` opens the window (titled Trailhead, with the app
id `trailhead` and its icon), adds Bevy's default plugins, the kit's `ThemePlugin` with every app's `paint` applied and
its `UiPlugin`, then the `ShellPlugin` with the apps, and runs until the window is closed. In the apps repo, its
`shell/` crate is the same binary with that repo's apps, which the server's builder builds after each merge and
`trailhead app` runs. Started that way (`TRAILHEAD_SHELL_BUILD` names the build), the shell shows an update notice at
the foot of the rail when a newer build is ready. Pressing it opens a panel listing the commits the newer build brings,
whose Restart ends `Shell::run` with the restart code the launcher answers; a binary needs nothing of its own for this,
as long as it ends by calling `Shell::run`.

While the connection is live and the server has an apps repo (`[shell] repo`), the foot of the rail also holds an Ask
button, above the update notice; under them is the bell (section 6, "Notifications"), in any shell with a connection,
and under it all the gear that opens the settings (section 2, "Settings pages"). Ask opens a form for asking for a change to one of the apps the server's factory
builds (the projects pointed at the apps repo) or for a new app, which the server turns into a graded task, or a new
project and its first task (`docs/app-platform.md`, "Asking for an app"). Nothing of it is an app's: a shell with one
app has no rail, and so no Ask and no update notice, and its bell and gear are the app's to place.

## 10. Views

Everything about building an app's views (the widgets, colors, sizes, spacing, icons, layers, in-place updates,
scrolling, the rules that came from real bugs, recipes and the design-system lint) is in the kit's guide,
`clients/trailhead-ui/GUIDE.md`.
