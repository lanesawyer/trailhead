# Cost and time controls

> **Design note.** It records decisions and intent; where it says something is built or not, check the code if it matters.

The control plane enforces budgets. Token and dollar counts come from the daemon's usage events; wall-clock, sandbox-hours and step counts from its own tracking. A hard stop removes the sandbox's connector label. Connector spend also rolls into the Sprites bill.

| Scope | Limits | On breach |
| --- | --- | --- |
| Step | Tokens, wall-clock | Fail the step; restore checkpoint; retry or escalate |
| Run | Dollars, wall-clock, step count (the profile's budget, per run) | Checkpoint, suspend the sandbox, park the task for a human |
| Task | `max_attempts`, `max_review_rounds` | Park the task for a human |
| Global | Dollars per day, concurrent sandboxes | Stop starting new runs; running ones finish their step |

- **Per run, not per task.** Dollar, wall-clock and step budgets are checked against one run's own spend, not the task's running total across attempts, reworks and reviews. A task's total is bounded by `max_attempts` and `max_review_rounds` times the per-run budgets, plus the runs that count toward neither limit: transient failures, reviewer infrastructure failures, and conflict reworks (bounded by `max_conflict_reworks`). A task-level dollar cap (say `[repo] max_task_usd`, checked against prior plus current spend) is not built; adding one is a policy decision for a human.
- **Stall detection.** No progress event for a set window counts as a breach.
- **Suspend, don't destroy.** A breached run keeps its sandbox and checkpoint so a human can resume it. The exception is a reviewer: its sandbox is destroyed before the run is matched, so a reviewer breach has nothing to resume. The task is parked with a question instead, and reopening it reviews the PR afresh ([review.md](review.md#what-counts-as-a-review-round)).
- **Numbers TBD.** Start conservative and record real spend per task type before loosening. Raising a budget is a policy loosening and needs a human ([review.md](review.md)).

## Where spend numbers come from

| Layer | Source | Freshness | Used for |
| --- | --- | --- | --- |
| Per task | `usage` on each Messages response, counted by the daemon | Immediate | Run budgets |
| Global | Provider usage API, polled at most once a minute | About 5 minutes behind | Daily cap; reconciling daemon counts |
| Backstop | Spend limit on a dedicated factory workspace at the provider | Enforced by the provider | Hard ceiling if the factory's accounting fails |
| Subscription | The harness's usage-window readings (Claude Code's `rate_limit_event`) | Once per agent session | Pausing new runs before a shared subscription runs out |

- **Anthropic.** The [Usage and Cost API](https://platform.claude.com/docs/en/build-with-claude/usage-cost-api) reports usage in 1-minute buckets, filterable by API key and workspace, typically within 5 minutes. The cost endpoint is daily only and omits Priority Tier, so minute-level dollars come from pricing usage tokens. It needs an Admin API key (or an OAuth token with `org:admin`), which can also manage the organization: the one powerful credential the control plane holds. The connector forwards only the Messages API, so this call goes from the control plane directly. Store the key in the OS keyring.
- **Dedicated workspace and key.** The connector's key belongs to a factory-only workspace, so usage numbers are the factory's alone and that workspace's spend limit is the backstop. Confirm per-workspace spend limits in the Console.
- **Lag means overshoot.** Five minutes of every running sandbox's spend can land after the daily cap is hit, which is why per-run budgets come from the daemon in real time.
- **Provider-agnostic.** `ModelProvider::spend` ([seams.md](seams.md)) wraps each provider's usage API.

## Subscription usage windows

Decided 2026-10-28 (TRAIL-160). In subscription mode, the factory shares the maintainer's Claude plan with everything else on it (OpenClaw), so it mustn't use a window up. Claude Code starts every `stream-json` session with a `rate_limit_event` that reports each window's utilization and reset time (`five_hour`, `seven_day`). This was observed, not documented, so a missing or changed event means no reading and no pause.

- The harness reports each window as `RunEvent::Quota { window, used_percent, resets_at }`, and the control plane keeps the latest reading in memory.
- Subscription usage is one pool for the whole machine, so the pause thresholds are host-wide and belong in the user `config.toml` (next bullet). `[factory.usage] pause_at` in a repo's `.trailhead.toml` sets a threshold per window, in percent. At or past it, no new runs start; runs already going finish. The control plane logs the pause once, then sets a timer for the reset and starts runs again when it fires.
- `pause_at` also goes in `[factory]` of the per-machine `<config dir>/config.toml`, alongside `max_sandboxes` and `repos`. That value wins per window; the repo-level `[factory.usage] pause_at` still works but is deprecated and logs a warning, and where repos disagree the strictest applies.
- Readings arrive only while something runs, and only once per session, so a long run's reading goes stale. Leave margin in the threshold.
- A run that hits the limit mid-way fails and is requeued, and the pause holds it until the reset. It still uses up an attempt (TRAIL-159).
- Any harness can report the same event; Codex CLI reports similar windows.
