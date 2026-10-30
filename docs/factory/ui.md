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
| Platforms | Linux first, matching Trailhead; macOS and Windows stay open (TRAIL-131) | Leaning |
| Name | Working name `trailhead-ui`; Lookout is a candidate ([naming.md](naming.md)) | Open |

## Views

- **Trail map (watch).** The spatial view and the reason to use a game engine. Campsites are pooled Sprites, agents are run sub-agents (`factory/trail-42-a1`) moving between them, tasks are waypoints. Claims, handoffs, questions, review verdicts and spend show as movement and markers. Everything on it comes from the event stream.
- **Task board (author).** Lists, filters and forms: create tasks under the queue task, write and edit specs, comment, answer questions. Leans on feathers' text input for multiline specs, so prototype that first.
- **Inspector (steer).** An overlay on either view for the selected task, run or agent: release a claim, cancel, move status, answer a question, approve `in_review` → `approved` (then `done`), undo an event, revert everything an agent did since a time.

Approving needs care. In Trailhead, approving and merging are one step ([lifecycle.md](lifecycle.md): "approved and merged are the same step"), and in the MVP a person merges on GitHub and then marks the task `done` by hand. An approve button that only moves the status lets a task show `done` while its PR is still open, and still leaves the merge to GitHub. Until the merger agent exists, the inspector should either link to the PR and only offer `done` once the PR is merged, or show approving as "merge on GitHub" and let the factory notice the merge. Whichever the factory adopts (a merger agent, or closing tasks when their PR merges), the UI follows it instead of adding a third path.

Views are Bevy `States`; the inspector is a panel that both views spawn.

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
2. Load the snapshot: projects, then `GET /v1/tasks` with `include=claim,open_questions,last_handoff`, paged.
3. Open the stream with `Last-Event-ID` set to the id from step 1. Anything written during step 2 arrives again and is ignored by version.

On a dropped stream, reconnect with the last applied id; the server resumes with nothing missed. The stream reads only `events`, not `events_archive`, so after a long disconnect (or on any doubt) the client redoes all three steps rather than resuming. App start always does the full sync.

## Connection gating

A `Connection` state: `Offline`, `Syncing`, `Live`. Every command system runs only in `Live`, and the UI greys out its controls otherwise, with the reason shown. A command in flight when the connection drops is retried with its idempotency key once the client is `Live` again, or reported failed; it never silently disappears.

## Identity

Nothing new. Tokens already have a `principal` of user or agent, and every write records the actor and the user it acted for, so the event log tells "Lane approved it" from "`factory/trail-42-a1` did it."

- The UI reads the user token the CLI saved for this server (`<config dir>/servers/<instance>/`, or the keyring entry named after it), or runs the same `trailhead login` flow: paste a printed token, confirm with `/v1/me`.
- It acts only as the user. No `--as <agent>` equivalent.
- Proposal: move `trailhead_server::creds` into a small crate so the UI can share it without depending on the server.

## Transport

Decided (2026-10-28): the UI is the factory's UI, so it connects to the control plane over iroh, using its own protocol (`trailhead/ui/0`) on the control plane's endpoint. It does not use the daemons' API tunnel (`trailhead/api/0`).

- **What it carries.** The factory status snapshot and stream ([Factory status](#factory-status)), and Trailhead API requests forwarded to the server's loopback port as the API tunnel does, so the UI's HTTP client talks to a local port.
- **Admission.** A persisted allowlist of UI endpoint ids, added by a person (for example `trailhead factory allow <endpoint-id>`), plus the user's bearer token on every API request. The allowlist decides who can connect, and the token decides what they can do.
- **Why not the API tunnel.** That tunnel admits only daemons this control plane started, and only reaches the Trailhead API. Keeping the UI out of it means a compromised sandbox can't see factory status or reach steering, and how daemons get in can change without touching the UI.
- **Factory stopped.** There's little to watch, so the map shows Trailhead's last known state marked "factory stopped". The task board may fall back to the server URL the CLI already uses. Once the control plane moves into the server process (later work in [mvp.md](mvp.md)), the server and the factory are one endpoint, and this case mostly goes away.

## Factory status

The map shows what the factory is doing right now, and most of that isn't in Trailhead. The same gap is why `trailhead factory status` and `logs` (TRAIL-109) don't exist yet, so both should read one surface rather than each building their own.

What Trailhead already has, through events: claims and their holders (`factory/trail-n-aK`), each task's `factory` metadata (attempt, `host`, spend, PR, review rounds), questions, handoffs and verdicts. The control plane writes metadata per step, so these move without polling.

What only the control plane knows:

- **Sandboxes.** Every pooled Sprite and its state: idle, busy with a task, paused, or broken and waiting to be replaced (TRAIL-135). Today this lives in Sprite labels and the control plane's memory. These are the campsites, and the map needs the empty ones too.
- **Run phase.** Preparing (and whether it's a cold build), agent, checks, publishing, reviewing. Metadata says a run exists, not which of these it's in.
- **Live activity.** Steps, tool calls and spend as they stream from the daemon. Today they're only in the control plane's log.
- **The control plane itself.** Up, reconciling or stopped, which run it's reconnecting to, and the last error.

Proposal: the control plane serves a snapshot of the above plus a stream of factory events (sandbox state changes, run phase changes, steps and tool calls). The CLI's `factory status` reads the snapshot, `factory logs TRAIL-n` follows the stream for one task, and the UI uses both. None of it is stored. The control plane keeps no state of its own ([state.md](state.md)), so on boot it rebuilds the snapshot from compute labels and the tracker, and the stream starts fresh. Durable facts still go to Trailhead.

Tool calls and steps don't become Trailhead events. The event log holds reversible changes with before and after snapshots. A run produces hundreds of tool calls, which would bury the changes people undo and push real history toward the archive.

When the factory isn't running, the map shows Trailhead's last known state (claims, metadata, open questions) with the factory marked stopped, and the board still works if the UI can reach the server.

## Repo layout

The UI lives at `clients/trailhead-ui`, outside `crates/`, so the default build never compiles Bevy. Glob support in `default-members` was checked with cargo 1.97.

```toml
[workspace]
members = ["crates/*", "clients/*"]
default-members = ["crates/*"]
```

- `cargo build`, `cargo test`, CI and Sprites build only `crates/*`; the UI builds with `-p trailhead-ui` or `--workspace`, in its own CI job.
- Bevy's dependencies share `Cargo.lock` with the server. If an upgrade forces a version bump on a shared dependency, or rust-analyzer's indexing gets slow, move the UI to its own repo.
- The UI depends on the API, not on `trailhead-core`. Request and response types come from a client crate or the OpenAPI document (to decide).

## First steps

Prerequisite, on the factory side: the status surface above, built with `factory status` and `logs` (TRAIL-109).

1. Spike multiline spec editing in feathers' text input.
2. Net thread: login, the three-step sync, the stream, and the `Connection` state, over a plain loopback URL first.
3. Task board and inspector against a local server.
4. The `trailhead/ui/0` protocol and its allowlist, then remote use.
5. Trail map.
