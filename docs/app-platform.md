# Trailhead as a shell of apps

> **Design note.** The direction was decided on 2026-11-19 (TRAIL-1572) and is in `docs/PLAN.md` as phase 7; nothing here is built. Statements are marked **Leaning** (Lane agreed in conversation; expect it to move as it is built), **Proposal** (not yet agreed) or **Open**.

## The idea

Trailhead becomes one desktop app, a shell, that holds several apps and can be asked to grow new ones. You switch between a todo list, the factory's own app, and whatever else you have asked for. Asking for an app, or for a change to one, hands the work to the factory, which builds it; the shell then offers the update and reloads.

The factory is what makes this practical: it already turns a spec into a checked, reviewed, merged change. What is new is a shell that hosts apps, a place for those apps to live, and somewhere for their data.

## Names

| Thing | Name | Status |
| --- | --- | --- |
| The shell | Trailhead | Leaning |
| The factory's app inside it | Lookout (what the whole Bevy client is called today) | Leaning |
| The UI kit | `trailhead-ui`, unchanged | Leaning |
| How you start it | `trailhead app`, the only command a person types. It is a launcher in the CLI that runs the newest built shell and falls back to the previous one | Built (TRAIL-1578) |
| The built shell | A file the builder writes and the launcher manages (`shell/builds/<rev>/trailhead-shell` in Trailhead's data directory), never run by hand. It is separate from the CLI because it is rebuilt on every app change, and the CLI, the server and the launcher that does the fallback must not be | Built (TRAIL-1578) |
| The todo app | Ticklist, a climber's list of routes to send (from [naming.md](factory/naming.md)) | Leaning |

## What an app is

- A Rust crate that exposes a Bevy plugin and a manifest (name, icon, the data collections it uses). **Leaning.**
- Compiled into the shell binary. A change means a rebuild and a relaunch: the shell shows "your update is ready", and the button restarts it with its state restored. **Built (TRAIL-1578):** a button at the foot of the rail opens a panel of what the newer build brings (TRAIL-1605), whose Restart saves the `LocalStore` and restarts onto the newest build. Loading `bsn!` scenes from files, once Bevy has it, may later let views reload without a rebuild.
- Built only from `trailhead-ui`. A lint in the apps repo refuses any other UI code, so an app that needs something the kit lacks cannot work around it. **Leaning.**
- A client of the API only, like Lookout today: it never touches the server's database.
- Limited in what it may reach: no file, network or process access of its own, enforced by lint. **Leaning** (see [Trust, grades and undo](#trust-grades-and-undo)).

Lookout always ships with the shell: it is how asked-for work is watched, answered and reviewed.

## The contract between shell and app

Every app is written against it, so a change to it later is a change to every app. A spike settled its shape (TRAIL-1575): the `trailhead-shell` crate (`clients/trailhead-shell`), with Lookout and the example app (`clients/trailhead-app-example`) both behind it. **Built** below means the spike has it and its tests hold it.

**An app is a `ShellApp`. Built.** One type per app crate, with these parts:

| Part | What it is |
| --- | --- |
| `manifest` | Its id, the name a person sees, and its icon on the rail |
| `build` | Its state, resources and logic. Runs headless, so an app is tested without a window |
| `build_ui` | What spawns and draws. Only a windowed shell calls it |
| `paint` | Its own tokens on the theme all apps share: the colors that encode its data |
| `link_jobs` | Its own work on the net thread while a control plane is held, as `LinkJob`s (Lookout's factory status and log streams). The shell takes them once, before `build`; they report over the app's own channels. Empty by default |
| `settings_pages` | Its pages in the shell's settings (Lookout's Factory and Logs), each a title and a system that fills the page when it is shown. The shell takes them once, after `build`. Empty by default |
| `collections` | What it keeps in the app data store: its `collections.toml` and the collections it reads, each with the Rust type its records deserialize into. The shell takes them once, before `build`, and mirrors them. None by default (TRAIL-1581) |

**The shell says which app is shown, and an app hangs everything on that. Built.** The shown app is the `ActiveApp` state. An app spawns what it draws on entering its own value and despawns it on leaving, runs its systems only in it, and makes its own views a sub-state of it. Lookout's `View` is such a sub-state, so switching away leaves the view, which tears its panel down with no code of Lookout's own.

**A switch is covered. Built (TRAIL-1619).** Since two apps are never drawn at once, the shell covers a switch rather than moving one app out as the other comes in (`transition.rs`, drawn by `cover.rs`): pressing an app on the rail marks it at once, a cover closes over the app area beside the rail (about 160 ms, eased, in real time), `ActiveApp` changes while it is fully closed, and it opens on the app picked (about 160 ms). How is the person's choice on the Appearance page, where each way plays in a small preview drawn by the same code: None, Fade, Wipe (in the direction of travel down or up the rail), Curtain, Iris, Switchback, or Random (another each time). It is kept in the shell's `LocalStore` entry (`transition`); a desktop that asks for less motion switches at once. A switch asked for while one is under way retargets it, so there is only ever one cover. An app does nothing for this and sees only that its `OnEnter` runs a moment after the press.

**Teardown is of what is drawn, not of what is known. Built.** A switched-away app has no entities on screen, but its logic keeps running: Lookout's mirror stays in step with the server while another app is shown, so coming back needs no reload. Keeping an app's drawing alive is the later fix if rebuilding it is slow enough to annoy.

**The rail is in the person's order. Built (TRAIL-1621).** Dragging an app's button up or down the rail (or Alt and Up or Down on it) moves it there, on the kit's reorderable list (`trailhead_ui::ReorderList`): a press that moves a few pixels lifts the button and the others slide aside, and the release that drops it never also opens the app. The order is kept in the shell's `LocalStore` entry (`rail_order`) and applied at the next launch over the order the binary gives, an app no longer given dropped and a new one added at the end; the first app shown, the direction a switch's cover travels and the apps' settings pages follow it. The plus under the apps and the rail's foot (Ask, the update notice, the gear) stay put.

**A place to draw is the window beside the rail. Built.** The shell hands over no parent entity. An app fills the kit's `beside_rail`, or has a kit `sidebar` and fills `beside_sidebar`; the kit moves both over when a rail exists. A shell with one app has no rail and the app has the whole window, so Lookout alone looks as it did.

**Local storage. Built.** `LocalStore` is a small JSON file per app on the client's disk, for what must outlive a switch or a restart: the view it was on, an unsent draft. An app reads it when it is built and writes to it when the thing changes; the shell saves the file after that frame. The shell keeps the app last shown there. It is not the app's data, which lives on the server.

**One connection, sync and mirror, the shell's. Built (TRAIL-1577).** The shell owns the connection to the server (`net.rs`, the net thread; `connection.rs`, the `Connection` state every app gates its commands on), commands (`commands.rs`, the outbox), and the sync and mirror (`sync.rs`, `mirror.rs`): the task engine's records as entities keyed by kind and Trailhead id, replaced when a newer version arrives, which Lookout and later Ticklist both read. The binary tells the shell how to connect (`Shell::connecting`, `signing_in_with`); without that a shell has no connection. While it isn't live the shell draws the panel that says why (`connect.rs`), over whatever app is shown. An app reads the API through the net thread with a `Fetch`, whose answer comes back as the app's own message, and sends writes as commands; work of its own over the link is a `LinkJob`. Its tests run on the shell's `testing` helpers (the `test-support` feature): a headless or windowed test app holding the connection's pieces without its net thread, filled from fixtures; `testing::server` (the `test-server` feature) is an in-process server to go live against. The app data store has a second mirror of the same shape, keyed by app, collection and record (TRAIL-1581, below).

**The window is the shell's. Built (TRAIL-1577).** `Shell` opens it, titled Trailhead with the app id `trailhead`, and sets its icon; it owns the theme and its file and the kit's plugins, and the text size a person sets with Ctrl and `=`, `-` or `0`, which scales every app's whole UI and is kept in the shell's own `LocalStore` entry (`text_size`) and drawn at from the first frame of the next launch (TRAIL-1622). What it keeps on this machine (the endpoint key, the control plane dialed last, the theme, each app's `LocalStore` under `apps/`) is in `shell` under Trailhead's config directory, or `TRAILHEAD_SHELL_DIR`. `clients/trailhead-desktop` is the assembly: its binary, `trailhead-shell`, reads the environment, gives the shell its apps and runs it, as the apps repo's `shell/` will.

**Settings are the shell's. Built (TRAIL-1587, TRAIL-1620).** One modal, "Settings", opened from the gear at the foot of the rail, with Ctrl and `,` (Cmd on macOS), or `OpenSettings { page }` from an app's own code, lists its pages down its side (`settings.rs`, `settings_modal.rs`, `settings_pages.rs`, on the kit's `spawn_settings_tabs`). The shell's own come first, under Trailhead: Appearance (the theme, and the text size with every step shown), Connection (the server, the state and why it isn't live, who it signed in as, this window's endpoint id and the `allow` command, Re-dial and Disconnect), Server (below) and About (the build, whether a newer one is ready, the apps and their ids, where the shell keeps its files). Appearance also holds how the window switches apps, as a row of previews that play (TRAIL-1619). Then each app's pages, from `ShellApp::settings_pages`, under the app's name in the order of the rail, whichever app is shown. A page is spawned when it is shown and despawned when another is picked. A shell with one app has no rail, so an app with a place for a gear (Lookout's sidebar footer) puts one there, marked `SettingsGear`, while `Apps::has_rail` is false. Server, listed with a connection, edits what the server's user `config.toml` says (`server_page.rs`, its logic in `server_settings.rs`): the window builds (`[shell]`: the apps repo, the build profile, the jobs, how often the builder looks, and what it is building now), the factory (`[factory]`: the repos it runs, on the kit's `value_list`, whether it runs, and the usage pauses) and the file's path. Each section is edited on the page and sent with its own Save, only what changed; the page shows what the server answered, never the edit, and a setting at its default says so. The server checks the file the change would leave as it checks one at start and refuses it in its words, which the section shows with what was typed kept; a file changed on disk since the page read it is read again. The server reads the file only when it starts, so a saved change waits for a restart: the page names what waits and the command that restarts it (`trailhead server restart`, or Ctrl-C and the same command for one run at a terminal). There is no restart button: a restart drains the factory's runs. The API is `GET` and `PATCH /v1/config`, for a person only (`docs/factory/operating.md`, "Editing the user config from the window").

**Reading other apps' data. Leaning.** An app's manifest names the collections of other apps it reads, and tasks if it reads those. Sharing is the usual case; an app can also mark a collection as its own alone. Adding a line is a grade 3 change. Search and quick capture across apps belong to the shell.

What the spike found that the shell still has to take on:

- **Coming back to an app starts on its first view.** Built (TRAIL-1596): `add_remembered_view` keeps an app's view sub-state in the `LocalStore` and enters the kept view first. Lookout's board view does not use it yet.
- **An observer is not gated by the state.** A system takes `run_if(in_state(..))`; an observer added in `build` hears events while another app is shown, so each one checks that the entity is its own. The shell should offer a way to say it once.
- **Apps need the shell's test helpers** (a windowed test app, a stand-in second app). Built (TRAIL-1596): `trailhead_shell::testing` behind `test-support`, which no longer brings the server; that is `test-server`.

## The kit

The kit has to become a small, capable library, not a pile of one-off widgets. The rule Lookout already follows carries over: one component, built once, and a view that needs more makes the widget grow.

- When an app needs something the kit lacks, that is a task in the kit's queue (TRAIL-888) in the `trailhead` repo, reviewed by a person, and the app's task waits on it. **Leaning.**
- The apps repo pins the `trailhead` repo at a revision. Taking a newer kit is its own task.
- **How agents learn the kit. Leaning.** Through the kit's own documentation, written for agents and kept in the crate so it always matches the pinned revision, with an MCP server over it for looking widgets up. No gallery app of every widget.

**Drift between users. Open, leaning "pin, and fork if you must".** Each user's shell is built from their own pin, so two users never have to agree on a kit version. The upstream kit stays curated. Someone who cannot wait for a widget points their apps repo at their own fork (one `[patch]` line) and may send the change back. Instructions for agents on bringing a fork up to date with upstream are wanted, later. Until other people use this, Lane's asks go straight to the upstream kit, and the question is settled when the factory is opened to others (TRAIL-114).

## Repos and git

**Leaning: two repos.**

- **`trailhead`** (this one): the engine, the factory, the shell crate, the kit, Lookout, and a small example app that CI builds and that new apps are scaffolded from.
- **An apps repo**, one per user, private: every app that user has, in one cargo workspace.

```
apps/<name>/         one crate per app
connectors/<name>/   one crate per connector (later)
shell/               the binary: registers the apps, about ten lines
Cargo.toml           depends on the trailhead repo, pinned to a rev
.trailhead.toml      the factory's settings and path rules
```

One repo, not one per app, because the shell is a single binary that needs one list of apps and one `Cargo.lock`, a kit bump is one change, and the sandboxes keep one warm build cache.

- **A project per app.** Asking for an app creates a Trailhead project for it, with a brief and its first tasks, scoped to `apps/<name>/`. **Built (TRAIL-1595):** a project says which repo and path it builds in through `metadata.factory` (`{"repo": "owner/name", "path": "apps/<name>"}`, set by a person), and that repo's factory works its graded tasks from the next poll, telling the agents to stay inside the path ([operating.md](factory/operating.md), "Pointing a project at a repo"). **Built (TRAIL-1580):** asking for a new app from the window creates its project, pointed at `apps/<id>`, with a brief and its first task (see phase 5).
- **GitHub, with the code host that exists.** Changes are pull requests that merge themselves when they stay inside one app's directory and pass checks and agent review. Nobody has to open them. A code host over a plain local repo is a later option.
- **Review is trying it.** The person's gate on an app change is using the new build and keeping or rejecting it, not reading a diff. Rejecting is a revert and a rebuild.
- **The builder. Built (TRAIL-1578).** After a merge, a builder in the server pulls the apps repo and builds the shell, the running shell hears of it, and the launcher keeps the previous binary to fall back to. As built:
  - `[shell] repo` in the user config names the apps repo's checkout; without it there is no builder. The builder (`crates/trailhead-builder`) runs in the server whether or not the factory does, and a failure in it never stops the server.
  - Every `poll_secs` (60) it `git ls-remote`s the checkout's `origin` at its default branch, and builds a head it has not built, failed on or been told is bad, in a clone of its own (`<data dir>/shell/src`), with `cargo build --locked --profile <profile> -p <package> -j <jobs>` (release, 3 jobs) into a target directory of its own. One build at a time; a head that moves during a build is built after it.
  - Under `<data dir>/shell`: `builds/<rev>/trailhead-shell` for the current build and the one before it (older ones are removed), `state.json` (current, previous, building, the last failure with the tail of cargo's output), `build.log` and `bad`. `GET /v1/shell/build` and `trailhead app status` show them.
  - The running shell hears of a build by asking `GET /v1/shell/build` every 30 seconds while connected, not from the event stream, which carries the task engine's stored and undoable events. It knows its own build from `TRAILHEAD_SHELL_BUILD`, which the launcher sets, so the binary is not rebuilt to know it; started any other way it shows no notice.
  - It asks with `?since=<its build>`, and the answer's `changes` are the subjects of the commits since then (`git log` in the builder's clone, never while it checks the clone out), the newest 30 with `changes_total` for the rest (TRAIL-1605). The rail's button opens a panel listing them, each subject's `type(scope)` as a badge and its task key at the end, with Restart and Later. The answer's `repo` is the apps repo as `owner/name` when its `origin` is on GitHub, which the settings' About page links this build's commit by (TRAIL-1694).
  - `trailhead app` runs the newest good build. A shell that exits with the restart code (75, `trailhead_model::shell::RESTART_CODE`) is run again on the newest build; one that exits with an error within ten seconds of starting is marked bad (in `bad`, which the builder reads too) and the build before it is run.

The personal todo app lives in the apps repo, not in `trailhead`: it is the app Lane will keep asking to change, which is the point of the whole design, and upstream should not take that churn. **Leaning.**

## Data

**Leaning.** Everything stays on one server on the user's own machine, reached over iroh as Lookout reaches it now. No cloud server.

- **Tasks stay as they are.** The todo app is a client of the task API that already exists.
- **Other app data goes in a generic store in the server.** An app declares typed collections in its manifest; the server validates each write, records it in an event log so undo and revert cover it, and streams changes to clients. Apps ship no server code, so generated code never runs in the process that owns the tasks.
- **One SQLite file per app. Leaning.** The server opens each app's file beside its own; an open SQLite file costs little, and the tasks database is untouched by anything an app does. Removing, backing up or exporting an app is one file, and apps never wait on each other's writes. Each file carries its own event log, written in the same transaction as the data, because SQLite cannot commit atomically across files in WAL mode; "revert everything this agent did since noon" then runs over each file in turn. Only the server opens these files; apps and connectors go through the API.
- **Agents reach the same data** through MCP, so an agent can read and write an app's records as it does tasks.
- **Connectors** sync with outside services (CardDAV, CalDAV). Each is a separate process the server supervises, acting through the API under its own agent identity, so its writes are attributed and revertable and a crash stays outside the server.
- **Principle 6 narrows.** Trailhead stays the source of truth for tasks. App data may mirror an outside source through a connector.

- **Backups. Leaning; needed before Lane moves his todo list over.** The tasks database and every app's file, copied on a schedule to somewhere off the machine. Not designed yet.
- **No list while the server is off. Accepted for now.** [ui.md](factory/ui.md)'s rule stands: nothing works without a live connection, so Ticklist is unavailable when the home machine is off or unreachable. A read-only cache is the fix if that becomes a nuisance.

**Tasks or records. Proposal.** Many apps are a list of items, and the engine already allows a task nobody works (review.md: an ungraded task, "like an item on a shopping list"). The rule: something that gets done is a task, in a project of its own, and Ticklist shows it; something that is kept (a contact, a bookmark) is a record in the store. Records do not become tasks, because tasks carry status, claims and the ready queue, which a kept thing has no use for. The kit has one list widget that both are shown with.

How the store and connectors work is settled in the next section.

## The app data store and connectors

**Decided (2026-11-22, with Lane), not built.** The first app to need it is Cairn (CAIRN-1), which ranks a person's work from GitHub pull requests and Jira tickets. Phase 6 is the store (TRAIL-1581), phase 7 connectors (TRAIL-1582).

### The store

- **The app is the data boundary.** Each app has one SQLite file, `apps/<app id>.db` in the server's data directory, opened only by the server. It holds the app's records and their event log, written in one transaction. Apps may later share a collection; nothing does yet.
- **A collection's shape is declared in a file,** `apps/<id>/collections.toml` in the apps repo, not in the app's Rust: the server has to check writes and a connector has to know the shape, and neither links the app. The server reads the files from the checkout its builder last built, so a shape goes live with the window that uses it.

  ```toml
  [collections.snoozes]
  fields = { item = "string", until = "timestamp" }

  [collections.pull_requests]
  from = "github.pull_requests"
  ```

  A field is `string`, `integer`, `number`, `boolean`, `timestamp`, `day`, `url` or `json`; `?` after it makes it optional and `[]` a list. A write with a missing, unknown or mistyped field is refused.
- **A shape changes by addition only:** a new collection or a new optional field. Anything else is a new collection, with the records moved through the API so the move can be undone. A real migration system may come later.
- **A record** has an id, its collection, its fields, a version, and optionally a **key**: the name something outside gave it (`lanesawyer/trailhead#123`), unique in its collection. Writing by key means "make the record with this key look like this", so writing the same thing twice leaves one record, and a write that changes nothing records no event and moves no version.
- **Every change is an event** in the app's file, with the same batch, actor and before and after as the task log, so undoing an event or a batch and reverting an agent work the same way and run over each file in turn. Old events are archived as the task log's are.
- **Something done elsewhere is a record, not a task.** A pull request or a Jira ticket is finished in GitHub or Jira, so mirroring it as a task would be the sync principle 6 refuses. Tasks stay what is done in Trailhead.
- **Who may reach it.** A person, everything. A token carries the apps it may reach, as it carries projects; one that names none reaches no app data, so the factory's workers and reviewers never see it.
- **The API** is under `/v1/apps/{app}`: its collections; records listed, read, created, changed and removed, and written by key; a batch of several writes; its events, as a list and as a stream; undo. MCP gets the same for an agent whose token reaches the app.
- **In the window,** the shell keeps a second mirror beside the tasks': per app, a snapshot of its records and then its event stream, with a cursor per app since each file numbers its own events. An app reads its records from the mirror as typed values and writes through the shell's commands, so it still never opens the network. The shell's `testing` module gives an app's tests records from fixtures. Built: order of work, item 3.

### Connectors

- **A connector is a small Rust program under `connectors/<id>/` in the apps repo,** which the path rules already put at grade 4, so a person reads every one. It has no UI and is the only code of an app repo that may open the network. The builder builds it beside the window.
- **One sync per outside service, routed to the apps that want it.** A connector knows nothing of apps. It declares **feeds** (`github.pull_requests`), each with a shape, and each run hands the server the feed's whole current set, keyed. An app's collection says `from = "github.pull_requests"` and takes the feed's shape; the server writes the set into every such collection, each in its own file, adding, changing and removing what differs. A collection filled from a feed is read-only to everything else.
- **`connector.toml`** beside the code declares the feeds, the hosts it may reach, the settings it takes (which repos, the Jira site, who "me" is), the secrets it needs by name, and how often it runs.
- **The connector kit, `trailhead-connector`, is a crate in the `trailhead` repo.** It gives a connector its settings and secrets, an HTTP client that refuses a host the connector did not declare, paging and backoff, a small saved state (a cursor, an ETag), the call that hands over a feed, and a client that replays recorded answers so a connector is tested in a sandbox with no network and no token. A connector is then "fetch and map". The host check is in the library, not the OS; a container behind the factory's egress proxy is the stronger later step.
- **The server runs it on a schedule,** one run at a time with a time limit, as a process of its own acting as its own agent (`connector/<id>`) with a token that may only hand over its own feeds. A crash costs one run. "Sync now" runs it at once.
- **Polling, not push.** GitHub's and Jira's webhooks need an address the internet can reach, and the server has none. A poll that asks only for what changed is cheap, and a set that matches what is stored writes nothing.
- **In only.** Nothing is written back to an outside service.
- **Merging doesn't start one.** A person turns a connector on and gives it its secrets, on the server's machine (`trailhead connector login <id>`), kept in the credential store the factory's secrets are in. Whether it is on and its settings are in the server's settings table, so neither needs a restart.
- **One Connectors page in the window's settings** draws each connector's settings from what it declared, with its last run, its last error and Sync now; a connector that keeps failing raises a notification.
- **The apps repo's guides say all of this,** so a planner plans with a store and connectors and files a connector as its own grade 4 task.

### Order of work

1. The store in the engine: files, declarations, records, keys, events, undo. **Built (TRAIL-1581), engine only** (`crates/trailhead-core/src/apps`, its types in `trailhead_model::app_data`):
   - Each app's file is `apps/<app id>.db` beside the tasks database (`Store::with_app_dir` elsewhere), with its own migrations (`migrations/app/`). `Store::register_app` creates it and keeps the declaration in it; the file belongs to the workspace that created it.
   - A field's type is `kind`, `kind[]`, `kind?` or `kind[]?` (`kind?[]` is refused: a list's items are never missing). An optional field given as `null` is the same as one left out, and is not stored. A `url` is a scheme, `:` and the rest, with no spaces. A fed collection's fields are its feed's, filled in by `Declaration::with_feeds` before it is registered.
   - A shape that changes other than by addition is taken while the changed collection holds no records, and refused once it does.
   - Records are removed, not marked deleted; the event keeps what was removed. Replacing a set removes every record of the collection not in it, a record with no key included. Any write that would change nothing writes nothing, not only a write by key. An update never changes a record's key.
   - A fed collection is written only through `Store::replace_feed_set`, which nothing outside the engine calls yet; step 4 checks there that the caller is the feed's connector. Undo leaves a fed collection alone too, since the next run sets it.
   - A token's `apps` names the apps it reaches. A person's token that names none reaches every app unless it is limited to projects; any other token that names none reaches none, and a sub-agent's token inherits none.
   - Not yet: archiving old app events (undo still refuses events past the 90-day window).
2. The API, the stream, tokens scoped to apps, MCP. **Built (TRAIL-1581)** (`crates/trailhead-server/src/routes/apps.rs`, `/llms.txt` has the whole of it):
   - Records are at `/v1/apps/{app}/records/{collection}[/{id}]` (`PATCH` merges, `PUT` replaces, `If-Match` on both and `DELETE`), and by key at `/v1/apps/{app}/keys/{collection}/{key}`, the key one percent-encoded segment (`%2F`, `%23`). `POST /v1/apps/{app}/batch` takes several writes as one transaction and batch; `GET /v1/apps/{app}/events[/stream]` and `POST /v1/apps/{app}/undo` work as the task log's do, and `POST /v1/revert` reverts an actor's app writes too, reported per app in a new `apps` field. A page of records carries `trailhead-event-cursor`, the app's newest event as it was read, which a mirror streams from.
   - The server registers each `apps/<id>/collections.toml` of the checkout its builder last built, a fed collection's fields from `connectors/<connector>/connector.toml` there (`crates/trailhead-server/src/declarations.rs`), at start and after each build (`Builds::built`); a broken file is that app's refusal, logged, and its last declaration stays. `PUT /v1/apps/{app}/declaration` (a person only) registers one without an apps repo, for tests and the example app.
   - A token's `apps` are taken and shown by `POST /v1/tokens`, `POST /v1/agents`, `PATCH /v1/tokens/{id}` and `trailhead token`/`agent add --apps`. MCP has `app_collections`, `list_records`, `get_record`, `write_record` and `delete_record`. The connector kit's field type is the model's.
3. The window's record mirror and test helpers; the example app keeps a collection. **Built (TRAIL-1581)** (`clients/trailhead-shell/src/records.rs` and `record_sync.rs`; the shell's `GUIDE.md`, "An app's own data", is the app's side of it):
   - An app declares what it keeps with `ShellApp::collections`: `Collections::declared(include_str!("../collections.toml")).keep::<T>("name")`, each kept collection with the serde type its records' data reads as. The file is the one the server registers; the shell reads it for which collections are fed, and a test registers it from it. A kept collection the file doesn't declare is a panic when the app is built.
   - While signed in, the net thread runs one record sync per such app beside the task sync: `GET /v1/apps/{app}` (the declaration, and whether the server knows the app and the token reaches it), each kept collection paged 500 at a time, then the app's stream from the first page's cursor, with a cursor per app kept across a drop as the task stream's is (resumed within five minutes, else the snapshot again). An app the server doesn't know (404) or the token doesn't reach (403) is said once in the log and asked about again every thirty probe gaps (a minute by default); it never holds the connection back from `Live`.
   - Records are entities holding `Record<T>` (id, key, version, timestamps, data) and `RecordOf`. A snapshot replaces what is held of the app; an event replaces a record only when its `after.version` is newer, and a removal despawns it unless the copy held is newer than the one removed (a later page's, after an undo put it back). A record whose data doesn't read as `T` is left out and said once, and an older copy stays. `RecordMirror::loaded` tells "not yet" from "none".
   - Writes are the shell's commands (`records::create`, `create_keyed`, `put`, `Record::change`, `replace`, `delete`, with the version as `If-Match`), answered as task commands are. A write to a fed collection is refused by the shell at once as `Forbidden`, from the app's file or the declaration the server last gave.
   - `testing::fill_records` and `feed_record` (with `fixture::record_json` and `app_event`) fill a collection with no server; `testing::server::live`, `Server::declare_app` and `records_caught_up` go live against an in-process one without the test naming the network.
   - The example app keeps `notes` (`clients/trailhead-app-example/collections.toml`, `src/notes.rs`), with a logic test, an in-place test and a live test.
   - Not yet: a view of a collection shared with another app; the shell does not reload when the server's declaration changes under a running window (a new shape arrives with a new build, which restarts it).
4. The connector kit, feeds and their routing, scheduled runs, secrets, the CLI.
5. The Connectors page and the failure notification.
6. The builder builds connectors and hands the server the declarations; the apps repo's pin, path rules and guides.
7. Cairn: a GitHub connector, a Jira connector and the app, planned and built by the factory.

**Left for later:** backups; writing back to a service; push; migrations beyond addition; sharing a collection between apps; giving a secret from the window.

## Phases

Numbered within this proposal; where they sit among PLAN.md's phases is decided when the plan is updated.

Lane drives the order and decides when a phase is done. The groundwork (through the apps repo, at least) is done in interactive Claude Code sessions, not handed to the factory; once the shell can take a request, work moves into the app itself.

Before any of it: the Lookout and factory changes now in review are merged, grade 3's gates are decided (see [Trust, grades and undo](#trust-grades-and-undo)). The spike on [the contract between shell and app](#the-contract-between-shell-and-app) is done (TRAIL-1575).

1. **The kit as a library.** The lint that apps are held to, and the widgets the todo app will need.
2. **The shell.** A shell crate (window, connection, settings, app switcher, the app trait); Lookout becomes an app inside it; `trailhead app` launches it. A refactor with no new features.
3. **The apps repo.** Template, registration, path rules and self-merging, the builder and the relaunch. Proven with the example app.
4. **Ticklist,** in the apps repo, on the existing task API. First target: Lane moves his Todoist workflow onto it. Backups come with it.
5. **Asking for an app.** A request in the shell becomes a project, a brief and tasks; a planner run writes the spec and asks its questions through `needs_input`, which Lookout already shows.
   **Built (TRAIL-1580), without the planner:**
   - The rail's foot has an Ask button (a plus, "Ask for a change or a new app") while the connection is live and the server has an apps repo. It opens a form: a dropdown of what the ask is about (each app the factory builds, by name, then "A new app", the app shown chosen first), the new app's name, and the ask itself. Once the server has made it, the form says what: "Asked: TICK-17" and the task's title, and for a new app "Made the project TN, Trail Notes". TRAIL-1673 split it in two, below.
   - `GET /v1/shell/ask` lists what can be asked about: the active projects whose `metadata.factory.repo` is the apps repo, which the builder reads from its checkout's `origin`. `POST /v1/shell/ask` takes `{"text", "project"}` or `{"text", "new_app": {"name", "key"?}}`. Only a person may ask (an agent gets 403); the server acts as that person, so it may give the first grade and point a project at the repo with no new engine rule, and everything one ask writes is one batch to undo.
   - **A change** is one task in the app's project: titled from the ask's first line, its whole text in the description, grade 2 and priority 2, so the factory takes it from its next poll.
   - **A new app** gets an id from its name (lowercase words joined by `-`, starting with a letter, 2 to 32 characters, a name cargo takes for a crate, not `shell`, `lookout` or `example`, and not an app's that exists) and a project key: the one the person typed in the form (`new_app.key`, 2 to 10 letters or digits starting with a letter, refused when another project has it), else one made from the name (one word of up to six letters whole, so Cairn is CAIRN, a longer one its first four; the initials of several; a digit added until unique). The form's key field is optional, and a line under it says which key the tasks will carry (TRAIL-1688). Its project is named for it, pointed at `apps/<id>`, requires review, has the `factory` agent as reviewer when the factory has run here (the reply says so when it hasn't), and has a brief naming the path, the kit and the commit subjects. Its first task, at grade 2, is to scaffold the crate, register it in the shell and build a first version of what was asked; adding a crate touches `Cargo.toml` and `shell/`, so the PR is graded 3 and a person approves the new app.
   - **Built (TRAIL-1673): the window's two ways in.** A plus under the apps on the rail ("Ask for a new app") opens "Ask for a new app": its name, the ask, and a dropdown of the planner's model ("The repo's default", which sends none, then Fable, Opus, Sonnet and Haiku); a new app is always planned. The Ask button at the rail's foot is now a pencil ("Ask for a change to this app"), shown once the apps repo has an app to ask about, and opens "Ask for a change": the app (the app shown first, else the first app; "A new app" is no longer among them), the ask, and a "Plan this first" switch, off at first, with a note saying a planner talks the ask over and proposes tasks to approve before any work starts; on, the planner's model shows under it. The form sends `Ask::change` or `Ask::new_app`, with `.planned(model)` when planned. For a planned ask the answer says "Sent to the planner: TN-1" and that the planner comes back with questions or a plan to approve, on that task in Lookout as a question waiting on the person.
   - **The planner run is built** (TRAIL-1669), designed in [Planning an ask](#planning-an-ask). **Not offered:** asking about Lookout or the `trailhead` repo, and editing or withdrawing an ask from the window, are not offered.
6. **The app data store.** Designed in [The app data store and connectors](#the-app-data-store-and-connectors).
7. **Connectors.** Designed there too.

Later: Android (the only phone target for now; desktop is where this is mostly used), loading views from `bsn` files, the shell on a different machine from the server (the builder then ships binaries over iroh), a local code host. A possible second app for Lane: planning a work day and reviewing code.

## What the todo app needs from the engine

None of this blocks the first version: the basic app comes first and features follow as they are wanted. What a todo list needs of tasks is added to the task engine itself, for every client and agent, and the app uses it; the app does not keep task features of its own.

The engine has due days and times, scheduled days and times, reminders, priorities, subtasks and projects. Not there yet for a Todoist workflow:

- Recurring tasks (in PLAN.md's Later list).
- Labels (TRAIL-155).
- A one-time import from Todoist (PLAN.md phase 9).
- Reminders that reach the person: the engine emits events, which the shell turns into notifications ([Notifications](#notifications)).
- Quick capture without opening the shell. The CLI covers it today.

## Planning an ask

Decided with Lane, 2026-11-21. Built so far (TRAIL-1609's subtasks have the rest): the server's side of asking and of approving. `POST /v1/shell/ask` takes `plan` and `model` and makes a spec task (ungraded, `metadata.plan.spec`) for a planned change and for every new app; `POST /v1/tasks/{task}/plan/approve` takes a grade for each open, ungraded subtask of a spec task and, as the person, in one write, grades them, resolves the spec task's questions and marks its plan approved (`Store::approve_plan`). The spec task then stays open, ungraded, as the parent of the work. And the window's two buttons (TRAIL-1673): the plus under the apps asks for a new app with the planner's model, and the Ask button asks for a change with "Plan this first" and, when on, the model (phase 5 above). And the planner run (TRAIL-1669, [lifecycle.md](factory/lifecycle.md#the-flow-for-one-task)): a spec task is planned by turns, its session kept on the held sandbox, its plan written as ungraded subtasks with proposed grades, and nothing of it worked until a person grades it. No live sandbox has run a planner yet. And the chat panel (TRAIL-1670): a spec task's card and task modal always offer its graph, which shows the conversation beside the canvas ("No tasks are planned yet" until the planner has drawn some up): the ask (cut short, read whole in the task modal), every comment as a message, the reader's on the right, a line saying whose turn it is (the planner working, waiting on you, queued for the planner, approved), and a reply box that answers the newest open question as the task modal's answer form does, disabled with the reason when there is none. And approval on the graph (TRAIL-1674): on a plan not yet approved, each open, ungraded task shows a grade dropdown preset to the planner's proposal ("Grade…" without one), with why in its hint and among its facts in the task modal; Approve in the page's header sends the chosen grades for exactly those tasks, disabled with the reason beside it while the planner is working, there are no tasks, or one has no grade. Once approved, the dropdowns and Approve go and the turn line says approved. And editing by hand (TRAIL-1675): on the graph of any parent, Add in the header (a title and an optional description) makes an ungraded subtask, and each node's ⋯ menu offers Depends on… (any other task of the plan it doesn't wait on that wouldn't make a cycle), No longer depends on… (one it waits on in the plan), Split (a second task with its parent, dependencies, dependents and description, titled "<title> (2)") and Remove (cancels it, once confirmed); each is one ordinary write, so undo covers it.

**Asking.**

- **A plus below the app list** asks for a new app. It always goes to the planner, and its plan always needs a person's approval.
- **The Ask button** asks for a change to the app shown, with a "Plan this first" toggle, off by default. Off, the ask is one grade 2 task straight to a worker, as now. On, it goes to the planner, and the form shows a dropdown of the planner's model.
- A person says which asks are big; nothing guesses. A worker that finds a small ask is a big one stops and asks through `needs_input`, as any worker does.

**The planner run.**

- An ask to be planned becomes a **spec task**, the parent of everything the ask produces.
- The planner is a factory run with a job of its own: a sandbox with the repo checked out, so it plans against the real code and kit, and no PR. What it leaves is the spec and the subtasks.
- Its model defaults to an entry in `.trailhead.toml`. The dropdown at asking time overrides it, and the choice is kept on the spec task, so every turn of one conversation uses it. No settings page for it yet.

**The conversation.**

- The spec task's comments are the conversation, by turns: the planner posts and parks the task, the person replies, and the factory resumes the planner with the reply. This is `needs_input`, used more than once. Nothing streams.
- The window draws that thread as a chat panel on the spec task.
- The planner keeps its context from turn to turn: the factory remembers its Claude Code session and resumes it (a run resumed after an answer starts a new session today). The session is on the sandbox's disk, so the sandbox is held, paused, while the conversation is open, and released once it has sat idle for some hours; a reply after that starts a new session from the thread.
- Each turn the planner is given the plan as it stands in the engine and what a person changed in it since its last turn. The engine holds the plan, not the planner's memory.

**The plan.**

- The spec is written into the spec task's description.
- Each piece of work is a subtask with acceptance criteria, `blocked_by` the pieces it needs. A subtask is one worker run and one PR; a piece too big for that is several subtasks. Subtasks have no subtasks of their own: the plan is one level deep, though the engine allows more, and nesting can come back later as grouping (a big plan's phases drawn around their nodes).
- Subtasks are created ungraded, each carrying the grade the planner proposes and a line saying why. The factory skips an ungraded task, so nothing starts early.
- A person changes the plan by replying in the conversation, or by hand in the graph.
- **No plan document is committed.** The breakdown and the conversation are history, which the engine keeps. What later work needs is what the app is now: each app has a `README.md` in its folder saying what it does and why, written from the spec by the plan's first task and kept up by any task that changes what the app does, as one of its acceptance criteria.

**Approval.**

- Approving the plan is the person setting the grades: each subtask shows its proposed grade in a dropdown, the person changes any, and presses Approve. The server sets them as that person, in one batch to undo.
- After approval the planner is finished. The subtasks are ordinary factory tasks, taken as their dependencies clear.
- **The spec task is closed when the plan is finished. Built (TRAIL-1687).** Once every task of an approved plan is done or cancelled, at least one of them done, the factory closes the spec task with a handoff naming them and raises a notification (`factory.plan_done`). The spec task has no work of its own, so the engine lets a delegated reviewer finish a parent whose subtasks are all finished without a review of its own; nobody else may, and a parent with no subtasks, or one whose subtasks were all cancelled, still needs its review. A plan whose tasks were all cancelled is left open for the person.

**The graph.** A second way to look at work, beside the board, which is every task by status: one parent task's subtasks by what depends on what.

- It opens from any parent task, so an epic made by hand has it too.
- A node is a subtask in its status's colors, a line is `blocked_by`, and it changes as the factory works. Clicking a node opens the inspector.
- It lies on a canvas that pans and zooms with no edge, so a large plan can be read. The canvas and the graph laid out on it are kit widgets (`trailhead-ui`), for any app.
- First: read-only, with the grade dropdowns and Approve on a plan not yet approved. Then editing by hand: add a node, remove one, split one (a second node with the same parent and dependencies), and add or remove a dependency. No merge (cancel one, edit the other) and no moving to another parent (there is one).

**Left out:** planning again after approval (follow-ups are ordinary tasks); the planner filing kit tasks for what an app needs from the kit (TRAIL-1612); a streaming conversation, over ACP or otherwise, which could replace the turns without changing the panel or the thread; saving the session out of the sandbox so a waiting planner holds none. Telling a person the planner has replied waits on [notifications](#notifications) for the whole shell, a piece of its own.

**Order:** the graph, read-only (it needs nothing else here); the planner run; the chat panel; approval; the two buttons; editing by hand. The middle four prove themselves only together: the first real test is asking for a small new app and approving its plan.

## Notifications

Decided with Lane, 2026-11-21 (TRAIL-1671), except what is marked **Open**. What is built is marked so below.

One way for the server, the factory and any app to tell a person something needs them. The shell shows them, in the window and on the desktop.

**The server owns them.**

- A notification is a row in a table of its own in the server, beside the tasks. Each change to one (raised, read, unread, resolved) is an event, which is how it reaches every window, but not a change undo covers: undo leaves them alone, as it does the reminders. **Built (TRAIL-1676, TRAIL-1677):** the table, the rules below, `/v1/notifications` and `trailhead inbox`.
- It carries its source (an app's id, `factory` or `server`), a kind, a title, a body in plain text, a subject (what it is about: a task's key, or a string of the app's own), its actions, and when it was raised, read and resolved.
- It has no recipient. There is one person.
- **Read and resolved are two things.** Read: the person saw it. Resolved: the thing no longer needs them, whoever dealt with it and wherever (a question answered from the CLI). The server resolves by rule; a resolved one stays in the history.
- The window gets them on the event stream, as one more kind of record in the shell's mirror, so every app and the bell read the same ones, and a second machine agrees on what is read.

**What raises one.**

- **The server, by rule, from the engine's events:** an agent's question that isn't put to another agent (which covers the planner replying and a plan ready to approve, both questions on the spec task), a task an agent sent for review in a project where no agent may approve (where one may, whoever runs the review says when a person is needed, as the factory does), and the reminders the engine already emits (`task.due_soon`, `task.overdue`; TRAIL-1505 shares their names). The same rules resolve them: the answer, the approval, the task done.
- **The factory,** for what the engine cannot see: a run that failed, a task it parked, a budget breach, the factory paused. This takes TRAIL-111's place.
  **Built (TRAIL-1682):** through the `Tracker` trait, the factory raises `factory.review` when a PR waits on a person alone (every other gate passed), with a link to the pull request, and `factory.paused` once per usage pause; it resolves each when that stops. A failed run it gives up on, a park and a budget breach are each a question the factory asks on the task, which the first rule above already raises, so the factory raises nothing more for them ([operating.md](factory/operating.md#notifications)).
- **An app, while the window runs,** with a `Notify` command the shell sends to the server. An app cannot raise one while the window is closed; rules over an app's own data wait on the app data store.
  **Built (TRAIL-1681):** `trailhead_shell::notifications::Notify` (source, kind, title, body, subject, task, links),
  triggered from an app's code and sent as `POST /v1/notifications` while the connection is live, and
  `ResolveNotifications { source, subject }` as `POST /v1/notifications/resolve`. The example app raises one every tenth
  press and resolves it when the count is reset.
- An agent already has `ask`, which raises one by the first rule.

**Trust.** No rate limits and no guard against an app that raises too many. Lane asks for the apps; one that misbehaves gets changed. The shell does say which app raised each one.

**Kinds.** A kind is the sort of thing a notification is, as a short name: `lookout.question`, `lookout.review`, `factory.run_failed`, `task.due_soon`. Every notification has one. At first a kind is only that name, and levels are set per source (all of an app's, or the factory's). Later, when one app's notifications want different treatment, an app lists the kinds it raises in its manifest, each with the name a person reads ("A question for you") and the level it starts at, and the settings page shows a row for each before any has been raised.

**Levels.** Three: *silent* (in the list only), *bell* (the count, the ring and the sound) and *interrupt* (a desktop notification too). Every one is recorded whatever its level.
**Built (TRAIL-1680):** the shell holds the person's settings (`notification_settings`), read from the server when
the connection goes live, every minute and after a save, and does what `NotificationSettings::alert` says for each
arrival: a silent source is listed and left out of every count, a bell source rings the bell (with the sound) and never
reaches the desktop, an interrupting one rings the bell with window focus and goes to the desktop without. The
Notifications page among the shell's own settings sets a level for each app on the rail, then Factory, then
Trailhead.

**In the window.**

- **A bell at the rail's foot,** with the count unread. When one arrives while the window has focus the bell rings: it swings, grows a little and shrinks back. A desktop that asks for less motion sees only the count change.
- **Its panel** lists them, newest first, each with its source's icon and name, title, body, age and action buttons; the unread ones marked. Each can be marked read or unread, and all read at once. A switch between Unread and All; All pages back through everything the server keeps.
- They are listed one by one. Nothing is folded together in the list.
- **A count on an app's rail icon:** its unread ones. A setting hides the counts, the bell's too.
- **Pressing one in the list does nothing.** What can be done with it is its buttons: Mark read, and its actions.

**Built (TRAIL-1678):** the shell mirrors them (the snapshot loads the newest 200, read or not, and the stream keeps
them), and tells the rest of the shell of one that arrives raised and unread off the stream as a `NotificationArrived`
message, for the desktop notification and the sound to hang off. The bell sits at the rail's foot over the gear, with
the count of unread ones on it, and rings (the kit's `Ring`: a swing that grows a little and settles, half a second of
real time) when one arrives while the window has focus, not for a desktop that asks for less motion. Its panel lists
them as cards, newest first, with an Unread and All switch, Show older paging back under All, Mark read or unread on
each, a link button for each action and Mark all read; resolved ones say so. Each app's button on the rail has the count
of its own. `NotificationCounts { shown }` is the one switch for the counts, for the settings page to come. A shell with
one app has no rail, so Lookout puts the bell in its sidebar's footer beside the gear, and opening a task in Lookout
marks the notifications about it read. The desktop notifications, levels, quiet hours, focus, the sound, the
settings page and an app's `Notify` are built since, each marked below.

**Actions.** A notification can carry buttons. First, one sort: a link, which the shell opens in the desktop's browser (Lookout's review notification links the pull request on GitHub). The shell opens it, since an app may start no process. The desktop notification has the same buttons, which Ubuntu draws; a link needs no window focus. Later: buttons that act (Approve, Answer).

**On the desktop.**

- The shell sends them while it runs, on Ubuntu (GNOME), over the desktop's notification service on D-Bus, under the desktop entry `mise run shell-install` writes so they carry Trailhead's name and icon. The Mac and Windows are later.
- **Only when the window does not have focus.** With focus, the bell's ring is the notice.
- **A burst is one notification.** Several at once, those raised during quiet hours, and those raised while the window was not running become one: "10 things need you". The shell keeps the last one it announced in its `LocalStore` entry, per machine, to know what a launch missed.
- **Pressing one brings the window forward with the bell's panel open.** It goes no further than that (phase 2). To check on Ubuntu: that GNOME on Wayland lets the window come forward from the press; if it does not, the panel is still open when the person gets there.
- One read or resolved elsewhere is taken down.
- **Built (TRAIL-1679),** in `clients/trailhead-shell/src/desktop.rs`: arrivals without focus are gathered for a second and a half, then shown one by one, or as one saying how many when more than three came; a launch without focus says what was raised since this machine last announced one (`announced` in the shell's `LocalStore` entry); the desktop plays its own theme's message sound with it. It talks to `org.freedesktop.Notifications` with `zbus`, which the keyring already brought in, so no package was added. **Not yet seen on a real desktop:** a press reaching the window, and whether GNOME on Wayland lets the window come forward; a trial notification sent the same way did show.

**Quiet hours and focus.**

- They gate the interruption, never the record: no desktop notification and no ring, while the list and the count carry on.
- Quiet hours are a daily span. Focus is switched on by hand, until a time or until switched off, from the bell's panel.
- When either ends, one desktop notification says what arrived.
- The settings (levels, quiet hours, the sound, the counts) are kept on the server, so every machine agrees, on a Notifications page among the shell's own in Settings.
- **Built (TRAIL-1680):** an arrival during either is counted and held; when either ends, what was held and is still
  unread is said once: one ring of the bell with window focus, one desktop notification ("3 things need you") for those
  from interrupting sources without. The minute of the day is the window's machine's own clock and zone. Quiet hours
  (on or off, a start and an end in half-hour steps), the sound and the counts are on the Notifications page; focus is a
  row at the top of the bell's panel (for an hour, until ended, end it). Each saves the whole settings with a `PUT`,
  from those last read with only its part changed, and the page shows what a read after the save says. A launch during
  quiet hours or focus holds what it missed for their end.
- **Low priority:** a level above interrupt that breaks through both (the factory stopped, a budget breach). Lane may never use it, so it is built last if at all. Ubuntu's own Do Not Disturb hides ours with no work here.

**A sound.** One plays with the ring and with a desktop notification, never during quiet hours or focus, and a setting turns it off. It has to be made by a person, not generated, under an open licence. **Built (TRAIL-1680):** the desktop theme's own message sound (`message-new-instant`), on Ubuntu Yaru's, by Mads Rosendahl under CC-BY-SA-4.0, played by the desktop: a desktop notification names it as a hint, and the ring in a focused window starts `canberra-gtk-play` for it on a thread of its own (silent where the program is missing). No file of ours is shipped, so nothing needs recording beside one.

**How long they are kept.** Everything, forever. The rows are small (a few hundred bytes); pruning is dealt with if the table ever gets too big.

**Phase 2: going to the thing.** Pressing a notification, in the window or on the desktop, opens what it is about. Put off because each part is hard:

- A notification would carry a target naming the thing, not a view: a task (and a comment on it) or a project, for which the shell picks the app that opens tasks, or an app and a route only that app reads. The shell switches `ActiveApp` and triggers an event the app observes.
- A press can arrive before the mirror has synced, so the shell holds the target until the connection is live.
- A stored route outlives the build that wrote it, so an app has to cope with one it does not know, and with a thing since deleted.
- A settings modal or an unsent draft may be in the way.
- Raising the window from the desktop's notification needs an activation token on Wayland; whether winit passes one through is not checked.
- Until then a notification's text has to say where to look (the task's key), and its link does the rest.

**Also phase 2 or later:** a tray icon, so notifications arrive with the window closed (likely an effort of its own); snoozing one; hiding the text on a locked screen; the Mac. **Not planned:** email or a push to a phone.

**Where it lands.**

| Where | What |
| --- | --- |
| `trailhead-model` | The notification, its action, the kinds' names (with TRAIL-1505) |
| `trailhead-core` | A `notifications` table in a new migration; raise, read, resolve; the rules that raise and resolve from events |
| `trailhead-server` | `/v1/notifications` (list, raise, read, read all), the records on the sync stream, the settings |
| `trailhead-control` | Raising what the engine cannot see, probably through the `Tracker` trait |
| `trailhead-shell` | The mirror's records and the `Notify` command, the bell and its panel, the settings page, the desktop notifier behind a trait (so tests use a fake), quiet hours and the burst rule as plain logic |
| `trailhead-ui` | The ring, and a count badge if the kit lacks one |
| The desktop's sound theme | The sound (TRAIL-1680: none of ours is shipped) |
| Lookout | Its link actions; marking a task's notifications read when the task is opened |

**Order:** the table and API with the question and review rules; the bell and its panel; Ubuntu's desktop notifications; levels, quiet hours, focus and the sound; apps raising their own; kinds in the manifest and breaking through quiet hours, if wanted. The first two are enough to say the planner has replied.

## Trust, grades and undo

A change to an app can merge without a person reading it and then runs on the user's machine as that user. **Leaning:** accept that, keep the factory's grades ([review.md](factory/review.md)) as the control, and make undo dependable.

**The ladder. Decided for every repo, in [review.md](factory/review.md#grades).** The defaults did not climb evenly: grade 4 dropped the agent review that grade 3 required. Now each grade keeps everything below it and adds one thing, and that what puts a change at a grade is one question: how far does the damage reach if it is wrong?

| Grade | Reach if wrong | Areas | Gates |
| --- | --- | --- | --- |
| 1 | Nothing that runs | Docs, new or changed tests | Checks + CI |
| 2 | One module or one app | Ordinary code inside it | + one agent review |
| 3 | Things other code or stored data depend on | Shared code, public API, the shape of stored data and migrations, dependency manifests, changes across modules, large diffs | + the quorum by job, in place of the single review, with a round of rework. A person only when it deadlocks or security is overruled |
| 4 | The controls themselves | New or upgraded packages (the lockfile), credentials and network access, CI, the factory's policy, the agents' instructions | + a person, always. The quorum still runs, and its findings are what the person reads first |

Grades 1 to 3 merge themselves; Treeline moves up to sit above grade 3.

**In the apps repo** the same ladder, by path:

| Grade | Paths |
| --- | --- |
| 1 | `apps/*/tests/**`, docs |
| 2 | `apps/*/src/**` |
| 3 | `shell/**`, the app template, an app's manifest (its collections), `**/Cargo.toml`, a diff touching more than one app |
| 4 | `Cargo.lock` (a new package, the `trailhead` pin), `.trailhead.toml`, CI, `connectors/**`, agent instructions |

**Built (TRAIL-1595):** "a diff touching more than one app" is the grader's floor for a file outside the task's project path (grade 3), not a path rule; and a `Cargo.lock` that only adds or changes workspace members (no `source`), as adding an app does, is not counted as a new package, so adding an app is graded by its `Cargo.toml` (3) rather than the lockfile (4).

**Apps that need more care. Leaning.** A grade means the same approvals everywhere in a repo; apps do not get gates of their own, or nobody could say what "grade 2" means. An app differs by where it starts: a path rule gives it a floor (`3 = ["apps/<name>/**"]` for an app holding work or money data), which path rules can already do.

**Grading asked-for tasks. Decided (2026-11-21), built:** an ask from the window carries grade 2 itself (TRAIL-1580). Only a person can give a task its first grade, and the factory skips ungraded tasks, which is what holds a planner's tasks back until a person has looked: the planner proposes a grade for each task it creates, and approving the plan is the person setting them ([Planning an ask](#planning-an-ask)). Tasks in one plan differ in grade, so a default grade on the project, the earlier leaning, is dropped.

**A grader agent. Open, leaning not yet.** An agent that grades each task before work starts would add little: the task's grade is only a floor, the diff's paths raise the PR by what it really touched, and a reviewer can already raise it for a risk no path shows. If it is wanted later it is a duty of the planner, never of the worker (the author does not grade its own work), and it may only raise above the project's default; lowering stays a person's.

**Grade 3 without a person.** Lane wants grade 3 to merge on a quorum so that people come in only at grade 4. review.md already has a quorum (TRAIL-204, not built) whose reviewers may differ by model; that stays, and reviewers that differ by job are added to it. This changes the factory's review design for every repo, so it is written into review.md when settled; the state of it so far:

- **Reviewers differ by job. Leaning.** An engineer, a tester, a security reviewer and, where the diff touches UI, a UX reviewer, each with its own instructions. `distinct_models` stays an option and is not required.
- **A majority approves. Leaning.** One reviewer cannot hold a change up alone; its objection is recorded on the PR.
- **A round of rework on the feedback. Leaning.** The reviewers give their first verdicts without seeing each other's, the worker gets all the findings at once and reworks, and the reviewers look again. The majority is counted on that second look.
- **It never sticks. Leaning.** No majority after two rounds sends the change to a person, as review.md's quorum already does on disagreement.
- **Security outvoted. Leaning.** A security objection the majority overrules sends the change to a person instead of merging.
- **The UX reviewer needs to see the UI.** It judges the screenshots of [Seeing a change](#seeing-a-change).

## Seeing a change

Lane wants UI changes shown, live if possible. Three steps, each useful alone, chosen by how much a change needs. **Leaning.**

1. **Screenshots.** The sandbox renders the changed views headless and attaches the images to the task; Lookout shows them on the task, before and after, and the UX reviewer reads the same images. The local half is built (TRAIL-1614, on TRAIL-802's spike): `trailhead_shell::testing::shot` draws the shell to PNGs with no window, display or GPU (software rendering with Mesa's lavapipe, byte for byte the same from run to run on one renderer), and each app has an `#[ignore]`d `tests/shots.rs` that draws its views, named `<app>-<view>-<theme>` (`<app>-<view>-150-<theme>` at a text size of 150%) and set up from fixtures; `mise run shots` runs them all (the shell's guide, section 7). Where they go is built (TRAIL-1630): a task carries pictures in the engine, not on the code host (`POST /v1/tasks/{task}/attachments` with the PNG as the body, a `name` such as `ticklist-today-dark`, a `role` of `before` or `after` and a `set` naming the run), and Lookout's task modal shows them under Pictures, each set's pairs by name, before beside after, a new, removed or unchanged view said as such, and a pair opens large side by side or flipped in place (`docs/factory/ui.md`). The sandbox image has lavapipe, and the factory draws the shots before and after a change and attaches them (`[factory.shots]`, TRAIL-1614). CI does not run the shots yet, having no render adapter.
2. **Try it, on your machine.** A button on the task in Lookout has the builder build that PR's shell into a separate slot and open it as a second window beside the running one. It is the real app, live, built on demand. Offered only once checks and the agent gates have passed, since it runs unmerged code. **Open:** what data it runs on. A copy of the app's SQLite file is cheap, which is one more reason for a file per app; tasks have no such copy, so a previewed Ticklist would act on real tasks, with undo behind it, unless the server grows a preview scope.
3. **Live from the sandbox. Later, maybe.** The sandbox runs the shell and streams frames to Lookout over iroh, with input sent back: watching an agent's work as it happens, with no unmerged code on your machine, and usable from a phone. It needs a frame stream built and software rendering fast enough to watch.
- **Instead of the single review, or as well. Open.** Proposal: instead. The engineer is today's reviewer under another name, and reviewing twice buys nothing.
- **Cost** is not a constraint now (a subscription, so spend is nominal); budgets are raised if they bind.

**Lints still to add** for app crates: no `std::fs`, `std::net` or `std::process`; no `unsafe`; no UI outside the kit. Apps hold no credentials; connectors get theirs from the server.

**Undo.** Three paths, each of which has to work without thought:

- **A bad build:** the launcher falls back to the previous shell on its own when the new one fails to start (built, TRAIL-1578), and offers the previous one from a menu when it starts but is wrong (not built).
- **A bad change:** rejecting it in the shell reverts the merge and rebuilds.
- **Bad data:** app data is in the event log, so undo and revert-since cover whatever an app, an agent or a connector wrote.

Text a connector brings in from outside is one way a bad instruction could reach an agent; connectors being grade 4 and apps having no network access is the answer for now. Sandboxing apps properly would mean running them as WASM, which is not planned while there is one user.

## The window on another machine

Built in TRAIL-1635 (2026-11-21), for driving the factory from a laptop while the server stays at home. The window
already dials the server over iroh (`trailhead/ui/0`), so it runs anywhere the server allows; what was missing was a
build of it there, kept current.

- **Each machine builds its own window.** The server's builds are for the server's machine. A build for a Mac would
  mean cross-compiling a graphics app from Linux (Apple's SDK, a fragile toolchain) or a macOS runner in CI (ten
  times the minutes on a private repo), so neither. The other machine compiles the same commit the server built.
- **The server hands out the source.** `GET /v1/shell/source/{rev}` answers with the apps repo at a build it keeps
  (`current` or `previous`, nothing else) as a gzipped tar, to a person's token only. The other machine needs no
  access to the apps repo, which is private; the framework it is pinned to is public and cargo fetches it as usual.
- **`[shell] from = "<the server's endpoint id>"`** in the other machine's user config (`trailhead app from <id>`
  writes it; never with `repo`). `trailhead app` then runs the same builder the server runs, with its git replaced
  by the server (`crates/trailhead/src/cmd/app_remote.rs`): the head is the server's newest build, a checkout is that
  build's source unpacked, and cargo builds it into the same layout (`<data dir>/shell`), which the launcher starts
  from as it always has. It needs cargo, and no Trailhead server on that machine.
- **Two endpoints to allow.** The launcher has its own iroh identity (`launcher.key` beside the window's
  `endpoint.key`), since it keeps building while the window runs and two processes can't share one. While the server
  hasn't answered, `trailhead app` dials its endpoint to learn which it is: refused (the dial is taken, then closed
  as not allowed), and it prints both `trailhead factory allow` lines to run there; or not reached, and it says so
  and prints no lines, since allowing the machine again changes nothing (TRAIL-1667). It signs in with the token saved
  for that server, else `TRAILHEAD_SHELL_TOKEN`, else one pasted at its prompt (`trailhead --as-me token add laptop`
  on the server), and saves it where the window looks, so it is asked for once.
- **An update is offered when this machine has built it.** The launcher names its builder's state file to the window
  (`TRAILHEAD_SHELL_LOCAL_STATE`), and the window's notice reads the newest build from there instead of from the
  server: the server's newest isn't one this machine can start until it has compiled it, a minute or two later. What
  the update brings is still the server's list of commits.
- **One launcher at a time.** `trailhead app` holds `launcher.lock` in the shell's directory while it runs, and a
  second one is refused. Two would dial the server as the same two endpoints, and the relay sends the server's
  answers to whichever dialed last, so the other's dials time out for as long as both run (TRAIL-1665: seen on the
  Mac, reproduced with two relay-only clients on one key).
- **A macOS app.** `trailhead app install` there writes `~/Applications/Trailhead.app`: Trailhead's icon
  (`crates/trailhead/assets/trailhead.icns`, drawn by the shell's `icon` module with macOS's margin) and a script
  that runs `trailhead app` with the PATH of the terminal that installed it, since an app started from the Dock has
  no cargo on its PATH. What the launcher prints goes to `~/Library/Logs/Trailhead/launcher.log`. The window the
  launcher starts takes the bundle's identity, so it is "Trailhead" with the icon in the Dock and the menu bar (which
  is why the bundle isn't an `LSUIElement` agent: the window then had neither). The script sets `TRAILHEAD_APP_LOG`,
  which tells the launcher nobody reads what it prints, so it says in a dialog (`osascript`,
  `crates/trailhead/src/cmd/app_notice.rs`) why there is no window: while it waits for the server, with Quit, Show
  Log and Keep Waiting, taken down when the server answers; and when it gives up (another launcher holds the lock, a
  build failed, no token). Signing in still needs one start from a terminal, for the prompt. The dialogs have not
  been seen on a Mac yet (TRAIL-1666).
- **Not built:** the CLI or MCP reaching the server from another machine (TRAIL-71). The window is the way in from
  there.
- **Verified on Lane's Mac (2026-11-21):** the first build, the window signing in, an update arriving and being
  taken from the rail, and the app started from Spotlight with its icon.

## Other things to settle

- **How long a request takes. Accepted for now.** A small change still goes through a sandbox, the agent, checks, CI, review, a merge and a build of the shell, which Rust makes slow; requests are queued work to come back to, not edits that appear as you watch. A faster path for small changes inside one app (a warm local sandbox, no second CI pass) is designed only if the wait proves bad.
- **A crash in one app takes the shell down.** The launcher's fallback covers a crash at startup only. A way to start with one app switched off would cover the rest.
- **Version skew.** The server is built from the `trailhead` repo and the shell from the apps repo's pin. They need a protocol version check and a rule for moving the pin when the server is upgraded.
- **Build load.** Every merged app change builds the shell on the machine that also runs the server and perhaps the factory's local sandboxes. The builder has a job limit and a profile (`[shell] jobs` and `profile`), and a target directory of its own beside the apps repo's, which costs the disk of a second release build. Nothing yet keeps a build from running beside the factory's sandboxes.
- **The factory has to run unattended.** This design leans on self-merging changes with nobody watching, which is what phase 5 of PLAN.md is still hardening.
- **This goes ahead of TRAIL-114. Leaning.** TRAIL-114 makes the factory installable by someone who has only their own repo (`cargo install trailhead`, `trailhead factory init`, `login`, `start`). For now this is all for Lane, with other people a someday aim, and this proposal changes what they would install. TRAIL-114's `factory init` is still needed in phase 3, to set up the apps repo.
- **License.** The shell and Lookout are AGPL-3.0 and the kit is MIT OR Apache-2.0. A private apps repo linking them is fine; distributing a built shell carries the AGPL.

## In PLAN.md and review.md

Made on 2026-11-19: PLAN.md's vision, principles 5 and 6, phase 7, the decisions log and the non-goals; the grade ladder and the quorum in [review.md](factory/review.md); the review row of the decisions table in [overview.md](factory/overview.md). `AGENTS.md` still names phase 5 as current, which holds until this work starts.
