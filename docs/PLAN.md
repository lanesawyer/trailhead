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
5. **One server, reachable anywhere.** An instance is one server and one SQLite file, the single authority for its data. It runs wherever is convenient (Lane's PC, a home box, a VPS) and agents on other machines reach it over a tunnel. Trailhead assumes it is online and connected; there are no replicas and nothing syncs between instances.
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
                            |
                     SQLite file (WAL)
```

Remote machines reach the server through a tunnel (iroh via `dumbpipe`, Tailscale, SSH), never by binding it to a public interface. The CLI is a REST client of the server wherever it runs. Pointed at its own machine's server, it starts `trailhead serve` in the background if none is running.

| Crate | Role | Key dependencies |
| --- | --- | --- |
| `trailhead-core` | Schema, domain logic, storage trait, event log | `libsql`, `serde` |
| `trailhead-server` | REST, MCP, SSE, webhooks | `axum`, `tokio`, `utoipa`, `rmcp` |
| `trailhead` (CLI) | REST client for people and shell-capable agents; auto-starts a local server | `clap`, `reqwest` |

| Interface | Best for |
| --- | --- |
| CLI | Lane, and agents with shell access, on any machine |
| MCP | Agents without a shell, such as chat clients and OpenClaw |
| REST | Apps and widgets people build |

**Storage.** Trailhead targets SQLite through a small storage trait in `trailhead-core`, implemented with the `libsql` crate over a plain SQLite file in WAL mode. The server is one process, so the file is all the storage it needs wherever it runs. `libsql`'s `remote` feature (Turso Cloud) stays behind the storage seam but is deferred: it is only worth using if a deployment ever needs more than one server process.

Queries use standard SQLite SQL only, so the backend can be swapped for `rusqlite` or `sqlx`. Turso's newer `turso` crate is skipped.

**Atomic writes.** Every state change is one atomic statement, such as a claim as a single `UPDATE … WHERE unclaimed … RETURNING`.

**MCP.** `rmcp`, the official Rust MCP SDK, mounts on the same axum router at `/mcp`. It is pre-1.0, so expect API churn.

**Events.** SSE and webhooks are driven from the events table (outbox pattern), not in-memory notifications. Delivery survives a crash or restart.

## Data model

Every record carries a `workspace_id`, and every write records the actor and the user it acted for. A local install is one workspace with one user, so team support needs no migration later.

| Entity | Purpose | Key fields |
| --- | --- | --- |
| Workspace | Tenancy boundary | `id`, `name`, `default_claim_ttl` |
| User | A human | `id`, `name`, `email` |
| Agent | Acts for exactly one user; may be a sub-agent of another agent | `id`, `owner_user_id` (required), `parent_agent_id`, `name`, `revoked_at` |
| Token | Credential for a user or agent, hashed at rest | `id`, `principal` (user or agent), `scopes` (can be limited to projects), `revoked_at`, `rate_limit` |
| Project | Groups tasks; holds the context agents read before working | `id`, `key` (e.g. `AUTH`), `name`, `brief`, `status`, `metadata` (JSON) |
| Task | The unit of work | `id` (UUIDv7), `key` (e.g. `AUTH-42`), `project_id`, `title`, `description`, `status`, `priority`, `due_on` + `due_at` + `due_tz`, `scheduled_on` + `scheduled_at` + `scheduled_tz`, `parent_id`, `metadata` (JSON), `version`, `source`, `external_id`, `deleted_at` |
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

**Claims and leases.** An agent claims a task with a TTL: 30 minutes by default, configurable per workspace, project and claim, capped at 24 hours. The agent renews with a heartbeat about every third of the TTL. If it crashes, the task frees within the TTL. A human's claim has no TTL unless they ask for one (see Decisions). A claim is one atomic statement, so two agents can never hold the same task.

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
- **Time.** System timestamps are UTC. Due dates and scheduled days are a calendar date with an optional time and an IANA time zone.
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
- **Server.** `127.0.0.1:7241` by default, configurable (T-R-A-I/L; no IANA assignment). Stays running once auto-started. `trailhead service install` for launchd/systemd later.
- **Remote access.** Through a tunnel to the loopback port, so the server never listens publicly. iroh (`dumbpipe`) first: end-to-end encrypted QUIC with NAT traversal, and only peers holding the ticket can connect. Bearer tokens still decide who can do what. If the server's machine is asleep, remote agents wait; leases expire as usual, so nothing is lost.
- **Backups.** Not built in. The data is one SQLite file; Litestream or a periodic copy covers it.
- **Observability.** `tracing` structured logs carrying request and batch IDs.
- **Testing.** Concurrency tests in the core suite and CI: agents racing to claim one task, concurrent edits hitting ETag conflicts, bulk reverts racing new writes.

## Phases

1. **Core, local.** Schema and migrations, domain logic, projects, typed comments, event log with undo and outbox, claims and the ready queue, with tests.
2. **Server and CLI.** axum REST under `/v1`, OpenAPI via utoipa, SSE, MCP via rmcp, token auth, reminders. CLI as a REST client that auto-starts a local server.
3. **Clients.** Lane's own app, an OpenClaw plugin, `llms.txt` and an agent skill, webhooks. TypeScript SDK from OpenAPI later.
4. **Teams.** Deferred until a second person needs it. Other humans join the same server with their own accounts; the data model already carries workspaces, users and per-human permissions. Multiple workspaces and multi-process hosting (Turso, shared rate limits) come only if needed.
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
- **Review (Phase 1).** With `require_review`, agents cannot move a task to `done`; only humans can move `in_review` to `done`, in every project. **Delegated review (Phase 3):** a human can list agents as a project's `reviewers`. Those agents can approve (`in_review` → `done`, or `done` in a review-required project) like a human. Only humans can change the list, so agents can't grant themselves approval. For the same reason agents can turn `require_review` on but not off, and can't archive or unarchive a project; core checks this on updates and on undo/revert, so an agent can't loosen review by undoing a human's change. The other way to finish, used when a project doesn't require review, is unchanged: workers finish with `done` plus a handoff, and the coordinator merges.
- **Requests and batches (Phase 2).** Every HTTP request is one batch; its id comes back in the `trailhead-batch` header so a client can undo exactly what it just did. `trailhead undo` with no arguments undoes the caller's last batch.
- **Versions in bodies (Phase 2).** `PATCH` accepts `version` in the body as well as `If-Match`, for clients that can't set headers.
- **Creating with dependencies (Phase 2).** `POST /v1/tasks` takes `blocked_by` and `blocks`, so an agent can file a blocking subtask in one call.
- **Local credentials (Phase 2).** On first start the local server bootstraps the workspace and user and saves the user's CLI token to the OS keyring, or a `0600` file in the config directory when no keyring is reachable. `trailhead agent add` on a local server saves the agent's token, and `--as <name>` uses it.
- **Keyring (Phase 2).** The CLI reads a token from `TRAILHEAD_TOKEN`, then the OS keyring, then the `0600` file (`credentials`, or `agents/<name>`), and writes to the keyring when it can, else the file. It uses `keyring` 4 (pure-Rust D-Bus on Linux, so it builds without a secret service); any keyring error at runtime falls through to the file. Entries use service `trailhead:<absolute config dir>` and account `user` or `agent:<name>`, so side-by-side instances and tests never share tokens. `TRAILHEAD_NO_KEYRING=1` turns the keyring off; the CLI tests set it. Saving an agent token to the keyring deletes any older file copy. Existing agent token files are read as they are, not migrated. Nothing writes the `user` keyring entry yet; it is read so a future `login` can use it. The server and CLI share one credential store (`trailhead_server::creds`). At startup the server checks the local user's credential from the keyring or the file. If it's valid but still in a plaintext file, the server moves it into the keyring. It mints a new token only when neither has a valid one.
- **Default identity (Phase 2).** The nearest `.trailhead.toml` at or above the working directory can set `agent = "<name>"` and `project = "<KEY>"`. Identity precedence: `--token`/`TRAILHEAD_TOKEN`, then `--as`/`TRAILHEAD_AGENT`, then the file's `agent`, then the user's credential. `--as-me` skips `TRAILHEAD_AGENT` and the file. The file's `project` is the default `-p` for `ready`, `ls` and `add`, after `-p`/`TRAILHEAD_PROJECT`. Unknown keys are an error so typos don't silently fall back to acting as the user.
- **Dates in the CLI (Phase 2).** A date-only due date means the end of that day in the local zone, using that day's UTC offset (DST included), and the CLI records the zone's IANA name in `due_tz`. (Superseded by due days, Phase 3, below: the day is now stored as a day.) The CLI uses `jiff` and the system zoneinfo for this and for displaying local times. The server rejects a `due_tz` that isn't a known IANA zone, checked with `jiff`. Both build `jiff` with `tzdb-bundle-always`, so a machine or container image without `/usr/share/zoneinfo` falls back to a copy of the database compiled into the binary; the system copy still wins when present, so OS tzdata updates apply.
- **Local server lifecycle (Phase 2).** `/v1/health` reports the server's pid, binary path and that binary's mtime at startup. When the CLI finds a local server running from its own path but the file on disk is newer, it stops the server with SIGTERM (which drains in-flight requests) and starts a fresh one. Windows has no SIGTERM, so there the server waits on a named event, `Local\trailhead-stop-<pid>`, which `trailhead server stop` sets to get the same drain; a server without the event (an older binary) is killed with `taskkill /F`. Windows also can't replace a running executable, so there the CLI runs the server from a copy in `<data dir>/bin` (deleting copies no server holds) and passes the installed path in `TRAILHEAD_SERVER_EXE`, which the server reports as its binary; `cargo install` then works with the server up and the restart-on-upgrade check is unchanged. The CLI starts the server detached (`DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`) so closing the terminal doesn't take it down. A server running a different binary is left alone, so dev builds and the installed CLI don't fight. The pid is also kept in `server.pid` in the data directory. **Tunnels (Phase 3):** a tunnel puts another machine's server on a loopback port, so "local" can't mean the host alone. Each data directory gets a random id in `<data dir>/instance`, which `/v1/health` reports; the CLI only restarts, stops or auto-starts a server whose id matches its own. A loopback URL that answers with someone else's id is remembered in `<data dir>/tunnels`, and when it stops answering the CLI errors instead of starting an empty server in its place; `trailhead server start` there makes the port this machine's again. `/v1/health` returns the pid, binary and database paths only to callers with a valid token, since paths name the local user. Tokens for a server that isn't this machine's are saved per server, in `<config dir>/servers/<instance>/` (and a keyring service named after it), so one machine can hold tokens for its own server and others. `trailhead agent add <name> --print-token` prints a token without saving it; `trailhead login` on the other machine reads it from stdin, asks `/v1/me` whose it is, and saves it as that agent (or the user). It also records the URL as the machine's default in `<config dir>/config.toml` (`url`), below `--url` and `TRAILHEAD_URL`; per machine rather than in `.trailhead.toml`, which repos check in.
- **MCP (Phase 2).** `/mcp` uses rmcp's streamable HTTP transport in stateless mode with plain JSON responses, behind the same bearer tokens as REST. Each tool call is its own undo batch. Tools answer in the CLI's terse one-line-per-task text rather than JSON, and domain errors come back as tool errors (`error[code]: message`) the agent can act on instead of protocol failures. Project briefs are resources at `trailhead://projects/<KEY>/brief`. Agents can `undo_last`, which undoes only their own most recent batch with the usual conflict check. `list_projects`, `show_project`, `create_project` and `update_project` let agents start a project and write its brief (MCP clients don't all surface resources, so `show_project` serves the brief as a tool too); the usual scope rules apply, so scoped tokens can't create projects. `update_project` covers only name and brief: review settings and archiving stay REST/CLI-only. Revert, other people's batches, agents and tokens stay REST/CLI-only.
- **Event stream (Phase 2).** `GET /v1/events/stream` pages through the events table by id, so `Last-Event-ID` resumes with nothing missed. A commit signal inside the process wakes streams immediately, and a 2-second poll covers writes from other processes. Without a cursor the stream starts at the newest event. On shutdown the server ends open streams and allows in-flight requests 10 seconds before exiting. `trailhead watch` follows the stream and reconnects on its own.
- **MCP over stdio (Phase 2).** `trailhead mcp` bridges stdio to the server's `/mcp`, one POST per JSON-RPC message, authenticated with the CLI's credentials or `--as <agent>`. MCP clients that launch a command (Claude Code's `.mcp.json`) can then connect with no token in their config, and they get the CLI's server auto-start. The bridge outlives the server it connected to: if a request can't connect to the local server (stopped, or killed along with the process tree that started it), it starts the server again and retries once, running the binary now at its own path even if a reinstall replaced the file under it.
- **Idempotency keys (Phase 2).** Non-GET `/v1` requests may send `Idempotency-Key` (1-255 characters). Keys are scoped to the token that sent them, so two agents, or two tokens of the same agent with different scopes, never share stored responses. Replays still count against the token's rate limit. The request's fingerprint is its method, path with query, and a SHA-256 of the body. A middleware outside the per-request batch layer reserves the key before running the handler: a retry of the same request replays the stored status, body, `ETag`, `Location` and the original `trailhead-batch` with `idempotent-replayed: true`; the same key on a different request gets `422 idempotency_key_reused`; a duplicate while the first is still running gets `409 idempotency_key_in_flight` rather than waiting. Only 2xx responses are stored, so a failed request can be retried with its key. An unfinished reservation is released if the request is dropped and treated as abandoned after 5 minutes (a crashed server). Keys live 24 hours from first use and are pruned lazily on the next keyed request. The CLI doesn't retry writes, so it sends no key. Over MCP, `create_task` and `comment` take an optional `idempotency_key` argument that works the same way, stored in the same table under a `mcp/<tool>` namespace per token; the other tools already give the same result when repeated.
- **OpenAPI (Phase 2).** `GET /v1/openapi.json` serves an OpenAPI 3.1 document generated by `utoipa` from `#[utoipa::path]` annotations on the handlers, unauthenticated. Core model types derive `ToSchema` behind an optional `openapi` feature on `trailhead-core`, which the server enables. The bearer scheme, the `trailhead-batch` header and the 400/401/500 error responses are added to every operation by one modifier rather than repeated per handler; route-specific errors, `ETag` and `If-Match` are annotated on each route. The error `code` enum is a list kept in step with `Error::code()` by a unit test, and a test fails when a route is added without being documented. No Swagger UI.
- **Token scopes (Phase 2).** A token may be limited to a set of projects (`tokens.projects`, a JSON array of ids; NULL or empty means all). The scope rides on `Ctx.projects` and is enforced in core, so REST, MCP and SSE all get it. Reads of anything outside the scope (tasks, projects, comments, events, ready, search, undoing someone else's event) answer `not_found`, as do writes that name an out-of-scope task, so a scoped token can't probe for existence. Writes that name an out-of-scope project outright (creating a task in it, updating it) are `forbidden`, because the caller already knows the project. Scoped tokens can't create projects, agents or tokens. Parents and dependencies must sit entirely inside the scope; existing cross-project blockers still keep tasks out of `ready`, and show up as `(out of scope)` in `blocked_by` and `blocked_by` errors so an agent knows why it's stuck without learning what blocks it. A scoped token can undo or revert only events that touch nothing outside its scope (`forbidden` otherwise).
- **Rate limits (Phase 2).** `tokens.rate_limit_per_minute` (NULL means unlimited) is enforced in the server, on REST and `/mcp` alike, by an in-memory token bucket per token id: a full minute's burst, refilling continuously. Over the limit is `429` with code `rate_limited` and `Retry-After` in seconds. Buckets reset when the server restarts and aren't shared between processes; a hosted multi-process deploy needs a shared store. Limits are set when a token is issued (`POST /v1/tokens`, `POST /v1/agents`, `trailhead agent add --projects A,B --rate-limit N`) and changed in place with `PATCH /v1/tokens/{id}` (`trailhead token edit`): `projects` and `rate_limit_per_minute`, where an omitted field stays and `null` lifts the limit. Like revoking, only an unscoped human can change tokens, theirs and their agents'; agents can't widen their own. Changes apply on the token's next request.
- **Events archive (Phase 2).** `Store::archive_events` moves events older than 90 days (by the Store clock) to `events_archive`, same columns and ids, in batches of 500 with one transaction each and at most 20 batches per call; the local server runs it hourly. It moves only a prefix of `events` by id, stopping at the first event inside the window, so an event still in `events` never has a later event in the archive and undo's conflict check keeps reading `events` alone. `list_events` and SSE read only `events`; `GET /v1/tasks/{task}/history?archived=true` also reads the archive. Undo of an event or batch older than 90 days, archived or not, fails with `409 undo_window_expired`. Bulk revert with a `since` older than 90 days is clamped to the window rather than failing, since "revert everything this agent did" is the common call.
- **Reminders (Phase 2).** `Store::emit_due_reminders` logs `task.due_soon` once an open task (not done, cancelled or deleted, in an active project) is within 24 hours of `due_at`, and `task.overdue` once `due_at` passes; a task first seen after its due date gets only `task.overdue`. The local server runs it every 60 seconds next to the claim reaper. `reminders_sent` records the `due_at` each reminder fired for, so each fires once per due date across restarts and fires again when the due date moves. Reminders are attributed to the workspace's first user (the owner), since no one acted and the task's creator may be an agent. They are notifications, not changes: their before and after are the same snapshot, undo and revert skip them, they never count as undo conflicts, and `changes_only` on `GET /v1/events` hides them so "undo my last change" in the CLI and MCP doesn't land on a reminder.
- **Scheduled days (Phase 3).** A task's `due_at` is its deadline; `scheduled_on` is the day someone plans to work it, so a day can be planned without losing the final due date. It's a plain `YYYY-MM-DD` with no zone (migration 0012), because "today" is a day on the planner's calendar, not an instant; the CLI resolves `today` and `tomorrow` in the local zone. `GET /v1/tasks?scheduled_through=DAY` (MCP `list_tasks` `scheduled_through`, CLI `ls --today`) lists tasks scheduled on or before that day, so a plan that slipped stays visible until it's done or moved. Scheduling doesn't change the ready queue's order and fires no reminders; deadlines do that. Event snapshots from before the column load with `scheduled_on` unset. **Scheduled times (Phase 3):** a task can also be planned for a time ("today at 1pm"): `scheduled_at` (UTC) plus an optional IANA `scheduled_tz` (migration 0013). The day stays in `scheduled_on`, which is still what `scheduled_through` filters on. Core derives the day from the time when a time or zone is given: the time's day in `scheduled_tz`, or at the timestamp's own offset without one, and rejects a `scheduled_on` that disagrees. It re-derives only then, because a stored time is UTC and has lost the offset. Setting a new `scheduled_on` alone drops the time ("move it to tomorrow" shouldn't carry 1pm along); clearing `scheduled_at` keeps the day; clearing `scheduled_on` clears all three. `sort=scheduled` orders by day, then timed tasks by time, then untimed ones by priority, unscheduled last; `ls --today` and MCP `list_tasks` with `scheduled_through` use it. The CLI's `--on` takes `today 1pm`, `tomorrow 9:30am`, `2026-10-20 13:00` or a time alone for today, in the local zone.
- **Due days (Phase 3).** Deadlines take the same shape as scheduling, because a timestamp alone can't tell "due Friday" from "due Friday at 11:59pm": `due_on` is the day, `due_at` an optional time, `due_tz` the zone (migration 0014). Unlike scheduling, `due_tz` is allowed without a time, since a day-only deadline still has to end somewhere: at 23:59:59 that day in `due_tz`, or UTC without one. That moment is stored internally as `due_by` (not in the API), recomputed on every write, and it is what reminders, `due_before` and the priority and ready orderings use; `reminders_sent.due_at` records it. The patch rules match scheduling: a new `due_on` alone drops the time (keeping the zone), a `null` `due_at` keeps the day, a `null` `due_on` removes the deadline. This changes one REST behavior: `due_at: null` used to remove the deadline and now keeps its day. Migration 0014 backfills existing rows in Rust (`backfill_due_days`), since it needs zone rules: a deadline at exactly 23:59:59 in its `due_tz` (what a date-only `--due` used to store) becomes day-only and any other becomes that day plus its time, with `due_by` left at the old instant so no reminder fires again. Event snapshots from before 0014 have no `due_on`; restoring one derives it from `due_at`. The CLI's `--due` takes everything `--on` does (`fri 5pm`, `today 5pm`, `2026-10-24`), both take weekday names (the next one, counting today) and RFC 3339 timestamps, and task lines show a due time when there is one.
- **Webhooks (Phase 3).** Each webhook keeps a cursor into the events table (the outbox), so delivery is ordered, at-least-once and survives restarts; a webhook starts at the newest event when created. The server loop wakes on every commit and polls every 2 seconds. Each POST carries the event JSON and `trailhead-signature: t=<unix>,v1=<hex HMAC-SHA256 of "<t>.<body>">` keyed by a `whsec_` secret shown once at creation (stored in plaintext because it's needed to sign), plus `trailhead-event-id`, `-event-kind` and `-webhook-id`. Receivers should reject old timestamps. Non-matching events just move the cursor. A failure stops that webhook's queue (no out-of-order delivery) and backs off 2^n seconds, capped at an hour; 20 failures in a row disable it. Every attempt is logged (`GET /v1/webhooks/{id}/deliveries`). Only humans with unscoped tokens can manage webhooks, since one can send any workspace data anywhere. **SSRF (Phase 3):** `AppState::webhook_targets` is `PublicOnly` by default, and only the local server (`serve_local`) sets `Any`, since posting to localhost or the LAN is the point there. `PublicOnly` resolves names through a resolver that keeps only public addresses, checked when connecting so DNS rebinding can't get past it, rejects private IP literals before sending, doesn't follow redirects and ignores proxy settings from the environment. Blocked: loopback, private, link-local (cloud metadata), CGNAT, multicast, reserved, documentation and benchmarking ranges, unique-local and link-local IPv6, NAT64 and IPv4-mapped forms. A blocked target is an ordinary failed delivery, so it shows in the delivery log and backs off. Targets are checked at delivery, not at creation, because DNS can change in between.
- **Listing (Phase 3).** `GET /v1/tasks` takes `sort` (`priority`, the default, or `updated_at`/`created_at`, `-` for newest first, ties broken by id) and pages with `limit` and `offset`. Offset rather than a cursor because the sort keys aren't unique or stable (`updated_at` moves on every edit), so a cursor wouldn't avoid skips or repeats under concurrent edits either; clients that need an exact log should read events. `GET /v1/events` takes the stream's `kinds` prefix filter, applied in SQL before `limit`. Both `GET /v1/events` and the stream take `project=KEY` (events on its tasks and on the project itself), and `GET /v1/events` takes `before=<id>` so a client can page back with `newest_first`. `include=claim` on `GET /v1/tasks` adds each task's live claim plus the holder's name (one extra query for the page), opt-in so the default payload stays a plain task. `include` also takes `open_questions` and `last_handoff` (one query each per page), so an inbox of waiting tasks is a single request rather than a detail fetch per task.
- **Human claims (Phase 3).** A claim held by a person (not an agent, even one acting on their behalf) gets no expiry unless a `ttl` is passed; `claims.expires_at` is nullable (migration 0011) and the API returns `null`. Leases exist so a crashed agent's task comes back, and people don't crash: they step away, and expiring their claims made them renew by hand and let agents take work mid-lunch. Rescue still works because any human can release or finish a task someone else holds. Renewing a never-expiring claim without a `ttl` is a no-op with no event. To keep forgotten claims visible, `trailhead mine` and `trailhead ready` show each held claim's age.
- **Search results (Phase 3).** `GET /v1/search` takes `project` and `status`, and each hit carries `match: {field, snippet, comment_id}`: the first of title, description or the newest matching comment, with an FTS5 `snippet()` excerpt. The snippet is plain text; markers are used internally to tell which column matched and are stripped, so clients needn't sanitise markup and highlight query terms themselves.
- **Agent docs (Phase 3).** `GET /llms.txt` serves one page of markdown (llms.txt convention: H1, summary blockquote, sections) from `crates/trailhead-server/src/llms.txt`, at the root rather than under `/v1` so it needs no token and stays out of the OpenAPI document. It covers auth, the claim loop, statuses, review, dependencies, ETags, idempotency, undo, events and every error code with its HTTP status; tests fail if a code from `ERROR_CODES` is missing or listed with a status the server doesn't return. The workflow and etiquette for agents ship as a portable skill in `skills/trailhead/SKILL.md` (CLI and MCP side by side), separate from `llms.txt` because one teaches how to behave and the other how to build a client.
- **OpenClaw (Phase 3).** No plugin for now. OpenClaw mounts MCP servers (`openclaw mcp set trailhead '{"command":"trailhead","args":["--as","openclaw","mcp"]}'`) and loads Agent Skills, so Trailhead's MCP server and `skills/trailhead` cover the integration; see `docs/integrations/openclaw.md`. OpenClaw's Workboard has no external-backend interface, and syncing would break the no-sync principle, so the two stay separate. A TypeScript tool plugin is worth building only for something MCP and webhooks can't do.
- **Sub-agents (Phase 3).** Agents create their own sub-agents explicitly; they never borrow a human's credentials to do it. A sub-agent is named `<parent>/<name>`, acts for the same human, and is recorded on every write it makes. Tokens an agent issues go only to its own sub-agents and inherit the issuing token's projects and rate limit. A request for more is `forbidden`. Revoking an agent (`POST /v1/agents/{agent}/revoke`, by the owner or any agent above it) revokes everything under it at once. Over MCP, one connection can serve several agents: `register_agent` creates a sub-agent, and the `as_agent` argument on any tool acts as one that is already registered. Unregistered names are refused. This is how OpenClaw's agents, which share one MCP server, each get their own identity. Locally, sub-agent tokens are saved as `agents/<parent>.<name>`.
- **One server, reachable anywhere (Phase 3).** Local and hosted modes merge into one: the same binary and SQLite file, run wherever is convenient, reached from other machines over a tunnel. A replicated operation log (each machine with its own copy, synced over iroh) was considered and rejected: claims need one authority, since two offline copies could each grant the same task, and the event log stores entity snapshots for undo rather than mergeable operations. Fly.io + Turso and shared rate limits (TRAIL-61) are deferred until something needs more than one server process. Multi-user stays in the data model, so code shouldn't assume Lane is the only human.

## Non-goals

- An official Trailhead app, for now.
- Two-way sync with any other tool.
- Replicas, offline sync or merging between instances.
- Browser clients calling the API directly.
- Agents that act without an owning user.

## Later

- Recurring tasks (iCalendar RRULE plus a time zone).
- Project-level dependencies.
- TOON as an optional compact output format.
