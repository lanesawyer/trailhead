# MVP

Goal: one real task goes from a Trailhead spec to a merged PR in one repo, with two human touches: approving the spec and merging.

**Done 2026-10-28 (TRAIL-75).** TRAIL-103 went from spec to merged PR #5 with its spend recorded, and TRAIL-120 followed (PR #6). Stopping and restarting the control plane mid-run requeued cleanly, several times. A budget breach (TRAIL-141) parked its task and suspended its Sprite. The evidence is on TRAIL-75. What followed is phase 5 in `docs/PLAN.md`: making the factory run unattended.

## In scope

- One repo, one worker profile (Sonnet) and one reviewer profile.
- `Compute`: Sprites through the factory's own thin API wrapper; bootstrap from the stock image; per-step checkpoints; install-phase egress.
- `Harness`: Claude Code through the Anthropic connector. In practice the MVP ran in subscription mode, on a Claude OAuth token ([sandbox.md](sandbox.md)).
- `Tracker`: Trailhead on Lane's machine, reached from sandboxes through its iroh tunnel.
- `CodeHost`: GitHub through the connector, publishing with the Git Data API.
- Standard checks, one agent review posted as a PR comment, a human merges.
- Per-task budget and wall-clock limit from daemon usage counts; parking through `ask`.
- Stateless control plane on Lane's machine, with reconcile on boot: `trailhead-factory run --config factory.toml` (or `mise run factory`), a process beside the local server that shares its database and forwards the API tunnel to it. Moving it into the server process is later work.
- `tracing` logs only.

## Out of scope

- Suspend during review: requested changes start a fresh sandbox from the PR branch, so resume affinity isn't needed yet.
- Grades (risk tiers), quorums, auto-merge, the merger. (Typed verdicts shipped anyway, as TRAIL-84.)
- Planner agent and spec collaboration: Lane writes the task spec by hand.
- Provider usage API, daily cap, workspace spend limit.
- Multiple repos, profile escalation, previews, outcome tracking, onboarding agent, OTLP export.

## Milestones

| # | Milestone | Proves |
| --- | --- | --- |
| M0 | Spikes: Iroh in a locked-down Sprite; Claude Code through the Anthropic connector; a PR through the GitHub connector; checkpoint seeding another Sprite | The four unknowns, before building on them |
| M1 | `trailhead-factory` types, the five traits, `trailhead-testkit` fakes; the MVP lifecycle runs in CI on fakes | The trait shapes |
| M2 | `trailhead-compute-sprites`: create, labels, egress, checkpoint, restore, destroy; bootstrap | Real compute |
| M3 | `trailhead-daemon` over Iroh with the Claude Code harness, checks, per-step checkpoints and budget | A run inside a sandbox |
| M4 | Trailhead `Tracker` implementations, with sandboxes on the tunnel | Work flows from Trailhead |
| M5 | `trailhead-codehost-github` and the reviewer agent | End to end to a PR |

Engine work the MVP needs: only unattended tunnel access ([engine-changes.md](engine-changes.md) #5); the tunnel itself exists. Sandbox tokens are revoked when a run finishes and also expire (TRAIL-85).

## Done when

- A real task in the chosen repo reaches a merged PR, with spend recorded on the Trailhead task.
- Killing the control plane mid-run and restarting it resumes or cleanly requeues the task, with no orphan sandboxes.
- A budget breach parks the task and suspends its sandbox.
- The full lifecycle also passes in CI on fakes.

Decided: Trailhead itself was the first repo, with its factory work queued under TRAIL-102.
