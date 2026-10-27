# Cost and time controls

The control plane enforces budgets. Token and dollar counts come from the daemon's usage events; wall-clock, sandbox-hours and step counts from its own tracking. A hard stop removes the sandbox's connector label. Connector spend also rolls into the Sprites bill.

| Scope | Limits | On breach |
| --- | --- | --- |
| Step | Tokens, wall-clock | Fail the step; restore checkpoint; retry or escalate |
| Task | Dollars, wall-clock, step count, review rounds | Checkpoint, suspend the sandbox, park the task for a human |
| Global | Dollars per day, concurrent sandboxes | Stop starting new runs; running ones finish their step |

- **Stall detection.** No progress event for a set window counts as a breach.
- **Suspend, don't destroy.** A breached run keeps its sandbox and checkpoint so a human can resume it.
- **Numbers TBD.** Start conservative and record real spend per task type before loosening. Raising a budget is a policy loosening and needs a human ([review.md](review.md)).

## Where spend numbers come from

| Layer | Source | Freshness | Used for |
| --- | --- | --- | --- |
| Per task | `usage` on each Messages response, counted by the daemon | Immediate | Task and step budgets |
| Global | Provider usage API, polled at most once a minute | About 5 minutes behind | Daily cap; reconciling daemon counts |
| Backstop | Spend limit on a dedicated factory workspace at the provider | Enforced by the provider | Hard ceiling if the factory's accounting fails |

- **Anthropic.** The [Usage and Cost API](https://platform.claude.com/docs/en/build-with-claude/usage-cost-api) reports usage in 1-minute buckets, filterable by API key and workspace, typically within 5 minutes. The cost endpoint is daily only and omits Priority Tier, so minute-level dollars come from pricing usage tokens. It needs an Admin API key (or an OAuth token with `org:admin`), which can also manage the organization: the one powerful credential the control plane holds. The connector forwards only the Messages API, so this call goes from the control plane directly. Store the key in the OS keyring.
- **Dedicated workspace and key.** The connector's key belongs to a factory-only workspace, so usage numbers are the factory's alone and that workspace's spend limit is the backstop. Confirm per-workspace spend limits in the Console.
- **Lag means overshoot.** Five minutes of every running sandbox's spend can land after the daily cap is hit, which is why per-task budgets come from the daemon in real time.
- **Provider-agnostic.** `ModelProvider::spend` ([seams.md](seams.md)) wraps each provider's usage API.
