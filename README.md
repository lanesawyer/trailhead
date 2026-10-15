# Trailhead

A fast, headless task engine that humans and agents share. It owns the task data and the rules around it: projects, claims with leases, a ready queue, typed comments for handoffs and questions, and an event log where every write can be undone.

There is no app. Every interface is a client: REST under `/v1` for apps people build, MCP at `/mcp` for agents without a shell, and the `trailhead` CLI.

**Status:** Phase 1 (the core library) is done. Phase 2, the server and CLI, is next. See [docs/PLAN.md](docs/PLAN.md) for the architecture and roadmap.

## Development

```sh
cargo test
cargo clippy --all-targets
cargo fmt --all
```

## License

[AGPL-3.0](LICENSE)
