# Task-engine changes for the factory

Changes `trailhead-core` and `trailhead-server` need before the factory can use them fully. Statuses checked on 2026-10-28, after the MVP. Each is filed in `TRAIL`; the task key is in its heading. The MVP needs only #5 ([mvp.md](mvp.md)).

## 1. Resume affinity (TRAIL-83)

Moving to `in_review` releases the claim, and an agent's lease expires while `needs_input`, so requested changes or an answered question return the task to the general ready queue. Add a `reserved_for` agent that `ready` respects, or let agent claims survive review and parking. Needed for suspend-during-review.

```sh
trailhead add "Resume affinity: keep a task for its agent through in_review and needs_input" -p TRAIL -P 2
```

## 2. Typed review verdicts (TRAIL-84, done)

Done: a `review` comment kind carries a `verdict` (`approve` or `request_changes`) and optionally the `model`; the author is the reviewer. It records the verdict without moving the task. CLI: `trailhead verdict <task> approve|request-changes <body> [--model]`.

```sh
trailhead add "Typed review verdicts: review comment kind with approve/request_changes" -p TRAIL -P 2
```

## 3. Approved vs merged (TRAIL-86)

Decided (2026-10-29): add an `approved` status between `in_review` and `done`. `approved` means a reviewer cleared the work and it is waiting to be finished; for the factory, waiting to merge. Only humans and delegated reviewers move a task to `approved`; whoever finishes it moves it to `done`. `in_review` → `done` stays allowed for work with nothing left after approval. A new push or requested change sends an `approved` task back to `in_review` or `open`. It releases the claim like `in_review`. Depends on 2.

## 4. Token expiry (TRAIL-85, done)

Done: tokens take an optional `expires_in_seconds` and never outlive the token that issued them. The control plane gives each sandbox token its profile's wall-clock budget plus 30 minutes.

```sh
trailhead add "Optional expiry on tokens, for short-lived sub-agent tokens" -p TRAIL -P 2
```

## 5. Tunnel access from sandboxes (TRAIL-78, done)

Done differently in M4: sandboxes reach the server through the control plane's iroh API tunnel, not `dumbpipe`, so no engine change was needed ([state.md](state.md#trailhead-access)). The original plan follows.

Mostly done: Trailhead is one server reachable over an iroh tunnel (`dumbpipe`), and `trailhead login` saves a printed token on another machine. What the factory still needs is the unattended version: a sandbox opens the tunnel and uses a token the control plane passes in at start, with no interactive login. Later, the server could embed iroh itself instead of relying on `dumbpipe`.

```sh
trailhead add "Unattended tunnel access: sandboxes open the dumbpipe tunnel and use a token passed in at start" -p TRAIL -P 2
```

## 6. Grades (TRAIL-201)

Decided (2026-10-29): tasks get an optional `grade`, 1 to 4 ([review.md](review.md#grades)). A human sets the first grade, lowers it or clears it; an agent can only raise it, checked in core on updates and on undo and revert, like `require_review`. In the event log, the CLI (`--grade`), REST, MCP, and as a filter on `ready` and `list`.

## Later, not blocking

- Trailhead enforcing approval counts itself instead of trusting the control plane.
- A documented `metadata` schema for factory fields.
- Parent agents acting on tasks their sub-agents hold, if the control plane needs to write run metadata.
