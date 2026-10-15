# Trailhead

A fast, headless task engine that humans and agents share. It owns the task data and the rules around it: projects, claims with leases, a ready queue, typed comments for handoffs and questions, and an event log where every write can be undone.

There is no app. Every interface is a client: REST under `/v1` for apps people build, MCP at `/mcp` for agents without a shell, and the `trailhead` CLI.

Agents can learn the API from `GET /llms.txt` on any server, and the workflow from the portable skill in [skills/trailhead/SKILL.md](skills/trailhead/SKILL.md).

**Status:** Phases 1 (core) and 2 (server, CLI, MCP) are done; Phase 3 (clients: webhooks, agent docs, integrations) is under way. See [docs/PLAN.md](docs/PLAN.md) for the architecture and roadmap.

## Development

```sh
cargo test
cargo clippy --all-targets
cargo fmt --all
```

CI (`.github/workflows/ci.yml`) runs `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings` and `cargo test --locked` on every pull request and push to `main`.

## License

[AGPL-3.0](LICENSE)
