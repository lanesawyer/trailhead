# Compute targets: local first, Sprites as overflow

Proposal (2026-10-31, TRAIL-504), for review, with the choices below marked **Decided** made by Lane the same day. Today a factory runs on one compute. This page lets it run on several at once, prefer the free one, and give the local machine back when Lane needs it. Docker on the local machine and Sprites are the first two targets; any later `Compute` is one more.

## What's decided

- **Decided: local first, Sprites as paid overflow.** A run goes to the highest-priority target with room.
- **Decided: overflow is automatic, under a daily spend cap.** $5 a day for this repo. The cap is part of the factory's config.
- **Decided: a factory can be local only.** A repo that lists one target never overflows.
- **Decided: workers and reviewers needn't share a target.** A review only needs the PR.
- **Decided: slow to take the machine, quick to give it back,** with manual `yield` and `reclaim`.
- **Decided: runs don't move between targets.** Moving a run in progress isn't a goal. Local work drains, or is frozen or stopped in place. A run that's stopped starts again from the task, as any requeued run does.

## Targets

Proposal. A repo lists its targets in `.trailhead.toml`:

```toml
[factory.targets.local]
kind = "docker"
priority = 1
max_sandboxes = 2

[factory.targets.sprites]
kind = "sprites"
priority = 2
max_sandboxes = 4
daily_usd = 5
usd_per_vcpu_hour = 0.0385
usd_per_gb_hour = 0.021875
```

`[factory.compute]` keeps working and means one target, so existing files don't change. Setting both is an error. Listing only `local` is how a factory stays local.

Today `Factory` and `RemoteDaemon` each hold one `Compute`, and these are set once per process: the daemon's directories (`Launch`), the connector list URL, and the model endpoint. They move into a `Target` that also carries its name, priority and limits. The `Compute` trait doesn't change.

**Placement.** For each ready task, in order, take the first target that is available, under its `max_sandboxes` and under its spend cap. If none qualifies, the task waits. Reviews are placed the same way, on their own.

**What stays put.** A parked or reworked task whose sandbox still exists stays on that target, because the sandbox holds its state. The task's `factory` metadata records the target beside `host`.

**Pools.** Each target has its own pool. Names carry the target, because `trailhead-w1` could otherwise exist on both.

**Reconcile.** It lists every target. A target that can't be reached is unknown, not empty: its sandboxes aren't gone and its tasks aren't requeued. Without this, a frozen VM or a stopped engine would look like every local run had died.

**The subscription.** Usage pause applies across targets, because they share one plan. That's the shared host state in TRAIL-168, which comes first. TRAIL-167's many repos reshape the same `Factory` code, so targets should be built to sit under a per-repo `Factory` and leave host-wide state to TRAIL-168.

## Spend

Proposal. Sprites has a usage API: `GET /v1/usage` and `/v1/usage/{sprite}` return `cpu_time_sec` and `reserved_memory_byte_sec`, by time bucket and per Sprite, and need the `usage.read` scope. It reports usage, not dollars, so the rates are config, from [Fly's pricing](https://fly.io/sprites.md) ($0.0385 per vCPU hour and $0.021875 per GB hour on 2026-10-31).

It can't be trusted alone. Checked 2026-10-31 with the factory's token: a 200 with `partial: false`, zero usage, `last_recorded_at: null`, and an empty `/v1/usage/filters/sprites`, for an organization whose Sprites had run the night before. An empty answer looks exactly like nothing spent, so a cap built on it alone would never trip.

So the control plane also keeps its own estimate, and the cap uses the higher of the two: its sandbox time on the target, priced as if the Sprite ran flat out (the billing docs' 8 vCPUs and 4 GB, about $0.40 an hour, or about 12.6 Sprite-hours for $5). That over-counts an idle Sprite, which fails towards spending less. The estimate is kept in a file under the data dir, because the control plane has no database and a restart mustn't reset the day. A day is the machine's local day. Polls stay rare, at most once a minute, like the other usage polls.

At the cap, the target takes no new runs, those running finish, and the log and `factory status` say so. So the cap can be passed by what's already running. It covers compute only: storage and a plan's included allowance aren't counted, so it can differ from the invoice.

The cap is per repo for now. A host-wide cap belongs with TRAIL-168 and TRAIL-169's user config.

## Availability

Proposal. Each target is `available`, `draining` or `frozen`. Only an available one gets new runs. The state lives in a file under the data dir, re-read each pass as the merge mode is, so a command or a signal watcher can change it without talking to the running process.

```sh
trailhead factory yield local            # no new runs; running ones finish
trailhead factory yield local --freeze   # give the RAM back now
trailhead factory reclaim local          # available again
```

A manual `yield` beats the signals until `reclaim`, so a wrong guess is always one command from fixed.

**Signals.** RAM percentage alone is a poor signal, because Linux fills spare memory with cache. In the order to build them:

1. **Schedule:** a night window.
2. **Idle and pressure:** logind's idle hint, `/proc/pressure/memory` and `MemAvailable`.
3. **Games:** GameMode, Steam's child processes or GPU use.

Taking the machine needs every signal quiet for a while, about 10 minutes. Giving it back needs any one of them, within seconds. Lane's machine is a desktop that doesn't sleep, so sleep isn't handled here. On a machine that sleeps, every target stalls, because the control plane is on it.

## Giving the machine back

Stopping containers doesn't free the host's memory: qemu keeps what the guest has touched. Only stopping or suspending the VM does. So there are three levels:

1. **Drain.** No new runs. Costs nothing and frees nothing yet.
2. **Throttle.** `docker update --cpus` applies live and frees CPU at once, but no RAM.
3. **Freeze.** Suspend the VM to disk, if that works (TRAIL-508). Fallback: stop the VM.

**A frozen run stays local.** Its task keeps its claim, so it isn't ready and no other target can pick it up. The factory adds a note on the task saying it's frozen and on which target, and records that in the task's metadata. While frozen:
- The control plane renews the lease, because the daemon that normally does is frozen too.
- Wall-clock budgets, heartbeat checks and stall detection are stopped.
- Reconcile leaves it alone.

On `reclaim`, the VM resumes and the run carries on. A model call cut off by the freeze looks like an API timeout, which the harness already resumes as an infrastructure failure.

If the VM has to be stopped rather than frozen, its runs are lost. Those tasks are requeued without charging an attempt, and they go wherever placement sends them, which may be a Sprite under the cap.

## Open questions

- Does a lima or qemu snapshot free the RAM and resume cleanly? The spike decides whether freezing exists.
- How long may a frozen run wait before it should be stopped and requeued? Leaning: a day.
- Should overflow wait? With two local places, a third queued task spends money at once. Leaning: no wait, since the cap bounds it.
- Why does `/v1/usage` report nothing for this organization, when the dashboard has numbers? Until that's answered, the estimate is what actually enforces the cap.
- Should a target's `priority` be per role, so reviews can prefer Sprites while local builds run?

## Order of work

1. TRAIL-168: shared host state.
2. TRAIL-505: targets, placement and reconcile.
3. TRAIL-506: the spend cap.
4. TRAIL-507: `yield` and `reclaim`.
5. TRAIL-508: the freeze spike.
6. TRAIL-509: freeze or stop, and hold runs.
7. TRAIL-510: signals.

TRAIL-500 and TRAIL-413 are prerequisites for trusting Docker as a default target.
