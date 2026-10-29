# Operating the factory

How to run the factory day to day, as it works today. It assumes this repo and its dev setup; the path to running it without the repo is TRAIL-114.

## One-time setup

- `BWS_ACCESS_TOKEN` in your environment, for a Bitwarden Secrets Manager machine account with read access to the Trailhead project. `fnox.toml` maps `SPRITES_TOKEN` and `CLAUDE_CODE_OAUTH_TOKEN` from it.
- A `factory` agent: `trailhead --as-me agent add factory`. The control plane acts as it, and each run gets a sub-agent (`factory/trail-42-a1`).
- To have merged PRs finish their tasks: `GITHUB_TOKEN` in the control plane's environment (read access to the repo's pull requests is enough), and the factory agent added to the project's reviewers, `trailhead --as-me project edit TRAIL --reviewers factory`, since only a reviewer can finish a task in review. Without the token the control plane logs a warning at start and merges aren't noticed.
- `factory.toml` at the repo root: the repo, the queue task, checks, egress, profiles and budgets, and `max_sandboxes`.
- The local Trailhead server running (`trailhead server status`); the CLI starts it on demand.

## Starting and stopping

- `mise run factory` builds the daemon and the control plane in release mode and runs it in the foreground. Its output is the log.
- `trailhead-factory check --config factory.toml` (under `fnox exec --`) reports what it can see without running anything: queued tasks, Sprites, its iroh endpoint. Stop `run` first; they share the lock below.
- Ctrl-C stops it. Runs in flight are cancelled first (up to 15 seconds), and the next start's reconcile requeues their tasks and returns their pooled Sprites to idle. Restarting is always safe, though a cancelled run counts as an attempt (TRAIL-159).
- The factory clones from GitHub, not your checkout: push `main` before queuing work that builds on local commits.
- One control plane per machine, whatever repo it runs: they share an iroh identity and would undo each other's runs. `run` and `check` take an exclusive lock on `<data dir>/factory/control.lock` before touching anything, and a second one fails with `a control plane is already running on this machine (pid N, running owner/repo, …)`. The lock goes when the process exits, even after a crash, so stop the running one first; `check` can't run beside it.

Local sandboxes on Docker instead of Sprites (`docs/factory/sandbox.md#docker`): `mise run docker-up` and `mise run docker-image` once, `[compute] kind = "docker"` in the factory file, and the control plane built with `--features docker` and run with `DOCKER_HOST` set to the VM's socket. Until TRAIL-174 and TRAIL-175 land, Docker sandboxes have unfiltered egress and can't publish to GitHub, so this is for trying it out, not for real runs.

## Handing it work

- The factory only takes ready subtasks of the queue task (`[repo] queue` in `factory.toml`, TRAIL-102), highest priority first, up to `max_sandboxes` at once. Putting a task under the queue is the approval: there's no separate "go".
- Create work there: `trailhead --as-me add "title" -p TRAIL --parent TRAIL-102 -P 2 -d "spec"`. To move an existing task in, `trailhead --as-me edit TRAIL-n --parent TRAIL-102`; `--parent none` takes it back out.
- Write the spec as you would for a careful contractor: which files, acceptance criteria, what not to touch, how to check it. TRAIL-105 is a good example.
- **The agent sees only the title and description.** Comments, including `decision` comments, don't reach it, so fold any decision into the description before queuing.
- **Keep queued tasks from colliding.** Each run branches from `main` as it is when the run starts, and PRs wait for you to merge, so two queued tasks that edit the same files produce conflicting PRs. Chain them with `trailhead block TRAIL-a TRAIL-b`: the second waits until the first is done, which is when you merge it.
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
- To chase connection drops, set `[daemon] log = "info,iroh=debug"` in `factory.toml` (any `tracing` filter). Each daemon then starts with `--log <filter>`; without it they keep their default. It applies to daemons started after a restart of the factory.
- A Sandbox's own daemon log is in the Sprites API for now (`/v1/sprites/<name>/services/trailhead-daemon/logs`); `trailhead factory logs` is TRAIL-109.

## What happens to a task

| Outcome | What you see | What you do |
| --- | --- | --- |
| PR opened | Task `in_review`, a PR on GitHub with the reviewer's comment, a typed verdict on the task | Check CI is green and the diff's size is believable, and merge on GitHub |
| PR merged | Within a poll (`--poll-secs`, default 30 s) the task is `done`, with a handoff naming the PR and the merge commit, and `merged` (who, PR, commit) in its `factory` metadata. Tasks chained behind it unblock | Nothing. If `GITHUB_TOKEN` isn't set, or the factory isn't a project reviewer, finish it yourself: `trailhead --as-me done TRAIL-n -m "merged"` |
| PR closed without merging | The task is `needs_input` with a question naming the PR | Abandoned: cancel the task. To carry on, reopen the PR on GitHub, then reopen the task; the next run continues on the PR's branch |
| Reviewer approved with follow-ups | The PR comment ends with a "Follow-ups filed in Trailhead" list; each is a new open task in the project, not queued under TRAIL-102, whose description says which task and PR it came from. A title that's already an open task is listed as "(already open)" and not filed again. A request for changes files nothing | Triage them: queue the ones worth doing under TRAIL-102, cancel the rest |
| Reviewer requested changes | The review is posted on the PR and kept in the task's metadata; the task is open again and the next run starts from the PR branch head with the review in its prompt | Usually nothing. After `max_review_rounds` (`[repo]`, default 3) rounds the task is parked with a question holding the last review; fix the PR or settle it, then reopen (the count starts again) |
| Checks failed | A note on the task with the end of the failing check's output; the task is open again and retried | Usually nothing. The next attempt is told what failed; after `max_attempts` (`[repo]`, default 3) failures in a row the task is parked with a question summarizing the failure. On a rework, the count starts from the review that asked for changes |
| Infrastructure failed (compute API error, wake failure, daemon lost without reattaching, git fetch timeout, or Claude Code's own API timeout, no response, overload or 5xx) | The attempt does not count toward `max_attempts`, and the run name's attempt number counts only attempts that did. The task goes back to the queue after a backoff of 1 min, doubling up to 15 min, per task. The failures in a row are counted separately (`infra_failures` in the `factory` metadata); at `max_infra_failures` (`[repo]`, default 10) the task is parked with a question saying it was the infrastructure, not the task, with the last error. An agent that ends a run resets the count | Fix the host, the daemon or the API; nothing to do on the task unless it's parked, then reopen it (the count starts again; its sandbox, if it had one, stays for inspection and counts against `max_sandboxes`). A deliberate stop of the control plane does not count an attempt either. The agent giving up for other reasons, failed checks and a budget breach still count |
| Question | Task `needs_input` with a question comment; its Sprite stays busy | Answer it, set the task open, restart the factory (TRAIL-108 removes the restart) |
| Budget or time breach | As a question, saying which limit it hit | Raise the budget in `factory.toml` or cancel the task |
| Usage window nearly full | `subscription usage window nearly full; starting no new runs until it resets`, with how long until the reset | Nothing; runs start again at the reset. Change `[usage] pause_at` in `factory.toml` to leave more or less room |
| Dropped model call | `claude's API call failed; resuming the session` in the log | Nothing; it resumes up to three times |

## Sandboxes and cost

- Workers run in a lazy pool of warm Sprites (`trailhead-w1`, …, up to `max_sandboxes`). The first task pays a cold build of the repo (about 12 minutes for Trailhead today; TRAIL-120 should cut it). After that, each task restores the warm checkpoint and builds incrementally, in about a minute.
- Between tasks, pooled Sprites are restored (wiping the last task), have no egress and no connector access, and pause about two minutes after their last task. A paused Sprite costs only storage. Stopping the factory doesn't delete the pool; destroy a Sprite in the Sprites dashboard if you want it gone. A Sprite that keeps failing to release is given up on after five tries or ten minutes: an `ERROR` line in the log, the label `state=broken`, and a new Sprite for the next task. Destroy it in the dashboard if the factory can't.
- Reviewers get a fresh Sprite each, skip the build, and are destroyed after.
- Each run is capped by its profile's budget: dollars (API-equivalent in subscription mode), minutes and steps.

## Known rough edges

- Answering a question needs a restart (TRAIL-108).
- No `status` or `logs` commands (TRAIL-109).
- New work is noticed on the next poll, not immediately (TRAIL-158).
- Reviewers try to build and can't, wasting time (TRAIL-136).
- A pooled Sprite has 8 GB and no swap; heavy builds have restarted one mid-run (TRAIL-140).
- No notifications (TRAIL-111); runs in a terminal rather than as a service (TRAIL-112).
- The GitHub connector has classic `repo` scope on all of Lane's repos and lets every Sprite use it (TRAIL-95).
