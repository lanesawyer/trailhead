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

The split between `build` and `build_ui` is the kit's rule that logic and drawing are separate plugins: everything an
app decides is testable without a window, and only what draws needs one.

## 3. The `ActiveApp` state

The shell decides which app is shown, as the `ActiveApp(pub AppId)` state. It starts on the first app given, unless
the `LocalStore` remembers another one from last time, and the rail sets it when a person presses an app's button. An
app hangs everything it draws on its own value:

- **Spawn on entering, despawn on leaving.** What the app draws is spawned in `OnEnter(ActiveApp(MY_ID))` and despawned
  in `OnExit(ActiveApp(MY_ID))`. A switched-away app has no entities on screen; it is drawn again when it is switched
  back to.
- **Gate the systems.** Systems that keep the app's pages in step run with `.run_if(in_state(ActiveApp(MY_ID)))`.
- **Logic keeps running.** Teardown is of what is drawn, not of what is known: the app's resources stay, and its
  logic (in `build`) is not gated, so its state stays current while another app is shown and coming back needs no
  reload.
- **Views are a sub-state.** An app with more than one view makes them a sub-state of `ActiveApp`, so leaving the app
  leaves the view, which tears its page down with no code of the app's own:

  ```rust
  #[derive(SubStates, Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
  #[source(ActiveApp = ActiveApp(MY_APP))]
  pub enum View {
      #[default]
      List,
      Detail,
  }

  // In build_ui:
  app.add_sub_state::<View>()
      .add_systems(OnEnter(View::List), spawn_list)
      .add_systems(OnExit(View::List), despawn_list);
  ```

  This is how Lookout's `View` works (`clients/trailhead-lookout/src/board/mod.rs`). Coming back to the app starts on
  the sub-state's default view; to come back where it was, keep the view in the `LocalStore` and set it on entering.
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
app has no rail, and the app has the whole window. Use `Surface::Window` for a page's own background, and give overlays
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
- **What it is not for:** the app's data. That lives on the server, as tasks or (later) in the app data store. Losing
  this file loses nothing a person cannot put back in a moment, and a write that fails is logged and tried again on
  the next change, never worth stopping the shell for.
- **When to read and write it:** read it in `build`, into the app's resources; write it in the system or observer that
  changes the thing. Do not write it every frame: `set` compares, but there is nothing to gain.

A shell built without a directory (`ShellPlugin` without `store_in`, as in most tests) holds the values only while it
runs.

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
`Dependency`), with its `TrailheadId` and its `Rev`. The sync is the only thing that changes them: a snapshot replaces
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

### A write: the commands

Every write is a command: one REST request, with `If-Match` and an `Idempotency-Key`, through the shell's outbox
(`trailhead_shell::commands`).

- **Send** a `Submit { source: Entity, request: Request }` message. `source` is the control the answer goes back to
  (the button, the inline field). `Request::new(call, if_match)` takes a `trailhead_client::http::Call` (the endpoint
  and its typed body, such as `Call::UpdateTask { key, update }`) and the version the control saw, and makes a fresh
  idempotency key that every retry of this command carries.
- **Read** `CommandDone { source, id, outcome }` messages and take those whose `source` is yours. `Outcome` is
  `Ok { version }`, `Conflict { current_version, message }` (the version the control saw is stale), `Forbidden(..)`,
  `RateLimited { retry_after, message }` or `Failed(..)`.
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

An app carries three kinds of test, and the example app has one of each.

- **Logic, headless.** Build a Bevy app with `MinimalPlugins` and `StatesPlugin` (the test adds what
  `MinimalPlugins` lacks; the app's plugins never add it for the test's sake), add
  `ShellPlugin::new().with(MyApp)` (and `.store_in(dir)` with a temporary directory to test what outlives a restart),
  update once, and drive it by triggering the app's events and setting `NextState<ActiveApp>`. No window, no kit
  plugins: `build` only.
- **Drawing, in a windowed test app.** The same with `.windowed()`, which also runs `build_ui` and the rail; it needs
  `AssetPlugin`, `ScenePlugin`, and the `Font` and `Image` assets initialised, and a few updates for scenes to settle.
  The kit's `UiPlugin` and `ThemePlugin` are not added, so this checks what is spawned and kept, not how it looks. A
  second app that does nothing stands in for "another app is shown". The shell does not ship these helpers yet, so the
  example app writes its own (`headless`, `windowed`, `settle`, `show` and `Other` in its `tests` module); copy them.
- **The lint**, over `src`, `tests` and `Cargo.toml` (section 1).

For an app that uses the connection, the shell's `test-support` feature (turn it on for `trailhead-shell` in
`[dev-dependencies]`) adds:

- `trailhead_shell::fixture`: whole model values and their JSON from the few fields a test cares about (`task`,
  `task_json`, `project`, `comment`, `claim`, `agent`, `user`, `sync_event`, `at`). Short names stand in for ids
  (`"t7"`), each turned into a stable UUID by `id`/`sid`. Write them into the app as `SyncItem` messages to fill the
  mirror without a server.
- `trailhead_shell::testing`: an in-process Trailhead server behind a local iroh endpoint (`Server::new().await`,
  `Server::config` for the shell's `NetConfig`), and helpers to drive the app until it gets somewhere (`until`,
  `allowed_and_live`, `send`, `state`, `reason`). Lookout's `tests/factory_link.rs` uses it.

A headless test of logic that reads `State<Connection>` without a connection adds `init_state::<Connection>()` and
sets it, as Lookout's `detail.rs` tests do.

What the example app's tests cover, and why each matters:

- `pressing_adds_to_the_count_and_it_is_kept_for_the_next_shell`: the logic runs with no window, and what is kept in
  the `LocalStore` is there for a second shell built on the same directory.
- `switching_away_despawns_the_page_and_switching_back_shows_the_kept_count`: the page exists only while the app is
  shown, and the app's logic keeps running while it is not (a press while another app is shown still counts).
- `pressing_the_button_changes_the_count_line_and_nothing_else_is_respawned`: a change is made in place. The count
  line keeps its entity and the set of entities is the same before and after, which is the kit's rule that a surface
  is spawned once and updated in place.

What no test here can check is how the app looks: see the kit's guide, section 6.

## 8. The example app, file by file

`clients/trailhead-app-example` is the reference a new app is copied from. Copy the crate, rename it, and change the
parts below.

- **`Cargo.toml`.** Depends on `bevy` 0.20 with only the features it uses (`std`, `bevy_state`, `bevy_ui_widgets`,
  `bevy_scene`), on `trailhead-shell` and on `trailhead-ui`; `tempfile` for tests. An app that reads the mirror adds
  `trailhead-model`; one that writes adds `trailhead-client` with `http`; its tests add `trailhead-shell` with
  `test-support`. No feathers, and nothing that reaches the file system, the network or other processes.
- **`src/lib.rs`**, in order:
  - The id, `EXAMPLE: AppId = AppId("example")`, and the `LocalStore` key `PRESSES`.
  - `Example`, the type given to the shell.
  - The logic's state and event: `Presses` (a resource) and `Press` (an event any caller can trigger).
  - The markers: `ExamplePage` (the page's root, present only while the app is shown), `CountLine`, `PressButton`.
  - `impl ShellApp for Example`: `manifest` names it with `Icon::Flag`; `build` reads the kept count from the
    `LocalStore` and adds the observer that counts a press and keeps the count; `build_ui` adds the observer of the
    button, `spawn_page` on `OnEnter(ActiveApp(EXAMPLE))`, `despawn_page` on `OnExit`, and `show_count` gated on the
    app being shown. It has no `paint` and no `link_jobs`.
  - `spawn_page` builds one page: a root filling `beside_rail()` with `BesideRail` and `Surface::Window`, a title, a
    dim line, the count line and a button, each a kit scene spawned under it with `ChildOf`.
  - `show_count` sets the count line's text in place with `set_text` when `Presses` changes.
  - The tests (section 7).
- **`tests/design_system.rs`**: the lint over the app's sources, with an empty allowlist.

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

Adding an app is one more `.with(MyApp)`, after the ones already given: the rail shows the apps in that order, and the
first is shown at the first start. `Shell::new(dir)` keeps what the window remembers on this machine under `dir` (the
theme picked, in `theme`, and each app's `LocalStore`, under `apps/`); `NetConfig::from_env` puts that under `shell` in
Trailhead's config directory, or `TRAILHEAD_SHELL_DIR`. `Shell::run` opens the window (titled Trailhead, with the app
id `trailhead` and its icon), adds Bevy's default plugins, the kit's `ThemePlugin` with every app's `paint` applied and
its `UiPlugin`, then the `ShellPlugin` with the apps, and runs until the window is closed. In the apps repo, its
`shell/` crate is the same binary with that repo's apps.

## 10. Views

Everything about building an app's views (the widgets, colors, sizes, spacing, icons, layers, in-place updates,
scrolling, the rules that came from real bugs, recipes and the design-system lint) is in the kit's guide,
`clients/trailhead-ui/GUIDE.md`.
