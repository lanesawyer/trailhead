# Trailhead

Headless, agent-native task engine in Rust + SQLite. AGPL-3.0.

Read `docs/PLAN.md` before making design decisions. It is the source of truth for architecture, data model, and phase order. If a change conflicts with the plan, flag it rather than silently diverging, and update the plan when a decision changes.

Current phase: 2 (server and CLI). Phase 1 core is in `crates/trailhead-core`.

## Development

- `cargo test` runs everything, including the concurrency suite in `crates/trailhead-core/tests/concurrency.rs`.
- `cargo clippy --all-targets` and `cargo fmt --all` must be clean.
- Migrations are numbered `.sql` files in `crates/trailhead-core/migrations`, registered in `src/db.rs`. Never edit an applied migration; add a new one.

## Tracking work

Work on Trailhead is tracked in Trailhead, in the `TRAIL` project. Read its brief with `trailhead project show TRAIL`. Agents act through their own identity, never Lane's: pass `--as <agent>` (or set `TRAILHEAD_AGENT`). The `claude` agent's token is already saved locally.

```sh
trailhead --as claude ready -p TRAIL          # what's next
trailhead --as claude claim TRAIL-3           # take it; prints the last handoff
trailhead --as claude renew TRAIL-3           # leases last 30 minutes; renew on long work
trailhead --as claude comment TRAIL-3 "..." -k decision
trailhead --as claude ask TRAIL-3 "..."       # parks the task until Lane answers
trailhead --as claude done TRAIL-3 -m "handoff: what changed, anything surprising"
trailhead --as claude add "..." -p TRAIL -P 2 # file follow-ups instead of scope-creeping
```

`trailhead` is installed from this repo with `cargo install --path crates/trailhead --locked`. The CLI auto-starts a local server that keeps running on the old binary after a reinstall; kill it (`pkill -x trailhead`) so the next command starts the new one.
