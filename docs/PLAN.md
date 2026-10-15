# Trailhead — Project Plan

## Vision

Trailhead is a fast, headless task engine that humans and agents share. It owns the task data and the rules around it. Every interface, including any UI, is a client.

It is not a todo app. People use it three ways:

- **Build on it.** Vibe code a personal todo app, or a task widget inside a bigger app, against its API.
- **Talk to it through an agent.** The agent reads, claims and completes tasks, and tells the person what's next.
- **Plug it into agent platforms.** OpenClaw first, other platforms after.

There is no official Trailhead app at the start. Lane's own app is the first real client and the dogfood test.

The long-term ambition is to replace other task tools, not to sync with them.

## Principles

1. **The API is the product.** Contracts are versioned, stable and documented well enough that an agent can build a client from the docs alone.
2. **Agents act for people.** Every agent belongs to a user. There are no independent agents, and every change traces back to a human.
3. **Nothing an agent does is permanent.** Every write is logged and reversible, including bulk reverts.
4. **Agents pay for every token.** Responses are compact by default, with full detail on request.
5. **One deployment, one mode.** An instance is either local and personal, or hosted and shared. Nothing syncs between them.
6. **Trailhead is the source of truth.** Other tools can be notified or imported from, never synced with.

## License

Everything is **AGPL-3.0**: the server, CLI, MCP server, SDKs and plugins. Client libraries included.

## Architecture

One Rust workspace producing one binary, built around a core crate that every interface shares.

```
 Your app        Agents        OpenClaw plugin     trailhead CLI
 (REST + SSE)    (MCP)         (REST)              (REST, auto-starts server)
      \             |               |                 /
       +------------+---------------+----------------+
                            |
              trailhead-server (axum)
      REST /v1 · MCP (rmcp) · SSE events · outbound webhooks
                            |
                    trailhead-core
   domain rules · claims/leases · ready queue · event log, undo, outbox
                 /                          \
         libsql (local)               libsql (remote)
     Local SQLite file               Turso Cloud
     (one user, one workspace)       (teams, server on Fly.io)
```

A deployment uses one storage mode, chosen by config. The CLI is a REST client of the server in every mode. Run locally, it starts `trailhead serve` in the background if no server is running.

| Crate | Role | Key dependencies |
| --- | --- | --- |
| `trailhead-core` | Schema, domain logic, storage trait, event log | `libsql`, `serde` |
| `trailhead-server` | REST, MCP, SSE, webhooks | `axum`, `tokio`, `utoipa`, `rmcp` |
| `trailhead` (CLI) | REST client for people and shell-capable agents; auto-starts a local server | `clap`, `reqwest` |

| Interface | Best for |
| --- | --- |
| CLI | Lane, and agents with shell access, local or hosted |
| MCP | Agents without a shell, such as chat clients and OpenClaw |
| REST | Apps and widgets people build |

**Storage.** Trailhead targets SQLite through a small storage trait in `trailhead-core`. The first implementation uses the `libsql` crate, which opens plain local SQLite files and, with its `remote` feature, Turso Cloud over HTTP. Local mode is a SQLite file in WAL mode with no Turso involvement. Hosted mode runs the server on Fly.io against Turso, in the same region.

Turso is a vendor risk, so nothing may depend on it beyond the storage trait. Queries use standard SQLite SQL only. If Turso goes away, hosted mode falls back to a SQLite file on a Fly.io volume, or swaps the backend for `rusqlite` or `sqlx`. Turso's newer `turso` crate is skipped for the same reason.

**Atomic writes.** Every state change is one atomic statement, such as a claim as a single `UPDATE … WHERE unclaimed … RETURNING`.

**MCP.** `rmcp`, the official Rust MCP SDK, mounts on the same axum router at `/mcp`. It is pre-1.0, so expect API churn.

**Events.** SSE and webhooks are driven from the events table (outbox pattern), not in-memory notifications. Delivery survives a crash or restart.

## Data model

Every record carries a `workspace_id`, and every write records the actor and the user it acted for. A local install is one workspace with one user, so team support needs no migration later.

| Entity | Purpose | Key fields |
| --- | --- | --- |
| Workspace | Tenancy boundary | `id`, `name`, `default_claim_ttl` |
| User | A human | `id`, `name`, `email` |
| Agent | Acts for exactly one user | `id`, `owner_user_id` (required), `name` |
| Token | Credential for a user or agent, hashed at rest | `id`, `principal` (user or agent), `scopes` (can be limited to projects), `revoked_at`, `rate_limit` |
| Project | Groups tasks; holds the context agents read before working | `id`, `key` (e.g. `AUTH`), `name`, `brief`, `status`, `metadata` (JSON) |
| Task | The unit of work | `id` (UUIDv7), `key` (e.g. `AUTH-42`), `project_id`, `title`, `description`, `status`, `priority`, `due_at` + `due_tz`, `parent_id`, `metadata` (JSON), `version`, `source`, `external_id`, `deleted_at` |
| Dependency | Task A blocks task B, across projects too | `blocker_id`, `blocked_id` |
| Claim | A lease on a task | `task_id`, `agent_id`, `expires_at` |
| Comment | Typed discussion, handoffs and questions on a task | `id`, `task_id`, `author_id`, `on_behalf_of`, `kind`, `addressed_to`, `body`, `resolved_at`, `created_at` |
| Event | Append-only log of every write | `id`, `batch_id`, `actor_id`, `on_behalf_of`, `task_id`, `kind`, `before`, `after`, `at` |
| Webhook | Outbound notification | `url`, `event_kinds`, `secret` |

- **`metadata`** is free-form JSON so builders can add fields without forking the schema.
- **`version`** increments on every write. It is the ETag.
- **`deleted_at`** makes deletes soft, so they can always be undone.
- **`source` and `external_id`** exist only so a rerun import doesn't create duplicates.
- **`description` vs comments.** The description is the task's current spec and gets edited. Comments are the append-only history of the work.

## Projects

Every task belongs to exactly one project, and projects don't nest.

- Each project has a `brief`: goals, conventions and a definition of done that agents read before claiming work. MCP also serves it as a resource.
- `ready` and tokens can be scoped to a project.
- Claim TTL and other defaults can be overridden per project.
- Task dependencies can cross projects.

## Agent-native features

**Claims and leases.** An agent claims a task with a TTL: 30 minutes by default, configurable per workspace, project and claim, capped at 24 hours. The agent renews with a heartbeat about every third of the TTL. If it crashes, the task frees within the TTL. A claim is one atomic statement, so two agents can never hold the same task.

**Typed comments.** Each comment has a `kind`:

- `handoff`: what was done, what's left, gotchas. The latest one is returned with the task when the next agent claims it.
- `progress`: short status updates while working.
- `question`: needs an answer, addressed to a human or another agent. Marks the task `needs_input` and removes it from the ready queue until answered.
- `decision`: a choice made and why, so later agents don't relitigate it.
- `note`: anything else. The default.

**Agents helping agents.** For real work, an agent creates a subtask that blocks its own task; the original becomes ready again once the subtask is done. For a quick question, it uses a `question` comment addressed to the other agent.

**Statuses.** `open`, `in_progress`, `needs_input`, `in_review`, `done`, `cancelled`, with transitions enforced in core. `in_review` means an agent finished and someone must approve before `done`. Review can be required per project. "Blocked" is computed from dependencies, never stored.

**Ready queue.** `ready` returns open, unclaimed tasks with no unfinished blockers, sorted by priority then due date.

**Undo and revert.** Every write logs before and after state. Undo is a new write that restores the earlier state.

- Undo one event.
- Undo a batch: everything from one request or MCP call.
- Revert everything agent X did since time T.
- Conflict rule: a bulk revert skips any task a human edited after the agent did, and reports it.

**Idempotency keys.** Writes accept a key, so retries don't create duplicates.

**Compact output.** JSON is the canonical wire format. List endpoints accept `?fields=`. MCP and CLI default to a terse one-line-per-task layout, with `--json` for full detail.

## API contracts

- **Versioning.** All REST routes under `/v1` from the first commit.
- **Concurrency.** Task `version` exposed as `ETag`; writes send `If-Match`; stale gets `412`. MCP and CLI carry `version` in the body.
- **Errors.** Stable error codes such as `claim_held`, `version_conflict`, `blocked_by`.
- **Metadata.** JSON `metadata` on tasks and projects, filterable by key.
- **Time.** System timestamps are UTC. Due dates are a datetime plus an IANA time zone.
- **Events.** Every write emits an event, over SSE and outbound webhooks.
- **Reminders.** A scheduler emits `task.due_soon` events. Agents can also query `due_before=`.
- **Auth.** Bearer tokens per user and per agent, scoped and revocable. CORS off by default.
- **Portability.** JSONL export and import.

## Operational decisions

- **Task IDs.** UUIDv7 internally, plus a per-project key like `AUTH-42` used everywhere.
- **Search.** SQLite FTS5 over titles, descriptions and comments. Turso Cloud lists FTS5 as built-in. The first spike confirms the local `libsql` build has it; if not, local search falls back to `LIKE`.
- **Stopping a bad agent.** Per-token rate limits, instant revocation, bulk revert.
- **Migrations.** No ORM. Numbered `.sql` files embedded in the binary, applied at startup by a small in-house runner with a `schema_migrations` table, each migration in a transaction.
- **Event retention.** Events kept forever. Events older than 90 days move to `events_archive`. Undo and bulk revert cover the last 90 days.
- **Config and data location.** OS-standard directories via the `directories` crate.
- **CLI credentials.** Like `gh`: OS credential store via `keyring`, falling back to a `0600` file in the config directory. `TRAILHEAD_TOKEN` overrides both.
- **Local server.** `127.0.0.1:7241` by default, configurable (T-R-A-I/L; no IANA assignment). Stays running once auto-started. `trailhead service install` for launchd/systemd later.
- **Backups.** Not built in. Local is one SQLite file; Turso handles hosted.
- **Observability.** `tracing` structured logs carrying request and batch IDs.
- **Testing.** Concurrency tests in the core suite and CI: agents racing to claim one task, concurrent edits hitting ETag conflicts, bulk reverts racing new writes.

## Phases

1. **Core, local.** Schema and migrations, domain logic, projects, typed comments, event log with undo and outbox, claims and the ready queue, with tests.
2. **Server and CLI.** axum REST under `/v1`, OpenAPI via utoipa, SSE, MCP via rmcp, token auth, reminders. CLI as a REST client that auto-starts a local server.
3. **Clients.** Lane's own app, an OpenClaw plugin, `llms.txt` and an agent skill, webhooks. TypeScript SDK from OpenAPI later.
4. **Teams.** Fly.io + Turso, multiple workspaces, user accounts, per-agent tokens, permissions.
5. **Importers.** One-way, one-time imports from Todoist, Vikunja, GitHub Issues, Jira.

### Phase 1 first steps

1. Spike: create a local libsql database, check FTS5, run the first migration.
2. Schema: workspaces, users, agents, projects, tasks, comments, claims, events.
3. Atomic claim and ready-queue query, concurrency test written first.

## Decisions log

Decisions made while building, including where the build refines or departs from the text above.

- **Storage seam (Phase 1).** The seam is the `Store` API rather than a Rust trait: libsql types never leave `trailhead-core`, and only `Store::open_local`/`open_remote` know which backend is in use. Swapping backends means rewriting the crate's query layer, not its callers. Revisit if a second backend actually lands.
- **Write serialization (Phase 1).** All writes go through one connection behind an async mutex, each in a `BEGIN IMMEDIATE` transaction. Reads use a pool of connections, one per in-flight read, because a connection shared by concurrent reads leaks one read's snapshot into another.
- **Claims (Phase 1).** A claim has `holder_id` (a user or an agent) and `on_behalf_of`, not `agent_id`, so a human can claim from the CLI. An agent cannot edit, release or finish a task another actor holds; a human can, so people can always rescue work from a stuck agent.
- **Lease expiry (Phase 1).** Expired claims are reaped lazily by `claim` and `ready`, and by `reap_expired_claims` for a server timer. Reaping moves an `in_progress` task back to `open` and is logged as the lapsed holder.
- **Bulk revert conflicts (Phase 1).** Stricter than the rule above: a revert skips any entity that *anyone* other than the reverted actor changed afterwards, human or agent, and reports it. Single-event and batch undo fail with `undo_conflict` instead of skipping.
- **Undo model (Phase 1).** Every event stores full before/after snapshots of one entity (`project`, `task`, `claim`, `comment`, `dependency`). Undo restores the snapshot from before the earliest event in the set and logs `<entity>.reverted` with `undoes` set. Undo of an undo works the same way. Comments gained a `deleted_at` so they can be undone.
- **Events (Phase 1).** `events.id` is an autoincrementing integer and serves as the SSE/webhook cursor.
- **Questions (Phase 1).** `resolve_comment` answers a question, optionally posting the answer as a note addressed to the asker. When the last open question resolves, the task returns to `in_progress` if still claimed, otherwise `open`.
- **Priority (Phase 1).** 1 (urgent) to 4 (low), default 3.
- **Review (Phase 1).** With `require_review`, agents cannot move a task to `done`; only humans can move `in_review` to `done`, in every project.
- **Requests and batches (Phase 2).** Every HTTP request is one batch; its id comes back in the `trailhead-batch` header so a client can undo exactly what it just did. `trailhead undo` with no arguments undoes the caller's last batch.
- **Versions in bodies (Phase 2).** `PATCH` accepts `version` in the body as well as `If-Match`, for clients that can't set headers.
- **Creating with dependencies (Phase 2).** `POST /v1/tasks` takes `blocked_by` and `blocks`, so an agent can file a blocking subtask in one call.
- **Local credentials (Phase 2).** On first start the local server bootstraps the workspace and user and writes the user's CLI token to a `0600` file in the config directory. `trailhead agent add` on a local server saves the agent's token under `agents/<name>` there, and `--as <name>` uses it. OS keyring support comes later.
- **Dates in the CLI (Phase 2).** A date-only due date means the end of that day in local time. `due_tz` isn't set yet.
- **Deferred to Phase 2.** Idempotency keys (stored and replayed at the server layer), `task.due_soon` reminders, `events_archive` and the 90-day undo window, token scopes and rate limits.

## Non-goals

- An official Trailhead app, for now.
- Two-way sync with any other tool.
- Syncing a local instance with a hosted one.
- Browser clients calling the API directly.
- Agents that act without an owning user.

## Later

- Recurring tasks (iCalendar RRULE plus a time zone).
- Project-level dependencies.
- TOON as an optional compact output format.
