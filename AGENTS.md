# Trailhead

Headless, agent-native task engine in Rust + SQLite. AGPL-3.0.

Read `docs/PLAN.md` before making design decisions. It is the source of truth for architecture, data model, and phase order. If a change conflicts with the plan, flag it rather than silently diverging, and update the plan when a decision changes.

Current phase: 2 (server and CLI). Phase 1 core is in `crates/trailhead-core`.

## Development

- `cargo test` runs everything, including the concurrency suite in `crates/trailhead-core/tests/concurrency.rs`.
- `cargo clippy --all-targets` and `cargo fmt --all` must be clean.
- Migrations are numbered `.sql` files in `crates/trailhead-core/migrations`, registered in `src/db.rs`. Never edit an applied migration; add a new one.
