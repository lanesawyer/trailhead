# Trailhead

Headless, agent-native task engine in Rust + SQLite. AGPL-3.0.

Read `docs/PLAN.md` before making design decisions. It is the source of truth for architecture, data model, and phase order. If a change conflicts with the plan, flag it rather than silently diverging, and update the plan when a decision changes.

The software factory being built into Trailhead is designed in `docs/factory/`. Start at `docs/factory/README.md`, which says which file covers what, and open only the one your work needs.

Current phase: 4 (factory MVP, TRAIL-75). The task engine's core is in `crates/trailhead-core`; its server, MCP and CLI are in `crates/trailhead-server` and `crates/trailhead`. The factory's crates are `crates/trailhead-factory` (types and traits), `crates/trailhead-control` (control plane) and `crates/trailhead-testkit` (fakes).

## Development

- `cargo test` runs everything, including the concurrency suite in `crates/trailhead-core/tests/concurrency.rs`.
- `cargo clippy --all-targets` and `cargo fmt --all` must be clean.
- Migrations are numbered `.sql` files in `crates/trailhead-core/migrations`, registered in `src/db.rs`. Never edit an applied migration; add a new one.

## Tracking work

Work on Trailhead is tracked in Trailhead, in the `TRAIL` project. This is how we dogfood it, so it isn't optional:

- **Start of a session:** run `trailhead --as claude ready -p TRAIL` and read the brief with `trailhead project show TRAIL`.
- **Before writing code,** claim a task. If what Lane asked for has no task, create one and claim it. Don't do untracked work.
- **While working,** renew the claim on long tasks, record decisions as `decision` comments, and use `ask` for anything only Lane can answer.
- **When finished,** close the task with a handoff (`done -m`). If you stop partway, leave a `handoff` and `release` the task so the next agent can pick it up.
- **Follow-ups and bugs** you notice become new tasks, not scope creep.
- **Friction with Trailhead itself** is the most valuable thing to report: file it as a task.

In Claude Code, prefer the `trailhead` MCP tools (`mcp__trailhead__ready`, `claim`, `comment`, `update_task`, …), which act as the `claude` agent; `.mcp.json` runs them through `trailhead --as claude mcp`. Use the CLI for what MCP doesn't cover (undo, revert, agents, tokens, `watch`) and when acting as a different agent.

Agents act through their own identity, never Lane's: pass `--as <agent>` (or set `TRAILHEAD_AGENT`). A `.trailhead.toml` with `agent = "<name>"` at or above the working directory does the same; `--as-me` overrides it. This repo's `.trailhead.toml` makes `claude` the default here (with `TRAIL` as the default project), so plain `trailhead` commands act as `claude`; Lane uses `--as-me`. Claude Code uses `claude`, whose token is already saved locally. Other agent tools should ask Lane to run `trailhead agent add <name>` rather than borrowing `claude` or acting as Lane. When `claude` starts worker agents, it creates their identities itself as sub-agents (`trailhead agent add openapi` here makes `claude/openapi`, used with `--as claude/openapi`), never with `--as-me`.

```sh
trailhead --as claude ready -p TRAIL          # what's next
trailhead --as claude claim TRAIL-3           # take it; prints the last handoff
trailhead --as claude renew TRAIL-3           # leases last 30 minutes; renew on long work
trailhead --as claude comment TRAIL-3 "..." -k decision
trailhead --as claude ask TRAIL-3 "..."       # parks the task until Lane answers
trailhead --as claude done TRAIL-3 -m "handoff: what changed, anything surprising"
trailhead --as claude add "..." -p TRAIL -P 2 # file follow-ups instead of scope-creeping
```

`trailhead` is installed from this repo with `cargo install --path crates/trailhead --locked`. The CLI starts a local server in the background and restarts it automatically after a reinstall. `trailhead server status|start|stop|restart` manages it by hand; a dev build (`target/debug/trailhead`) never restarts a server started from a different binary.
