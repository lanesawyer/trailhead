# Run state and Trailhead access

> **Design note.** It records decisions and intent; where it says something is built or not, check the code if it matters.

Decided: the control plane keeps no database of its own. Run state lives in Trailhead and the Sprites API, and on restart the control plane rebuilds its view from those plus reconnecting daemons. This replaced an earlier plan for a SQLite event log in the control plane.

## Where state lives

| State | Where |
| --- | --- |
| Task status, ownership, lease | Trailhead claims and statuses |
| Sandbox name, PR URL, profile, grade, attempt and escalation count, spend so far | Trailhead task `metadata` |
| Live sandboxes, per-repo concurrency count | Sprites API, by labels (`repo`, `task`, `role`) |
| Pooled worker Sprites and their warm checkpoints | Sprites API, labels (`pool`, `state`, `warm`) |
| Spend within the current step | The daemon, re-reported on reconnect |
| Sub-agent to revoke when a task ends | Trailhead (agent named after the task) |
| Global daily spend | Provider usage API ([budgets.md](budgets.md)) |

On Docker (TRAIL-119), where the table says Sprites API read the Docker engine for which sandboxes exist, plus the Docker Compute's state files (`<data dir>/factory/docker/<name>.json`) for their labels, since Docker labels can't change after creation. Those files belong to the `Compute`, not the control plane: it still keeps no database, and a lost file leaves that container unlabelled, so reconcile no longer sees it and it stays until removed by hand (`docker ps --filter label=dev.trailhead.sandbox`).

## Rules that make it safe

- **Deterministic names.** A task's sandbox and sub-agent are named from the task key and attempt (`trail-42-a1`), so a crash between creating a sandbox and recording it leaves something the reconciler can find.
- **Idempotency keys** on every Trailhead write, so a retry after a crash doesn't double-count spend.
- **Reconcile on boot.** List live sandboxes by label, list tasks with factory metadata, match by name, destroy orphans, requeue tasks whose sandbox is gone.
- **Metadata per step, not per token.** Each write bumps the task version and emits an event.

## Trailhead access

Trailhead already has the service layer the factory needs: REST under `/v1`, MCP at `/mcp`, SSE and webhooks, behind per-agent bearer tokens that can be scoped to projects and rate-limited. Agents create sub-agents (`<parent>/<name>`) whose tokens inherit their scope; revoking an agent revokes everything under it.

- **Identity.** The control plane acts as one agent (`factory`, which Lane creates with `trailhead --as-me agent add factory`) and creates a sub-agent per sandbox, scoped to the repo's project, revoked when the task finishes.
- **Keys.** The control plane's iroh key and the master key its daemons' keys derive from are created once and kept in the credential store (`secret:factory-keys`: keyring, or a `0600` file). Losing them strands running sandboxes, which reconcile then requeues.
- **Orchestration.** The control plane follows Trailhead's events in-process (it's the same server when the factory is built in).
- **Reaching the server.** Trailhead is one server and one SQLite file wherever it runs, reached from other machines through a tunnel to its loopback port (`docs/PLAN.md`, "Remote access"). People and outside agents use `dumbpipe`. Decided in M4 (2026-10-27), replacing a `dumbpipe` client and ticket in each sandbox: sandboxes use an iroh API tunnel instead. The control plane serves `trailhead/api/0` on its iroh endpoint and forwards each stream to the server's loopback port. It lets in only daemons it started, and the server still checks every bearer token. The daemon exposes the tunnel as a loopback port, and its REST `Tracker` talks to `/v1` through it. A sandbox holds no ticket and needs no egress beyond the iroh relays it already has. Its Trailhead access lives and dies with the control plane, which hosts the server anyway. Turso and hosted mode are deferred.
- **Server asleep.** If the server's machine sleeps, sandboxes wait and leases expire as usual, so nothing is lost; budgets should treat that time as stalled, not spent.
- **Token in the sandbox.** A Custom API connector would inject one token for every sandbox and lose per-sandbox identity, so each sandbox holds its own short-lived, project-scoped sub-agent token, passed in by the control plane at start. It's the one credential a sandbox holds directly.


