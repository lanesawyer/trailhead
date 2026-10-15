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
- **Lost claims (Phase 2).** Renewing a claim you no longer hold returns `claim_lost` (409) with what happened (it expired, or who took it) instead of the generic `not_claim_holder`, because a renew is the moment a paused agent finds out. `GET /v1/claims`, `trailhead mine` and the MCP `my_claims` tool list the caller's live claims. Comments and updates from a lapsed holder are still allowed without a warning; renewing is the signal.
- **Bulk revert conflicts (Phase 1).** Stricter than the rule above: a revert skips any entity that *anyone* other than the reverted actor changed afterwards, human or agent, and reports it. Single-event and batch undo fail with `undo_conflict` instead of skipping.
- **Undo model (Phase 1).** Every event stores full before/after snapshots of one entity (`project`, `task`, `claim`, `comment`, `dependency`). Undo restores the snapshot from before the earliest event in the set and logs `<entity>.reverted` with `undoes` set. Undo of an undo works the same way. Comments gained a `deleted_at` so they can be undone.
- **Events (Phase 1).** `events.id` is an autoincrementing integer and serves as the SSE/webhook cursor.
- **Questions (Phase 1).** `resolve_comment` answers a question, optionally posting the answer as a note addressed to the asker. When the last open question resolves, the task returns to `in_progress` if still claimed, otherwise `open`.
- **Priority (Phase 1).** 1 (urgent) to 4 (low), default 3.
- **Review (Phase 1).** With `require_review`, agents cannot move a task to `done`; only humans can move `in_review` to `done`, in every project.
- **Requests and batches (Phase 2).** Every HTTP request is one batch; its id comes back in the `trailhead-batch` header so a client can undo exactly what it just did. `trailhead undo` with no arguments undoes the caller's last batch.
- **Versions in bodies (Phase 2).** `PATCH` accepts `version` in the body as well as `If-Match`, for clients that can't set headers.
- **Creating with dependencies (Phase 2).** `POST /v1/tasks` takes `blocked_by` and `blocks`, so an agent can file a blocking subtask in one call.
- **Local credentials (Phase 2).** On first start the local server bootstraps the workspace and user and saves the user's CLI token to the OS keyring, or a `0600` file in the config directory when no keyring is reachable. `trailhead agent add` on a local server saves the agent's token, and `--as <name>` uses it.
- **Keyring (Phase 2).** The CLI reads a token from `TRAILHEAD_TOKEN`, then the OS keyring, then the `0600` file (`credentials`, or `agents/<name>`), and writes to the keyring when it can, else the file. It uses `keyring` 4 (pure-Rust D-Bus on Linux, so it builds without a secret service); any keyring error at runtime falls through to the file. Entries use service `trailhead:<absolute config dir>` and account `user` or `agent:<name>`, so side-by-side instances and tests never share tokens. `TRAILHEAD_NO_KEYRING=1` turns the keyring off; the CLI tests set it. Saving an agent token to the keyring deletes any older file copy. Existing agent token files are read as they are, not migrated. Nothing writes the `user` keyring entry yet; it is read so a future `login` can use it. The server and CLI share one credential store (`trailhead_server::creds`). At startup the server checks the local user's credential from the keyring or the file. If it's valid but still in a plaintext file, the server moves it into the keyring. It mints a new token only when neither has a valid one.
- **Default identity (Phase 2).** The nearest `.trailhead.toml` at or above the working directory can set `agent = "<name>"` and `project = "<KEY>"`. Identity precedence: `--token`/`TRAILHEAD_TOKEN`, then `--as`/`TRAILHEAD_AGENT`, then the file's `agent`, then the user's credential. `--as-me` skips `TRAILHEAD_AGENT` and the file. The file's `project` is the default `-p` for `ready`, `ls` and `add`, after `-p`/`TRAILHEAD_PROJECT`. Unknown keys are an error so typos don't silently fall back to acting as the user.
- **Dates in the CLI (Phase 2).** A date-only due date means the end of that day in the local zone, using that day's UTC offset (DST included), and the CLI records the zone's IANA name in `due_tz`. The CLI uses `jiff` and the system zoneinfo for this and for displaying local times. The server rejects a `due_tz` that isn't a known IANA zone in the system's zoneinfo, checked with `jiff`.
- **Local server lifecycle (Phase 2).** `/v1/health` reports the server's pid, binary path and that binary's mtime at startup. When the CLI finds a local server running from its own path but the file on disk is newer, it stops the server with SIGTERM (which drains in-flight requests) and starts a fresh one. A server running a different binary is left alone, so dev builds and the installed CLI don't fight. The pid is also kept in `server.pid` in the data directory.
- **MCP (Phase 2).** `/mcp` uses rmcp's streamable HTTP transport in stateless mode with plain JSON responses, behind the same bearer tokens as REST. Each tool call is its own undo batch. Tools answer in the CLI's terse one-line-per-task text rather than JSON, and domain errors come back as tool errors (`error[code]: message`) the agent can act on instead of protocol failures. Project briefs are resources at `trailhead://projects/<KEY>/brief`. Agents can `undo_last`, which undoes only their own most recent batch with the usual conflict check. Revert, other people's batches, agents and tokens stay REST/CLI-only.
- **Event stream (Phase 2).** `GET /v1/events/stream` pages through the events table by id, so `Last-Event-ID` resumes with nothing missed. A commit signal inside the process wakes streams immediately, and a 2-second poll covers writes from other processes. Without a cursor the stream starts at the newest event. On shutdown the server ends open streams and allows in-flight requests 10 seconds before exiting. `trailhead watch` follows the stream and reconnects on its own.
- **MCP over stdio (Phase 2).** `trailhead mcp` bridges stdio to the server's `/mcp`, one POST per JSON-RPC message, authenticated with the CLI's credentials or `--as <agent>`. MCP clients that launch a command (Claude Code's `.mcp.json`) can then connect with no token in their config, and they get the CLI's server auto-start.
- **Idempotency keys (Phase 2).** Non-GET `/v1` requests may send `Idempotency-Key` (1-255 characters). Keys are scoped to the token that sent them, so two agents, or two tokens of the same agent with different scopes, never share stored responses. Replays still count against the token's rate limit. The request's fingerprint is its method, path with query, and a SHA-256 of the body. A middleware outside the per-request batch layer reserves the key before running the handler: a retry of the same request replays the stored status, body, `ETag`, `Location` and the original `trailhead-batch` with `idempotent-replayed: true`; the same key on a different request gets `422 idempotency_key_reused`; a duplicate while the first is still running gets `409 idempotency_key_in_flight` rather than waiting. Only 2xx responses are stored, so a failed request can be retried with its key. An unfinished reservation is released if the request is dropped and treated as abandoned after 5 minutes (a crashed server). Keys live 24 hours from first use and are pruned lazily on the next keyed request. The CLI doesn't retry writes, so it sends no key. Over MCP, `create_task` and `comment` take an optional `idempotency_key` argument that works the same way, stored in the same table under a `mcp/<tool>` namespace per token; the other tools already give the same result when repeated.
- **OpenAPI (Phase 2).** `GET /v1/openapi.json` serves an OpenAPI 3.1 document generated by `utoipa` from `#[utoipa::path]` annotations on the handlers, unauthenticated. Core model types derive `ToSchema` behind an optional `openapi` feature on `trailhead-core`, which the server enables. The bearer scheme, the `trailhead-batch` header and the 400/401/500 error responses are added to every operation by one modifier rather than repeated per handler; route-specific errors, `ETag` and `If-Match` are annotated on each route. The error `code` enum is a list kept in step with `Error::code()` by a unit test, and a test fails when a route is added without being documented. No Swagger UI.
- **Token scopes (Phase 2).** A token may be limited to a set of projects (`tokens.projects`, a JSON array of ids; NULL or empty means all). The scope rides on `Ctx.projects` and is enforced in core, so REST, MCP and SSE all get it. Reads of anything outside the scope (tasks, projects, comments, events, ready, search, undoing someone else's event) answer `not_found`, as do writes that name an out-of-scope task, so a scoped token can't probe for existence. Writes that name an out-of-scope project outright (creating a task in it, updating it) are `forbidden`, because the caller already knows the project. Scoped tokens can't create projects, agents or tokens. Parents and dependencies must sit entirely inside the scope; existing cross-project blockers still keep tasks out of `ready`, and show up as `(out of scope)` in `blocked_by` and `blocked_by` errors so an agent knows why it's stuck without learning what blocks it. A scoped token can undo or revert only events that touch nothing outside its scope (`forbidden` otherwise).
- **Rate limits (Phase 2).** `tokens.rate_limit_per_minute` (NULL means unlimited) is enforced in the server, on REST and `/mcp` alike, by an in-memory token bucket per token id: a full minute's burst, refilling continuously. Over the limit is `429` with code `rate_limited` and `Retry-After` in seconds. Buckets reset when the server restarts and aren't shared between processes; a hosted multi-process deploy needs a shared store. Limits are set when a token is issued (`POST /v1/tokens`, `POST /v1/agents`, `trailhead agent add --projects A,B --rate-limit N`) and can't be changed afterwards; issue a new token and revoke the old one.
- **Events archive (Phase 2).** `Store::archive_events` moves events older than 90 days (by the Store clock) to `events_archive`, same columns and ids, in batches of 500 with one transaction each and at most 20 batches per call; the local server runs it hourly. It moves only a prefix of `events` by id, stopping at the first event inside the window, so an event still in `events` never has a later event in the archive and undo's conflict check keeps reading `events` alone. `list_events` and SSE read only `events`; `GET /v1/tasks/{task}/history?archived=true` also reads the archive. Undo of an event or batch older than 90 days, archived or not, fails with `409 undo_window_expired`. Bulk revert with a `since` older than 90 days is clamped to the window rather than failing, since "revert everything this agent did" is the common call.
- **Reminders (Phase 2).** `Store::emit_due_reminders` logs `task.due_soon` once an open task (not done, cancelled or deleted, in an active project) is within 24 hours of `due_at`, and `task.overdue` once `due_at` passes; a task first seen after its due date gets only `task.overdue`. The local server runs it every 60 seconds next to the claim reaper. `reminders_sent` records the `due_at` each reminder fired for, so each fires once per due date across restarts and fires again when the due date moves. Reminders are attributed to the workspace's first user (the owner), since no one acted and the task's creator may be an agent. They are notifications, not changes: their before and after are the same snapshot, undo and revert skip them, they never count as undo conflicts, and `changes_only` on `GET /v1/events` hides them so "undo my last change" in the CLI and MCP doesn't land on a reminder.

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
