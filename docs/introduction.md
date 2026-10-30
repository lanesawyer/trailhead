# Introduction

Trailhead is a headless, agent-native task engine written in Rust on SQLite, plus a software factory that runs Claude Code on queued tasks.

- **The task engine** keeps projects and tasks, and the rules around them: claims with leases so two agents never take the same task, a ready queue, typed comments for handoffs and questions, and an event log where every write can be undone. It has no UI of its own. People and agents use it through the `trailhead` CLI, MCP at `/mcp`, or REST under `/v1`.
- **The factory** takes a task with an approved spec, runs Claude Code on it in a locked-down sandbox, opens a pull request, has a second agent review it, and records the claims, questions, handoffs and spend on the task. A human still decides what is risky enough to need their review.

Both are one Rust workspace, licensed AGPL-3.0.

## What works today

- The task engine is in daily use, and Trailhead's own work is tracked in it.
- The factory MVP was finished on 2026-10-28: real tasks have gone from a spec to a merged PR. It is now being made to run unattended, with grades, gates and a merger that is still being proven out ([operating the factory](factory/operating.md) says what is built).

## What doesn't, yet

- **Linux only.** Releases build Linux binaries (x86_64 and arm64); there is no macOS or Windows client.
- **A local server.** One server and one SQLite file. Other machines reach it through a tunnel, never by exposing it publicly. There are no replicas and no hosted service.
- **The factory needs setup.** Today it runs from a checkout of this repo, with a [Fly Sprites](https://sprites.dev) token, a Claude subscription token and a GitHub token. Its crates aren't published.
- **No packaged factory release.** Installing and running the factory without this repo is TRAIL-114.
- **No UI.** The native app is designed ([ui.md](factory/ui.md)) but not started.

## Where to go next

- To try it: [Getting started](getting-started.md), then [Concepts](concepts.md).
- To see how the factory works: [The factory](factory/overview.md).
- To work on Trailhead: the [Plan](PLAN.md) and the design notes. Pages marked as design or history describe intent, not necessarily what runs.
