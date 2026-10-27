# Task-engine changes for the factory

Changes `trailhead-core` and `trailhead-server` need before the factory can use them fully. Checked against commit `bc3fa26`. None are filed in `TRAIL` yet; each has a ready command. The MVP needs only #5 ([mvp.md](mvp.md)).

## 1. Resume affinity

Moving to `in_review` releases the claim, and an agent's lease expires while `needs_input`, so requested changes or an answered question return the task to the general ready queue. Add a `reserved_for` agent that `ready` respects, or let agent claims survive review and parking. Needed for suspend-during-review.

```sh
trailhead add "Resume affinity: keep a task for its agent through in_review and needs_input" -p TRAIL -P 2
```

## 2. Typed review verdicts

Add a `review` comment kind carrying `approve` or `request_changes`, the reviewer, and optionally the model, so gates and quorums are computed from records rather than prose.

```sh
trailhead add "Typed review verdicts: review comment kind with approve/request_changes" -p TRAIL -P 2
```

## 3. Approved vs merged

Today approval and finishing are one step. Document the convention that a project's only delegated reviewer is a merger agent that moves the task to `done` after merging, or add an `approved` status. Depends on 2.

```sh
trailhead add "Separate approval from finishing: document merger-reviewer convention or add approved status" -p TRAIL -P 3
```

## 4. Token expiry

Tokens can be revoked but never expire. Add an optional expiry, so short-lived per-sandbox sub-agent tokens clean themselves up if revoking is missed.

```sh
trailhead add "Optional expiry on tokens, for short-lived sub-agent tokens" -p TRAIL -P 2
```

## 5. Tunnel access from sandboxes

Mostly done: Trailhead is one server reachable over an iroh tunnel (`dumbpipe`), and `trailhead login` saves a printed token on another machine. What the factory still needs is the unattended version: a sandbox opens the tunnel and uses a token the control plane passes in at start, with no interactive login. Later, the server could embed iroh itself instead of relying on `dumbpipe`.

```sh
trailhead add "Unattended tunnel access: sandboxes open the dumbpipe tunnel and use a token passed in at start" -p TRAIL -P 2
```

## Later, not blocking

- Trailhead enforcing approval counts itself instead of trusting the control plane.
- A documented `metadata` schema for factory fields.
- Parent agents acting on tasks their sub-agents hold, if the control plane needs to write run metadata.
