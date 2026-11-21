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

**An app is a `ShellApp`. Built.** One type per app crate, with five parts:

| Part | What it is |
| --- | --- |
| `manifest` | Its id, the name a person sees, and its icon on the rail |
| `build` | Its state, resources and logic. Runs headless, so an app is tested without a window |
| `build_ui` | What spawns and draws. Only a windowed shell calls it |
| `paint` | Its own tokens on the theme all apps share: the colors that encode its data |
| `link_jobs` | Its own work on the net thread while a control plane is held, as `LinkJob`s (Lookout's factory status and log streams). The shell takes them once, before `build`; they report over the app's own channels. Empty by default |

**The shell says which app is shown, and an app hangs everything on that. Built.** The shown app is the `ActiveApp` state. An app spawns what it draws on entering its own value and despawns it on leaving, runs its systems only in it, and makes its own views a sub-state of it. Lookout's `View` is such a sub-state, so switching away leaves the view, which tears its panel down with no code of Lookout's own.

**Teardown is of what is drawn, not of what is known. Built.** A switched-away app has no entities on screen, but its logic keeps running: Lookout's mirror stays in step with the server while another app is shown, so coming back needs no reload. Keeping an app's drawing alive is the later fix if rebuilding it is slow enough to annoy.

**A place to draw is the window beside the rail. Built.** The shell hands over no parent entity. An app fills the kit's `beside_rail`, or has a kit `sidebar` and fills `beside_sidebar`; the kit moves both over when a rail exists. A shell with one app has no rail and the app has the whole window, so Lookout alone looks as it did.

**Local storage. Built.** `LocalStore` is a small JSON file per app on the client's disk, for what must outlive a switch or a restart: the view it was on, an unsent draft. An app reads it when it is built and writes to it when the thing changes; the shell saves the file after that frame. The shell keeps the app last shown there. It is not the app's data, which lives on the server.

**One connection, sync and mirror, the shell's. Built (TRAIL-1577).** The shell owns the connection to the server (`net.rs`, the net thread; `connection.rs`, the `Connection` state every app gates its commands on), commands (`commands.rs`, the outbox), and the sync and mirror (`sync.rs`, `mirror.rs`): the task engine's records as entities keyed by kind and Trailhead id, replaced when a newer version arrives, which Lookout and later Ticklist both read. The binary tells the shell how to connect (`Shell::connecting`, `signing_in_with`); without that a shell has no connection. While it isn't live the shell draws the panel that says why (`connect.rs`), over whatever app is shown. An app reads the API through the net thread with a `Fetch`, whose answer comes back as the app's own message, and sends writes as commands; work of its own over the link is a `LinkJob`. Its tests run on the shell's `testing` helpers (the `test-support` feature): a headless or windowed test app holding the connection's pieces without its net thread, filled from fixtures; `testing::server` (the `test-server` feature) is an in-process server to go live against. The app data store later gets a second mirror of the same shape, keyed by collection.

**The window is the shell's. Built (TRAIL-1577).** `Shell` opens it, titled Trailhead with the app id `trailhead`, and sets its icon; it owns the theme and its file and the kit's plugins. What it keeps on this machine (the endpoint key, the control plane dialed last, the theme, each app's `LocalStore` under `apps/`) is in `shell` under Trailhead's config directory, or `TRAILHEAD_SHELL_DIR`. `clients/trailhead-desktop` is the assembly: its binary, `trailhead-shell`, reads the environment, gives the shell its apps and runs it, as the apps repo's `shell/` will.

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
  - It asks with `?since=<its build>`, and the answer's `changes` are the subjects of the commits since then (`git log` in the builder's clone, never while it checks the clone out), the newest 30 with `changes_total` for the rest (TRAIL-1605). The rail's button opens a panel listing them, each subject's `type(scope)` as a badge and its task key at the end, with Restart and Later.
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

**Open:** how a collection's shape changes once it holds data (who writes the migration, when it runs, how undo crosses it).

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
   - The rail's foot has an Ask button (a plus, "Ask for a change or a new app") while the connection is live and the server has an apps repo. It opens a form: a dropdown of what the ask is about (each app the factory builds, by name, then "A new app", the app shown chosen first), the new app's name, and the ask itself. Once the server has made it, the form says what: "Asked: TICK-17" and the task's title, and for a new app "Made the project TN, Trail Notes".
   - `GET /v1/shell/ask` lists what can be asked about: the active projects whose `metadata.factory.repo` is the apps repo, which the builder reads from its checkout's `origin`. `POST /v1/shell/ask` takes `{"text", "project"}` or `{"text", "new_app": {"name"}}`. Only a person may ask (an agent gets 403); the server acts as that person, so it may give the first grade and point a project at the repo with no new engine rule, and everything one ask writes is one batch to undo.
   - **A change** is one task in the app's project: titled from the ask's first line, its whole text in the description, grade 2 and priority 2, so the factory takes it from its next poll.
   - **A new app** gets an id from its name (lowercase words joined by `-`, starting with a letter, 2 to 32 characters, a name cargo takes for a crate, not `shell`, `lookout` or `example`, and not an app's that exists) and a project key (the first four letters of one word, the initials of several, a digit added until unique). Its project is named for it, pointed at `apps/<id>`, requires review, has the `factory` agent as reviewer when the factory has run here (the reply says so when it hasn't), and has a brief naming the path, the kit and the commit subjects. Its first task, at grade 2, is to scaffold the crate, register it in the shell and build a first version of what was asked; adding a crate touches `Cargo.toml` and `shell/`, so the PR is graded 3 and a person approves the new app.
   - **Not built:** the planner run. An ask is one task, its text the spec, with no questions asked first. No project default grade either: every task in an asked-for project so far is a person's ask, which carries its own grade. Asking about Lookout or the `trailhead` repo, and editing or withdrawing an ask from the window, are not offered.
6. **The app data store.**
7. **Connectors.**

Later: Android (the only phone target for now; desktop is where this is mostly used), loading views from `bsn` files, the shell on a different machine from the server (the builder then ships binaries over iroh), a local code host. A possible second app for Lane: planning a work day and reviewing code.

## What the todo app needs from the engine

None of this blocks the first version: the basic app comes first and features follow as they are wanted. What a todo list needs of tasks is added to the task engine itself, for every client and agent, and the app uses it; the app does not keep task features of its own.

The engine has due days and times, scheduled days and times, reminders, priorities, subtasks and projects. Not there yet for a Todoist workflow:

- Recurring tasks (in PLAN.md's Later list).
- Labels (TRAIL-155).
- A one-time import from Todoist (PLAN.md phase 9).
- Reminders that reach the person: the engine emits events, and nothing shows a desktop notification yet.
- Quick capture without opening the shell. The CLI covers it today.

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

**Grading asked-for tasks. Leaning on the default; an engine change. Not built:** an ask from the window carries grade 2 itself (TRAIL-1580), which covers asks until a planner creates tasks. Only a person can give a task its first grade, and the factory skips ungraded tasks, so tasks a planner creates for an asked-for app would never be worked. A project carries a default grade, set by a person when the app is created, and tasks agents create in it start there. A person still set the first grade, once, for the app.

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

1. **Screenshots.** The sandbox renders the changed views headless and attaches the images to the task; Lookout shows them on the task, before and after, and the UX reviewer reads the same images. The local half is built (TRAIL-1614, on TRAIL-802's spike): `trailhead_shell::testing::shot` draws the shell to PNGs with no window, display or GPU (software rendering with Mesa's lavapipe, byte for byte the same from run to run on one renderer), and each app has an `#[ignore]`d `tests/shots.rs` that draws its views, named `<app>-<view>-<theme>` and set up from fixtures; `mise run shots` runs them all (the shell's guide, section 7). Not built: lavapipe in the sandbox image, the factory drawing the shots before and after a change and attaching them, and Lookout showing them. CI does not run the shots yet, having no render adapter.
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
