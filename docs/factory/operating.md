# Operating the factory

How to run the factory day to day, as it works today. It assumes this repo and its dev setup; the path to running it without the repo is TRAIL-114.

## One-time setup

- `BWS_ACCESS_TOKEN` in your environment, for a Bitwarden Secrets Manager machine account with read access to the Trailhead project. `fnox.toml` maps `SPRITES_TOKEN` and `CLAUDE_CODE_OAUTH_TOKEN` from it.
- A `factory` agent: `trailhead --as-me agent add factory`. The control plane acts as it, and each run gets a sub-agent (`factory/trail-42-a1`).
- `factory.toml` at the repo root: the repo, the queue task, checks, egress, profiles and budgets, and `max_sandboxes`.
- The local Trailhead server running (`trailhead server status`); the CLI starts it on demand.

## Starting and stopping

- `mise run factory` builds the daemon and the control plane in release mode and runs it in the foreground. Its output is the log.
- `trailhead-factory check --config factory.toml` (under `fnox exec --`) reports what it can see without running anything: queued tasks, Sprites, its iroh endpoint.
- Ctrl-C stops it. Runs in flight are abandoned, and the next start's reconcile requeues them and returns their pooled Sprites to idle. Restarting is always safe.
- Run only one at a time. Two would share an iroh identity and undo each other's runs (TRAIL-113).

## Handing it work

- The factory only takes ready subtasks of the queue task (`[repo] queue` in `factory.toml`, TRAIL-102), highest priority first, up to `max_sandboxes` at once. Putting a task under the queue is the approval: there's no separate "go".
- Create work there: `trailhead --as-me add "title" -p TRAIL --parent TRAIL-102 -P 2 -d "spec"`. Moving an existing task in waits on TRAIL-110.
- Write the spec as you would for a careful contractor: which files, acceptance criteria, what not to touch, how to check it. TRAIL-105 is a good example.
- To take work back: `trailhead --as-me edit TRAIL-n -s cancelled`.

## Watching a run

The log shows, per task:

```
preparing a worker sandbox task=TRAIL-105 run=trail-105-a6
starting worker task=TRAIL-105 run=trail-105-a6 sandbox=trailhead-w1
step task=TRAIL-105 step=7 spent=$0.0412
tool task=TRAIL-105 tool=Bash detail=cargo test -p trailhead-daemon
run finished task=TRAIL-105 outcome=…
```

- `trailhead show TRAIL-n` shows the status, the claim (`factory/trail-n-aK` while running, renewed as it goes), notes and questions. The `factory` metadata holds the attempt, the pooled Sprite (`host`), spend and the PR.
- `trailhead watch` streams changes.
- A Sandbox's own daemon log is in the Sprites API for now (`/v1/sprites/<name>/services/trailhead-daemon/logs`); `trailhead factory logs` is TRAIL-109.

## What happens to a task

| Outcome | What you see | What you do |
| --- | --- | --- |
| PR opened | Task `in_review`, a PR on GitHub with the reviewer's comment, a typed verdict on the task | Review and merge on GitHub, then `trailhead --as-me done TRAIL-n -m "merged"` |
| Checks failed | A note on the task with the end of the failing check's output; the task is open again and retried | Usually nothing. The next attempt is told what failed; after `max_attempts` (`[repo]`, default 3) the task is parked with a question summarizing the failure |
| Question | Task `needs_input` with a question comment; its Sprite stays busy | Answer it, set the task open, restart the factory (TRAIL-108 removes the restart) |
| Budget or time breach | As a question, saying which limit it hit | Raise the budget in `factory.toml` or cancel the task |
| Dropped model call | `claude's API call failed; resuming the session` in the log | Nothing; it resumes up to three times |

## Sandboxes and cost

- Workers run in a lazy pool of warm Sprites (`trailhead-w1`, …, up to `max_sandboxes`). The first task pays a cold build of the repo (about 12 minutes for Trailhead today; TRAIL-120 should cut it). After that, each task restores the warm checkpoint and builds incrementally, in about a minute.
- Between tasks, pooled Sprites are restored (wiping the last task), have no egress and no connector access, and pause about two minutes after their last task. A paused Sprite costs only storage. Stopping the factory doesn't delete the pool; destroy a Sprite in the Sprites dashboard if you want it gone.
- Reviewers get a fresh Sprite each, skip the build, and are destroyed after.
- Each run is capped by its profile's budget: dollars (API-equivalent in subscription mode), minutes and steps.

## Known rough edges

- Answering a question needs a restart (TRAIL-108).
- No `status` or `logs` commands (TRAIL-109); no way to move a task into the queue (TRAIL-110).
- No notifications (TRAIL-111); runs in a terminal rather than as a service (TRAIL-112); no single-instance lock (TRAIL-113).
- The GitHub connector has classic `repo` scope on all of Lane's repos and lets every Sprite use it (TRAIL-95).
