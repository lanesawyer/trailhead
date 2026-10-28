# Factory overview

The factory turns Trailhead tasks into agent runs inside locked-down Fly.io Sprites, talks to them over Iroh, and gates every change through agent or human review.

## Principles

- **Sandboxes are untrusted and disposable.** They hold no raw credentials and reach nothing beyond declared egress, but they work independently within those limits.
- **The control plane coordinates; sandboxes act.** Sandboxes call model providers and GitHub themselves through Fly.io Connectors. The control plane owns orchestration and budget decisions.
- **Every step is a transaction.** Checkpoint before, restore on failure.
- **Humans review by risk, not by habit.** Policy decides when a human is required.
- **Pluggable everywhere it matters.** Compute, agent harness, model provider, task tracker and code host are traits; see [seams.md](seams.md).

## Home: Trailhead

Decided (2026-10-26): build the factory into Trailhead and evolve Trailhead into it. The task engine becomes one part of the factory, and the todo-app use case becomes a separate client (working names `trailhead-todo` or `trailhead-ticklist`).

- **One process for coordination.** With the factory feature on, the Trailhead server is the control plane. The orchestrator follows the events table in-process.
- **One tunnel.** Trailhead already reaches other machines through an iroh tunnel to its loopback port (`dumbpipe`). Sandboxes use the same tunnel, and the factory's daemon protocol rides on it as server routes alongside REST and MCP.
- **Keep the `Tracker` trait.** In-process inside the server, REST from the daemon. It keeps the engine/factory boundary clean, makes test fakes possible, and leaves other trackers open.
- **Engine still usable alone.** Factory crates sit behind a Cargo feature, so `cargo install trailhead` without it stays a small task engine for existing users (OpenClaw, Lane's reader app).
- **Plan and license.** `docs/PLAN.md` vision, principles and phases need rewriting to match. Trailhead is AGPL-3.0, so the factory is too.

## Components

- **Trailhead server (control plane).** Task engine, orchestrator, reconciler, review router, budget meter. Talks to the Sprites API, the provider usage API, and daemons over Iroh. Holds no agent credentials.
- **Sandbox (Sprite).** The daemon, an agent adapter wrapping a CLI agent or an own loop, and the repo with its mise environment, bootstrapped from the stock image. Holds no raw keys.
- **Connector gateway (Fly.io).** Injects credentials for the model provider and GitHub on the sandbox's behalf.
- **Egress.** Sandboxes reach the Iroh relay and the connector gateway, plus package registries during install phases only. See [sandbox.md](sandbox.md).

## Decisions

| Area | Decision | Notes |
| --- | --- | --- |
| Language | Rust | Control plane and in-sandbox daemon |
| Home | Inside Trailhead | Server is the control plane; factory behind a Cargo feature |
| Plugin seams | `Compute`, `Harness`, `ModelProvider`, `Tracker`, `CodeHost` | Traits in core; vendor crates compiled in by feature; in-memory fakes for tests |
| Compute | Fly.io Sprites | Own thin wrapper over the Sprites HTTP API, not the unofficial `sprites` crate |
| Environment | mise, hidden behind standard checks | Users give stack and commands for setup, fmt, lint, typecheck, test, build |
| Bootstrap | Lazy pool of warm Sprites | Each worker Sprite is restored to its own warm checkpoint before and after every task; the pool grows to `max_sandboxes`; reviewers get fresh Sprites |
| Rollback | Checkpoints as transactions | Checkpoint before each agent step; restore on failure or rejection |
| Communication | Iroh 1.0 | n0 public relays for now; relay configurable to self-host later |
| Identity | Per-sandbox Trailhead sub-agent token | The tunnel ticket controls who can connect; the token decides what they can do |
| Secrets | Sprites Connectors | Anthropic first (swappable) and GitHub; sandboxes never hold raw tokens |
| Network | Deny by default | Iroh relay and connector gateway; registries only during install |
| Dependencies | Workers may install; gate at merge | New dependency raises the review tier |
| Agent profiles | Per repo | Opus, Sonnet, Haiku mapped by role; utility calls run in sandboxes too |
| Scheduling | One sandbox per task, cap per repo | No file locking; overlap resolved through PRs and merge conflicts |
| Review gates | Configurable per repo | Fully autonomous repos allowed; top tier uses an independent agent quorum; loosening policy always needs a human |
| Run state | No control-plane database | Trailhead plus the Sprites API; reconcile on boot |
| Trailhead access | Existing REST/MCP over Trailhead's iroh tunnel | One server, one SQLite file, wherever it runs; Turso deferred |
| Protocol | Strongly typed | Shared Rust types over Iroh; daemon name TBD |
| Specs | Human-led, written with AI, entered through Trailhead | Approved spec is the source of acceptance criteria |
| Control plane location | Runs anywhere | Starts on Lane's machine |
| Human interface | Native Bevy app with feathers ([ui.md](ui.md)) | In this repo outside the default build; a client of the API like any other |
