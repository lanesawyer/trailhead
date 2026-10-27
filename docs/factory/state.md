# Run state and Trailhead access

Decided: the control plane keeps no database of its own. Run state lives in Trailhead and the Sprites API, and on restart the control plane rebuilds its view from those plus reconnecting daemons. This replaced an earlier plan for a SQLite event log in the control plane.

## Where state lives

| State | Where |
| --- | --- |
| Task status, ownership, lease | Trailhead claims and statuses |
| Sandbox name, PR URL, profile, tier, attempt and escalation count, spend so far | Trailhead task `metadata` |
| Live sandboxes, per-repo concurrency count | Sprites API, by labels (`repo`, `task`, `role`) |
| Checkpoints, including the per-repo warm checkpoint | Sprites API, lockfile hash in the checkpoint comment |
| Spend within the current step | The daemon, re-reported on reconnect |
| Sub-agent to revoke when a task ends | Trailhead (agent named after the task) |
| Global daily spend | Provider usage API ([budgets.md](budgets.md)) |

## Rules that make it safe

- **Deterministic names.** A task's sandbox and sub-agent are named from the task key and attempt (`trail-42-a1`), so a crash between creating a sandbox and recording it leaves something the reconciler can find.
- **Idempotency keys** on every Trailhead write, so a retry after a crash doesn't double-count spend.
- **Reconcile on boot.** List live sandboxes by label, list tasks with factory metadata, match by name, destroy orphans, requeue tasks whose sandbox is gone.
- **Metadata per step, not per token.** Each write bumps the task version and emits an event.

## Trailhead access

Trailhead already has the service layer the factory needs: REST under `/v1`, MCP at `/mcp`, SSE and webhooks, behind per-agent bearer tokens that can be scoped to projects and rate-limited. Agents create sub-agents (`<parent>/<name>`) whose tokens inherit their scope; revoking an agent revokes everything under it.

- **Identity.** The control plane acts as one agent and creates a sub-agent per sandbox, scoped to the repo's project, revoked when the task finishes.
- **Orchestration.** The control plane follows Trailhead's events in-process (it's the same server when the factory is built in).
- **Reaching the server.** Trailhead is one server and one SQLite file wherever it runs, reached from other machines through a tunnel to its loopback port, iroh via `dumbpipe` first (`docs/PLAN.md`, "Remote access"). Only ticket holders can connect; bearer tokens decide what they can do. A sandbox runs the tunnel client and talks to Trailhead on loopback, so its egress needs only the iroh relays it already has. Turso and hosted mode are deferred.
- **Server asleep.** If the server's machine sleeps, sandboxes wait and leases expire as usual, so nothing is lost; budgets should treat that time as stalled, not spent.
- **Token in the sandbox.** A Custom API connector would inject one token for every sandbox and lose per-sandbox identity, so each sandbox holds its own short-lived, project-scoped sub-agent token, passed in by the control plane at start. It's the one credential a sandbox holds directly.

```toml
[trailhead]
ticket = "<dumbpipe ticket for the server>"
```
