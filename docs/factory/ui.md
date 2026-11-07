# Human interface

> **Design note.** It records decisions and intent; where it says something is built or not, check the code if it matters.

The factory's own client for people: watch runs live, steer them, and author tasks and specs. A native Bevy app, built in this repo but kept out of the default build. Trailhead facts checked against commit `9e68492`.

## Decisions

| Area | Decision | Status |
| --- | --- | --- |
| Official | The factory's first-party client, replacing "no official app" in `docs/PLAN.md` | Decided (2026-10-28) |
| When | After the factory runs unattended without regular fixes (PLAN phase 6), and after the status surface below | Decided (2026-10-28) |
| Toolkit | Bevy 0.20 with `bevy_feathers` for panels, forms and text input; no egui | Decided (2026-10-27) |
| Role | A client like any other: reads through the API, writes through commands, never touches `trailhead-core` or the database | Decided |
| Home | This repo, outside the default build (`default-members`); moving it to its own repo later is cheap | Decided |
| Offline | No local actions without a live connection; no optimistic updates | Decided |
| Identity | A user-principal token, the same kind the CLI holds | Decided (already built) |
| Transport | Embedded iroh, dialing the control plane on its own UI protocol, not the daemons' API tunnel | Decided (2026-10-28) |
| Platforms | Linux first, matching Trailhead; macOS and Windows later through Bevy's own targets (TRAIL-131) | Decided (2026-11-02) |
| Name | Lookout: crate `trailhead-lookout` in `clients/trailhead-lookout`, binary `lookout` | Decided (2026-11-02) |
| Look | One shared feathers component kit in the `trailhead-ui` crate (`clients/trailhead-ui`, MIT OR Apache-2.0, nothing Trailhead in it) used by every view; kanban board of cards; dropdown filters; Lucide icon buttons ([Look and components](#look-and-components)) | Decided (2026-11-05) |
| API types | A hand-written client crate, not generated from the OpenAPI document; the API is ours and the UI reaches it through the iroh tunnel anyway | Decided (2026-11-02) |

## Views

- **Trail map (watch).** The spatial view and the reason to use a game engine. Campsites are pooled Sprites, agents are run sub-agents (`factory/trail-42-a1`) moving between them, tasks are waypoints. Claims, handoffs, questions, review verdicts and spend show as movement and markers. Everything on it comes from the event stream.

  Built (TRAIL-702, `map.rs`): campsites on a grid at left (state colour, role, warm, task and phase held), waypoints at right for tasks in `in_progress`, `needs_input`, `in_review` or `approved` and any with a run. A claim holder is a token at its waypoint, which walks to the campsite once its run has a sandbox and back when the run ends. Markers on the waypoint: claim holder, open questions, last handoff, in review, approved, reviewing, spend. It is UI nodes, not sprites, until art exists; the board has a button to the map and back. Not done: pan and zoom, selecting a spot for the inspector, an animation for a verdict landing.
- **Task board (author).** Lists, filters and forms: create tasks under the queue task, write and edit specs, comment, answer questions. Leans on feathers' text input for multiline specs; no spike first, the editor is built with the forms and anything feathers lacks is filed then.

  Built (TRAIL-698, `author.rs`): feathers' `FeathersTextInput` does multiline with `EditableText { allow_newlines, visible_lines }` and word-boundary wrapping, so there is no custom box. It lacks a taller container (the frame is one row high; overridden to auto here), a scrollbar and placeholder text. New tasks go under `LOOKOUT_QUEUE` (default TRAIL-102); the form sends one command at a time and closes only on `ok`.
- **Inspector (steer).** An overlay on either view for the selected task, run or agent: release a claim, cancel, move status, answer a question, approve `in_review` → `approved` (then `done`), undo an event, revert everything an agent did since a time.

Approving needs care. In Trailhead, approving and merging are one step ([lifecycle.md](lifecycle.md): "approved and merged are the same step"), and in the MVP a person merges on GitHub and then marks the task `done` by hand. An approve button that only moves the status lets a task show `done` while its PR is still open, and still leaves the merge to GitHub. Until the merger agent exists, the inspector should either link to the PR and only offer `done` once the PR is merged, or show approving as "merge on GitHub" and let the factory notice the merge. Whichever the factory adopts (a merger agent, or closing tasks when their PR merges), the UI follows it instead of adding a third path.

Views are Bevy `States`; the inspector is a panel that both views spawn.

**The task modal (2026-11-06, TRAIL-997).** Selecting a task opens it in a large modal in the middle of the window, laid out like Todoist's task view, in place of the side inspector: a header with the breadcrumb (project › parent › key), a main column (title, description, subtasks that open in the same modal, open questions, the PR with its gates) and a sidebar of facts (project, status, priority, grade and whether it is above Treeline, claim, dates, dependencies, the factory run, created/updated/version), then the inspector's actions. The whole task is fetched when it is selected (`GET /v1/tasks/{key}`, `detail.rs`), since the mirror holds only open questions and the last handoff, and fetched again when the stream brings an event about it. The side panel stays, for agents now and anything later that suits a panel. Still to come: the full activity list with a comment box (TRAIL-999), editing in place (TRAIL-1000, on the kit's inline-editable widgets, TRAIL-998), and moving up and down the column with a ⋯ menu for the actions (TRAIL-1001).

## Look and components

Decided 2026-11-05 (TRAIL-796), after Lane's first real use: Lookout worked but read as a debug overlay of raw text rows. The look is built from one small component kit, and every view uses it.

**One component, used everywhere.** Each kind of control or surface is built once in the `trailhead-ui` crate (`clients/trailhead-ui/src`), one widget to a file, and every view calls it; only widgets about tasks and the board (the card, the column, the status tokens) live in `clients/trailhead-lookout/src/widgets/`, which re-exports the kit so views import from `crate::widgets`. An app adds its own colors by painting tokens onto the kit's theme (`trailhead_ui::paint`). A second, slightly different button, card or dropdown is a bug, even when it's quicker to write. If a widget doesn't do what a view needs, the widget grows the option. The rules agents follow for the kit are in the `trailhead-ui` skill (`.claude/skills/trailhead-ui/SKILL.md`), which any app built on the kit uses; Lookout's own part (its tokens, card, column and views) is the short `lookout-ui` skill (`.claude/skills/lookout-ui/SKILL.md`), which points at it.

**Built on feathers.** Buttons, menus, dropdowns (`FeathersSelect`), text inputs, scrollbars, list views and panes come from `bevy_feathers` 0.20, wrapped by the kit so their props are set in one place. The kit doesn't draw its own controls where feathers has one. Much of it is ported from Lane's `bevy-data-explorer` (`src/widgets/`, `src/app/theme.rs`), whose rules have already paid for themselves:

- **Theme tokens, not literal colors** (`ThemeBackgroundColor`, `ThemeTextColor`), so a theme switch repaints everything. Lookout's own colors (status columns, grades, priorities) are tokens added to the theme in one place.
- **Named sizes and spacing.** Text sizes come from a `size` scale and every gap, padding and margin from a `space` scale named for its purpose; a test fails on a literal one.
- **Icons are Lucide glyphs** from an embedded icon font (`Icon`), so they take the button's text color and stay sharp. Its license ships beside it.
- **Button roles.** A primary button (one per surface, like "New task"), a normal one, and an icon-only tool button with a tooltip. A button that opens a menu shows a chevron; one that acts doesn't.

**The board.** A kanban board: one fixed-width column per status in the lifecycle order, each with its own background (a theme token per status) and a header with the status and count. The columns scroll on their own, and the board scrolls sideways when they don't fit. Each task is a card: key and title (title clamped to two lines), small badges for priority, grade, holder and open questions, and actions as icon buttons. Clicking the card opens the inspector. A card never shows a handoff or a question body in full: that belongs to the inspector. A column with hundreds of cards (`open` held 366 on 2026-11-05) builds only what's on screen.

**The header bar.** The app name, filters as dropdowns (project, parent, holder), a "closed: hidden/shown" toggle for the done and cancelled columns in place of buttons that cycle through values, a primary "New task" button, and the connection state as a small status chip (dot and words) instead of a banner of text. A search field (the kit's) narrows the board as you type by task key (`801`, `trail-801`) and title, case-insensitive (`board::build` takes the query in `Filter`); after a 300 ms pause the query also goes to `GET /v1/search` (`search.rs`), and tasks matching only in a description or comment join the board with a "in description: …" line on the card. Answers for a query that has since changed are dropped. Escape or the clear button empties it.

**Panels.** The inspector, the forms and the connect panel use the same pane, section, field (label above input) and button widgets. Choices with a fixed set of values, such as a task's priority or status in the form, are dropdowns too.

**Modals.** Every form and dialog is one kit widget, `clients/trailhead-ui/src/modal.rs` (TRAIL-840): feathers' `FeathersDialog` on a backdrop over the window, centered, with a title and a close X. It closes on the X, Escape or a click outside, by default for every modal; a save in flight locks it (`ModalLocked`). The dialog is never taller than `MAX_HEIGHT_VH` of the window: the body scrolls with the kit's scrollbar and the footer (Save, Cancel) stays in view. A kind of modal is a marker type implementing `Modal` (title, width).

**Checking the look.** A sandbox can't open a window, so the look gets a human review (the `clients/**` grade rule). TRAIL-796's follow-ups include rendering screenshots headlessly, with a software Vulkan driver, so a worker and its reviewer can see what they built.

## Settings

A gear at the right of the board header opens a `Settings` modal (the kit's modal, centered), with pages picked from tabs across the top (`settings.rs`, TRAIL-843). A page is a `Page` variant plus its fields.

- **Connection.** The factory endpoint dialed, this app's endpoint id with a copy button, who the token acts as, and Disconnect, which drops the control plane and the saved choice of it (`NetCommand::Disconnect`) and closes the modal, leaving the connect panel.
- **Factory.** Read-only, from what Lookout already has: running or stopped, the repos the factory serves with their control state, and the queue task with how many tasks are under it. The status doesn't carry the factory's configured queue list, so the queue shown is the one new tasks go under.

Factory controls (budgets, queues, pausing) land on the Factory page as they become settable at runtime; today none is, so nothing there writes. No new server API. The look needs a human check.

## Architecture

```
Bevy app ── channels ── net thread (tokio)
                          ├─ reqwest → 127.0.0.1:<port> ─┐
                          └─ iroh endpoint ── trailhead/api/0 ──→ control plane → server loopback
```

- **Net thread.** The pattern from haunt_your_friends: a tokio runtime on its own thread, bridged to Bevy with channels. Not `bevy_replicon`: the server is the authority and there's no world to replicate.
- **Commands.** Plain REST under `/v1`, one request per action, with `If-Match` for ETags and an `Idempotency-Key` so a retry after a dropped connection can't apply twice. The response is only a result: `ok` with the new version, or an error (`409` conflict, `forbidden`, `rate_limited`) shown on the control that sent it. Successful writes are not applied locally.
- **State.** `GET /v1/events/stream` is the only thing that changes the world. Events carry entity snapshots, so applying one replaces the entity if its version is newer, which makes replays and duplicates harmless.
- **ECS mirror.** Tasks, projects, comments, claims, agents and campsites are entities with components, keyed by Trailhead id. The map, the board and the inspector query the same world.

## Sync

1. Note the newest event id (`GET /v1/events` newest first, limit 1).
2. Load the snapshot: projects, then `GET /v1/tasks` with `include=claim,open_questions,last_handoff,last_review`, paged.
3. Open the stream with `Last-Event-ID` set to the id from step 1. Anything written during step 2 arrives again and is ignored by version.

On a dropped stream, reconnect with the last applied id; the server resumes with nothing missed. The stream reads only `events`, not `events_archive`, so after a long disconnect (or on any doubt) the client redoes all three steps rather than resuming. App start always does the full sync.

## Connection gating

A `Connection` state: `Offline`, `Syncing`, `Live`. Every command system runs only in `Live`, and the UI greys out its controls otherwise, with the reason shown. A command in flight when the connection drops is retried with its idempotency key once the client is `Live` again, or reported failed; it never silently disappears.

As built (TRAIL-694): `ConnectionPlugin` runs the net thread (`net.rs`) and mirrors it into the `Connection` state, a `ConnectionStatus { reason }` resource (`Reason` says plainly why: no target, bad id, `NotAllowed`, unreachable, needs login, token rejected, dialing, signing in), `LocalEndpoint` (Lookout's own id, for `trailhead factory allow`) and, once signed in, `Api { base, token, user }`. The net thread probes `/v1/health` through the tunnel every 2 seconds. A refused probe, a `NotAllowed`, and a stored token that keeps being rejected are retried more slowly (the backoff under "Factory stopped" below); the connect panel shows when the next try is. `Syncing` covers dialing, signing in, and loading the snapshot (`Reason::Loading`), and again if the feed drops (`Reason::Reconnecting`); `Live` means the stream is open. The endpoint key and the last control plane dialed are in Lookout's config directory (`LOOKOUT_CONFIG_DIR` overrides it). The control plane is given as an endpoint id; a longer ticket with addresses isn't built.

As built (TRAIL-696): `commands.rs`. A control sends a `Submit { source, request }` (method, path, body, `If-Match` version, and an `Idempotency-Key` made once per command) and reads a `CommandDone { source, id, outcome }`, where `Outcome` is `Ok { version }`, `Conflict` (the server's `412 version_conflict`, or a `409`), `Forbidden`, `RateLimited` or `Failed`. The `Outbox` holds every command until it has an answer. Dispatch runs only in `Live`; a `Submit` made outside `Live` is refused at once with `Failed`. A command sent when the link leaves `Live` goes back in the queue and is sent again with the same key on return; an answer to any attempt settles it. A request that gets no usable answer (transport error, 5xx, `idempotency_key_in_flight`) is retried with a short backoff, and reported `Failed` after 5 sends.
As built (TRAIL-697): `board.rs`. `build` is a pure function from the mirror's tasks, comments and claims (`trailhead-model` types in `Mirrored` components, TRAIL-923) to status columns (subtasks nested under a parent in the same column, otherwise naming it), with each card's claim holder, open questions and last handoff; `Filter` (project, parent, holder, closed) is stepped through by feathers buttons. `BoardUiPlugin` draws it in `View::Board`, greyed out with the connection's reason when not `Live`. It doesn't use the CLI and MCP's text formatter (TRAIL-148): the board draws widgets, not lines of text. The look needs a human review.
As built (TRAIL-699): `inspector.rs`. `Selection` (a task or an agent) is set by the board's "inspect" buttons; `build` makes the panel from the mirror: status, holder, run, open questions, and the PR with its merge state and gates read from the task's `factory` metadata. `plan` turns each `Action` (release, cancel, move status, resolve a question, undo the task's last event, revert the holder since 1/6/24/72 hours ago) into a `commands::Request`; a refusal shows beside its control. There is no approve or done control, and a factory task's status moves leave out `approved` and `done`: the merger approves, and the task closes when its PR merges. Each open question has an "answer" button that opens the answer form from `author.rs`, the same one the board opens; "resolve" closes a question without an answer. The panel sits above the board (`GlobalZIndex(2)`) and below the form (the modal, 10). The look needs a human review.


## Identity

Nothing new. Tokens already have a `principal` of user or agent, and every write records the actor and the user it acted for, so the event log tells "a person approved it" from "`factory/trail-42-a1` did it."

- Built (TRAIL-694): the token is found by the server's instance id from `/v1/health`, in the same place the CLI keeps it; `NetCommand::Login` takes a pasted token, confirms it with `/v1/me`, refuses an agent's, and saves it there. The window has no text input yet, so `lookout <endpoint-id>` and `LOOKOUT_TOKEN` stand in.
- The UI reads the user token the CLI saved for this server (`<config dir>/servers/<instance>/`, or the keyring entry named after it), or runs the same `trailhead login` flow: paste a printed token, confirm with `/v1/me`.
- It acts only as the user. No `--as <agent>` equivalent.
- Proposal: move `trailhead_server::creds` into a small crate so the UI can share it without depending on the server.

## Transport

Decided (2026-10-28): the UI is the factory's UI, so it connects to the control plane over iroh, using its own protocol (`trailhead/ui/0`) on the control plane's endpoint. It does not use the daemons' API tunnel (`trailhead/api/0`).

- **What it carries.** The factory status snapshot and stream ([Factory status](#factory-status)), and Trailhead API requests forwarded to the server's loopback port as the API tunnel does, so the UI's HTTP client talks to a local port.
- **Admission.** A persisted allowlist of UI endpoint ids, added by a person (for example `trailhead factory allow <endpoint-id>`), plus the user's bearer token on every API request. The allowlist decides who can connect, and the token decides what they can do.
- **Built (TRAIL-700).** Each stream opens with one framed JSON request, `{"op":"api"}`, after which it is raw bytes to the server's loopback port (the event stream included). Status (TRAIL-692, TRAIL-701) adds ops beside `api`; a request the control plane doesn't know resets the stream. Built (TRAIL-701): `{"op":"status"}` answers with framed JSON `StatusFrame`s, a `snapshot` of every repo and then one `event` per change (`{frame, repo, seq, event, ...}`), from the same boards as the loopback surface; a control plane with no status feed resets the stream. The allowlist is the file `<data dir>/factory/ui-allowed`, one endpoint id per line, read on every connection and stream, and rechecked every second on an open one so `disallow` cuts live connections too: `trailhead factory allow <id>`, `disallow <id>` and `allowed`. The control plane's endpoint id is logged at startup and shown by `trailhead factory status`. The control plane doesn't parse the forwarded HTTP; the server rejects any request without a valid token.
- **Why not the API tunnel.** That tunnel admits only daemons this control plane started, and only reaches the Trailhead API. Keeping the UI out of it means a compromised sandbox can't see factory status or reach steering, and how daemons get in can change without touching the UI.
- **Factory stopped.** There's little to watch, so the map shows Trailhead's last known state marked "factory stopped". The task board may fall back to the server URL the CLI already uses. Built (TRAIL-701): Lookout follows the status stream on its own connection and redials every 2 s after a drop; when it ends or can't be dialed, `FactoryStatus.link` becomes `Stopped` ("factory stopped") and the campsite and run entities keep their last state until the next snapshot replaces them. The board falls back to `NetConfig.server_url` (`TRAILHEAD_URL`, else `url` in the CLI's `config.toml`, else `http://127.0.0.1:7241`) only when the control plane is gone; one that refuses this Lookout (not on the allowlist) is still reported as such. Built (TRAIL-780): a refused status request, a `NotAllowed`, and a stored token that keeps being rejected back off, retried at 2 s, then 4 s, 8 s and so on up to 64 s (`STATUS_BACKOFF_STEPS` doublings of `NetConfig.status_retry`); the gap resets on a new target, a login or a success, and drops still retry at 2 s. The connect panel and the factory label say when the next try is ("trying again in 8 s"). It returns to the tunnel, with a fresh snapshot, once the control plane answers again. Once the control plane moves into the server process (later work in [mvp.md](mvp.md)), the server and the factory are one endpoint, and this case mostly goes away.

## Factory status

The map shows what the factory is doing right now, and most of that isn't in Trailhead. `trailhead factory status` and `logs --follow` read the same surface the UI will, rather than each building their own.

What Trailhead already has, through events: claims and their holders (`factory/trail-n-aK`), each task's `factory` metadata (attempt, `host`, spend, PR, review rounds), questions, handoffs and verdicts. The control plane writes metadata per step, so these move without polling.

What only the control plane knows:

- **Sandboxes.** Every pooled Sprite and its state: idle, busy with a task, paused, or broken and waiting to be replaced (TRAIL-135). Today this lives in Sprite labels and the control plane's memory. These are the campsites, and the map needs the empty ones too.
- **Run phase.** Preparing (and whether it's a cold build), agent, checks, publishing, reviewing. Metadata says a run exists, not which of these it's in.
- **Live activity.** Steps, tool calls and spend as they stream from the daemon. Today they're only in the control plane's log.
- **The control plane itself.** Up, reconciling or stopped, which run it's reconnecting to, and the last error.

The control plane serves a snapshot of the above plus a stream of factory events (sandbox state changes, run phase changes, steps and tool calls). The CLI's `factory status` reads the snapshot, `factory logs TRAIL-n --follow` follows the stream for one task, and the UI uses both. None of it is stored. The control plane keeps no state of its own ([state.md](state.md)), so on boot it rebuilds the snapshot from compute labels and the tracker, and the stream starts fresh. Durable facts still go to Trailhead.

Tool calls and steps don't become Trailhead events. The event log holds reversible changes with before and after snapshots. A run produces hundreds of tool calls, which would bury the changes people undo and push real history toward the archive.

### As built

The types are in `trailhead-factory` (`status.rs`) and the board that holds them in `trailhead-control` (`StatusBoard`, one per repo). The control plane updates it where the run loop, the pool and the lifecycle already change state, and reconcile rebuilds its sandboxes from the compute labels each time it lists them, which is also how a boot does.

- **Snapshot.** `control` (`up`, `reconciling`, `reattaching` to a task, `stopped`), the last error with its time, every sandbox (`idle`, `busy`, `paused` for a parked task's, `broken`, with its task, role, target and whether it has a warm checkpoint), and each run (`preparing` and whether `cold`, `agent`, `checks`, `publishing`, `reviewing`, with its sandbox, steps, tool calls, spend and last tool). `seq` is the last event it includes.
- **Stream.** Each event has a `seq` that starts from 1 on every boot: control changes and errors, sandbox changes, run phase changes and ends, steps and tool calls. `checks` and `publishing` come from the daemon, which sends `RunEvent::Checking` and `RunEvent::Publishing`; the rest the control plane knows itself. Tool calls and steps are only here, never Trailhead events.
- **Serving.** `trailhead-factory run` binds a loopback port and writes where, with a random bearer token, to `<data dir>/factory/status.json` (mode `0600`, removed on a clean stop). `GET /status` returns `{"repos": [...]}` with one snapshot per repo. `GET /events[?task=TRAIL-n]` streams newline-delimited JSON, one `{repo, seq, event, ...}` per line, from the moment it connects. The token is there because tool calls can name commands and paths. The UI's iroh protocol later carries the same two things.
- **Clients.** `trailhead-factory status` asks the port and falls back to reading the compute and tracker when nothing answers. `logs TRAIL-n --follow` prints the daemon log once, then follows the task's events instead of polling.

When the factory isn't running, the map shows Trailhead's last known state (claims, metadata, open questions) with the factory marked stopped, and the board still works if the UI can reach the server.

## Repo layout

The UI lives at `clients/trailhead-lookout`, outside `crates/`, so the default build never compiles Bevy. Glob support in `default-members` was checked with cargo 1.97.

```toml
[workspace]
members = ["crates/*", "clients/*"]
default-members = ["crates/*"]
```

- Plain `cargo build` and `cargo test` and the factory's sandboxes build only `crates/*`; the UI builds with `-p trailhead-lookout` or `--workspace`. CI checks the whole workspace on every PR, the UI in its own job, never skipped by path (decided 2026-11-02). In the factory, UI checks run only when a task's diff touches `clients/**` (TRAIL-691), and until they do, `clients/**` is grade 3.
- Bevy's dependencies share `Cargo.lock` with the server. If an upgrade forces a version bump on a shared dependency, or rust-analyzer's indexing gets slow, move the UI to its own repo.
- The UI depends on the API, not on `trailhead-core`. Request and response types come from a hand-written client crate, `crates/trailhead-client`, shared with the CLI (TRAIL-693): saved credentials (`creds`) and the `Health` and `ApiError` bodies. Domain types (statuses, grades, priorities, tasks and comments) come from `trailhead-model`, the crate `trailhead-core` re-exports, so Lookout shares them without the store (TRAIL-918).

## First steps

Decided (2026-11-02): the factory builds the UI. The epic is TRAIL-685 and it is a factory queue task, so its subtasks run like TRAIL-102's. A sandbox can't open a window, so UI tasks test headlessly (`MinimalPlugins`) and the look gets a human review. The multiline-editor spike is dropped.

1. Scaffold `clients/trailhead-lookout`, CI over the whole workspace (TRAIL-690); a shared client crate for API types and credentials (TRAIL-693).
2. The control plane's `trailhead/ui/0` protocol with API forwarding and its allowlist (TRAIL-700). Iroh from the start, decided 2026-11-02: reaching your factory from anywhere is the point, so there's no loopback-only stage.
3. Net thread over iroh, login and `Connection` (TRAIL-694); sync and the stream into the ECS mirror (TRAIL-695, built: `sync.rs` runs the three steps and resumes a dropped stream, past five minutes down it starts over; `mirror.rs` applies them, tasks and projects by `version`, comments and claims by event id, deleted entities despawned; a link that goes `Offline` keeps the stream position, so a short outage resumes; agents have no events, so they change only with a snapshot, and the snapshot holds every task, closed ones included); commands (TRAIL-696).
4. Task board (TRAIL-697), forms with the spec editor (TRAIL-698), inspector (TRAIL-699).
5. The control plane's status snapshot and stream (TRAIL-692), carried over `trailhead/ui/0` into Lookout (TRAIL-701).
6. Trail map (TRAIL-702).
7. Look and components (TRAIL-796): the widget kit by hand, ported from bevy-data-explorer (TRAIL-797); then task cards (TRAIL-798) and the board as kanban columns (TRAIL-799), the header bar with dropdown filters (TRAIL-800), and the inspector, forms and connect panel on the kit (TRAIL-801). Headless screenshots for workers and reviewers are a spike (TRAIL-802).

On the factory side: checks scoped to paths (TRAIL-691), then UI checks, disk and time for Bevy builds in `.trailhead.toml` (TRAIL-703).
