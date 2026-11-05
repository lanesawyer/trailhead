# The control plane inside the server

> **Proposal, for Lane to approve (TRAIL-838).** This is the plan for TRAIL-164, with the CLI feature (TRAIL-116) and the pieces that fold in with it. Nothing here is built. Facts are from the code at `56734a0`; guesses are marked **Guess**. It touches Decided items in [overview.md](overview.md) and [state.md](state.md) only to carry them out ("the server is the control plane when the `factory` feature is on"), but the lifecycle and config choices below are new.

## Why

Today `trailhead-factory run` (crate `trailhead-factory-host`) is a second process beside `trailhead serve`. It opens the same SQLite file as a second `Store`, holds its own iroh endpoint, and forwards the daemons' API tunnel and Lookout's `trailhead/ui/0` streams to the server's loopback port. That costs:

- **Polling across processes.** The server's `Store::changes()` watch (bumped after each commit in the process) never fires for the factory, so `wake.rs` reads the events table every second (TRAIL-158). It is a 1-second timer and a second knob (`--poll-secs`) where the commit itself could wake the loop.
- **Two processes to keep up.** The CLI starts and restarts the server by itself; nothing starts or restarts the factory. `mise run install` can only warn about a stale one.
- **A hand-made lock.** `control.lock` stops two control planes on one machine (TRAIL-113); the server already has one-instance logic (`server.pid`, the `instance` id).
- **Two endpoints for Lookout.** Lookout dials the factory's iroh endpoint for everything, and when the factory is down it can't reach the server either, hence the "factory stopped" fallback to a loopback URL that is useless away from the machine.
- **A second set of files** (`status.json` loopback surface, bearer token, `ui-allowed`) next to the server's own.

## What moves where

| Today (`trailhead-factory-host`) | After | Notes |
| --- | --- | --- |
| Run loop, reconcile, merger, reviews, `Host`, `StatusBoard` (`trailhead-control`) | Unchanged crate, started by the server | One `Factory` per repo, as now. |
| `CoreTracker` over an `Arc<Store>` | Same, on the server's own store | `AppState.store` is a `Store` by value; it becomes `Arc<Store>` (small, mechanical). The tracker then needs no second SQLite connection pool. |
| `wake.rs` (poll the events table) | Deleted; run loops wait on `store.changes()` | See [Lifecycle](#lifecycle). |
| iroh endpoint, `trailhead/api/0` tunnel, daemon protocol (`RemoteDaemon`, `Peers`), `trailhead/ui/0` | Bound by the server's factory module | The API tunnel's target becomes the server's actual listen address (from the listener, not `--server`); `server_check` becomes unnecessary because the server is the process asking. |
| `surface.rs` loopback status port and `status.json` | Routes on the server (`GET /v1/factory/status`, `/events`) behind the normal bearer token | Retires a second token file and port. `status` and `logs --follow` become ordinary API clients. **Guess:** the iroh `{"op":"status"}` op stays, fed by the same boards. |
| Credentials: env, then fnox (`fnox.rs`), then the keyring (`login.rs`), TRAIL-545 | Same code, moved to a `factory` module | See below. |
| Docker compute and its gateway container (`trailhead-compute-docker`) | Built in with a `docker` sub-feature | The gateway is a container the compute manages, not part of the server's request path. |
| `ops.rs` (`status`, `logs`), `allow`/`disallow`/`allowed`, `login`, `check` | CLI subcommands (`trailhead factory ...`) | They talk to the server API where a server is needed; `login` and `allow*` stay file/keyring operations. |

### Credentials

The factory resolves Sprites, GitHub, GitHub-writer and Claude tokens from the environment, then fnox (run in the working directory, so it finds the repo's `fnox.toml`), then the keyring. That order stays, with one change forced by the move: the server is usually started in the background by some CLI call, in whatever directory and environment that call had, so the environment and the working directory are accidents. **Proposal:** resolve at control-plane start, not at server start; look in fnox with the directory of each repo in the `repos` list (not the process's cwd), and treat the keyring and `trailhead factory login` as the supported unattended path. The server logs which source each secret came from (never the value), and `trailhead factory check` prints the same. The iroh key and daemon master key stay in the credential store under `factory-keys`, so sandboxes started by the old process are still reachable after the switch.

### Crates and the `factory` feature

The server gains, behind a `factory` feature, these dependencies: `trailhead-factory`, `trailhead-control`, `trailhead-daemon` (with `net`, for `ApiTunnel`, `UiProtocol`, `RemoteDaemon`), `trailhead-compute-sprites`, `trailhead-harness-claude-code` (through the daemon registry), `trailhead-provider-anthropic`, `trailhead-codehost-github`, `iroh`, and, under `factory-docker`, `trailhead-compute-docker` (and so `bollard`). The code from `trailhead-factory-host` becomes a `factory` module in `trailhead-server` (**Guess:** or a new `trailhead-factory-server` crate depended on by `trailhead-server`, if the module grows past what `lib.rs` should hold; a separate crate also keeps the compile-time cost out of the server's own tests).

Rules that keep the engine lean:

- `trailhead-server` and `trailhead` each have `factory` (default on once TRAIL-116 lands, off until then) and `docker`. With `--no-default-features` the server's dependency tree is today's, and `trailhead factory ...` prints "built without the factory feature". A CI job builds `-p trailhead --no-default-features` and checks `cargo tree` has no `iroh`.
- `trailhead-core` and `trailhead-client` never depend on the factory crates, as now (principle 10). The CLI keeps the `[factory]` tables unparsed in `.trailhead.toml` and `config.toml`; only the server's factory module parses them.
- **Publishing.** The factory crates are `publish = false`, and a published crate can't depend on an unpublished one, even optionally. So `cargo install trailhead` stays the lean engine until the factory crates are publishable; until then the `factory` feature builds from this repo (`cargo install --path crates/trailhead --features factory`). Publishing them (or vendoring the control plane into one crate) is a decision for Lane (see questions).
- The daemon stays a separate binary built for Linux x86_64 and uploaded to sandboxes; it is not linked into the server (`daemon_binary::resolve` is unchanged).

## Lifecycle

**Start.** `trailhead serve` (which the CLI spawns) starts the control plane after the HTTP listener is bound and the store is open, and only if the factory is configured: the `factory` feature is on **and** the user config has `[factory] enabled = true` or lists `repos` (**Guess** on the exact key; the point is an opt-in). Opt-in matters because any `trailhead` command can auto-start the server, and a bare `trailhead ready` must never begin spending money on sandboxes. A server with the feature but no repos listed behaves exactly as today.

Start order: take the control-plane lock, read configs, resolve credentials, find the daemon binary, bind the iroh endpoint, build each repo's `Factory`, then spawn the run loops. A configuration error (a repo whose file won't parse, colliding queues, a missing credential for the chosen compute) is a failed control plane, not a failed server: the server logs the error, keeps serving the API, and reports `control: failed(reason)` in `/v1/factory/status`, so a person can still reach tasks to fix things. `trailhead serve --require-factory` (and `mise run factory`) exit instead, for people who prefer that. **Guess:** failing soft is the better default because the CLI, MCP and Lookout all depend on that server.

**Stop and drain.** Today the factory process treats the first Ctrl-C as drain (no new runs, in-flight runs finish) and the second as cancel, and the server stops on SIGINT or SIGTERM after finishing requests (`DRAIN`). Both can't hang on the same signal. **Proposal:**

- SIGTERM or the first SIGINT stops the HTTP listener's *new work* in the factory sense: the control plane drains. The server keeps serving requests while runs finish, because runs and sandboxes call the API through the tunnel and need it.
- A second signal cancels the runs (they requeue on the next start, as now), then the server's own graceful shutdown proceeds.
- `trailhead server stop` sends SIGTERM and waits; with runs in flight that can take an hour. So `stop` prints "draining N run(s); run `trailhead server stop --now` to cancel" and `--now` sends the second signal. A drain has no timeout of its own other than the run budgets. **Guess:** that is right for a manual stop.
- `trailhead server restart`, and the CLI's automatic restart after a reinstall, are the dangerous part. The CLI restarts a server whose binary is newer than the one it runs, on any command, with a SIGTERM. A person running `mise run install` while three runs are live would drain a long time or, if the CLI doesn't wait, get killed mid-run. **Proposal:** the automatic restart sends SIGTERM, waits up to its existing timeout, and if the control plane is still draining it leaves the old server running and prints "a newer trailhead is installed; N runs are in flight; restart with `trailhead server restart` when they finish". The old server keeps working, so nothing breaks; a restart is a choice. This is the main change to the CLI's lifecycle.

Cancelling is always safe in the sense the code already guarantees: reconcile on the next start puts things right from compute labels and the tracker ([state.md](state.md)). What is in the AGENTS.md rule for `trailhead-control` (record a verdict or spend before teardown) holds unchanged, since the run loop is the same code.

**Waking.** `Factory::run_draining` takes a `Notify`. Instead of `wake_on_changes` polling `list_events` every second, the server's module runs one task per control plane on `store.changes()`: on each change it reads the events after its cursor (`list_events` with the three kinds, as `wake.rs` does now) and notifies the loops of the repos whose project matches. This keeps the event cursor, because the commit signal is only a hint that *something* committed (it says nothing of what), and the table stays the source of truth. The result is no 1-second timer and no fixed latency, the wake follows the commit, and the periodic `--poll` stays as a safety net (default raised from 30 s, **Guess**: 5 minutes). Writes from another process (a second server on the same file is not supported; the CLI always goes through the server) never reach the signal, which is why the poll stays.

**One instance (TRAIL-113).** The server already guards against a second server on the same data directory (the bind and `server.pid`; two servers on one SQLite file isn't supported). The control plane still takes `control.lock` in `<data dir>/factory`, because `trailhead-factory run` stays available as a fallback for a release (below), and running both would flap daemon connections (they share one iroh identity). The lock stays as it is, with the holder line naming the server pid. Once the standalone binary is deleted, TRAIL-113 is satisfied by the server's own single instance and the lock becomes a belt-and-braces check that can stay.

## Config

- **Per repo (TRAIL-163):** unchanged. Each repo's `[factory]` table in its checked-in `.trailhead.toml`.
- **Per machine:** the user `config.toml`'s `[factory]` table (`max_sandboxes`, `pause_at`, `repos`), unchanged (TRAIL-167, TRAIL-516). It is where the server finds out which repos to run, so `repos` (a list of directories) is now what turns the control plane on. The server no longer has a working directory that means anything, so the "nearest `.trailhead.toml` above the cwd" fallback goes away for the server; `check` and `--repo` keep it for a person at a terminal.
- **One control plane per repo, in one server.** Each listed repo gets its own `Factory` and run loop sharing the store, the endpoint, the compute targets and the `Host` (the cross-repo sandbox cap), as `setup()` does today. The collision and compute/daemon-agreement checks (`repos::validate`) run at start and fail the control plane, not the server.
- **Reloading.** The merge mode is re-read from the repo file each pass (as now). Anything else needs `trailhead factory restart` (**Guess:** drain the control plane and start it again inside the running server, which avoids restarting the HTTP server for a config edit; build it in step 4, not before).
- **Flags that go away.** `--server` (the process knows its own address), `--poll-secs` (config), `--model-endpoint`, `--sprites-token`, `--github-token`, `--claude-oauth-token`, `--daemon` become either `[factory]` keys in the user config or stay environment variables, since a background server has no flags. **Guess:** `[factory] daemon = "<path>"` for the daemon binary, and the others as they are (env, fnox, keyring).
- **What replaces `trailhead factory run`.** `trailhead server start` (and any command that auto-starts it). `trailhead factory run` becomes "run the server in the foreground with the control plane required", i.e. `trailhead serve --require-factory`, so a person who wants to watch logs and Ctrl-C still can, and `mise run factory` keeps meaning that. `status`, `logs`, `allow`, `disallow`, `allowed`, `login` and `check` keep their names under `trailhead factory`.
- **`mise run install`** installs one binary with the features it wants (`cargo install --locked --path crates/trailhead --features factory,docker`) plus the daemon, drops the `trailhead-factory-host` install, and no longer warns about a stale factory process, since the server's restart rules above cover it.
- **Logs.** `factory.log` (rotating file, `logfile.rs`) is joined by the server's own log: **Guess:** the control plane logs into the server's `server.log` with a `repo=` span, and `trailhead factory logs` filters it. Today the server's log is `server_log()` and the factory's is its own file, so this is a small merge, but it does change where a person looks.

## Lookout and remote access

- **One endpoint for everything.** The server binds one iroh endpoint when the factory is on and serves `trailhead/api/0` (daemons only), `trailhead/ui/0` (Lookout: status and the forwarded API) and, in step 5, the person-facing API tunnel. Lookout dials that one id for the board *and* the map. Its `Api` and `Connection` code doesn't change; what changes is who answers.
- **Daemons and Lookout stay separate protocols** ([ui.md](ui.md#transport)): a compromised sandbox still can't reach `trailhead/ui/0` or factory steering, so the allowlist and the "only daemons this control plane started" check carry over as they are.
- **"Factory stopped".** The map's `Stopped` state and the board's fallback to `NetConfig.server_url` exist because the factory process could die while the server lived (or the reverse). With one process, the stream is either up or the whole server is (then nothing answers and Lookout shows "unreachable" as it already does for a dead control plane). What survives is a control plane that is *not running inside a live server*: feature off, no repos configured, or failed to start. The status op then answers with `control: disabled` or `failed(reason)` instead of closing, and Lookout shows that reason over the board. The loopback fallback goes: it was only ever useful on the machine itself. A UI built without the factory feature on the other end gets an unknown-op reset, the same as a control plane without a status feed today.
- **The `{"op":"status"}` frame** gains a `control` field at the top for that. This is a protocol addition (backward compatible, Lookout ignores what it doesn't know). **Guess:** no version bump needed for an added field.
- **TRAIL-71 (remote access, `trailhead tunnel`).** PLAN.md's open item is whether to keep `dumbpipe`/`trailhead tunnel` or embed iroh. With the factory on, the server already holds an endpoint and forwards `trailhead/ui/0` API streams to its own port, which *is* embedded-iroh remote access for Lookout. **Proposal:** TRAIL-71 is answered by this: the server serves a plain `trailhead/api/0`-style person stream on the same endpoint, gated by the existing allowlist plus bearer tokens, and `trailhead login --endpoint <id>` dials it (step 5). That replaces `dumbpipe` instructions for people and agents without the factory. Without the `factory` feature there's no iroh and `dumbpipe` remains the answer; embedding iroh *without* the factory feature (so a lean server is remotely reachable) is a separate choice and **not** proposed here, since it brings iroh into the lean build. Lane decides; TRAIL-71 should be closed or redefined either way.

## Migration in steps

Each step ships alone and leaves `trailhead-factory run` working until step 6.

1. **Arc the store and move the code (no behaviour change).** `AppState.store` becomes `Arc<Store>`; `trailhead-factory-host`'s `setup`, `fnox`, `login`, `repos`, `surface`, `logfile` move into a `factory` module of `trailhead-server` (or the new crate) behind a `factory` feature that is off; the standalone binary calls into it, so there is one copy. Breaks: only the build. Rollback: revert; nothing runs differently. *Task: "Move the factory host's setup into a library behind a `factory` feature".*
2. **Run it in the server, opt-in.** `serve_local` starts the control plane when the feature is on and `[factory] repos` is set, with the soft-fail behaviour and `--require-factory`; `trailhead factory run` becomes `serve --require-factory`; the lock and the standalone binary stay. Breaks: a person with `repos` listed and the old binary also running gets the lock error (clear message); a configuration error is now in the server log. Rollback: remove `repos` (or the feature) and run `trailhead-factory run` as before; reconcile adopts whatever sandboxes the other left. *Tasks: "Start the control plane from the server", "Drain and cancel the control plane on SIGTERM/SIGINT, `server stop --now`".*
3. **Wake from the commit signal.** Replace `wake.rs` with the `changes()` watcher; raise the poll default. Breaks: a missed wake (covered by the poll). Rollback: revert to the interval watcher. *Task (TRAIL-158): "Wake the run loops from the store's commit signal".*
4. **One endpoint, one status surface.** Move the iroh endpoint and the status routes into the server; drop `status.json` and the loopback port; add `control` to the status frame; Lookout drops the loopback fallback and shows `disabled`/`failed`. Breaks: older Lookout against a newer server (it falls back to showing "factory stopped" until updated), and `trailhead-factory status` from the old binary (no port). Rollback: re-run the old binary; Lookout reconnects to its endpoint id, which is the same key, since the keys stay in the credential store. *Tasks: "Serve factory status from the server API", "Lookout: show control-plane state from the status frame, remove the loopback fallback", "`trailhead factory status`/`logs` over the API".*
5. **The CLI feature, install and the restart rule (TRAIL-116).** `factory` and `docker` features on the `trailhead` crate; `trailhead factory <cmd>` built in instead of exec'ing a second binary; `mise run install` installs one binary plus the daemon; the CLI's automatic restart declines to kill a server with runs in flight; CI builds the lean variant. Person-facing endpoint stream if Lane approves it for TRAIL-71. Breaks: scripts that call `trailhead-factory`; a person who relied on the auto-restart killing a stale server. Rollback: `cargo install` the previous tag; the user config is unchanged. *Tasks: "Add the `factory` feature to the CLI and server", "`mise run install` for one binary", "Don't restart a server with runs in flight", "CI: lean build has no iroh", "TRAIL-71 decision".*
6. **Delete the standalone binary.** Remove `trailhead-factory-host` and its lock role, update `AGENTS.md`, `operating.md`, `seams.md`, `PLAN.md` (the TRAIL-164 mentions), `overview.md` and `mvp.md`. Rollback: the previous release's binary still works against the same data, because nothing in the store's schema or the keys changed.

No database migration is needed in any step. The files the factory keeps (`factory-keys`, `ui-allowed`, `spend-*.json`, the Docker gateway's directory under `<data dir>/factory`) stay where they are, so every step can go back.

## What can go wrong

- **The server now spends money.** A bug or a bad config in the control plane runs in the same process that serves the API. Mitigation: opt-in; the control plane's run loops are tasks that can fail without the server (as `run_repo` already does per repo) and panics are caught per loop. **Guess:** don't also catch a panic in the iroh accept loop; let the server stay up and report `failed`.
- **Memory and compile time.** `iroh`, `bollard` and the harness code join the server build; a full build is slower. Hence the feature and the CI lean check.
- **Restart hazards.** Covered above: the CLI's auto-restart and `server stop` must respect running work. This is the one place a person could lose a run, and even then reconcile requeues it.
- **Secrets in a background process.** Environment tokens set in a terminal aren't there when a CLI call elsewhere starts the server; the keyring and fnox lookups are the supported path and `check` shows what resolved.
- **Two `Store`s no more, but one writer.** The control plane's tracker writes through the server's single writer connection, so a long run-loop write (a burst of metadata updates) queues with API writes. It was true before through SQLite's own lock; now it's in-process and visible in the same tracing. **Guess:** not a problem at one machine's scale.

## Open questions for Lane

1. **Opt-in key.** Is `[factory] repos` being non-empty the switch, or do you want an explicit `enabled = true`? (Proposal: `repos` listed, plus `trailhead factory disable` to stop without editing.)
2. **Fail soft or hard** when the control plane can't start inside the server (proposal: soft, with `--require-factory` for hard).
3. **Auto-restart on reinstall:** is leaving a server with runs in flight alone (and telling you) right, or should install drain and restart anyway?
4. **Publishing.** Do the factory crates become publishable so `cargo install trailhead` can include the factory, or does the full build stay a from-the-repo install and `crates.io` stays the engine only?
5. **TRAIL-71:** is the factory's iroh endpoint also the person/agent remote-access path (step 5), or does `trailhead tunnel` stay a separate, factory-free question, with iroh kept out of a lean server?
6. **Module or crate** for the moved host code: a `factory` module in `trailhead-server`, or a `trailhead-factory-server` crate it depends on? (Proposal: a crate.)
7. **One log or two.** Merge the control plane's log into `server.log`, or keep `factory.log` beside it?
8. **Config reload:** is `trailhead factory restart` (drain and start the control plane again inside the server) worth building, or is restarting the server enough?

## Tasks to file

Not filed; each is a step above. Under TRAIL-164 unless noted:

- Move the factory host's setup into a library behind a `factory` feature (step 1; includes `Arc<Store>`).
- Start the control plane from the server, with `--require-factory` and soft failure (step 2).
- Drain and cancel on SIGTERM/SIGINT; `server stop --now` (step 2).
- Wake the run loops from the store's commit signal (step 3; this is TRAIL-158).
- Serve factory status from the server API; `status` and `logs` over it (step 4).
- Move the iroh endpoint into the server; add `control` to the status frame (step 4).
- Lookout: show control-plane state and drop the loopback fallback (step 4, `clients/`, queue TRAIL-685).
- `factory` and `docker` features on `trailhead`; `trailhead factory` built in; one-binary `mise run install` (step 5, TRAIL-116).
- Don't restart a server with runs in flight (step 5).
- CI: lean build has no iroh (step 5).
- Decide TRAIL-71 against this design (step 5).
- Remove `trailhead-factory-host` and update the docs (step 6).
